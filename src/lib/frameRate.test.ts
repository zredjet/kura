import { describe, expect, it } from 'vitest';

import { estimateFrameMs } from './frameRate';

const repeat = (value: number, n: number) => Array.from({ length: n }, () => value);

describe('estimateFrameMs', () => {
  it('差が少なすぎれば分からない', () => {
    expect(estimateFrameMs([])).toBeNull();
    expect(estimateFrameMs(repeat(40, 4))).toBeNull();
  });

  it('よくある速さに寄せる', () => {
    expect(estimateFrameMs(repeat(41.66, 10))).toBeCloseTo(1000 / 24, 9);
    expect(estimateFrameMs(repeat(41.708, 10))).toBeCloseTo(1001 / 24, 9);
    expect(estimateFrameMs(repeat(40.1, 10))).toBeCloseTo(40, 9);
    expect(estimateFrameMs(repeat(33.36, 10))).toBeCloseTo(1001 / 30, 9);
    expect(estimateFrameMs(repeat(16.66, 10))).toBeCloseTo(1000 / 60, 9);
    expect(estimateFrameMs(repeat(8.3, 10))).toBeCloseTo(1000 / 120, 9);
  });

  it('外れ値（落ちたコマ・二重のコマ）に強い中央値', () => {
    const deltas = [...repeat(40, 8), 80, 120, 0.5];
    expect(estimateFrameMs(deltas)).toBeCloseTo(40, 9);
  });

  it('よくある速さに当たらなければ中央値のまま', () => {
    expect(estimateFrameMs(repeat(66.7, 6))).toBeCloseTo(66.7, 9);
  });

  it('ありえない値は捨てる', () => {
    expect(estimateFrameMs([...repeat(2000, 10), ...repeat(1, 10)])).toBeNull();
    expect(estimateFrameMs([...repeat(Number.NaN, 10)])).toBeNull();
  });
});
