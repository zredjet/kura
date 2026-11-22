import type { PlaylistContentsDto } from '../../ipc/playlists';

/** 一覧の行（ファイル単位：見出しとアイテム、ユニット単位：エントリと、開いたエントリの中身）。 */
export type ListRow =
  /** 「鎌倉（フォルダ）から · 24 件」（ファイル単位で unit / folder エントリを展開した前）。 */
  | { kind: 'header'; entry: number }
  /** アイテム 1 件（ファイル単位の行、ユニット単位で開いたエントリの中身）。 */
  | { kind: 'item'; entry: number; itemId: number; indent: boolean }
  /** エントリ 1 行（ユニット単位）。 */
  | { kind: 'entry'; entry: number };

/** 一覧の行から「ここから再生」で渡すもの（`play_queue` の `fromItem`・`fromEntry`）。 */
export interface PlayFrom {
  itemId: number;
  /** 押した行のエントリ（同じアイテムが複数のエントリにあっても、その行から始める。M11 の 2 回目の確認 P2）。 */
  entryId: number;
}

/**
 * 行から「ここから再生」で渡すもの：アイテムの行はそのアイテム、エントリの行・ファイル単位の見出しは先頭のメンバー
 * （`coverItemId`）と、どれも行のエントリ（docs/playlists.md §1.1）。メンバーのないエントリは undefined（先頭から）。
 */
export function playFromOf(
  row: ListRow,
  contents: Pick<PlaylistContentsDto, 'rows'>,
): PlayFrom | undefined {
  const entry = contents.rows[row.entry];
  if (!entry) {
    return undefined;
  }
  const itemId = row.kind === 'item' ? row.itemId : entry.coverItemId;
  return itemId === null ? undefined : { itemId, entryId: entry.entryId };
}

/**
 * ↩ で「ここから再生」する行：フォーカスのある行が選んだエントリの行なら、その行（ダブルクリック・右クリックの
 * 「ここから再生」と同じく押した行から。M11 の 3 回目の確認 P1）。フォーカスが行の外（▸）や選択の外の行
 * （⌘ で外した行）にあれば、選んだエントリの最初の行（エントリの行・見出し・先頭のメンバー）。どちらもなければ undefined。
 */
export function enterRow(
  rows: readonly ListRow[],
  focused: ListRow | undefined,
  isSelected: (row: ListRow) => boolean,
): ListRow | undefined {
  if (focused && isSelected(focused)) {
    return focused;
  }
  return rows.find(isSelected);
}

/** 行の高さ（design.md §3.1：ユニット行 60px、展開した中身 44px、見出し 28px、ファイルの行 52px）。 */
export function rowHeight(row: ListRow | undefined, granularity: 'file' | 'unit'): number {
  if (!row) {
    return 0;
  }
  switch (row.kind) {
    case 'header':
      return 28;
    case 'entry':
      return 60;
    case 'item':
      return granularity === 'unit' ? 44 : 52;
  }
}

/** 中身から行を作る。`entry` は `contents.rows` の添字。 */
export function listRows(
  contents: Pick<PlaylistContentsDto, 'rows' | 'flatItemIds'>,
  granularity: 'file' | 'unit',
  opened: ReadonlySet<number>,
): ListRow[] {
  const out: ListRow[] = [];
  let flat = 0;
  contents.rows.forEach((row, entry) => {
    const items = contents.flatItemIds.slice(flat, flat + row.itemCount);
    flat += row.itemCount;
    if (granularity === 'unit') {
      out.push({ kind: 'entry', entry });
      if (row.target.kind !== 'item' && opened.has(row.entryId)) {
        items.forEach((itemId) => out.push({ kind: 'item', entry, itemId, indent: true }));
      }
      return;
    }
    if (row.target.kind === 'item') {
      // ゴミ箱に入ったアイテムのエントリは出さない
      items.forEach((itemId) => out.push({ kind: 'item', entry, itemId, indent: false }));
      return;
    }
    if (items.length > 0) {
      out.push({ kind: 'header', entry });
      items.forEach((itemId) => out.push({ kind: 'item', entry, itemId, indent: false }));
    }
  });
  return out;
}
