import type { TagRefDto } from '../../ipc/tags';

export interface TagCount {
  tag: TagRefDto;
  count: number;
}

/**
 * インスペクタのタグを、全件に付いているもの（チップ）と一部にだけ付いているもの（「3/10」の件数付き）に分ける。
 * `own` はユニット自体のタグ：メンバーがいなければチップに、メンバーに付いていなければ「0/10」で出す
 * （押すとメンバー全員に付く。docs/playlists.md §2.1）。
 */
export function splitTags(
  counts: readonly TagCount[],
  total: number,
  own: readonly TagRefDto[] = [],
): { full: TagRefDto[]; partial: TagCount[] } {
  const full: TagRefDto[] = [];
  const partial: TagCount[] = [];
  const seen = new Set<number>();
  for (const entry of counts) {
    seen.add(entry.tag.id);
    if (total > 0 && entry.count >= total) {
      full.push(entry.tag);
    } else {
      partial.push(entry);
    }
  }
  for (const tag of own) {
    if (seen.has(tag.id)) {
      continue;
    }
    if (total === 0) {
      full.push(tag);
    } else {
      partial.push({ tag, count: 0 });
    }
  }
  return { full, partial };
}
