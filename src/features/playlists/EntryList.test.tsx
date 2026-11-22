import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { MenuAction, MenuEntry } from '../../components/Menu/menuStore';
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
// 右クリックで開いたメニューの項目
const menus = vi.hoisted(() => [] as MenuEntry[][]);
vi.mock('../../components/Menu/menuStore', () => ({
  openMenuAt: (_x: number, _y: number, _label: string, entries: MenuEntry[]) => {
    menus.push(entries);
  },
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
});

/** `act` で 1 回だけ再生を頼み、そのとき渡した「ここから」（`fromItem`・`fromEntry`）を返す。 */
function played(act: () => void) {
  vi.mocked(playPlaylist).mockClear();
  act();
  expect(playPlaylist).toHaveBeenCalledTimes(1);
  expect(playPlaylist).toHaveBeenLastCalledWith(7, 'P', false, expect.anything());
  return vi.mocked(playPlaylist).mock.lastCall?.[3];
}

/** 同じ行を ダブルクリック・右クリックの「ここから再生」・選んで ↩ の 3 つの道で再生し、渡した値を返す。 */
function playEachWay(row: HTMLElement) {
  const doubleClick = played(() => fireEvent.doubleClick(row));
  let shortcut: string | undefined;
  const menu = played(() => {
    fireEvent.contextMenu(row);
    const first = menus.at(-1)?.[0] as MenuAction;
    expect(first.label).toBe(ja.playlist.entryMenu.playFrom);
    shortcut = first.shortcut;
    first.onSelect();
  });
  // メニューの手がかりの ↩ が、同じ行で同じことをする
  expect(shortcut).toBe('↩');
  const enter = played(() => {
    fireEvent.click(row);
    expect(document.activeElement).toBe(row);
    fireEvent.keyDown(row, { key: 'Enter' });
  });
  return { doubleClick, menu, enter };
}

describe('EntryList の ↩・ダブルクリック・右クリックの「ここから再生」（M11 の 3 回目の確認 P1）', () => {
  beforeEach(() => {
    menus.length = 0;
  });

  it('ユニット単位：エントリの行と、▸ で開いた中身のどの行も、3 つの道で押した行のアイテムとエントリを渡す', () => {
    show('unit');
    fireEvent.click(screen.getByRole('button', { name: ja.playlist.expand('Y') }));
    const cases: Array<[HTMLElement, { itemId: number; entryId: number }]> = [
      [screen.getByRole('button', { name: /^X/ }), { itemId: 3, entryId: 10 }],
      [screen.getByRole('button', { name: /^Y/ }), { itemId: 2, entryId: 11 }],
      [screen.getByRole('button', { name: /b-audio/ }), { itemId: 2, entryId: 11 }],
      // 開いた中身のフォントの行：エントリの先頭のメンバー b でなく f（Rust が後ろの c から始める）
      [screen.getByRole('button', { name: /f-font/ }), { itemId: 3, entryId: 11 }],
      [screen.getByRole('button', { name: /c-video/ }), { itemId: 4, entryId: 11 }],
    ];
    for (const [row, want] of cases) {
      expect(playEachWay(row)).toEqual({ doubleClick: want, menu: want, enter: want });
    }
  });

  it('ファイル単位：メンバーのどの行も、3 つの道で押した行のアイテムとエントリを渡す', () => {
    show('file');
    const [xf, yf] = screen.getAllByRole('button', { name: /f-font/ });
    const cases: Array<[HTMLElement, { itemId: number; entryId: number }]> = [
      [xf!, { itemId: 3, entryId: 10 }],
      [screen.getByRole('button', { name: /a-image/ }), { itemId: 1, entryId: 10 }],
      [screen.getByRole('button', { name: /b-audio/ }), { itemId: 2, entryId: 11 }],
      // 「Y から」の 2 つ目の行（フォント）：見出しの先頭のメンバー b でなく f
      [yf!, { itemId: 3, entryId: 11 }],
      [screen.getByRole('button', { name: /c-video/ }), { itemId: 4, entryId: 11 }],
    ];
    for (const [row, want] of cases) {
      expect(playEachWay(row)).toEqual({ doubleClick: want, menu: want, enter: want });
    }
  });

  it('複数を選んでいても、↩ はフォーカスのある（最後に押した）行から', () => {
    show('file');
    fireEvent.click(screen.getByRole('button', { name: /a-image/ }));
    const c = screen.getByRole('button', { name: /c-video/ });
    expect(
      played(() => {
        fireEvent.click(c, { shiftKey: true });
        fireEvent.keyDown(c, { key: 'Enter' });
      }),
    ).toEqual({ itemId: 4, entryId: 11 });
  });

  it('フォーカスのある行が選択の外なら、↩ は選んだエントリの最初の行（先頭のメンバー）から', () => {
    show('file');
    fireEvent.click(screen.getByRole('button', { name: /c-video/ }));
    const a = screen.getByRole('button', { name: /a-image/ });
    // ⌘ で X を足してから外す：フォーカスは a に残り、選択は Y だけ
    fireEvent.click(a, { metaKey: true });
    fireEvent.click(a, { metaKey: true });
    expect(document.activeElement).toBe(a);
    expect(played(() => fireEvent.keyDown(a, { key: 'Enter' }))).toEqual({
      itemId: 2,
      entryId: 11,
    });
  });
});
