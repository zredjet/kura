import { describe, expect, it } from 'vitest';

import { insertBefore, moveBlock } from './reorder';

const order = [1, 2, 3, 4, 5];

describe('reorder', () => {
  it('moves a block one step keeping its inner order', () => {
    expect(moveBlock(order, new Set([3, 4]), -1)).toEqual([1, 3, 4, 2, 5]);
    expect(moveBlock(order, new Set([2, 4]), 1)).toEqual([1, 3, 2, 5, 4]);
    // 端では動かない
    expect(moveBlock(order, new Set([1]), -1)).toEqual(order);
    expect(moveBlock(order, new Set([4, 5]), 1)).toEqual(order);
  });

  it('inserts dragged items before a target or at the end', () => {
    expect(insertBefore(order, new Set([4, 2]), 1)).toEqual([2, 4, 1, 3, 5]);
    expect(insertBefore(order, new Set([1]), null)).toEqual([2, 3, 4, 5, 1]);
    expect(insertBefore(order, new Set([2, 3]), 5)).toEqual([1, 4, 2, 3, 5]);
    // 自分の上に落としたら何もしない
    expect(insertBefore(order, new Set([2, 3]), 3)).toEqual(order);
  });
});
