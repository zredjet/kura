import { describe, expect, it } from 'vitest';

import {
  EMPTY_SELECTION,
  extendBy,
  extendTo,
  moveFocus,
  prune,
  selectAll,
  selectOne,
  toggle,
} from './selection';

const order = new Uint32Array([10, 20, 30, 40, 50]);

describe('selection', () => {
  it('selects one and toggles while keeping the click order', () => {
    let s = selectOne(30);
    s = toggle(s, 10);
    s = toggle(s, 50);
    expect(s).toEqual({ ids: [30, 10, 50], anchor: 50, focus: 50 });
    s = toggle(s, 10);
    expect(s.ids).toEqual([30, 50]);
    s = toggle(toggle(s, 50), 30);
    expect(s).toEqual(EMPTY_SELECTION);
  });

  it('extends a range from the anchor in either direction', () => {
    expect(extendTo(selectOne(20), 40, order).ids).toEqual([20, 30, 40]);
    const back = extendTo(selectOne(40), 10, order);
    expect(back).toEqual({ ids: [40, 30, 20, 10], anchor: 40, focus: 10 });
    // 基点がなければ 1 件だけ
    expect(extendTo(EMPTY_SELECTION, 30, order)).toEqual(selectOne(30));
  });

  it('moves and extends with arrow keys', () => {
    expect(moveFocus(EMPTY_SELECTION, 1, order)).toEqual(selectOne(10));
    expect(moveFocus(selectOne(30), 1, order)).toEqual(selectOne(40));
    expect(moveFocus(selectOne(50), 1, order)).toEqual(selectOne(50));
    const wide = extendBy(extendBy(selectOne(30), -1, order), -1, order);
    expect(wide).toEqual({ ids: [30, 20, 10], anchor: 30, focus: 10 });
    // 範囲を戻すと縮む
    expect(extendBy(wide, 1, order).ids).toEqual([30, 20]);
  });

  it('selects all and prunes removed ids', () => {
    expect(selectAll(order)).toEqual({ ids: [10, 20, 30, 40, 50], anchor: 10, focus: 50 });
    const s = { ids: [20, 30, 40], anchor: 20, focus: 40 };
    const pruned = prune(s, new Uint32Array([10, 30, 50]));
    expect(pruned).toEqual({ ids: [30], anchor: 30, focus: 30 });
    expect(prune(s, order)).toBe(s);
  });
});
