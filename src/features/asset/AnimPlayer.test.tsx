import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { AnimDto } from '../../ipc/asset';
import { animUrl } from '../../ipc/media';
import type { Cell } from '../../lib/asset/timeline';
import {
  type AnimClock,
  AnimPlayer,
  displaySize,
  type DrawFrame,
  gridSource,
  stripSource,
} from './AnimPlayer';

const t = ja.asset;

/** 偽の時計：advance で時刻を進め、待っている描く時刻を 1 回分回す。 */
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

/** 描いたコマ（元の矩形）を覚える偽の描く関数。 */
function recorder() {
  const drawn: { src: Cell; smoothing: boolean }[] = [];
  const draw: DrawFrame = (_canvas, _image, src, smoothing) => drawn.push({ src, smoothing });
  return { draw, drawn, last: () => drawn[drawn.length - 1] };
}

const IMAGE = {} as CanvasImageSource;
const load = vi.fn(async () => IMAGE);

const ANIM: AnimDto = {
  hash: 'ab'.repeat(32),
  version: 1,
  rev: 0,
  canvasW: 16,
  canvasH: 16,
  cols: 4,
  frameCount: 8,
  durations: [100, 100, 100, 100, 100, 100, 100, 0],
  tags: [
    { name: 'walk', from: 0, to: 3, direction: 'forward', repeat: 0 },
    { name: 'bounce', from: 4, to: 7, direction: 'pingPong', repeat: 2 },
  ],
};

/** コマ f の帯の中の矩形（4 列の帯）。 */
function cell(f: number): Cell {
  return { x: (f % 4) * 16, y: Math.floor(f / 4) * 16, w: 16, h: 16 };
}

function player(
  props: Partial<Parameters<typeof AnimPlayer>[0]> & { clock: AnimClock; draw: DrawFrame },
) {
  return render(
    <AnimPlayer
      source={stripSource(ANIM)}
      pixelArt={false}
      alpha={null}
      load={load}
      reducedMotion={false}
      dpr={1}
      {...props}
    />,
  );
}

describe('AnimPlayer（canvas のアニメのプレビュー）', () => {
  it('帯は kura:// の anim から読み、経過時間でコマを進める（タイマーの刻みに依らない）', async () => {
    const c = fakeClock();
    const r = recorder();
    player({ clock: c.clock, draw: r.draw });
    expect(load).toHaveBeenCalledWith(animUrl(ANIM.hash, 0));
    await waitFor(() => expect(c.pending()).toBe(1));
    act(() => c.advance(0));
    expect(r.last()?.src).toEqual(cell(0));
    expect(screen.getByText('1 / 8')).toBeInTheDocument();
    // 250ms 進めば 3 コマ目（刻みを飛ばしても）
    act(() => c.advance(250));
    expect(r.last()?.src).toEqual(cell(2));
    expect(screen.getByText('3 / 8')).toBeInTheDocument();
    // 0ms のコマは 100ms：700〜800ms は 8 コマ目、800ms で 1 周
    act(() => c.advance(500));
    expect(r.last()?.src).toEqual(cell(7));
    act(() => c.advance(40));
    expect(r.last()?.src).toEqual(cell(7));
    act(() => c.advance(10));
    expect(r.last()?.src).toEqual(cell(0));
    // 同じコマのうちは描き直さない
    const count = r.drawn.length;
    act(() => c.advance(10));
    expect(r.drawn).toHaveLength(count);
  });

  it('一時停止で止まり、コマ送りで 1 つずつ動く。再生でそこから続ける', async () => {
    const c = fakeClock();
    const r = recorder();
    player({ clock: c.clock, draw: r.draw });
    await waitFor(() => expect(c.pending()).toBe(1));
    act(() => c.advance(150));
    fireEvent.click(screen.getByRole('button', { name: t.pause }));
    expect(c.pending()).toBe(0);
    expect(screen.getByText('2 / 8')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.nextFrame }));
    expect(screen.getByText('3 / 8')).toBeInTheDocument();
    expect(r.last()?.src).toEqual(cell(2));
    fireEvent.click(screen.getByRole('button', { name: t.previousFrame }));
    fireEvent.click(screen.getByRole('button', { name: t.previousFrame }));
    expect(screen.getByText('1 / 8')).toBeInTheDocument();
    // 先頭から前へは最後のコマ
    fireEvent.click(screen.getByRole('button', { name: t.previousFrame }));
    expect(screen.getByText('8 / 8')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.play }));
    expect(screen.queryByRole('button', { name: t.nextFrame })).toBeNull();
    act(() => c.advance(100));
    expect(r.last()?.src).toEqual(cell(0));
  });

  it('「視差効果を減らす」では自動で再生しない（先頭のコマと再生ボタン）', async () => {
    const c = fakeClock();
    const r = recorder();
    player({ clock: c.clock, draw: r.draw, reducedMotion: true });
    await waitFor(() => expect(r.drawn).toHaveLength(1));
    expect(r.last()?.src).toEqual(cell(0));
    expect(c.pending()).toBe(0);
    act(() => c.advance(1000));
    expect(r.drawn).toHaveLength(1);
    expect(screen.getByRole('button', { name: t.play })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.play }));
    act(() => c.advance(0));
    act(() => c.advance(120));
    expect(r.last()?.src).toEqual(cell(1));
  });

  it('タグを選ぶとその範囲を回す（ピンポンは端を重ねない）。保存しない', async () => {
    const c = fakeClock();
    const r = recorder();
    player({ clock: c.clock, draw: r.draw, initialTag: 'bounce' });
    const select = screen.getByRole('combobox', { name: t.tag });
    expect(select).toHaveValue('bounce');
    await waitFor(() => expect(c.pending()).toBe(1));
    const seen: number[] = [];
    for (let i = 0; i < 8; i++) {
      act(() => c.advance(i === 0 ? 0 : 100));
      seen.push(Number(screen.getByText(/ \/ 8$/).textContent?.split(' / ')[0]));
    }
    // 5 6 7 8 7 6 → 5 6（8 コマ目は 0ms = 100ms）
    expect(seen).toEqual([5, 6, 7, 8, 7, 6, 5, 6]);
    fireEvent.change(select, { target: { value: 'walk' } });
    act(() => c.advance(0));
    expect(r.last()?.src).toEqual(cell(0));
    fireEvent.change(select, { target: { value: '' } });
    expect(screen.getByRole('option', { name: t.allFrames })).toBeInTheDocument();
  });

  it('ドット絵は箱に収まる最大の整数倍で最近傍、ほかは広げずに補間', async () => {
    const c = fakeClock();
    const pixel = recorder();
    const { container, unmount } = player({ clock: c.clock, draw: pixel.draw, pixelArt: true });
    await waitFor(() => expect(c.pending()).toBe(1));
    act(() => c.advance(0));
    const canvas = container.querySelector('canvas');
    // 272 × 220 の箱に 16px を 13 倍（208px）
    expect(canvas).toHaveAttribute('width', '208');
    expect(canvas).toHaveAttribute('data-render', 'pixelated');
    expect(pixel.last()?.smoothing).toBe(false);
    unmount();
    expect(c.pending()).toBe(0);

    expect(displaySize(16, 16, 272, 220, 1, false)).toEqual({
      width: 16,
      height: 16,
      smoothing: true,
    });
    expect(displaySize(1000, 500, 272, 220, 2, false)).toEqual({
      width: 272,
      height: 136,
      smoothing: true,
    });
    expect(displaySize(16, 16, 272, 220, 2, true)).toEqual({
      width: 216,
      height: 216,
      smoothing: false,
    });
  });

  it('箱に収まらないドット絵は縮めて補間する（最近傍は広げるときだけ。細い線が消えないように）', async () => {
    const c = fakeClock();
    const r = recorder();
    const big = { ...ANIM, canvasW: 512, canvasH: 512 };
    const { container } = player({
      clock: c.clock,
      draw: r.draw,
      pixelArt: true,
      source: stripSource(big),
    });
    await waitFor(() => expect(c.pending()).toBe(1));
    act(() => c.advance(0));
    const canvas = container.querySelector('canvas');
    // 272 × 220 の箱に 512px を 220px へ縮める
    expect(canvas).toHaveAttribute('width', '220');
    expect(canvas).toHaveAttribute('data-render', 'auto');
    expect(r.last()?.smoothing).toBe(true);
    expect(displaySize(512, 512, 272, 220, 2, true)).toEqual({
      width: 220,
      height: 220,
      smoothing: true,
    });
  });

  describe('市松模様（MediaImage と同じ checkerFor を通す）', () => {
    /** 測った大きさを知らせる ResizeObserver（jsdom にはないので差し替える）。 */
    let observed: ResizeObserverCallback[] = [];
    const Original = globalThis.ResizeObserver;
    beforeEach(() => {
      observed = [];
      globalThis.ResizeObserver = class {
        constructor(cb: ResizeObserverCallback) {
          observed.push(cb);
        }
        observe() {}
        unobserve() {}
        disconnect() {}
      } as unknown as typeof ResizeObserver;
    });
    afterEach(() => {
      globalThis.ResizeObserver = Original;
    });
    /** 枠の中身の大きさを測ったことにする。 */
    function measure(width: number, height: number) {
      act(() => {
        for (const cb of observed) {
          cb([{ contentRect: { width, height } } as ResizeObserverEntry], {} as ResizeObserver);
        }
      });
    }
    const checker = (container: HTMLElement) =>
      container.querySelector('canvas')?.hasAttribute('data-checker') ?? false;

    it('透過と分かったものに、枠の大きさを測ってから敷く（測る前の仮の大きさでは敷かない）', async () => {
      const c = fakeClock();
      const { container } = player({
        clock: c.clock,
        draw: recorder().draw,
        alpha: 'binary',
        pixelArt: true,
      });
      await waitFor(() => expect(c.pending()).toBe(1));
      expect(checker(container)).toBe(false);
      measure(272, 220);
      // 16px を 13 倍（208px）
      expect(container.querySelector('canvas')).toHaveAttribute('data-checker', 'true');
    });

    it('不透明・未解析には敷かない', async () => {
      for (const alpha of ['none', null] as const) {
        const c = fakeClock();
        const { container, unmount } = player({
          clock: c.clock,
          draw: recorder().draw,
          alpha,
          pixelArt: true,
        });
        await waitFor(() => expect(c.pending()).toBe(1));
        measure(272, 220);
        expect(checker(container)).toBe(false);
        unmount();
      }
    });

    it('表示の短い辺が 48px 未満なら敷かない（広げない 16px・狭い枠の 2 倍）', async () => {
      const c = fakeClock();
      const small = player({ clock: c.clock, draw: recorder().draw, alpha: 'partial' });
      await waitFor(() => expect(c.pending()).toBe(1));
      measure(272, 220);
      expect(checker(small.container)).toBe(false);
      small.unmount();

      const f = fakeClock();
      const narrow = player({
        clock: f.clock,
        draw: recorder().draw,
        alpha: 'partial',
        pixelArt: true,
        frameHeight: 'fill',
      });
      await waitFor(() => expect(f.pending()).toBe(1));
      measure(40, 40);
      // 40px の枠に 16px を 2 倍（32px）
      expect(narrow.container.querySelector('canvas')).toHaveAttribute('width', '32');
      expect(checker(narrow.container)).toBe(false);
      measure(100, 100);
      expect(checker(narrow.container)).toBe(true);
    });
  });

  it('帯を読めなければ理由を出す', async () => {
    const c = fakeClock();
    render(
      <AnimPlayer
        source={stripSource(ANIM)}
        pixelArt={false}
        alpha={null}
        load={() => Promise.reject(new Error('404'))}
        clock={c.clock}
        draw={recorder().draw}
        reducedMotion={false}
      />,
    );
    expect(await screen.findByText(t.animFailed)).toBeInTheDocument();
  });

  it('格子のシートは元の画像からセルを切って回す（コマの長さは設定、0 は 100ms）', async () => {
    const c = fakeClock();
    const r = recorder();
    const cells = [
      { x: 0, y: 0, w: 8, h: 8 },
      { x: 10, y: 0, w: 8, h: 8 },
    ];
    render(
      <AnimPlayer
        source={gridSource('file-url', cells, 0)}
        pixelArt={false}
        alpha={null}
        load={load}
        clock={c.clock}
        draw={r.draw}
        reducedMotion={false}
      />,
    );
    expect(load).toHaveBeenLastCalledWith('file-url');
    await waitFor(() => expect(c.pending()).toBe(1));
    act(() => c.advance(0));
    expect(r.last()?.src).toEqual(cells[0]);
    act(() => c.advance(100));
    expect(r.last()?.src).toEqual(cells[1]);
    expect(screen.queryByRole('combobox')).toBeNull();
  });
});
