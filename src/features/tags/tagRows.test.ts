import { describe, expect, it } from 'vitest';

import type { TagListEntryDto } from '../../ipc/tags';
import { tagRows } from './tagRows';

const entry = (namespace: string, name: string, items = 1): TagListEntryDto => ({
  tag: { id: Math.random(), namespace, name, color: null },
  items,
  auto: 0,
  units: 0,
  clause: namespace ? `${namespace}:${name}` : name,
});

const ENTRIES = [
  entry('作者', 'KR', 3),
  entry('作者', 'もりの'),
  entry('場所', '鎌倉'),
  entry('', '夕景', 2),
  entry('', 'ＲＥＤ'),
];

const shape = (rows: ReturnType<typeof tagRows>) =>
  rows.map((r) => (r.kind === 'heading' ? `#${r.namespace}(${r.count})` : r.entry.tag.name));

describe('tagRows', () => {
  it('名前空間が変わるところに見出しを入れ、件数を数える', () => {
    expect(shape(tagRows(ENTRIES, ''))).toEqual([
      '#作者(2)',
      'KR',
      'もりの',
      '#場所(1)',
      '鎌倉',
      '#(2)',
      '夕景',
      'ＲＥＤ',
    ]);
  });

  it('名前・名前空間で絞り込み、大文字小文字と全角半角を問わない', () => {
    expect(shape(tagRows(ENTRIES, '作者'))).toEqual(['#作者(2)', 'KR', 'もりの']);
    expect(shape(tagRows(ENTRIES, 'red'))).toEqual(['#(1)', 'ＲＥＤ']);
    expect(shape(tagRows(ENTRIES, '場所:鎌'))).toEqual(['#場所(1)', '鎌倉']);
    expect(tagRows(ENTRIES, 'ない')).toEqual([]);
  });
});
