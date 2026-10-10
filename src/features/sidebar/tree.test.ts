import { describe, expect, it } from 'vitest';

import type { FolderNodeDto } from '../../ipc/folders';
import { descendantsOf, dropPositionAt, moveTarget, pathOf, visibleRows } from './tree';

function node(id: number, parentId: number | null, name: string, pos: number): FolderNodeDto {
  return { id, parentId, name, color: null, pos, count: null, unitId: null };
}

// 写真(1) ─ 旅行(2) ─ 鎌倉(4)、家族(3)。イラスト(5)
const nodes = [
  node(5, null, 'イラスト', 2),
  node(1, null, '写真', 1),
  node(3, 1, '家族', 2),
  node(2, 1, '旅行', 1),
  node(4, 2, '鎌倉', 1),
];

describe('folder tree', () => {
  it('lists only expanded folders in order with depth', () => {
    const rows = visibleRows(nodes, (id) => id === 1);
    expect(rows.map((r) => [r.node.name, r.depth, r.hasChildren, r.expanded])).toEqual([
      ['写真', 0, true, true],
      ['旅行', 1, true, false],
      ['家族', 1, false, false],
      ['イラスト', 0, false, false],
    ]);
    expect(visibleRows(nodes, () => true).map((r) => r.node.id)).toEqual([1, 2, 4, 3, 5]);
  });

  it('builds paths and descendants', () => {
    expect(pathOf(nodes, 4)).toEqual(['写真', '旅行', '鎌倉']);
    expect([...descendantsOf(nodes, 1)].sort()).toEqual([1, 2, 3, 4]);
  });

  it('decides where a dropped folder goes', () => {
    expect(dropPositionAt(0.1)).toBe('before');
    expect(dropPositionAt(0.5)).toBe('inside');
    expect(dropPositionAt(0.9)).toBe('after');

    expect(moveTarget(nodes, 5, 2, 'inside')).toEqual({ parentId: 2, beforeId: null });
    expect(moveTarget(nodes, 5, 2, 'before')).toEqual({ parentId: 1, beforeId: 2 });
    expect(moveTarget(nodes, 5, 2, 'after')).toEqual({ parentId: 1, beforeId: 3 });
    expect(moveTarget(nodes, 5, 3, 'after')).toEqual({ parentId: 1, beforeId: null });
    // 自分の中・子孫の中へは移せない
    expect(moveTarget(nodes, 1, 4, 'inside')).toBeNull();
    expect(moveTarget(nodes, 2, 2, 'before')).toBeNull();
  });
});
