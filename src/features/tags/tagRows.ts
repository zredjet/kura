import type { TagListEntryDto } from '../../ipc/tags';

/** タグの一覧の行：名前空間の見出しか、タグ 1 つ。 */
export type TagRow =
  { kind: 'heading'; namespace: string; count: number } | { kind: 'tag'; entry: TagListEntryDto };

/** 絞り込みに使う形（大文字小文字・全角半角をそろえる）。 */
function fold(text: string): string {
  return text.normalize('NFKC').toLowerCase();
}

/**
 * タグの一覧の行を作る（純粋関数）。並びは受け取った順（名前空間ごとにまとまっている）のまま、
 * 名前空間が変わるところに見出しを入れる。`query` があれば、名前・名前空間・「名前空間:名前」に含むものだけ。
 */
export function tagRows(entries: readonly TagListEntryDto[], query: string): TagRow[] {
  const q = fold(query.trim());
  const matched = q
    ? entries.filter((e) => {
        const { namespace, name } = e.tag;
        return (
          fold(name).includes(q) || fold(namespace ? `${namespace}:${name}` : name).includes(q)
        );
      })
    : entries;
  const rows: TagRow[] = [];
  let heading: { kind: 'heading'; namespace: string; count: number } | null = null;
  for (const entry of matched) {
    if (!heading || heading.namespace !== entry.tag.namespace) {
      heading = { kind: 'heading', namespace: entry.tag.namespace, count: 0 };
      rows.push(heading);
    }
    heading.count += 1;
    rows.push({ kind: 'tag', entry });
  }
  return rows;
}
