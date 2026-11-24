import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AnimDto } from '../../ipc/asset';
import type { ItemSummaryDto } from '../../ipc/items';
import { fileUrl, thumbUrl } from '../../ipc/media';
import { getCached } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { useFontFace } from '../asset/AssetStage';
import { useEditorStore } from '../editor/editorStore';
import { updatePlayerPrefs } from '../player/playerActions';
import { usePlayerStore } from '../player/playerStore';
import { ja } from '../../i18n/ja';
import { audioAsset, imageAsset, summary, withAuditionLoop } from '../asset/testAssets';
import { type AssetData, useAssetData } from '../asset/useAssetData';
import { selectOne } from './selection';
import { QuickLook } from './QuickLook';

vi.mock('../../stores/itemCache', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../stores/itemCache')>()),
  getCached: vi.fn(),
  requestItems: vi.fn(),
}));
vi.mock('../asset/useAssetData', () => ({ useAssetData: vi.fn() }));
vi.mock('../asset/AssetStage', () => ({
  useFontFace: vi.fn(() => null),
  QuickLookAnim: ({ anim }: { anim: AnimDto }) => <div data-testid="anim" data-hash={anim.hash} />,
  FontSpecimen: ({ family, centered }: { family: string; centered?: boolean }) => (
    <div data-testid="specimen" data-family={family} data-centered={String(centered ?? false)} />
  ),
}));
vi.mock('../asset/WaveformView', () => ({
  WaveformView: (p: {
    hash: string;
    loop: readonly [number, number] | null;
    onSeek?: (ms: number) => void;
  }) => (
    <button
      type="button"
      data-testid="waveform"
      data-hash={p.hash}
      data-loop={p.loop?.join('-') ?? ''}
      data-seekable={p.onSeek ? 'true' : undefined}
      onClick={() => p.onSeek?.(4_000)}
    />
  ),
}));
vi.mock('../player/playerActions', () => ({ updatePlayerPrefs: vi.fn() }));

const ANIM = { hash: 'ab'.repeat(32), rev: 0 } as AnimDto;

function show(item: ItemSummaryDto, data: Partial<AssetData> = {}) {
  vi.mocked(getCached).mockImplementation((id) => (id === item.id ? item : undefined));
  vi.mocked(useAssetData).mockReturnValue({
    asset: null,
    anim: { status: 'none' },
    reload: vi.fn(),
    ...data,
  });
  useLibraryStore.setState({
    list: { ids: Uint32Array.from([item.id]), aspects: new Uint16Array(1), groups: [] },
  });
  useViewStore.setState({ quickLook: true, selection: selectOne(item.id) });
  return render(<QuickLook />);
}

// design.md §3.1：アニメはクイックルックでも回す。音声は波形を大きく。フォントは見本
describe('QuickLook の素材（M11）', () => {
  beforeEach(() => vi.clearAllMocks());

  it('アニメの帯があれば画像の代わりに回す。なければ今までどおり画像', () => {
    const { container, unmount } = show(summary(), {
      asset: imageAsset(),
      anim: { status: 'ready', anim: ANIM },
    });
    expect(screen.getByTestId('anim')).toHaveAttribute('data-hash', ANIM.hash);
    expect(container.querySelector('img')).toBeNull();
    expect(useAssetData).toHaveBeenCalledWith(1);
    unmount();
    const still = show(summary(), { asset: imageAsset(), anim: { status: 'tooLarge' } });
    expect(screen.queryByTestId('anim')).toBeNull();
    expect(still.container.querySelector('img')).not.toBeNull();
  });

  it('アニメの画像は、帯を待つ間・作れないときも元のファイルを <img> に置かない（先頭のコマのサムネイル）', () => {
    // <img> に元の GIF・APNG・アニメの WebP を置くと WebKit が自分で回す（prefers-reduced-motion も見ない）
    const gif = summary({ ext: 'gif', animated: true, frameCount: 8 });
    const states: AssetData['anim'][] = [
      { status: 'none' },
      { status: 'loading' },
      { status: 'tooLarge' },
      { status: 'failed' },
    ];
    for (const anim of states) {
      const view = show(gif, { asset: anim.status === 'none' ? null : imageAsset(), anim });
      expect(view.container.querySelector('img')).toHaveAttribute(
        'src',
        thumbUrl(gif.hash, 'l', 0),
      );
      view.unmount();
    }
    // 一覧の概要が古くても、素材の欄でアニメと分かれば止める
    const stale = show(summary({ ext: 'gif' }), {
      asset: imageAsset(),
      anim: { status: 'loading' },
    });
    expect(stale.container.querySelector('img')).toHaveAttribute('src', thumbUrl(gif.hash, 'l', 0));
    stale.unmount();
    // 動かない画像は今までどおり元のファイル
    const png = summary({ ext: 'png' });
    const still = show(png, {
      asset: imageAsset({ effective: { pixelArt: false, animated: false, frameCount: 1 } }),
    });
    expect(still.container.querySelector('img')).toHaveAttribute('src', fileUrl(png.hash));
  });

  it('音声は波形（ループの区間つき）を出す', () => {
    show(summary({ id: 2, hash: 'cd'.repeat(32), kind: 'audio' }), { asset: audioAsset() });
    expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '185220-4057200');
  });

  it('音声の波形のループの区間は、試聴の設定を先に使う（M13）', () => {
    show(summary({ id: 2, hash: 'cd'.repeat(32), kind: 'audio' }), {
      asset: withAuditionLoop(audioAsset(), 100, 200),
    });
    expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '100-200');
  });

  it('フォントを読めたら見本を出し、読めなければ種類の絵のまま', () => {
    vi.mocked(useFontFace).mockReturnValue('kura-specimen-x');
    show(summary({ kind: 'font' }));
    expect(screen.getByTestId('specimen')).toHaveAttribute('data-family', 'kura-specimen-x');
    // 枠の中で縦横とも中央に（インスペクタは左寄せのまま。design.md §3.1 のフォントの見本）
    expect(screen.getByTestId('specimen')).toHaveAttribute('data-centered', 'true');
  });

  it('動画では素材の欄を読まない', () => {
    show(summary({ kind: 'video' }));
    expect(useAssetData).toHaveBeenCalledWith(null);
  });
});

// design.md §3.1「画像エディタ」：クイックルックから開いたら、閉じるとクイックルックに戻る
describe('QuickLook と画像エディタ', () => {
  beforeEach(() => vi.clearAllMocks());

  it('エディタを開いている間は描かず（エディタが下に隠れない）、閉じたら開いていた印のまま戻る', () => {
    const view = show(summary());
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    act(() => useEditorStore.setState({ phase: 'open' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(useViewStore.getState().quickLook).toBe(true);
    act(() => useEditorStore.setState({ phase: 'closed' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    view.unmount();
  });
});

// ADR-230：動画・音声は WebKit の標準の操作を使わず、下に自前のカプセル
describe('QuickLook の再生の操作', () => {
  const play = vi.fn(() => Promise.resolve());
  const pause = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockImplementation(play);
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(pause);
    usePlayerStore.setState({ prefs: { ...usePlayerStore.getState().prefs, volume: 40 } });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('動画は標準の操作を出さず、カプセルに再生・5 秒・シーク・音量を出す', () => {
    const { container } = show(summary({ kind: 'video' }));
    const video = container.querySelector('video');
    expect(video).not.toBeNull();
    expect(video).not.toHaveAttribute('controls');
    expect(video?.volume).toBeCloseTo(0.4, 6);
    const group = screen.getByRole('group', { name: ja.quickLook.controls });
    expect(group).toBeInTheDocument();
    expect(screen.getByRole('button', { name: ja.quickLook.back })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: ja.quickLook.forward })).toBeInTheDocument();
    expect(screen.getByRole('slider', { name: ja.seek.label })).toBeInTheDocument();
    expect(screen.getByRole('slider', { name: ja.quickLook.volume })).toHaveValue('40');
  });

  it('再生ボタンで鳴らし、音量はプレーヤーの設定を変える', () => {
    show(summary({ kind: 'video' }));
    // jsdom の要素は止まったまま（paused）なので、押すと鳴らす
    fireEvent.click(screen.getByRole('button', { name: ja.quickLook.play }));
    expect(play).toHaveBeenCalled();
    fireEvent.change(screen.getByRole('slider', { name: ja.quickLook.volume }), {
      target: { value: '75' },
    });
    expect(updatePlayerPrefs).toHaveBeenCalledWith({ volume: 75 });
  });

  it('音声も標準の操作を出さず、波形を押すとシークする', () => {
    const { container } = show(summary({ id: 2, hash: 'cd'.repeat(32), kind: 'audio' }), {
      asset: audioAsset(),
    });
    const audio = container.querySelector('audio');
    expect(audio).not.toHaveAttribute('controls');
    expect(screen.getByRole('group', { name: ja.quickLook.controls })).toBeInTheDocument();
    const wave = screen.getByTestId('waveform');
    expect(wave).toHaveAttribute('data-seekable', 'true');
    fireEvent.click(wave);
    expect(audio?.currentTime).toBe(4);
  });

  it('画像・フォントではカプセルを出さない', () => {
    show(summary());
    expect(screen.queryByRole('group', { name: ja.quickLook.controls })).toBeNull();
  });
});
