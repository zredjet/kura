import { describe, expect, it } from 'vitest';

import type { TagRefDto } from '../../ipc/tags';
import { splitTags } from './splitTags';

function tag(id: number, name: string): TagRefDto {
  return { id, namespace: '', name, color: null };
}

const sunset = tag(1, '夕景');
const kamakura = tag(2, '鎌倉');
const pixiv = tag(3, 'pixiv');

describe('splitTags', () => {
  it('全件に付いているタグはチップ、一部だけなら件数付き', () => {
    const { full, partial } = splitTags(
      [
        { tag: sunset, count: 10 },
        { tag: pixiv, count: 3 },
      ],
      10,
    );
    expect(full).toEqual([sunset]);
    expect(partial).toEqual([{ tag: pixiv, count: 3 }]);
  });

  it('ユニット自体のタグでも、メンバーの一部にしかなければ件数付きで出す', () => {
    const { full, partial } = splitTags(
      [
        { tag: sunset, count: 10 },
        { tag: kamakura, count: 4 },
      ],
      10,
      [kamakura, pixiv],
    );
    expect(full).toEqual([sunset]);
    expect(partial).toEqual([
      { tag: kamakura, count: 4 },
      { tag: pixiv, count: 0 },
    ]);
  });

  it('メンバーがいないユニットは、ユニット自体のタグをチップで出す', () => {
    expect(splitTags([], 0, [sunset])).toEqual({ full: [sunset], partial: [] });
  });
});
