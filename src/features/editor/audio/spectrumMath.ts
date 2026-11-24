/**
 * スペクトルの面の計算（M17 段 5。純粋な関数。docs/design.md「音声エディタのスペクトルとダイナミクス」、式は
 * `crates/kura-engine/src/audio/spec.rs` の `kura-spec/1`）。
 *
 * - 帯の表：帯 k は 20 Hz × 2^(k/48) から 20 Hz × 2^((k+1)/48)（最後の帯はナイキストで切る）。中心は √(lo·hi)。共有のベクタ
 *   （`crates/kura-core/tests/vectors/edit/spec_bands.json`）で Rust と照らす（表示だけなので末尾の桁の差は許す）。
 * - 軸：横は対数の周波数（20 Hz〜ナイキスト）、縦は dB（上が大きい）。
 * - 表示の加工（表示だけ。値は変えない）：傾きの補正（1 kHz を中心に dB/oct）・落ちる速さ（dB/s）・最大の保持。
 */

/** 帯の下端（Hz）・1 オクターブの帯の数・長い窓の帯の上端（Hz）（`kura-spec/1`）。 */
export const LOW_HZ = 20;
export const BANDS_PER_OCTAVE = 48;
export const LONG_BELOW_HZ = 200;
/** 傾きの補正の中心（Hz）。 */
export const TILT_PIVOT_HZ = 1_000;
/** 音名の基準（A4）。 */
export const A4_HZ = 440;

/** 帯 1 つ。 */
export interface SpecBandEdge {
  lo: number;
  hi: number;
  /** 中心（√(lo·hi)）。 */
  center: number;
  /** 長い窓（4N 点）の帯。 */
  long: boolean;
}

/** レート `rate` の帯の表（`kura-spec/1` と同じ作り方）。 */
export function specBands(rate: number): SpecBandEdge[] {
  if (!(rate > 0)) return [];
  const nyq = rate / 2;
  const edges = [LOW_HZ];
  for (;;) {
    const e = LOW_HZ * Math.pow(2, edges.length / BANDS_PER_OCTAVE);
    if (e > nyq) break;
    edges.push(e);
  }
  const last = edges[edges.length - 1];
  if (last !== undefined && last < nyq) edges.push(nyq);
  const out: SpecBandEdge[] = [];
  for (let i = 0; i + 1 < edges.length; i++) {
    const lo = edges[i] ?? 0;
    const hi = edges[i + 1] ?? 0;
    out.push({ lo, hi, center: Math.sqrt(lo * hi), long: hi <= LONG_BELOW_HZ });
  }
  return out;
}

/** 周波数の軸（Hz）。 */
export interface FreqAxis {
  lo: number;
  hi: number;
}

/** レートの軸（20 Hz〜ナイキスト）。 */
export function freqAxis(rate: number): FreqAxis {
  return { lo: LOW_HZ, hi: Math.max(LOW_HZ * 2, rate / 2) };
}

/** 周波数の x（0〜width。軸の外も外挿する）。 */
export function freqToX(f: number, axis: FreqAxis, width: number): number {
  return (width * Math.log(Math.max(f, 1e-9) / axis.lo)) / Math.log(axis.hi / axis.lo);
}

/** x の周波数。 */
export function xToFreq(x: number, axis: FreqAxis, width: number): number {
  if (!(width > 0)) return axis.lo;
  return axis.lo * Math.pow(axis.hi / axis.lo, x / width);
}

/** 縦の範囲（dB。上・下）。 */
export interface DbRange {
  top: number;
  bottom: number;
}

/** dB の y（0〜height。上が top）。 */
export function dbToY(db: number, range: DbRange, height: number): number {
  return ((range.top - db) / (range.top - range.bottom)) * height;
}

/** y の dB。 */
export function yToDb(y: number, range: DbRange, height: number): number {
  if (!(height > 0)) return range.top;
  return range.top - (y / height) * (range.top - range.bottom);
}

/** 傾きの補正（dB。1 kHz を中心に `perOct` dB/oct）。 */
export function tiltDb(f: number, perOct: number): number {
  return perOct === 0 ? 0 : perOct * Math.log2(Math.max(f, 1e-9) / TILT_PIVOT_HZ);
}

/**
 * 落ちる速さ：`shown`（今出している dB）を `target` に向けて、上がるときはすぐ、下がるときは `dbPerSec` × `dt` 秒までに
 * する（`shown` を書き換える）。`shown` が空（長さ 0）なら `target` を写す。
 */
export function decayToward(
  shown: Float32Array,
  target: ArrayLike<number>,
  dbPerSec: number,
  dt: number,
): void {
  const fall = Math.max(0, dbPerSec * dt);
  for (let k = 0; k < shown.length; k++) {
    const t = target[k] ?? Number.NEGATIVE_INFINITY;
    const s = shown[k] ?? t;
    shown[k] = t >= s ? t : Math.max(t, s - fall);
  }
}

/** 最大の保持：`hold` を `values` との大きいほうにする。 */
export function holdMax(hold: Float32Array, values: ArrayLike<number>): void {
  for (let k = 0; k < hold.length; k++) {
    const v = values[k] ?? Number.NEGATIVE_INFINITY;
    if (v > (hold[k] ?? Number.NEGATIVE_INFINITY)) hold[k] = v;
  }
}

/** 山：帯の番号・中心の周波数・dB。 */
export interface SpecPeak {
  band: number;
  freq: number;
  db: number;
}

/**
 * ポインタの周波数 `f` の前後 `halfOct` オクターブにある、周りより `prominence` dB 以上高い極大のうち最も高いもの。
 * 周りは極大の前後それぞれ `halfOct` オクターブの中の最小（両側とも `prominence` 以上下がること）。
 */
export function findPeak(
  bands: readonly SpecBandEdge[],
  db: ArrayLike<number>,
  f: number,
  halfOct = 1 / 6,
  prominence = 3,
): SpecPeak | null {
  if (bands.length === 0 || !(f > 0)) return null;
  const span = Math.round(halfOct * BANDS_PER_OCTAVE);
  const lo = f / Math.pow(2, halfOct);
  const hi = f * Math.pow(2, halfOct);
  let best: SpecPeak | null = null;
  for (let k = 0; k < bands.length; k++) {
    const b = bands[k];
    if (!b || b.center < lo || b.center > hi) continue;
    const v = db[k] ?? Number.NEGATIVE_INFINITY;
    if (!Number.isFinite(v)) continue;
    const left = db[k - 1] ?? Number.NEGATIVE_INFINITY;
    const right = db[k + 1] ?? Number.NEGATIVE_INFINITY;
    if (v < left || v < right) continue;
    let minL = v;
    let minR = v;
    for (let j = 1; j <= span; j++) {
      minL = Math.min(minL, db[k - j] ?? minL);
      minR = Math.min(minR, db[k + j] ?? minR);
    }
    if (v - minL < prominence || v - minR < prominence) continue;
    if (!best || v > best.db) best = { band: k, freq: b.center, db: v };
  }
  return best;
}

const NOTE_NAMES = ['C', 'C♯', 'D', 'D♯', 'E', 'F', 'F♯', 'G', 'G♯', 'A', 'A♯', 'B'] as const;

/** 周波数の音名（A4 = 440 Hz の平均律）とセント（−50〜+50）。 */
export function noteOf(f: number): { name: string; cents: number } | null {
  if (!(f > 0)) return null;
  const midi = 69 + 12 * Math.log2(f / A4_HZ);
  const nearest = Math.round(midi);
  const cents = Math.round((midi - nearest) * 100);
  const name = NOTE_NAMES[((nearest % 12) + 12) % 12] ?? 'C';
  const octave = Math.floor(nearest / 12) - 1;
  return { name: `${name}${octave}`, cents };
}

/** 周波数の目盛り（Hz）。 */
export const FREQ_TICKS = [20, 50, 100, 200, 500, 1_000, 2_000, 5_000, 10_000, 20_000] as const;

/** 周波数の短い書き方（「20 Hz」「2.80 kHz」の数の部分と単位）。 */
export function freqParts(f: number): { value: string; unit: 'Hz' | 'kHz' } {
  if (f >= 1_000) {
    const k = f / 1_000;
    return { value: k >= 10 ? k.toFixed(1) : k.toFixed(2), unit: 'kHz' };
  }
  return { value: f >= 100 ? f.toFixed(0) : f.toFixed(1), unit: 'Hz' };
}

/** 目盛りの短い書き方（「20」「1k」「20k」）。 */
export function tickLabel(f: number): string {
  return f >= 1_000 ? `${f / 1_000}k` : String(f);
}

/**
 * 対数の格子 `freqs`（昇順）の値 `values` を、周波数 `f` で対数の周波数の直線で補う（格子の外は端の値。値が null の点は
 * 飛ばせないので null）。
 */
export function curveAt(
  freqs: readonly (number | null)[],
  values: readonly (number | null)[],
  f: number,
): number | null {
  const n = Math.min(freqs.length, values.length);
  if (n === 0) return null;
  const first = freqs[0];
  const last = freqs[n - 1];
  if (first === null || first === undefined || last === null || last === undefined) return null;
  if (f <= first) return values[0] ?? null;
  if (f >= last) return values[n - 1] ?? null;
  let a = 0;
  let b = n - 1;
  while (b - a > 1) {
    const m = (a + b) >> 1;
    const fm = freqs[m];
    if (fm === null || fm === undefined) return null;
    if (fm <= f) a = m;
    else b = m;
  }
  const fa = freqs[a];
  const fb = freqs[b];
  const va = values[a];
  const vb = values[b];
  if (fa == null || fb == null || va == null || vb == null) return null;
  const t = Math.log(f / fa) / Math.log(fb / fa);
  return va + (vb - va) * t;
}
