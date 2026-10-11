import { describe, expect, it } from 'vitest';

import { queueRows } from './SidePanel';

const group = (start: number, len: number, kind: 'unit' | 'item' = 'unit') => ({
  entryId: start + 1,
  target: kind === 'unit' ? { kind, id: start + 1 } : { kind, id: start + 1 },
  name: `g${start}`,
  start,
  len,
});

describe('queueRows', () => {
  it('ファイル単位はスロットをそのまま並べる', () => {
    expect(queueRows({ itemIds: [1, 2], groups: [] }, null, new Set())).toEqual([
      { kind: 'slot', index: 0, indent: false },
      { kind: 'slot', index: 1, indent: false },
    ]);
  });

  it('ユニット単位は見出しを付け、今のまとまりと開いたものだけ中を出す。単体は見出しなし', () => {
    const queue = {
      itemIds: [1, 2, 3, 4, 5],
      groups: [group(0, 2), group(2, 1, 'item'), group(3, 2)],
    };
    expect(queueRows(queue, 0, new Set())).toEqual([
      { kind: 'group', group: 0, open: true },
      { kind: 'slot', index: 0, indent: true },
      { kind: 'slot', index: 1, indent: true },
      { kind: 'slot', index: 2, indent: false },
      { kind: 'group', group: 2, open: false },
    ]);
    expect(queueRows(queue, 0, new Set([2])).slice(-3)).toEqual([
      { kind: 'group', group: 2, open: true },
      { kind: 'slot', index: 3, indent: true },
      { kind: 'slot', index: 4, indent: true },
    ]);
  });
});
