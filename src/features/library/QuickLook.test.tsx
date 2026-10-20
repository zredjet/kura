import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { AnimDto } from '../../ipc/asset';
import type { ItemSummaryDto } from '../../ipc/items';
import { fileUrl, thumbUrl } from '../../ipc/media';
import { getCached } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { useFontFace } from '../asset/AssetStage';
import { audioAsset, imageAsset, summary } from '../asset/testAssets';
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
  FontSpecimen: ({ family }: { family: string }) => (
    <div data-testid="specimen" data-family={family} />
  ),
}));
vi.mock('../asset/WaveformView', () => ({
  WaveformView: (p: { hash: string; loop: readonly [number, number] | null }) => (
    <div data-testid="waveform" data-hash={p.hash} data-loop={p.loop?.join('-') ?? ''} />
  ),
}));

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

  it('フォントを読めたら見本を出し、読めなければ種類の絵のまま', () => {
    vi.mocked(useFontFace).mockReturnValue('kura-specimen-x');
    show(summary({ kind: 'font' }));
    expect(screen.getByTestId('specimen')).toHaveAttribute('data-family', 'kura-specimen-x');
  });

  it('動画では素材の欄を読まない', () => {
    show(summary({ kind: 'video' }));
    expect(useAssetData).toHaveBeenCalledWith(null);
  });
});
