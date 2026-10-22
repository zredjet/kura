import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { PlaylistContentsDto, PlaylistDto } from '../../ipc/playlists';
import { EntryList } from './EntryList';
import { playPlaylist } from './playlistActions';

// 仮想スクロールは全部の行を返す（jsdom には大きさがない）
vi.mock('@tanstack/react-virtual', () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({ index: i, key: i, start: i * 60, size: 60 })),
    getTotalSize: () => count * 60,
    measure: () => {},
  }),
}));
vi.mock('./playlistActions', () => ({
  playPlaylist: vi.fn(() => Promise.resolve()),
  removeFromPlaylist: vi.fn(() => Promise.resolve()),
  snapshotPlaylist: vi.fn(() => Promise.resolve()),
}));
const ITEMS: Record<number, { title: string; kind: string }> = {
  1: { title: 'a-image', kind: 'image' },
  2: { title: 'b-audio', kind: 'audio' },
  3: { title: 'f-font', kind: 'font' },
  4: { title: 'c-video', kind: 'video' },
};
vi.mock('../../stores/itemCache', () => ({
  getCached: (id: number) =>
    ITEMS[id]
      ? {
          id,
          title: ITEMS[id].title,
          kind: ITEMS[id].kind,
          durationMs: 1000,
          thumbState: 'none',
          hash: 'h',
        }
      : undefined,
  requestItems: () => {},
  useItemCacheVersion: () => 0,
}));
vi.mock('../../components/MediaImage/MediaImage', () => ({
  MediaImage: () => null,
}));

function playlist(granularity: 'file' | 'unit'): PlaylistDto {
  return { id: 7, name: 'P', kind: 'manual', granularity } as unknown as PlaylistDto;
}

// エントリ X(f, a)・Y(b, f, c)：同じフォント f が 2 つのエントリにある（M11 の 2 回目の確認 P2）
const contents = {
  rows: [
    {
      entryId: 10,
      target: { kind: 'unit', id: 100 },
      name: 'X',
      itemCount: 2,
      durationMs: 0,
      kinds: ['image', 'font'],
      coverItemId: 3,
      imageDurationMs: null,
    },
    {
      entryId: 11,
      target: { kind: 'unit', id: 101 },
      name: 'Y',
      itemCount: 3,
      durationMs: 0,
      kinds: ['audio', 'video', 'font'],
      coverItemId: 2,
      imageDurationMs: null,
    },
  ],
  flatEntryIds: [10, 10, 11, 11, 11],
  flatItemIds: [3, 1, 2, 3, 4],
  itemCount: 5,
  totalMs: 0,
  images: 1,
  videos: 1,
  audios: 1,
  fonts: 2,
  models: 0,
} as unknown as PlaylistContentsDto;

function show(granularity: 'file' | 'unit') {
  render(
    <EntryList
      playlist={playlist(granularity)}
      contents={contents}
      scrollRef={{ current: document.createElement('div') }}
      onChanged={() => {}}
    />,
  );
}

describe('EntryList の「ここから再生」', () => {
  beforeEach(() => {
    vi.mocked(playPlaylist).mockClear();
  });

  it('ユニット単位で開いた中身の行は、そのアイテムと行のエントリを渡す（P1）', () => {
    show('unit');
    fireEvent.click(screen.getByRole('button', { name: ja.playlist.expand('Y') }));
    const fonts = screen.getAllByRole('button', { name: /f-font/ });
    // 開いたのは Y だけなので、f の行は Y の中の 1 つ
    expect(fonts).toHaveLength(1);
    fireEvent.doubleClick(fonts[0]!);
    expect(playPlaylist).toHaveBeenLastCalledWith(7, 'P', false, { itemId: 3, entryId: 11 });
    fireEvent.doubleClick(screen.getByRole('button', { name: /c-video/ }));
    expect(playPlaylist).toHaveBeenLastCalledWith(7, 'P', false, { itemId: 4, entryId: 11 });
  });

  it('ファイル単位で同じフォントの 2 つの行は、それぞれのエントリを渡す（P2）', () => {
    show('file');
    const fonts = screen.getAllByRole('button', { name: /f-font/ });
    expect(fonts).toHaveLength(2);
    fireEvent.doubleClick(fonts[0]!);
    expect(playPlaylist).toHaveBeenLastCalledWith(7, 'P', false, { itemId: 3, entryId: 10 });
    fireEvent.doubleClick(fonts[1]!);
    expect(playPlaylist).toHaveBeenLastCalledWith(7, 'P', false, { itemId: 3, entryId: 11 });
  });

  it('↩ は選んだエントリの最初の行（先頭のメンバー）とそのエントリを渡す', () => {
    show('file');
    const fonts = screen.getAllByRole('button', { name: /f-font/ });
    fireEvent.click(fonts[1]!);
    fireEvent.keyDown(fonts[1]!, { key: 'Enter' });
    expect(playPlaylist).toHaveBeenLastCalledWith(7, 'P', false, { itemId: 2, entryId: 11 });
  });
});
