/**
 * 波形（docs/design.md §3.1）：`kura://localhost/peaks/{hash}` が返す `kura-peaks` 版 1 を読み、SVG の path にする。
 * 並びは crates/kura-engine/src/analysis/audio/peaks.rs の冒頭。共有のベクタ（crates/kura-engine/tests/vectors/peaks/）で
 * Rust の `from_canonical` と同じに読む・断ることを照らす。
 */

/** `kura-peaks` の 1 段。 */
export interface PeakLevel {
  /** 1 ビンのフレーム数。 */
  binFrames: number;
  /** ビンの数（⌈フレーム数 / binFrames⌉）。 */
  bins: number;
  /** ビンの順 → チャンネルの順 → [min, max]（16 ビットの格子）。 */
  data: Int16Array;
}

export interface Peaks {
  /** 解析の版（audio.peaks の VERSION）。並びの約束ではないので、読むときに見ない。 */
  analysisVersion: number;
  channels: number;
  rate: number;
  frames: number;
  /** 段（ビンの細かい順）。 */
  levels: PeakLevel[];
}

const MAGIC = 'kurapeak';
const LAYOUT_VERSION = 1;
const VALUE_MIN_MAX_I16 = 1;
const HEADER = 32;
const ENTRY = 24;
const MAX_LEVELS = 16;
const SAFE = BigInt(Number.MAX_SAFE_INTEGER);

/**
 * `kura-peaks` 版 1 を読む。外から来たバイト列として、magic・並びの版・値の型・段の数・ビンの大きさが増えていくこと・
 * ビンの数・データの位置・詰め物が 0・長さがちょうど・min ≤ max を確かめる（確保の前に長さを照らす）。読めなければ null。
 */
export function parsePeaks(buf: ArrayBuffer): Peaks | null {
  if (buf.byteLength < HEADER) return null;
  const v = new DataView(buf);
  const bytes = new Uint8Array(buf);
  for (let i = 0; i < MAGIC.length; i++) {
    if (bytes[i] !== MAGIC.charCodeAt(i)) return null;
  }
  if (v.getUint16(8, true) !== LAYOUT_VERSION) return null;
  const analysisVersion = v.getUint16(10, true);
  const channels = v.getUint16(12, true);
  const levelCount = v.getUint8(14);
  if (channels === 0 || levelCount < 1 || levelCount > MAX_LEVELS) return null;
  if (v.getUint8(15) !== VALUE_MIN_MAX_I16 || v.getUint32(20, true) !== 0) return null;
  const rate = v.getUint32(16, true);
  const framesBig = v.getBigUint64(24, true);
  if (framesBig > SAFE) return null;
  const tableEnd = HEADER + ENTRY * levelCount;
  if (buf.byteLength < tableEnd) return null;
  const levels: PeakLevel[] = [];
  let cursor = tableEnd;
  let prev = 0;
  for (let l = 0; l < levelCount; l++) {
    const at = HEADER + ENTRY * l;
    const binFrames = v.getUint32(at, true);
    if (binFrames === 0 || binFrames <= prev || v.getUint32(at + 4, true) !== 0) return null;
    prev = binFrames;
    const b = BigInt(binFrames);
    const want = (framesBig + b - 1n) / b;
    if (v.getBigUint64(at + 8, true) !== want) return null;
    const bins = Number(want);
    // 最初の段は表の直後（8 の倍数）、次の段は前の段の終わりを 8 の倍数に切り上げた位置
    const aligned = Math.ceil(cursor / 8) * 8;
    if (v.getBigUint64(at + 16, true) !== BigInt(aligned)) return null;
    const len = bins * channels * 2;
    if (aligned + len * 2 > buf.byteLength) return null;
    for (let p = cursor; p < aligned; p++) {
      if (bytes[p] !== 0) return null;
    }
    const data = new Int16Array(len);
    for (let i = 0; i < len; i += 2) {
      const lo = v.getInt16(aligned + i * 2, true);
      const hi = v.getInt16(aligned + i * 2 + 2, true);
      if (lo > hi) return null;
      data[i] = lo;
      data[i + 1] = hi;
    }
    levels.push({ binFrames, bins, data });
    cursor = aligned + len * 2;
  }
  if (cursor !== buf.byteLength) return null;
  return { analysisVersion, channels, rate, frames: Number(framesBig), levels };
}

/**
 * 描く幅（装置の画素の列の数）に合う段：描く区間 `spanFrames` でのビンの数が列の数以上の段のうち、いちばん粗いもの
 * （なければいちばん細かい段）。
 */
export function pickLevel(peaks: Peaks, spanFrames: number, pixels: number): PeakLevel | null {
  let best: PeakLevel | null = null;
  for (const level of peaks.levels) {
    if (Math.ceil(spanFrames / level.binFrames) >= pixels) best = level;
  }
  return best ?? peaks.levels[0] ?? null;
}

/**
 * 描く列ごとの [min, max]（チャンネルをまとめる）。列 c はフレーム
 * [from + ⌊c·span/n⌋, from + ⌊(c+1)·span/n⌋) を受け持ち、掛かるビンの min の min・max の max を取る。
 * ビンが列より粗いとき（短い音声・拡大）は、列の始まりのビンを使う（同じ値の列が続く）。区間の外の列は 0。
 */
export function columns(
  level: PeakLevel,
  channels: number,
  frames: number,
  from: number,
  to: number,
  n: number,
): Int16Array {
  const count = Math.max(0, Math.floor(n));
  const out = new Int16Array(count * 2);
  const start = Math.max(0, from);
  const span = Math.max(0, Math.min(to, frames) - start);
  if (count === 0 || span === 0 || channels <= 0 || level.binFrames <= 0) return out;
  for (let c = 0; c < count; c++) {
    const a = start + Math.floor((c * span) / count);
    const b = start + Math.floor(((c + 1) * span) / count);
    const b0 = Math.floor(a / level.binFrames);
    const b1 = Math.min(level.bins, Math.max(b0 + 1, Math.ceil(b / level.binFrames)));
    let lo = 32767;
    let hi = -32768;
    for (let bin = b0; bin < b1; bin++) {
      for (let ch = 0; ch < channels; ch++) {
        const at = (bin * channels + ch) * 2;
        lo = Math.min(lo, level.data[at] ?? 0);
        hi = Math.max(hi, level.data[at + 1] ?? 0);
      }
    }
    if (lo > hi) {
      lo = 0;
      hi = 0;
    }
    out[c * 2] = lo;
    out[c * 2 + 1] = hi;
  }
  return out;
}

/**
 * SVG の path（viewBox は `0 0 列の数 height`。中央が 0）。列ごとに縦の線を 1 本。色は CSS（`stroke: currentColor`）で
 * 付けるので、ここは形だけ。無音の列も 1 単位の線を引く（無音が見える）。
 */
export function waveformPath(cols: Int16Array, height: number): string {
  const n = Math.floor(cols.length / 2);
  const mid = height / 2;
  const parts: string[] = [];
  for (let c = 0; c < n; c++) {
    const lo = cols[c * 2] ?? 0;
    const hi = cols[c * 2 + 1] ?? 0;
    const top = Math.min(mid - (hi / 32768) * mid, mid - 0.5);
    const bottom = Math.max(mid - (lo / 32768) * mid, mid + 0.5);
    parts.push(`M${c + 0.5} ${top.toFixed(2)}V${bottom.toFixed(2)}`);
  }
  return parts.join('');
}

/**
 * フレームの区間 [start, end)（ループの区間など）を、描く範囲 [from, to) を幅 `width` に描いたときの矩形にする。
 * 範囲に掛からなければ null。
 */
export function regionX(
  start: number,
  end: number,
  from: number,
  to: number,
  width: number,
): { x: number; width: number } | null {
  const span = to - from;
  if (!(span > 0) || !(width > 0)) return null;
  const a = Math.max(start, from);
  const b = Math.min(end, to);
  if (!(b > a)) return null;
  const x = ((a - from) / span) * width;
  return { x, width: ((b - from) / span) * width - x };
}
