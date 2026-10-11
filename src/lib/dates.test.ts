import { describe, expect, it } from 'vitest';

import { agoLabel, DAY_MS, dayLabel, localDay, relativeDateTime } from './dates';

// 2026-10-08（木）は 1970-01-01 から 20,734 日目
const OCT8 = 20_734;

describe('dayLabel', () => {
  it('今日・昨日・今年・前の年', () => {
    expect(dayLabel(OCT8 + 2, OCT8 + 2)).toBe('今日');
    expect(dayLabel(OCT8 + 1, OCT8 + 2)).toBe('昨日');
    expect(dayLabel(OCT8, OCT8 + 2)).toBe('10月8日（木）');
    expect(dayLabel(OCT8 - 365, OCT8)).toBe('2025年10月8日（水）');
  });
});

describe('localDay', () => {
  it('時差を足してから日に区切る', () => {
    const utcMidnight = OCT8 * 86_400_000;
    expect(localDay(utcMidnight - 60_000, 0)).toBe(OCT8 - 1);
    expect(localDay(utcMidnight - 60_000, 540)).toBe(OCT8);
  });
});

describe('agoLabel', () => {
  it('今日・昨日・日・週・か月・年', () => {
    const now = new Date(2026, 9, 11, 12).getTime();
    const ago = (days: number) => agoLabel(now - days * DAY_MS, now);
    expect([ago(0), ago(1), ago(10), ago(21), ago(70), ago(400)]).toEqual([
      '今日',
      '昨日',
      '10日前',
      '3週間前',
      '2か月前',
      '1年前',
    ]);
  });
});

describe('relativeDateTime', () => {
  it('今日 21:02', () => {
    const now = new Date(2026, 9, 11, 23).getTime();
    expect(relativeDateTime(new Date(2026, 9, 11, 21, 2).getTime(), now)).toBe('今日 21:02');
  });
});
