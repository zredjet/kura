import { describe, expect, it } from 'vitest';

import { CHECKER_MIN_PX, isPowerOfTwo, pixelFit, renderingFor, showChecker } from './pixel';

// docs/design.md §3.1「素材の表示」：市松模様とドット絵の拡大
describe('市松模様', () => {
  it('解析で透過と分かったものだけに敷く（未解析・不透明には敷かない）', () => {
    expect(showChecker(null)).toBe(false);
    expect(showChecker(undefined)).toBe(false);
    expect(showChecker('none')).toBe(false);
    expect(showChecker('binary')).toBe(true);
    expect(showChecker('partial')).toBe(true);
  });

  it('表示の短い辺が 48px 未満の所（24px の行・44px の表紙）には敷かない', () => {
    expect(CHECKER_MIN_PX).toBe(48);
    expect(showChecker('partial', 24)).toBe(false);
    expect(showChecker('partial', 44)).toBe(false);
    expect(showChecker('partial', 47.9)).toBe(false);
    expect(showChecker('partial', 48)).toBe(true);
    expect(showChecker('binary', 220)).toBe(true);
    expect(showChecker('none', 220)).toBe(false);
  });
});

describe('ドット絵の最近傍', () => {
  const n = { w: 64, h: 32 };

  it('広げるときだけ pixelated（縮めるときは補間）', () => {
    expect(renderingFor(true, n, { w: 128, h: 64 }, 1)).toBe('pixelated');
    // 装置の画素で数える：CSS で縮めても DPR 2 なら等倍
    expect(renderingFor(true, n, { w: 32, h: 16 }, 2)).toBe('pixelated');
    expect(renderingFor(true, n, { w: 32, h: 16 }, 1)).toBe('auto');
    expect(renderingFor(true, n, { w: 64, h: 32 }, 1)).toBe('pixelated');
    expect(renderingFor(false, n, { w: 640, h: 320 }, 2)).toBe('auto');
  });

  it('cover（タイル）は長いほうの倍率、contain は短いほうの倍率で決める', () => {
    // 正方形 40px：contain は 64 → 40 に縮める、cover は 32 → 40 に広げる
    expect(renderingFor(true, n, { w: 40, h: 40 }, 1, 'contain')).toBe('auto');
    expect(renderingFor(true, n, { w: 40, h: 40 }, 1, 'cover')).toBe('pixelated');
    expect(renderingFor(true, n, { w: 40, h: 40 }, 2, 'contain')).toBe('pixelated');
  });

  it('大きさが分からない（読み込み前・0）なら補間のまま', () => {
    expect(renderingFor(true, { w: 0, h: 0 }, { w: 100, h: 100 }, 2)).toBe('auto');
    expect(renderingFor(true, n, { w: 0, h: 0 }, 2)).toBe('auto');
    expect(renderingFor(true, n, { w: 100, h: 100 }, 0)).toBe('auto');
    expect(renderingFor(true, { w: Number.NaN, h: 4 }, { w: 100, h: 100 }, 1)).toBe('auto');
  });

  it('箱に収まる最大の整数倍（装置の画素で整数倍。DPR 2 なら 0.5px 刻み）', () => {
    // インスペクタのプレビュー（272 × 220）
    expect(pixelFit(32, 32, 272, 220)).toEqual({
      width: 192,
      height: 192,
      scale: 6,
      integer: true,
    });
    expect(pixelFit(32, 32, 272, 220, 2)).toEqual({
      width: 208,
      height: 208,
      scale: 13,
      integer: true,
    });
    // 装置の画素で 544 × 440 の箱：5 × 3 は 108 倍（CSS では 270 × 162）
    expect(pixelFit(5, 3, 272, 220, 2)).toEqual({
      width: 270,
      height: 162,
      scale: 108,
      integer: true,
    });
    // ちょうど 1 倍
    expect(pixelFit(272, 220, 272, 220)).toEqual({
      width: 272,
      height: 220,
      scale: 1,
      integer: true,
    });
  });

  it('1 倍に満たなければ縮めて収める（このときは整数倍でない）', () => {
    const f = pixelFit(1000, 500, 272, 220);
    expect(f.integer).toBe(false);
    expect(f.width).toBeCloseTo(272, 9);
    expect(f.height).toBeCloseTo(136, 9);
    expect(f.scale).toBeCloseTo(0.272, 9);
  });

  it('大きさのないものは 0', () => {
    expect(pixelFit(0, 10, 100, 100)).toEqual({ width: 0, height: 0, scale: 0, integer: false });
    expect(pixelFit(10, 10, 0, 100)).toEqual({ width: 0, height: 0, scale: 0, integer: false });
    expect(pixelFit(10, 10, 100, 100, 0)).toEqual({
      width: 0,
      height: 0,
      scale: 0,
      integer: false,
    });
  });
});

describe('2 のべき乗（検索の `2のべき乗` と同じ判定）', () => {
  it('幅・高さの両方が 2 のべき乗', () => {
    expect(isPowerOfTwo(64, 256)).toBe(true);
    expect(isPowerOfTwo(1, 1)).toBe(true);
    expect(isPowerOfTwo(2 ** 31, 2)).toBe(true);
    expect(isPowerOfTwo(64, 96)).toBe(false);
    expect(isPowerOfTwo(96, 64)).toBe(false);
    expect(isPowerOfTwo(0, 0)).toBe(false);
    expect(isPowerOfTwo(-4, 4)).toBe(false);
    expect(isPowerOfTwo(3.5, 4)).toBe(false);
    expect(isPowerOfTwo(2 ** 32 - 1, 1)).toBe(false);
    expect(isPowerOfTwo(Number.NaN, 4)).toBe(false);
  });
});
