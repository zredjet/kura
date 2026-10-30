/**
 * 音声エディタの PCM のチャンク（M13 の段 6。設計 §2.1・§2.4・§2.5）。
 *
 * - チャンクは出力のフレームの `CHUNK`（Rust の `AUDIO_CHUNK`）ごとの格子。`/edit/{session}/{rev}/pcm/{a}-{b}`（出力）・
 *   `/edit/{session}/input/pcm/{a}-{b}`（元）をチャンク 1 つずつ取り、`src/lib/asset/wav.ts` の `readPcmWav` で読む。
 * - 手元の表（`PcmChunkCache`）はチャンクの番号 → {rev, 標本}。表示の結果の `changed` に掛かるチャンクを捨て（`all` なら
 *   全部）、掛からないものは古い rev のまま使う（中身は同じ）。64 MiB を超えたら長く使っていないものから捨てる。波形の
 *   拡大（PCM で描く）と編集中の試聴が同じ表を使う。
 */

import type { EditSpanDto, EditSpansChangeDto } from '../../../ipc/edit';
import { touches } from './peakTiles';

/** チャンクのフレーム数（Rust の `kura_engine::value::AUDIO_CHUNK`）。 */
export const CHUNK = 65_536;
/** 手元に持つ PCM の上限（バイト。浮動小数）。 */
export const PCM_CACHE_BYTES = 64 * 1024 * 1024;

/** チャンク 1 つの標本。 */
export interface PcmChunk {
  /** 取った表示の rev（元なら 0）。 */
  rev: number;
  /** チャンクの頭のフレーム。 */
  start: number;
  frames: number;
  /** チャンネルごとの標本（−1〜1）。 */
  planes: Float32Array<ArrayBuffer>[];
}

/** フレームのチャンクの番号。 */
export function chunkOf(frame: number): number {
  return Math.floor(Math.max(0, frame) / CHUNK);
}

/** チャンク `index` の区間 `[a, b)`（全体の長さで切る。外なら null）。 */
export function chunkSpan(index: number, total: number): [number, number] | null {
  const a = index * CHUNK;
  if (!Number.isSafeInteger(index) || index < 0 || a >= total) return null;
  return [a, Math.min(total, a + CHUNK)];
}

/** 区間 `[start, end)` をチャンクの境にそろえる（全体の長さで切る。空なら null）。 */
export function alignSpan(start: number, end: number, total: number): EditSpanDto | null {
  if (!(end > start)) return null;
  const a = Math.max(0, Math.floor(Math.max(0, start) / CHUNK) * CHUNK);
  const b = Math.min(total, Math.ceil(Math.min(end, total) / CHUNK) * CHUNK);
  return b > a ? { start: a, end: b } : null;
}

/** 区間 `[a, b)` が `valid` のどれか 1 つに覆われるか（Rust は覆われない区間の PCM を 404 にする）。 */
export function covered(valid: readonly EditSpanDto[], a: number, b: number): boolean {
  return valid.some((s) => s.start <= a && b <= s.end);
}

/** 手元の PCM のチャンクの表。 */
export class PcmChunkCache {
  private readonly entries = new Map<number, PcmChunk & { bytes: number }>();
  private used = 0;

  constructor(private readonly maxBytes = PCM_CACHE_BYTES) {}

  get bytes(): number {
    return this.used;
  }

  get size(): number {
    return this.entries.size;
  }

  /** チャンク（なければ undefined。使ったものを後ろへ）。 */
  get(index: number): PcmChunk | undefined {
    const e = this.entries.get(index);
    if (!e) return undefined;
    this.entries.delete(index);
    this.entries.set(index, e);
    return e;
  }

  /** 使った印を付けずに見る（描く前の確かめ）。 */
  peek(index: number): PcmChunk | undefined {
    return this.entries.get(index);
  }

  has(index: number): boolean {
    return this.entries.has(index);
  }

  /** 置く（上限を超えたら古いものから捨てる）。 */
  put(index: number, chunk: PcmChunk): void {
    const old = this.entries.get(index);
    if (old) {
      this.entries.delete(index);
      this.used -= old.bytes;
    }
    const bytes = chunk.planes.reduce((n, p) => n + p.byteLength, 0) + 64;
    this.entries.set(index, { ...chunk, bytes });
    this.used += bytes;
    for (const [key, e] of this.entries) {
      if (this.used <= this.maxBytes || key === index) break;
      this.entries.delete(key);
      this.used -= e.bytes;
    }
  }

  /** 変わった区間に掛かるチャンクを捨てる（`all` なら全部）。捨てた番号を返す。 */
  invalidate(changed: EditSpansChangeDto): number[] {
    const dropped: number[] = [];
    if (changed.type === 'spans' && changed.spans.length === 0) return dropped;
    for (const [index, e] of this.entries) {
      if (touches(changed, e.start, e.start + CHUNK)) {
        this.entries.delete(index);
        this.used -= e.bytes;
        dropped.push(index);
      }
    }
    return dropped;
  }

  clear(): void {
    this.entries.clear();
    this.used = 0;
  }
}

/**
 * PCM から列ごと・チャンネルごとの [min, max] を `out` に、埋めたかを `has` に書く（`fillPeakColumns` と同じ形）。
 * 区間の外の列は 0 で埋めたことにする。チャンクがない列は埋めない。
 */
export function fillPcmColumns(
  input: {
    get: (index: number) => PcmChunk | undefined;
    channels: number;
    frames: number;
    start: number;
    span: number;
    columns: number;
  },
  out: Float32Array,
  has: Uint8Array,
): number {
  const { get, channels, frames, start, span, columns } = input;
  const n = Math.max(0, Math.floor(columns));
  out.fill(0, 0, n * channels * 2);
  has.fill(0, 0, n);
  if (n === 0 || !(span > 0) || channels <= 0) return 0;
  let filled = 0;
  for (let c = 0; c < n; c++) {
    const lo = Math.max(0, Math.floor(start + (c * span) / n));
    // 1 列が 1 フレームより細かいときは、その位置のフレーム 1 つ
    const hi = Math.min(frames, Math.max(lo + 1, Math.ceil(start + ((c + 1) * span) / n)));
    if (hi <= lo) {
      has[c] = 1;
      filled++;
      continue;
    }
    let ok = true;
    for (let ch = 0; ch < channels; ch++) {
      out[(c * channels + ch) * 2] = 1;
      out[(c * channels + ch) * 2 + 1] = -1;
    }
    for (let f = lo; f < hi && ok;) {
      const chunk = get(chunkOf(f));
      if (!chunk || chunk.planes.length !== channels) {
        ok = false;
        break;
      }
      const end = Math.min(hi, chunk.start + chunk.frames);
      for (let ch = 0; ch < channels; ch++) {
        const plane = chunk.planes[ch];
        const o = (c * channels + ch) * 2;
        let mn = out[o] ?? 0;
        let mx = out[o + 1] ?? 0;
        for (let k = f - chunk.start; k < end - chunk.start; k++) {
          const v = plane?.[k] ?? 0;
          if (v < mn) mn = v;
          if (v > mx) mx = v;
        }
        out[o] = mn;
        out[o + 1] = mx;
      }
      if (end <= f) {
        ok = false;
        break;
      }
      f = end;
    }
    if (ok) {
      has[c] = 1;
      filled++;
    } else {
      for (let ch = 0; ch < channels; ch++) {
        out[(c * channels + ch) * 2] = 0;
        out[(c * channels + ch) * 2 + 1] = 0;
      }
    }
  }
  return filled;
}
