/**
 * 描画の色の計算（色を選ぶ部品。DOM に触れない純粋な関数）。色は RGBA8 のストレート。
 */

export type Rgba = [number, number, number, number];

/** 色相（0〜360）・彩度（0〜1）・明るさ（0〜1）。 */
export interface Hsv {
  h: number;
  s: number;
  v: number;
}

function byte(v: number): number {
  return Math.min(255, Math.max(0, Math.round(v)));
}

/** RGB（0〜255）を HSV にする（無彩色の色相は 0）。 */
export function rgbToHsv(r: number, g: number, b: number): Hsv {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const d = max - min;
  let h = 0;
  if (d > 0) {
    if (max === rn) h = 60 * (((gn - bn) / d + 6) % 6);
    else if (max === gn) h = 60 * ((bn - rn) / d + 2);
    else h = 60 * ((rn - gn) / d + 4);
  }
  return { h, s: max === 0 ? 0 : d / max, v: max };
}

/** HSV を RGB（0〜255 の整数）にする。 */
export function hsvToRgb(h: number, s: number, v: number): [number, number, number] {
  const hh = (((h % 360) + 360) % 360) / 60;
  const c = v * s;
  const x = c * (1 - Math.abs((hh % 2) - 1));
  const m = v - c;
  const [r, g, b] =
    hh < 1
      ? [c, x, 0]
      : hh < 2
        ? [x, c, 0]
        : hh < 3
          ? [0, c, x]
          : hh < 4
            ? [0, x, c]
            : hh < 5
              ? [x, 0, c]
              : [c, 0, x];
  return [byte((r + m) * 255), byte((g + m) * 255), byte((b + m) * 255)];
}

function hex2(n: number): string {
  return byte(n).toString(16).padStart(2, '0').toUpperCase();
}

/** 16 進の表記（不透明なら「#RRGGBB」、そうでなければ「#RRGGBBAA」）。 */
export function toHex(rgba: readonly number[]): string {
  const [r = 0, g = 0, b = 0, a = 255] = rgba;
  return `#${hex2(r)}${hex2(g)}${hex2(b)}${a === 255 ? '' : hex2(a)}`;
}

/** 16 進の表記を読む（「#」は省いてよい。3・4・6・8 桁。読めなければ null）。 */
export function parseHex(text: string): Rgba | null {
  const s = text.trim().replace(/^#/, '');
  if (!/^[0-9a-f]+$/i.test(s)) return null;
  const digits =
    s.length === 3 || s.length === 4
      ? s
          .split('')
          .map((c) => c + c)
          .join('')
      : s;
  if (digits.length !== 6 && digits.length !== 8) return null;
  const at = (i: number) => parseInt(digits.slice(i, i + 2), 16);
  return [at(0), at(2), at(4), digits.length === 8 ? at(6) : 255];
}

/** CSS の色の値（見本・つまみの帯に使う。利用者が選んだ色そのもの）。 */
export function cssColor(rgba: readonly number[]): string {
  const [r = 0, g = 0, b = 0, a = 255] = rgba;
  return `rgba(${byte(r)}, ${byte(g)}, ${byte(b)}, ${Math.round((byte(a) / 255) * 1000) / 1000})`;
}

/** 色相の帯（0°・60°・…・360° の 7 色）。 */
export function hueStops(): string[] {
  return [0, 60, 120, 180, 240, 300, 360].map((h) => cssColor([...hsvToRgb(h, 1, 1), 255]));
}
