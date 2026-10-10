import { describe, expect, it } from 'vitest';

import {
  formatBytes,
  formatCount,
  formatDuration,
  formatLongDuration,
  shortHash,
  tildePath,
} from './format';

describe('formatCount', () => {
  it('3 桁ごとに区切る', () => {
    expect(formatCount(12480)).toBe('12,480');
    expect(formatCount(0)).toBe('0');
  });
});

describe('formatBytes', () => {
  it('1000 単位で、100 未満は小数 1 桁', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(8_400_000)).toBe('8.4 MB');
    expect(formatBytes(98_000_000)).toBe('98.0 MB');
    expect(formatBytes(212_000_000)).toBe('212 MB');
    expect(formatBytes(18_400_000_000)).toBe('18.4 GB');
    expect(formatBytes(1_200_000_000_000)).toBe('1.2 TB');
  });

  it('丸めで 1000 に届いたら次の単位にする', () => {
    expect(formatBytes(999_960)).toBe('1.0 MB');
  });
});

describe('tildePath', () => {
  it('ホームフォルダの中だけ「~」にする', () => {
    expect(tildePath('/Users/me/Pictures/K.kuralib', '/Users/me')).toBe('~/Pictures/K.kuralib');
    expect(tildePath('/Users/me', '/Users/me/')).toBe('~');
    expect(tildePath('/Users/meg/x', '/Users/me')).toBe('/Users/meg/x');
    expect(tildePath('/Volumes/SSD/K.kuralib', null)).toBe('/Volumes/SSD/K.kuralib');
  });
});

describe('formatLongDuration', () => {
  it('2時間14分・14分・45秒', () => {
    expect(formatLongDuration(8_040_000)).toBe('2時間14分');
    expect(formatLongDuration(7_200_000)).toBe('2時間');
    expect(formatLongDuration(840_000)).toBe('14分');
    expect(formatLongDuration(45_000)).toBe('45秒');
  });
});

describe('formatDuration', () => {
  it('時間があれば h:mm:ss、なければ m:ss', () => {
    expect(formatDuration(4_360_000)).toBe('1:12:40');
    expect(formatDuration(258_000)).toBe('4:18');
    expect(formatDuration(45_000)).toBe('0:45');
    expect(formatDuration(0)).toBe('0:00');
  });
});

describe('shortHash', () => {
  it('先頭と末尾 4 桁', () => {
    expect(shortHash('9f2c0000000041a7')).toBe('b3:9f2c…41a7');
  });
});
