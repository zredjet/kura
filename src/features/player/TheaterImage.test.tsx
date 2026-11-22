import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { type AnimDto, getAnim, getAsset } from '../../ipc/asset';
import { animUrl, playUrl, thumbUrl } from '../../ipc/media';
import { IpcError } from '../../ipc/result';
import type { AnimClock, AnimPlayerHooks, DrawFrame } from '../asset/AnimPlayer';
import { imageAsset, summary } from '../asset/testAssets';
import { playerController } from './playerActions';
import { type ChromeProps, ImageLayer } from './Theater';
import styles from './Theater.module.css';
import { usePlayerKeys } from './usePlayerKeys';

vi.mock('../../ipc/asset', () => ({
  getAsset: vi.fn(),
  getAnim: vi.fn(),
  requestAnalysis: vi.fn(async () => true),
}));
vi.mock('../../ipc/jobs', () => ({ onJobProgress: vi.fn(async () => () => undefined) }));
const { togglePlay } = vi.hoisted(() => ({ togglePlay: vi.fn() }));
vi.mock('./playerActions', () => ({
  cycleRepeat: vi.fn(),
  playerController: vi.fn(() => ({ togglePlay })),
  toggleFullscreen: vi.fn(),
  toggleShuffle: vi.fn(),
}));

const t = ja.asset;

const ANIM: AnimDto = {
  hash: 'ab'.repeat(32),
  version: 1,
  rev: 0,
  canvasW: 16,
  canvasH: 16,
  cols: 4,
  frameCount: 8,
  durations: [100, 100, 100, 100, 100, 100, 100, 100],
  tags: [],
};

/** 一覧の概要が「アニメ」の GIF（サムネイルあり）。 */
const GIF = summary({ ext: 'gif', animated: true, frameCount: 8, thumbRev: 2 });

/** 偽の時計：advance で時刻を進め、待っている描く時刻を 1 回分回す（AnimPlayer.test と同じ）。 */
function fakeClock() {
  let now = 1000;
  const queue = new Map<number, () => void>();
  let next = 1;
  const clock: AnimClock = {
    now: () => now,
    request(cb) {
      const id = next++;
      queue.set(id, cb);
      return id;
    },
    cancel(id) {
      queue.delete(id);
    },
  };
  return {
    clock,
    pending: () => queue.size,
    advance(ms: number) {
      now += ms;
      const run = [...queue.values()];
      queue.clear();
      run.forEach((cb) => cb());
    },
  };
}

/** AnimPlayer に差し込む口（描いたコマの数を数える）。 */
function hooks(extra: Partial<AnimPlayerHooks> = {}) {
  const c = fakeClock();
  const drawn: number[] = [];
  const draw: DrawFrame = (_canvas, _image, src) => drawn.push(src.x);
  const load = vi.fn(async () => ({}) as CanvasImageSource);
  return { c, drawn, load, player: { clock: c.clock, draw, load, dpr: 1, ...extra } };
}

/** 後から決める約束（帯ができるまでを止めておく）。 */
function deferred<T>() {
  let resolve: (v: T) => void = () => undefined;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function srcOf(container: HTMLElement): string | null | undefined {
  return container.querySelector('img')?.getAttribute('src');
}

const chrome = (): ChromeProps => ({
  'data-chrome': '',
  onPointerEnter: vi.fn(),
  onPointerLeave: vi.fn(),
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(getAsset).mockResolvedValue(imageAsset());
});

// design.md §3.1「アニメのプレビュー」：シアターでもクイックルックと同じ部品で回す（M12）
describe('シアターの画像', () => {
  it('動かない画像は元のファイル。サムネイルがまだないアニメも元のファイル', async () => {
    vi.mocked(getAsset).mockResolvedValue(
      imageAsset({ effective: { pixelArt: false, animated: false, frameCount: 1 } }),
    );
    const png = summary({ ext: 'png' });
    const still = render(<ImageLayer item={png} active />);
    expect(srcOf(still.container)).toBe(playUrl(png.hash));
    await waitFor(() => expect(getAsset).toHaveBeenCalledWith(png.id));
    expect(getAnim).not.toHaveBeenCalled();
    expect(srcOf(still.container)).toBe(playUrl(png.hash));
    still.unmount();
    const pending = summary({ ext: 'gif', animated: true, thumbState: 'pending' });
    const { container } = render(<ImageLayer item={pending} active={false} />);
    expect(srcOf(container)).toBe(playUrl(pending.hash));
  });

  it('シアターを隠している間は素材の欄も帯も読まず、先頭のコマ（大きいサムネイル）で止める', () => {
    const { container } = render(<ImageLayer item={GIF} active={false} />);
    expect(srcOf(container)).toBe(thumbUrl(GIF.hash, 'l', 2));
    expect(getAsset).not.toHaveBeenCalled();
    expect(getAnim).not.toHaveBeenCalled();
  });

  it('アニメは帯ができるまで先頭のコマ（大きいサムネイル）で、できたら canvas で回す', async () => {
    const strip = deferred<AnimDto>();
    vi.mocked(getAnim).mockReturnValue(strip.promise);
    const h = hooks({ reducedMotion: false });
    const { container } = render(<ImageLayer item={GIF} active player={h.player} />);
    // 帯を待つ間：元の GIF を <img> に置かない（WebKit が自分で回すので）
    expect(srcOf(container)).toBe(thumbUrl(GIF.hash, 'l', 2));
    await waitFor(() => expect(getAnim).toHaveBeenCalledWith(GIF.id));
    expect(srcOf(container)).toBe(thumbUrl(GIF.hash, 'l', 2));
    expect(container.querySelector('canvas')).toBeNull();

    await act(async () => strip.resolve(ANIM));
    expect(h.load).toHaveBeenCalledWith(animUrl(ANIM.hash, 0));
    await waitFor(() => expect(h.c.pending()).toBe(1));
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('canvas')).not.toBeNull();
    act(() => h.c.advance(0));
    expect(screen.getByText('1 / 8')).toBeInTheDocument();
    act(() => h.c.advance(250));
    expect(screen.getByText('3 / 8')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.pause })).toBeInTheDocument();
  });

  it('大きすぎて帯を作れないときは、クイックルックと同じく先頭のコマのまま', async () => {
    vi.mocked(getAnim).mockRejectedValue(new IpcError({ code: 'TooLarge', message: '大きい' }));
    const h = hooks();
    const { container } = render(<ImageLayer item={GIF} active player={h.player} />);
    await waitFor(() => expect(getAnim).toHaveBeenCalledTimes(1));
    // 失敗を受け取ってからも
    await act(async () => undefined);
    expect(srcOf(container)).toBe(thumbUrl(GIF.hash, 'l', 2));
    expect(container.querySelector('canvas')).toBeNull();
    expect(h.load).not.toHaveBeenCalled();
  });

  describe('「視差効果を減らす」', () => {
    const original = window.matchMedia;
    beforeEach(() => {
      window.matchMedia = vi.fn((query: string) => ({
        matches: query === '(prefers-reduced-motion: reduce)',
        media: query,
        onchange: null,
        addListener: vi.fn(),
        removeListener: vi.fn(),
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })) as unknown as typeof window.matchMedia;
    });
    afterEach(() => {
      window.matchMedia = original;
    });

    it('自動で再生せず、先頭のコマと再生ボタン（クイックルックと同じ）', async () => {
      vi.mocked(getAnim).mockResolvedValue(ANIM);
      // reducedMotion は差し込まない（OS の設定を読む本物の道を通す）
      const h = hooks();
      render(<ImageLayer item={GIF} active player={h.player} />);
      await waitFor(() => expect(h.drawn).toHaveLength(1));
      expect(h.c.pending()).toBe(0);
      act(() => h.c.advance(1000));
      expect(h.drawn).toHaveLength(1);
      expect(screen.getByText('1 / 8')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: t.play })).toBeInTheDocument();
    });
  });

  it('操作の行はシアターの操作と同じ印を持ち、操作を隠すと一緒に消える', async () => {
    vi.mocked(getAnim).mockResolvedValue(ANIM);
    const h = hooks({ reducedMotion: false });
    const marks = chrome();
    const Stage = ({ idle, children }: { idle: boolean; children: ReactNode }) => (
      <section className={styles.stage} data-idle={idle || undefined}>
        <div className={styles.media}>{children}</div>
      </section>
    );
    const layer = <ImageLayer item={GIF} active chrome={marks} player={h.player} />;
    const { rerender } = render(<Stage idle={false}>{layer}</Stage>);
    const pause = await screen.findByRole('button', { name: t.pause });
    const row = pause.parentElement as HTMLElement;
    // 「7 / 8」と同じ行：上の操作・再生バーと同じく data-chrome とポインタの出入り（隠してよいかの判定に入る）
    expect(row).toHaveAttribute('data-chrome', '');
    const animControls = styles.animControls ?? '';
    expect(animControls).not.toBe('');
    expect(row).toHaveClass(animControls);
    fireEvent.pointerEnter(row);
    expect(marks.onPointerEnter).toHaveBeenCalled();
    expect(row).not.toHaveStyle({ opacity: '0' });
    rerender(<Stage idle>{layer}</Stage>);
    expect(row).toHaveStyle({ opacity: '0', pointerEvents: 'none' });
  });

  it('Space はプレーヤーの再生・一時停止のまま（アニメの操作の行にフォーカスがあっても奪われない）', async () => {
    vi.mocked(getAnim).mockResolvedValue(ANIM);
    const h = hooks({ reducedMotion: false });
    function Keys({ children }: { children: ReactNode }) {
      usePlayerKeys(true);
      return <>{children}</>;
    }
    render(
      <Keys>
        <ImageLayer item={GIF} active player={h.player} />
      </Keys>,
    );
    const pause = await screen.findByRole('button', { name: t.pause });
    await waitFor(() => expect(h.c.pending()).toBe(1));
    pause.focus();
    // 既定の動作（ボタンを押す）を止めて、プレーヤーの操作にする
    expect(fireEvent.keyDown(pause, { key: ' ' })).toBe(false);
    fireEvent.keyUp(pause, { key: ' ' });
    expect(playerController).toHaveBeenCalled();
    expect(togglePlay).toHaveBeenCalledTimes(1);
    // アニメは回り続ける
    expect(screen.getByRole('button', { name: t.pause })).toBeInTheDocument();
    act(() => h.c.advance(150));
    expect(screen.getByText('2 / 8')).toBeInTheDocument();
  });
});
