/**
 * エディタのキャンバスの座標の計算（DOM に触れない純粋な関数。docs/editing.md §12.7・§12.13）。
 *
 * - 座標は 3 つ：画面（キャンバスの面の左上からの CSS px）・画像（等倍のキャンバスの座標。整数の画素の左上が整数）・
 *   段 k の画像（各軸 ⌈n/2^k⌉。段の画素 i は等倍の [i·2^k, (i+1)·2^k) を覆う。右端・下端は等倍の大きさで切る）。
 * - 表示の変換は `{ zoom, ox, oy }`：画像の画素 1 つが画面の `zoom` CSS px で、画像の (0, 0) が画面の (ox, oy)。
 * - 100% は画像の 1 画素を 1pt（CSS px）。ドット絵は装置の画素の整数倍に吸着し（縮めるときは吸着しない）、位置も装置の
 *   画素にそろえる（最近傍の境目がにじまない）。
 * - タイルは 1 辺 256（`EditViewDto.tile`）・フレームの画像の座標（フレームの矩形は段の座標で、キャンバスの中の位置を持つ）。
 */

/** 表示の変換。 */
export interface Transform {
  /** 画像の 1 画素あたりの CSS px。 */
  zoom: number;
  /** 画像の (0, 0) の画面の位置（CSS px）。 */
  ox: number;
  oy: number;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Size {
  w: number;
  h: number;
}

/** 「全体を表示」のときに、上部と下のカプセルの分をあける量（CSS px）。 */
export interface Insets {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/** タイルの位置 [tx, ty]。 */
export type TileXY = readonly [number, number];

/** 段の画像の中のフレームの矩形（`EditFrameDto` の位置と大きさ）。 */
export interface FrameRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const ZOOM_MIN = 0.01;
export const ZOOM_MAX = 64;

/** ⌘+・⌘− で移る倍率（ドット絵は装置の画素の整数倍に吸着してから比べる）。 */
export const ZOOM_PRESETS: readonly number[] = [
  0.01,
  0.02,
  0.03,
  0.05,
  1 / 16,
  1 / 12,
  1 / 8,
  1 / 6,
  1 / 4,
  1 / 3,
  1 / 2,
  2 / 3,
  1,
  1.5,
  2,
  3,
  4,
  5,
  6,
  8,
  10,
  12,
  16,
  20,
  24,
  32,
  40,
  48,
  64,
];

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function positive(n: number): boolean {
  return Number.isFinite(n) && n > 0;
}

/** 装置の画素の比（有限の正の数でなければ 1）。 */
export function safeDpr(dpr: number): number {
  return positive(dpr) ? dpr : 1;
}

/**
 * 倍率を範囲に収め、ドット絵なら装置の画素の整数倍に吸着する（装置の画素で 1 倍未満に縮めるときは吸着しない。
 * 縮小は補間で描く）。
 */
export function snapZoom(zoom: number, dpr: number, pixelArt: boolean): number {
  const z = clamp(positive(zoom) ? zoom : 1, ZOOM_MIN, ZOOM_MAX);
  if (!pixelArt) return z;
  const d = safeDpr(dpr);
  const device = z * d;
  if (device < 1) return z;
  return Math.max(1, Math.round(device)) / d;
}

/**
 * 「全体を表示」の倍率：枠に収まる最大。ドット絵は装置の画素の整数倍（1 倍に満たなければ縮める）、ほかは広げない
 * （1 を超えない）。
 */
export function fitZoom(w: number, h: number, area: Size, dpr: number, pixelArt: boolean): number {
  if (![w, h, area.w, area.h].every(positive)) return 1;
  const s = Math.min(area.w / w, area.h / h);
  const d = safeDpr(dpr);
  if (pixelArt) {
    const device = s * d;
    return clamp(device >= 1 ? Math.floor(device) / d : s, ZOOM_MIN, ZOOM_MAX);
  }
  return clamp(Math.min(1, s), ZOOM_MIN, ZOOM_MAX);
}

/** 位置を装置の画素にそろえる。 */
export function snapOffset(t: Transform, dpr: number): Transform {
  const d = safeDpr(dpr);
  return { zoom: t.zoom, ox: Math.round(t.ox * d) / d, oy: Math.round(t.oy * d) / d };
}

/** 面のうち、上部と下のカプセルを除いた範囲（「全体を表示」の枠）。 */
export function fitArea(stage: Size, insets: Insets): Rect {
  return {
    x: insets.left,
    y: insets.top,
    w: Math.max(1, stage.w - insets.left - insets.right),
    h: Math.max(1, stage.h - insets.top - insets.bottom),
  };
}

/** 「全体を表示」の変換：枠の中央に置く。 */
export function fitTransform(
  w: number,
  h: number,
  area: Rect,
  dpr: number,
  pixelArt: boolean,
): Transform {
  const zoom = fitZoom(w, h, area, dpr, pixelArt);
  return snapOffset(
    {
      zoom,
      ox: area.x + (area.w - w * zoom) / 2,
      oy: area.y + (area.h - h * zoom) / 2,
    },
    dpr,
  );
}

/** 画面の点 (sx, sy) の下の画像の点を動かさずに倍率を変える。 */
export function zoomAround(t: Transform, zoom: number, sx: number, sy: number): Transform {
  const ix = (sx - t.ox) / t.zoom;
  const iy = (sy - t.oy) / t.zoom;
  return { zoom, ox: sx - ix * zoom, oy: sy - iy * zoom };
}

/** ⌘+（dir > 0）・⌘−（dir < 0）の次の倍率。端まで来ていれば今のまま。 */
export function nextZoom(zoom: number, dir: number, dpr: number, pixelArt: boolean): number {
  const eps = 1e-6;
  if (dir > 0) {
    for (const p of ZOOM_PRESETS) {
      const z = snapZoom(p, dpr, pixelArt);
      if (z > zoom * (1 + eps)) return z;
    }
    return zoom;
  }
  for (let i = ZOOM_PRESETS.length - 1; i >= 0; i--) {
    const z = snapZoom(ZOOM_PRESETS[i] ?? 1, dpr, pixelArt);
    if (z < zoom * (1 - eps)) return z;
  }
  return zoom;
}

/** 画像の一部が面に残るように位置を抑える（少なくとも `keep` CSS px。画像がそれより小さければ画像の大きさ）。 */
export function clampTransform(t: Transform, stage: Size, image: Size, keep = 48): Transform {
  const iw = image.w * t.zoom;
  const ih = image.h * t.zoom;
  const kx = Math.min(keep, iw);
  const ky = Math.min(keep, ih);
  return {
    zoom: t.zoom,
    ox: clamp(t.ox, kx - iw, stage.w - kx),
    oy: clamp(t.oy, ky - ih, stage.h - ky),
  };
}

/** 画面の点の下の画像の座標（小数）。 */
export function screenToImage(t: Transform, sx: number, sy: number): { x: number; y: number } {
  return { x: (sx - t.ox) / t.zoom, y: (sy - t.oy) / t.zoom };
}

/** 画面の点の下の画素（整数。画像の外でも返す）。 */
export function imagePixel(t: Transform, sx: number, sy: number): [number, number] {
  const p = screenToImage(t, sx, sy);
  return [Math.floor(p.x), Math.floor(p.y)];
}

/** 画像の矩形（等倍）の画面の矩形。 */
export function imageScreenRect(t: Transform, image: Size): Rect {
  return { x: t.ox, y: t.oy, w: image.w * t.zoom, h: image.h * t.zoom };
}

/** 2 つの矩形の重なり（なければ null）。 */
export function intersect(a: Rect, b: Rect): Rect | null {
  const x0 = Math.max(a.x, b.x);
  const y0 = Math.max(a.y, b.y);
  const x1 = Math.min(a.x + a.w, b.x + b.w);
  const y1 = Math.min(a.y + a.h, b.y + b.h);
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } : null;
}

/**
 * 面に見えている画像の範囲（等倍の整数の矩形。面の外へ `margin` CSS px 広げ、画像の中に切る）。見えていなければ null。
 */
export function visibleImageRect(t: Transform, stage: Size, image: Size, margin = 0): Rect | null {
  if (!positive(t.zoom)) return null;
  const x0 = Math.max(0, Math.floor((-margin - t.ox) / t.zoom));
  const y0 = Math.max(0, Math.floor((-margin - t.oy) / t.zoom));
  const x1 = Math.min(image.w, Math.ceil((stage.w + margin - t.ox) / t.zoom));
  const y1 = Math.min(image.h, Math.ceil((stage.h + margin - t.oy) / t.zoom));
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } : null;
}

/** タイルの範囲（半開区間）。 */
export interface TileRange {
  tx0: number;
  ty0: number;
  tx1: number;
  ty1: number;
}

/**
 * 等倍の矩形 `rect`（キャンバスの座標）を覆う、段 `level` のフレームのタイルの範囲（フレームの画像の座標・1 辺 `tile`）。
 * 段の座標には始まりを床・終わりを天井で直す（Rust の `view_area` と同じ）。重ならなければ null。
 */
export function tileRange(
  rect: Rect,
  level: number,
  frame: FrameRect,
  tile: number,
): TileRange | null {
  const s = 2 ** level;
  const lx0 = Math.floor(rect.x / s) - frame.x;
  const ly0 = Math.floor(rect.y / s) - frame.y;
  const lx1 = Math.ceil((rect.x + rect.w) / s) - frame.x;
  const ly1 = Math.ceil((rect.y + rect.h) / s) - frame.y;
  const cols = Math.ceil(frame.w / tile);
  const rows = Math.ceil(frame.h / tile);
  const tx0 = clamp(Math.floor(lx0 / tile), 0, cols);
  const ty0 = clamp(Math.floor(ly0 / tile), 0, rows);
  const tx1 = clamp(Math.ceil(lx1 / tile), 0, cols);
  const ty1 = clamp(Math.ceil(ly1 / tile), 0, rows);
  return tx0 < tx1 && ty0 < ty1 ? { tx0, ty0, tx1, ty1 } : null;
}

/** 範囲のタイル（行の順）。 */
export function tilesIn(range: TileRange | null): TileXY[] {
  const out: TileXY[] = [];
  if (!range) return out;
  for (let ty = range.ty0; ty < range.ty1; ty++) {
    for (let tx = range.tx0; tx < range.tx1; tx++) out.push([tx, ty]);
  }
  return out;
}

/** 範囲にタイルが入っているか。 */
export function inRange(range: TileRange | null, tx: number, ty: number): boolean {
  return range !== null && tx >= range.tx0 && tx < range.tx1 && ty >= range.ty0 && ty < range.ty1;
}

/** タイルの大きさ（段の画素。右端・下端は小さい）。 */
export function tileSize(frame: FrameRect, tx: number, ty: number, tile: number): Size {
  return {
    w: Math.max(0, Math.min(tile, frame.w - tx * tile)),
    h: Math.max(0, Math.min(tile, frame.h - ty * tile)),
  };
}

/** 段 `level` のタイルが覆う等倍の矩形（キャンバスの座標。右端・下端は段の大きさのまま 2^level 倍）。 */
export function tileImageRect(
  level: number,
  frame: FrameRect,
  tx: number,
  ty: number,
  tile: number,
): Rect {
  const s = 2 ** level;
  const size = tileSize(frame, tx, ty, tile);
  return {
    x: (frame.x + tx * tile) * s,
    y: (frame.y + ty * tile) * s,
    w: size.w * s,
    h: size.h * s,
  };
}

/** 等倍の矩形の画面の矩形。 */
export function toScreen(t: Transform, r: Rect): Rect {
  return { x: t.ox + r.x * t.zoom, y: t.oy + r.y * t.zoom, w: r.w * t.zoom, h: r.h * t.zoom };
}

/** 矩形を `k` 倍する（CSS px → 装置の画素）。 */
export function scaleRect(r: Rect, k: number): Rect {
  return { x: r.x * k, y: r.y * k, w: r.w * k, h: r.h * k };
}

/** 段の画素を補間して描くか：装置の画素で 1 倍未満に縮めるときだけ補間、広げるときは最近傍。 */
export function smoothingFor(zoom: number, dpr: number, level: number): boolean {
  return zoom * safeDpr(dpr) * 2 ** level < 1 - 1e-9;
}

/** 表示の倍率の文字（「200%」「12.5%」）。 */
export function zoomPercent(zoom: number): string {
  const p = zoom * 100;
  if (p >= 10) return `${Math.round(p)}%`;
  return `${Math.round(p * 10) / 10}%`;
}

/** ホイールの量を CSS px にする（行・ページ単位のものを直す）。 */
export function wheelPixels(delta: number, mode: number, page: number): number {
  if (mode === 1) return delta * 16;
  if (mode === 2) return delta * page;
  return delta;
}

/** ⌃ホイール（トラックパッドのピンチ）の量から倍率の掛け算の値。 */
export function wheelZoomFactor(deltaPx: number): number {
  return Math.exp(-clamp(deltaPx, -100, 100) * 0.01);
}

/** 画素の形式（`EditViewDto.format`）が透明を持てるか（アルファのある形式とパレット。エンジンの `can_be_transparent`）。 */
export function formatCanBeTransparent(format: string | null | undefined): boolean {
  return (
    format === 'grayA8' ||
    format === 'rgba8' ||
    format === 'grayA16' ||
    format === 'rgba16' ||
    format === 'indexed8'
  );
}

/** 端を装置の画素の整数にそろえた矩形（隣り合うタイルのあいだに隙間を作らない）。 */
export function roundRect(r: Rect): Rect {
  const x0 = Math.round(r.x);
  const y0 = Math.round(r.y);
  return { x: x0, y: y0, w: Math.round(r.x + r.w) - x0, h: Math.round(r.y + r.h) - y0 };
}
