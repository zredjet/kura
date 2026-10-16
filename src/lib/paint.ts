/**
 * 描画の規則（エンジンの `crates/kura-engine/src/paint` と同じ。docs/editing.md §10）。
 *
 * エディタで描いている最中の仮の表示に使い、確定したらエンジンの描画結果に置き換える。
 * 両者が同じになることを、エンジンが凍結した共有のベクタ（`crates/kura-engine/tests/vectors/paint/`）で確かめる。
 * どれも整数だけで計算する（32 ビットを超える値があるので、ビット演算ではなく四則演算で書く）。
 */

/** 画素の整数座標 [x, y]。 */
export type Point = readonly [number, number];

/** 筆の形。 */
export type BrushShape = 'square' | 'circle';

/** 筆（大きさは 1〜255）。 */
export interface Brush {
  shape: BrushShape;
  size: number;
}

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

/** 座標として受け付ける範囲（±2^31。エンジンの `geom::COORD_LIMIT`）。 */
const COORD_LIMIT = 2 ** 31;

function zigzag(v: number): number {
  return v >= 0 ? v * 2 : -v * 2 - 1;
}

function unzigzag(v: number): number {
  return v % 2 === 0 ? v / 2 : -(v + 1) / 2;
}

function putVarint(v: number, out: number[]): void {
  let rest = v;
  for (;;) {
    const b = rest % 128;
    rest = Math.floor(rest / 128);
    if (rest === 0) {
      out.push(b);
      return;
    }
    out.push(b + 128);
  }
}

function base64(bytes: readonly number[]): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const n = Math.min(3, bytes.length - i);
    const v = (bytes[i] ?? 0) * 65536 + (bytes[i + 1] ?? 0) * 256 + (bytes[i + 2] ?? 0);
    for (let k = 0; k <= n; k++) {
      out += ALPHABET[Math.floor(v / 2 ** (18 - 6 * k)) % 64];
    }
  }
  return out;
}

/** 点の列を詰めた文字列にする（差分の zigzag・LEB128・base64url。`=` なし）。 */
export function packPoints(points: readonly Point[]): string {
  const bytes: number[] = [];
  let px = 0;
  let py = 0;
  points.forEach(([x, y], i) => {
    putVarint(zigzag(i === 0 ? x : x - px), bytes);
    putVarint(zigzag(i === 0 ? y : y - py), bytes);
    px = x;
    py = y;
  });
  return base64(bytes);
}

/** 詰めた文字列から点の列を読む。正規の書き方でない・座標が範囲の外なら null（エンジンの `PointList::unpack` と同じ）。 */
export function unpackPoints(text: string): Point[] | null {
  if (text.length % 4 === 1) return null;
  const bytes: number[] = [];
  for (let i = 0; i < text.length; i += 4) {
    const chunk = text.slice(i, i + 4);
    let v = 0;
    for (let k = 0; k < chunk.length; k++) {
      const d = ALPHABET.indexOf(chunk.charAt(k));
      if (d < 0) return null;
      v += d * 2 ** (18 - 6 * k);
    }
    for (let k = 0; k < chunk.length - 1; k++) {
      bytes.push(Math.floor(v / 2 ** (16 - 8 * k)) % 256);
    }
  }
  const values: number[] = [];
  let at = 0;
  while (at < bytes.length) {
    let v = 0;
    let scale = 1;
    // エンジンと同じく、1 つの値は 10 バイトまで。値は整数で正確に表せる範囲（2^53）まで
    for (let n = 0; ; n++) {
      const b = bytes[at++];
      if (b === undefined || n >= 10) return null;
      v += (b % 128) * scale;
      scale *= 128;
      if (!Number.isSafeInteger(v)) return null;
      if (b < 128) break;
    }
    values.push(unzigzag(v));
  }
  if (values.length % 2 !== 0) return null;
  const points: Point[] = [];
  for (let i = 0; i < values.length; i += 2) {
    const dx = values[i] ?? 0;
    const dy = values[i + 1] ?? 0;
    const prev = points[points.length - 1];
    const p: Point = prev ? [prev[0] + dx, prev[1] + dy] : [dx, dy];
    // 座標は ±2^31 まで（エンジンの coord_ok と同じ）
    if (Math.abs(p[0]) > COORD_LIMIT || Math.abs(p[1]) > COORD_LIMIT) return null;
    points.push(p);
  }
  return packPoints(points) === text ? points : null;
}

/** ブレゼンハムの線（両端を含む）。 */
function line(a: Point, b: Point): Point[] {
  let [x, y] = a;
  const [x1, y1] = b;
  const dx = Math.abs(x1 - x);
  const dy = -Math.abs(y1 - y);
  const sx = x < x1 ? 1 : -1;
  const sy = y < y1 ? 1 : -1;
  let err = dx + dy;
  const out: Point[] = [];
  for (;;) {
    out.push([x, y]);
    if (x === x1 && y === y1) return out;
    const e2 = 2 * err;
    if (e2 >= dy) {
      err += dy;
      x += sx;
    }
    if (e2 <= dx) {
      err += dx;
      y += sy;
    }
  }
}

function isL(p: Point, c: Point, n: Point): boolean {
  return (
    (p[0] === c[0] || p[1] === c[1]) &&
    (n[0] === c[0] || n[1] === c[1]) &&
    p[0] !== n[0] &&
    p[1] !== n[1]
  );
}

/** 点の列をつないだ道（同じ点が続いたら 1 つ。ピクセルパーフェクトなら L 字の角を除く）。 */
export function path(points: readonly Point[], pixelPerfect: boolean): Point[] {
  const raw: Point[] = [];
  const first = points[0];
  if (!first) return raw;
  raw.push(first);
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1];
    const b = points[i];
    if (!a || !b) continue;
    for (const p of line(a, b).slice(1)) {
      const last = raw[raw.length - 1];
      if (!last || last[0] !== p[0] || last[1] !== p[1]) raw.push(p);
    }
  }
  if (!pixelPerfect) return raw;
  const out: Point[] = [];
  for (let i = 0; i < raw.length; i++) {
    const c = raw[i];
    if (!c) continue;
    const p = out[out.length - 1];
    const n = raw[i + 1];
    if (p && n && isL(p, c, n)) continue;
    out.push(c);
  }
  return out;
}

/** 筆の形の画素（点からのずれ。行の順）。 */
export function footprint(brush: Brush): Point[] {
  const s = Math.max(1, brush.size);
  const o = Math.floor((s - 1) / 2);
  const out: Point[] = [];
  for (let j = 0; j < s; j++) {
    for (let i = 0; i < s; i++) {
      const u = 2 * i + 1 - s;
      const v = 2 * j + 1 - s;
      if (brush.shape === 'square' || u * u + v * v <= s * s - s) out.push([i - o, j - o]);
    }
  }
  return out;
}

/** ストロークの画素（幅 × 高さの画像の中だけ。行の順、重なりなし）。 */
export function strokePixels(
  points: readonly Point[],
  brush: Brush,
  pixelPerfect: boolean,
  width: number,
  height: number,
): Point[] {
  const mask = new Uint8Array(width * height);
  const fp = footprint(brush);
  for (const [x, y] of path(points, pixelPerfect)) {
    for (const [dx, dy] of fp) {
      const px = x + dx;
      const py = y + dy;
      if (px >= 0 && py >= 0 && px < width && py < height) mask[py * width + px] = 1;
    }
  }
  const out: Point[] = [];
  mask.forEach((v, i) => {
    if (v) out.push([i % width, Math.floor(i / width)]);
  });
  return out;
}

/**
 * ストレートのアルファの source-over（8 ビット。並びは色＋アルファで、RGBA8 か GrayA8）。
 * エンジンの `imaging::composite::over` と同じ整数の式。
 */
export function over8(dst: readonly number[], src: readonly number[]): number[] {
  const n = src.length - 1;
  const sa = src[n] ?? 0;
  const da = dst[n] ?? 0;
  if (sa === 255) return [...src];
  if (sa === 0) return [...dst];
  const da2 = Math.floor((da * (255 - sa) + 127) / 255);
  const oa = sa + da2;
  if (oa === 0) return src.map(() => 0);
  const out = src.map((s, i) =>
    i === n ? oa : Math.floor((s * sa + (dst[i] ?? 0) * da2 + Math.floor(oa / 2)) / oa),
  );
  return out;
}
