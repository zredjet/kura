import { describe, expect, it } from 'vitest';

import { parseTime } from './time';

describe('parseTime', () => {
  it('「1:02.345」「62.345」「1:02:03.5」「0:04」を秒にする', () => {
    expect(parseTime('1:02.345')).toBeCloseTo(62.345, 9);
    expect(parseTime(' 62.345 ')).toBeCloseTo(62.345, 9);
    expect(parseTime('1:02:03.5')).toBeCloseTo(3723.5, 9);
    expect(parseTime('0:04')).toBe(4);
    expect(parseTime('83.4')).toBeCloseTo(83.4, 9);
    expect(parseTime('1:23.')).toBe(83);
  });

  it('全角の数字・「：」「．」も読む', () => {
    expect(parseTime('１')).toBe(1);
    expect(parseTime('1：02．5')).toBeCloseTo(62.5, 9);
    expect(parseTime('１：２３．４')).toBeCloseTo(83.4, 9);
  });

  it('読めなければ null（分・秒の欄は 60 未満、負は読まない）', () => {
    expect(parseTime('1:60')).toBeNull();
    expect(parseTime('1:61:00')).toBeNull();
    expect(parseTime('-1')).toBeNull();
    expect(parseTime('abc')).toBeNull();
    expect(parseTime('')).toBeNull();
    expect(parseTime('1::2')).toBeNull();
    expect(parseTime('.5')).toBeNull();
  });
});
