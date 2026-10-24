/**
 * 切り抜きの枠の計算（純粋な関数。M12 の段 5b。design.md §3.1「画像エディタ」の切り抜き）。
 *
 * - 枠は等倍の画像の座標の整数の矩形（左上 x・y と幅・高さ）。ドラッグの量（画像の画素。小数でもよい）を丸めて整数に
 *   吸着する。どの操作でも幅・高さは 1 以上で、画像（`bounds`）の中に収める。
 * - ハンドルは 4 隅と 4 辺、中をドラッグすると動かす。⇧ で比（始めた時の枠の縦横比）を保つ：隅は向かいの隅を、辺は向かいの
 *   辺の中央を止めて広げる。
 * - 矢印で 1px・⇧ 矢印で 10px 動かす。
 */

import type { Rect } from './viewMath';

/** 画像の大きさ。 */
export interface Bounds {
  w: number;
  h: number;
}

/** 枠のつかむ所。 */
export type CropHandle = 'move' | 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw';

export const CROP_HANDLES: readonly Exclude<CropHandle, 'move'>[] = [
  'nw',
  'n',
  'ne',
  'e',
  'se',
  's',
  'sw',
  'w',
];

/** 矢印 1 回の量（⇧ で 10px）。 */
export const NUDGE = 1;
export const NUDGE_SHIFT = 10;

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/** 画像の中の整数の矩形にする（幅・高さは 1 以上。画像が空なら 1×1）。 */
export function clampCrop(r: Rect, b: Bounds): Rect {
  const bw = Math.max(1, Math.round(b.w));
  const bh = Math.max(1, Math.round(b.h));
  const w = clamp(Math.round(r.w), 1, bw);
  const h = clamp(Math.round(r.h), 1, bh);
  return { x: clamp(Math.round(r.x), 0, bw - w), y: clamp(Math.round(r.y), 0, bh - h), w, h };
}

/** 画像全体か（切り抜いても変わらない）。 */
export function isFullCrop(r: Rect, b: Bounds): boolean {
  return r.x === 0 && r.y === 0 && r.w === b.w && r.h === b.h;
}

/** 同じ矩形か。 */
export function sameRect(a: Rect | null, b: Rect | null): boolean {
  return a !== null && b !== null && a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;
}

/** 比を保って、`maxW`・`maxH` の中に収める（1 以上の整数）。 */
function fitRatio(
  w: number,
  h: number,
  ratio: number,
  maxW: number,
  maxH: number,
): [number, number] {
  let fw = w;
  let fh = h;
  if (fw > maxW) {
    fw = maxW;
    fh = fw / ratio;
  }
  if (fh > maxH) {
    fh = maxH;
    fw = fh * ratio;
  }
  return [clamp(Math.round(fw), 1, Math.max(1, maxW)), clamp(Math.round(fh), 1, Math.max(1, maxH))];
}

/**
 * ドラッグした枠（`start` はつかんだ時の枠、`dx`・`dy` はつかんでからの量（画像の画素）。`keepRatio` は ⇧）。
 */
export function dragCrop(
  start: Rect,
  handle: CropHandle,
  dx: number,
  dy: number,
  b: Bounds,
  keepRatio = false,
): Rect {
  const s = clampCrop(start, b);
  if (handle === 'move') {
    return clampCrop({ ...s, x: s.x + dx, y: s.y + dy }, b);
  }
  const left = handle.includes('w');
  const right = handle.includes('e');
  const top = handle.includes('n');
  const bottom = handle.includes('s');
  const l0 = s.x;
  const t0 = s.y;
  const r0 = s.x + s.w;
  const b0 = s.y + s.h;
  if (!keepRatio) {
    const l = left ? clamp(Math.round(l0 + dx), 0, r0 - 1) : l0;
    const r = right ? clamp(Math.round(r0 + dx), l0 + 1, b.w) : r0;
    const t = top ? clamp(Math.round(t0 + dy), 0, b0 - 1) : t0;
    const btm = bottom ? clamp(Math.round(b0 + dy), t0 + 1, b.h) : b0;
    return clampCrop({ x: l, y: t, w: r - l, h: btm - t }, b);
  }
  const ratio = s.w / s.h;
  if ((left || right) && (top || bottom)) {
    // 隅：向かいの隅を止め、大きく動いた軸に合わせる
    const ax = left ? r0 : l0;
    const ay = top ? b0 : t0;
    const px = (left ? l0 : r0) + dx;
    const py = (top ? t0 : b0) + dy;
    const k = Math.max(Math.abs(px - ax) / s.w, Math.abs(py - ay) / s.h);
    const maxW = left ? ax : b.w - ax;
    const maxH = top ? ay : b.h - ay;
    const [w, h] = fitRatio(s.w * k, s.h * k, ratio, maxW, maxH);
    return clampCrop({ x: left ? ax - w : ax, y: top ? ay - h : ay, w, h }, b);
  }
  if (left || right) {
    // 左右の辺：向かいの辺を止め、高さは縦の中央を保って比で決める
    const ax = left ? r0 : l0;
    const want = Math.abs((left ? l0 : r0) + dx - ax);
    const cy = t0 + s.h / 2;
    const maxW = left ? ax : b.w - ax;
    const maxH = 2 * Math.min(cy, b.h - cy);
    const [w, h] = fitRatio(want, want / ratio, ratio, maxW, maxH);
    return clampCrop({ x: left ? ax - w : ax, y: cy - h / 2, w, h }, b);
  }
  // 上下の辺
  const ay = top ? b0 : t0;
  const want = Math.abs((top ? t0 : b0) + dy - ay);
  const cx = l0 + s.w / 2;
  const maxH = top ? ay : b.h - ay;
  const maxW = 2 * Math.min(cx, b.w - cx);
  const [w, h] = fitRatio(want * ratio, want, ratio, maxW, maxH);
  return clampCrop({ x: cx - w / 2, y: top ? ay - h : ay, w, h }, b);
}

/** 矢印で動かす（`shift` で 10px）。 */
export function nudgeCrop(r: Rect, key: string, shift: boolean, b: Bounds): Rect | null {
  const step = shift ? NUDGE_SHIFT : NUDGE;
  const delta: Record<string, [number, number]> = {
    ArrowLeft: [-step, 0],
    ArrowRight: [step, 0],
    ArrowUp: [0, -step],
    ArrowDown: [0, step],
  };
  const d = delta[key];
  return d ? clampCrop({ ...r, x: r.x + d[0], y: r.y + d[1] }, b) : null;
}

/** 矩形の値（`{x, y, w, h}`）を読む（読めなければ null）。 */
export function readRect(value: unknown): Rect | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as Record<string, unknown>;
  const n = (k: string) => (typeof v[k] === 'number' ? (v[k] as number) : null);
  const x = n('x');
  const y = n('y');
  const w = n('w');
  const h = n('h');
  return x === null || y === null || w === null || h === null ? null : { x, y, w, h };
}
