/**
 * 画像の素材の表示の決め方（docs/design.md §3.1「素材の表示」）：透過の下地の市松模様と、ドット絵の最近傍の拡大。
 * DOM に触れない純粋な関数。部品（MediaImage・AnimPlayer）はこれを呼んで `data-checker`・`image-rendering`・大きさを決める。
 */

/** 解析の透過（media_facts.alpha）。 */
export type AlphaKind = 'none' | 'binary' | 'partial';

/** 市松模様を敷く表示の短い辺の下限（CSS px）。24px の行・44px の表紙には敷かない。 */
export const CHECKER_MIN_PX = 48;

/**
 * 市松模様を敷くか。解析で透過（二値・半透明）と分かったものだけ。未解析（null）・不透明なら敷かない
 * （読み込み中のちらつきを避ける）。`shortSidePx`（表示の短い辺）を渡せば、小さい所では敷かない。
 */
export function showChecker(alpha: AlphaKind | null | undefined, shortSidePx?: number): boolean {
  if (alpha !== 'binary' && alpha !== 'partial') return false;
  return shortSidePx === undefined || shortSidePx >= CHECKER_MIN_PX;
}

export interface Size {
  w: number;
  h: number;
}

/**
 * 表示の部品が市松模様を敷くか（MediaImage の `<img>`・AnimPlayer の canvas で同じ規則）。画像（`ItemKindDto`）で、
 * 解析で透過と分かり、表示の大きさ `shown`（CSS px）が分かっていて短い辺が `CHECKER_MIN_PX` 以上のときだけ。
 * 大きさが分からないうち（null：枠を測る前・絵を読む前）は敷かない（小さい所で一瞬だけ出ないように）。
 */
export function checkerFor(
  kind: string,
  alpha: AlphaKind | null | undefined,
  shown: Size | null,
): boolean {
  return kind === 'image' && shown !== null && showChecker(alpha, Math.min(shown.w, shown.h));
}

/** 枠への収め方（タイルは cover、プレビューは contain）。 */
export type Fit = 'contain' | 'cover';

function positive(n: number): boolean {
  return Number.isFinite(n) && n > 0;
}

/**
 * `image-rendering` の値。ドット絵（実効値）で、装置の画素で 1 倍以上に広げるときだけ最近傍にする。
 * 縮めるときに最近傍にすると細い線が消えるので、縮小は既定の補間のまま。大きさが分からなければ補間。
 */
export function renderingFor(
  pixelArt: boolean,
  natural: Size,
  shown: Size,
  dpr: number,
  fit: Fit = 'contain',
): 'pixelated' | 'auto' {
  if (!pixelArt || ![natural.w, natural.h, shown.w, shown.h, dpr].every(positive)) return 'auto';
  const sx = (shown.w * dpr) / natural.w;
  const sy = (shown.h * dpr) / natural.h;
  const s = fit === 'cover' ? Math.max(sx, sy) : Math.min(sx, sy);
  return s >= 1 ? 'pixelated' : 'auto';
}

export interface PixelFit {
  /** 表示の大きさ（CSS px）。 */
  width: number;
  height: number;
  /** 装置の画素での倍率。 */
  scale: number;
  /** 整数倍か（偽なら箱に収まらないので縮めた。このときは補間で描く）。 */
  integer: boolean;
}

/**
 * ドット絵の表示の大きさ：箱（CSS px）に収まる最大の整数倍。装置の画素で整数倍にそろえる（DPR 2 なら CSS の
 * 0.5px 刻み）。1 倍に満たなければ縮めて収める。
 */
export function pixelFit(w: number, h: number, boxW: number, boxH: number, dpr = 1): PixelFit {
  if (![w, h, boxW, boxH, dpr].every(positive)) {
    return { width: 0, height: 0, scale: 0, integer: false };
  }
  const fit = Math.min((boxW * dpr) / w, (boxH * dpr) / h);
  if (fit >= 1) {
    const k = Math.floor(fit);
    return { width: (w * k) / dpr, height: (h * k) / dpr, scale: k, integer: true };
  }
  return { width: (w * fit) / dpr, height: (h * fit) / dpr, scale: fit, integer: false };
}

function powerOfTwo(n: number): boolean {
  if (!Number.isSafeInteger(n) || n <= 0) return false;
  let v = n;
  while (v % 2 === 0) v /= 2;
  return v === 1;
}

/** 幅・高さの両方が 2 のべき乗か（検索の `2のべき乗` と同じ判定。1 も 2 の 0 乗）。 */
export function isPowerOfTwo(w: number, h: number): boolean {
  return powerOfTwo(w) && powerOfTwo(h);
}
