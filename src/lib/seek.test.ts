import { describe, expect, it } from 'vitest';

import {
  clampSeek,
  END_GUARD_MS,
  frameStepTarget,
  moveScrub,
  msAtX,
  startScrub,
  tooltipLeft,
} from './seek';

describe('clampSeek', () => {
  it('0 と終わりの手前に収める', () => {
    expect(clampSeek(-50, 10_000)).toBe(0);
    expect(clampSeek(5_000, 10_000)).toBe(5_000);
    expect(clampSeek(10_000, 10_000)).toBe(10_000 - END_GUARD_MS);
    expect(clampSeek(99_999, 10_000)).toBe(10_000 - END_GUARD_MS);
  });

  it('長さが分からなければ 0 以上だけ、短すぎれば 0', () => {
    expect(clampSeek(12_345, null)).toBe(12_345);
    expect(clampSeek(-1, null)).toBe(0);
    expect(clampSeek(Number.NaN, 10_000)).toBe(0);
    expect(clampSeek(50, 80)).toBe(0);
  });
});

describe('msAtX', () => {
  it('バーの中の位置を時刻にし、外側は端に寄せる', () => {
    expect(msAtX(50, 200, 60_000)).toBe(15_000);
    expect(msAtX(-10, 200, 60_000)).toBe(0);
    expect(msAtX(250, 200, 60_000)).toBe(60_000);
  });

  it('幅・長さがなければ 0', () => {
    expect(msAtX(10, 0, 60_000)).toBe(0);
    expect(msAtX(10, 200, 0)).toBe(0);
  });
});

describe('ドラッグ（錨の方式）', () => {
  const W = 600;
  const D = 60_000; // 1px = 100ms

  it('⌥ なしは押した点から始まり、ポインタの真下を追う', () => {
    const a = startScrub(300, W, D, 1_000, false);
    expect(a.ms).toBe(30_000);
    expect(moveScrub(a, 330, W, D, false).ms).toBe(33_000);
    expect(moveScrub(a, 700, W, D, false).ms).toBe(D);
    expect(moveScrub(a, -20, W, D, false).ms).toBe(0);
  });

  it('⌥ ありは今の位置のまま始まり、1/10 の速さで動く', () => {
    const a = startScrub(300, W, D, 12_000, true);
    expect(a.ms).toBe(12_000);
    expect(moveScrub(a, 310, W, D, true).ms).toBeCloseTo(12_100, 6);
    expect(moveScrub(a, 290, W, D, true).ms).toBeCloseTo(11_900, 6);
  });

  it('途中で ⌥ を押し離ししても飛ばない', () => {
    let a = startScrub(300, W, D, 0, false); // 30,000
    let step = moveScrub(a, 320, W, D, false); // 32,000
    a = step.anchor;
    expect(step.ms).toBe(32_000);
    // ⌥ を押した（同じ x）
    step = moveScrub(a, 320, W, D, true);
    expect(step.ms).toBe(32_000);
    a = step.anchor;
    step = moveScrub(a, 340, W, D, true); // +20px × 100ms × 0.1
    expect(step.ms).toBeCloseTo(32_200, 6);
    a = step.anchor;
    // ⌥ を離した（同じ x）：その位置から 1 倍で動く
    step = moveScrub(a, 340, W, D, false);
    expect(step.ms).toBeCloseTo(32_200, 6);
    a = step.anchor;
    expect(moveScrub(a, 350, W, D, false).ms).toBeCloseTo(33_200, 6);
  });
});

describe('tooltipLeft', () => {
  it('左右の端からはみ出さない', () => {
    expect(tooltipLeft(5, 400, 60)).toBe(30);
    expect(tooltipLeft(200, 400, 60)).toBe(200);
    expect(tooltipLeft(395, 400, 60)).toBe(370);
  });

  it('幅が足りなければ真ん中', () => {
    expect(tooltipLeft(5, 40, 60)).toBe(20);
  });
});

describe('frameStepTarget', () => {
  const F = 1000 / 24; // 41.666…

  it('次・前のコマの真ん中へ', () => {
    // 10 コマ目の中ほど
    const pos = 10.3 * F;
    expect(frameStepTarget(pos, F, 1, null)).toBeCloseTo(11.5 * F, 6);
    expect(frameStepTarget(pos, F, -1, null)).toBeCloseTo(9.5 * F, 6);
  });

  it('続けて押しても 1 コマずつ進む（真ん中から真ん中へ）', () => {
    let pos = 0;
    for (let i = 0; i < 5; i++) {
      pos = frameStepTarget(pos, F, 1, null);
    }
    expect(pos).toBeCloseTo(5.5 * F, 6);
    for (let i = 0; i < 2; i++) {
      pos = frameStepTarget(pos, F, -1, null);
    }
    expect(pos).toBeCloseTo(3.5 * F, 6);
  });

  it('境目ちょうど（小数の誤差）でもそのコマと数える', () => {
    const pos = 3 * F - 1e-9;
    expect(frameStepTarget(pos, F, 1, null)).toBeCloseTo(4.5 * F, 6);
  });

  it('0 より前へは行かず、終わりから半コマ手前で止まる', () => {
    expect(frameStepTarget(0, F, -1, null)).toBeCloseTo(0.5 * F, 6);
    const end = 100 * F;
    expect(frameStepTarget(99.5 * F, F, 1, end)).toBeCloseTo(99.5 * F, 6);
  });

  it('コマの長さがなければ 0.1 秒', () => {
    expect(frameStepTarget(1_000, 0, 1, 10_000)).toBe(1_100);
    expect(frameStepTarget(50, 0, -1, 10_000)).toBe(0);
  });
});
