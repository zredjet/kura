import { describe, expect, it } from 'vitest';

import { parseFrames, parseTime, timeText } from './timeText';

describe('時刻の欄の文字', () => {
  it('フレームを「m:ss.mmm」（1 時間を超えれば時も）にする', () => {
    expect(timeText(48_000 * 4.2, 48_000)).toBe('0:04.200');
    expect(timeText(185_220, 44_100)).toBe('0:04.200');
    expect(timeText(48_000 * 3723.456, 48_000)).toBe('1:02:03.456');
    expect(timeText(10, 0)).toBe('');
  });

  it('「1:02.345」「62.345」「1:02:03.5」「0:04」を秒にする（読めなければ null）', () => {
    expect(parseTime('1:02.345')).toBeCloseTo(62.345, 9);
    expect(parseTime(' 62.345 ')).toBeCloseTo(62.345, 9);
    expect(parseTime('1:02:03.5')).toBeCloseTo(3723.5, 9);
    expect(parseTime('0:04')).toBe(4);
    expect(parseTime('１')).toBeNull();
    expect(parseTime('1：02．5')).toBeCloseTo(62.5, 9);
    expect(parseTime('1:60')).toBeNull();
    expect(parseTime('1:61:00')).toBeNull();
    expect(parseTime('-1')).toBeNull();
    expect(parseTime('abc')).toBeNull();
    expect(parseTime('')).toBeNull();
  });

  it('フレームにして範囲に収める', () => {
    expect(parseFrames('0:01.5', 48_000)).toBe(72_000);
    expect(parseFrames('9:00', 48_000, 0, 1000)).toBe(1000);
    expect(parseFrames('x', 48_000)).toBeNull();
    expect(parseFrames('1', 0)).toBeNull();
  });
});
