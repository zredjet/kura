import { describe, expect, it } from 'vitest';

import { cssColor, hsvToRgb, hueStops, parseHex, rgbToHsv, toHex } from './color';

describe('色の計算', () => {
  it('RGB と HSV は行き来しても同じ色に戻る', () => {
    for (const rgb of [
      [255, 0, 0],
      [0, 128, 255],
      [12, 34, 56],
      [200, 200, 200],
      [0, 0, 0],
      [255, 255, 255],
    ] as const) {
      const hsv = rgbToHsv(rgb[0], rgb[1], rgb[2]);
      expect(hsvToRgb(hsv.h, hsv.s, hsv.v)).toEqual([...rgb]);
    }
    expect(rgbToHsv(0, 255, 0)).toEqual({ h: 120, s: 1, v: 1 });
    expect(hsvToRgb(360, 1, 1)).toEqual([255, 0, 0]);
  });

  it('16 進の表記：不透明なら 6 桁、そうでなければ 8 桁。3・4 桁と「#」なしも読む', () => {
    expect(toHex([255, 128, 0, 255])).toBe('#FF8000');
    expect(toHex([255, 128, 0, 16])).toBe('#FF800010');
    expect(parseHex('#ff8000')).toEqual([255, 128, 0, 255]);
    expect(parseHex('ff800010')).toEqual([255, 128, 0, 16]);
    expect(parseHex('#f80')).toEqual([255, 136, 0, 255]);
    expect(parseHex('#f808')).toEqual([255, 136, 0, 136]);
    expect(parseHex('#ff80')).toEqual([255, 255, 136, 0]);
    expect(parseHex('nope')).toBeNull();
    expect(parseHex('#12345')).toBeNull();
  });

  it('CSS の色の値と色相の帯', () => {
    expect(cssColor([1, 2, 3, 255])).toBe('rgba(1, 2, 3, 1)');
    expect(cssColor([1, 2, 3, 0])).toBe('rgba(1, 2, 3, 0)');
    expect(hueStops()).toHaveLength(7);
  });
});
