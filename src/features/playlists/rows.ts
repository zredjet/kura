import type { PlaylistContentsDto } from '../../ipc/playlists';

/** 一覧の行（ファイル単位：見出しとアイテム、ユニット単位：エントリと、開いたエントリの中身）。 */
export type ListRow =
  /** 「鎌倉（フォルダ）から · 24 件」（ファイル単位で unit / folder エントリを展開した前）。 */
  | { kind: 'header'; entry: number }
  /** アイテム 1 件（ファイル単位の行、ユニット単位で開いたエントリの中身）。 */
  | { kind: 'item'; entry: number; itemId: number; indent: boolean }
  /** エントリ 1 行（ユニット単位）。 */
  | { kind: 'entry'; entry: number };

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
