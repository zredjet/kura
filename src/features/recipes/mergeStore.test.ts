import { describe, expect, it, vi } from 'vitest';

import { hexOf, readSheetNumber, rgbaOf } from './MergeSheet';
import { inListOrder, inNameOrder, sheetOf } from './mergeStore';

vi.mock('../../ipc/batch', () => ({
  DEFAULT_SHEET_NAME: '{名前} のシート',
  DEFAULT_SAVE_AS_NAME: '{名前} のコピー',
  precheckMerge: vi.fn(),
  cancelPrecheck: vi.fn(async () => null),
  onPrecheckProgress: vi.fn(async () => () => undefined),
  startMerge: vi.fn(),
}));

describe('まとめて 1 つにする', () => {
  it('一覧の並び：一覧にあるものは一覧の順、ないものは後ろに選んだ順', () => {
    expect(inListOrder([5, 1, 9, 3], new Uint32Array([3, 4, 5, 1]))).toEqual([3, 5, 1, 9]);
    expect(inListOrder([2, 1], null)).toEqual([2, 1]);
  });

  it('名前順は自然な順（数字は数として）。同じ名前は選んだ順', () => {
    const titles = new Map([
      [1, 'walk_10'],
      [2, 'walk_2'],
      [3, 'idle'],
      [4, 'walk_2'],
    ]);
    expect(inNameOrder([1, 2, 3, 4], titles)).toEqual([3, 2, 4, 1]);
  });

  it('シートの引数', () => {
    expect(sheetOf({ columns: null, gapX: 1, gapY: 2, fill: null, anchor: 'bottom' })).toEqual({
      columns: null,
      gapX: 1,
      gapY: 2,
      fill: null,
      anchor: 'bottom',
    });
  });

  it('数の欄：空は null、範囲の外・小数は読まない', () => {
    expect(readSheetNumber('', 1)).toBeNull();
    expect(readSheetNumber('8', 1)).toBe(8);
    expect(readSheetNumber('0', 1)).toBeUndefined();
    expect(readSheetNumber('0', 0)).toBe(0);
    expect(readSheetNumber('4097', 0)).toBeUndefined();
    expect(readSheetNumber('1.5', 0)).toBeUndefined();
  });

  it('色の欄と RGBA8', () => {
    expect(rgbaOf('#ff8000')).toEqual([255, 128, 0, 255]);
    expect(hexOf([1, 2, 255, 255])).toBe('#0102ff');
  });
});
