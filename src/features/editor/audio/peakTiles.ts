/**
 * 音声エディタの波形の段のタイル（M13 の段 6。設計 §2.1・§2.4、Rust は `crates/kura-core/src/edit/editor/audio/peaks.rs`）。
 *
 * - タイルは `/edit/{session}/{peaks.rev}/peaks/{bin}/{tile}`（immutable。`src/ipc/media.ts` の `editPeaksUrl`）。並びは
 *   リトルエンディアンで `"kpt1"`・u16 チャンネル数・u16 0・u32 bin・u32 tile・u32 count（1〜1,024）・u32 0・u64 frames・
 *   count × チャンネル × [min i16, max i16]（ビンの順 → チャンネルの順）。共有のベクタ
 *   （`crates/kura-core/tests/vectors/edit/peaks_tile.json`）で Rust と照らす。
 * - 手元の表（`PeakTileCache`）は (bin, tile) → {rev, タイル}。結果の `peaks.changed` に掛かるタイルを捨て（`all` なら全部）、
 *   掛からないものは古い rev のまま使う（中身は同じ）。32 MiB を超えたら長く使っていないものから捨てる。
 * - 描く列ごと・チャンネルごとの [min, max] を手元のタイルから埋める（`fillPeakColumns`）。段の順に試し、タイルのない
 *   列は次の（粗い）段で埋める。
 */

import type { EditSpansChangeDto } from '../../../ipc/edit';

/** タイルの頭の長さ。 */
export const TILE_HEADER = 32;
/** タイル 1 枚のビンの数（Rust の `TILE_BINS`）。 */
export const TILE_BINS = 1_024;
/** 手元に持つタイルの上限（バイト）。 */
export const PEAK_CACHE_BYTES = 32 * 1024 * 1024;

const MAGIC = [0x6b, 0x70, 0x74, 0x31]; // "kpt1"
const SAFE = BigInt(Number.MAX_SAFE_INTEGER);
/** チャンネル数の上限（Web Audio が扱える数）。 */
const MAX_CHANNELS = 32;

/** 波形の段のタイル 1 枚。 */
export interface PeakTile {
  channels: number;
  /** 1 ビンのフレーム数。 */
  bin: number;
  tile: number;
  /** ビンの数（1〜`TILE_BINS`）。 */
  count: number;
  /** その段の出力の長さ（フレーム）。 */
  frames: number;
  /** ビンの順 → チャンネルの順 → [min, max]（16 ビットの格子）。 */
  data: Int16Array;
}

/**
 * タイルを読む。外から来たバイト列として、magic・予約の 0・チャンネル数・ビンの数・長さがちょうど・min ≤ max・タイルが
 * 段の中にあることを確かめる。読めなければ null。
 */
export function parseEditPeaksTile(buf: ArrayBuffer, maxBins = TILE_BINS): PeakTile | null {
  if (buf.byteLength < TILE_HEADER) return null;
  const bytes = new Uint8Array(buf);
  if (MAGIC.some((b, i) => bytes[i] !== b)) return null;
  const v = new DataView(buf);
  const channels = v.getUint16(4, true);
  if (channels < 1 || channels > MAX_CHANNELS || v.getUint16(6, true) !== 0) return null;
  const bin = v.getUint32(8, true);
  const tile = v.getUint32(12, true);
  const count = v.getUint32(16, true);
  if (bin < 1 || count < 1 || count > maxBins || v.getUint32(20, true) !== 0) return null;
  const framesBig = v.getBigUint64(24, true);
  if (framesBig > SAFE) return null;
  const frames = Number(framesBig);
  const len = count * channels * 2;
  if (buf.byteLength !== TILE_HEADER + len * 2) return null;
  // タイルが段の中にある：先頭のビンは段のビンの数より前、最後のタイルでなければ満杯
  const levelBins = Math.ceil(frames / bin);
  const first = tile * maxBins;
  if (first >= levelBins || first + count > levelBins) return null;
  if (count < maxBins && first + count !== levelBins) return null;
  const data = new Int16Array(len);
  for (let i = 0; i < len; i += 2) {
    const lo = v.getInt16(TILE_HEADER + i * 2, true);
    const hi = v.getInt16(TILE_HEADER + i * 2 + 2, true);
    if (lo > hi) return null;
    data[i] = lo;
    data[i + 1] = hi;
  }
  return { channels, bin, tile, count, frames, data };
}

/** タイル 1 枚が受け持つフレーム数。 */
export function tileFrames(bin: number, tileBins = TILE_BINS): number {
  return bin * tileBins;
}

/** 段のタイルの数。 */
export function tileCount(frames: number, bin: number, tileBins = TILE_BINS): number {
  if (!(frames > 0) || !(bin > 0)) return 0;
  return Math.ceil(Math.ceil(frames / bin) / tileBins);
}

/** フレームの区間 `[start, end)` に掛かるタイルの番号の範囲 `[first, last)`（段の中に収める）。 */
export function tileRange(
  start: number,
  end: number,
  bin: number,
  frames: number,
  tileBins = TILE_BINS,
): [number, number] {
  const n = tileCount(frames, bin, tileBins);
  const per = tileFrames(bin, tileBins);
  const a = Math.min(n, Math.max(0, Math.floor(Math.max(0, start) / per)));
  const b = Math.min(n, Math.max(a, Math.ceil(Math.min(end, frames) / per)));
  return [a, b];
}

/** 区間の変わり `changed` が、フレームの区間 `[a, b)` に掛かるか。 */
export function touches(changed: EditSpansChangeDto, a: number, b: number): boolean {
  if (changed.type === 'all') return true;
  return changed.spans.some((s) => s.start < b && a < s.end);
}

interface Entry {
  rev: number;
  tile: PeakTile;
  bytes: number;
}

/** 手元のタイルの表（(bin, tile) → {rev, タイル}。長く使っていないものから捨てる）。 */
export class PeakTileCache {
  private readonly entries = new Map<string, Entry>();
  private used = 0;

  constructor(private readonly maxBytes = PEAK_CACHE_BYTES) {}

  private static key(bin: number, tile: number): string {
    return `${bin}/${tile}`;
  }

  /** 持っているバイト数。 */
  get bytes(): number {
    return this.used;
  }

  /** 持っているタイルの数。 */
  get size(): number {
    return this.entries.size;
  }

  /** タイル（なければ undefined。使ったものを後ろへ）。 */
  get(bin: number, tile: number): { rev: number; tile: PeakTile } | undefined {
    const k = PeakTileCache.key(bin, tile);
    const e = this.entries.get(k);
    if (!e) return undefined;
    this.entries.delete(k);
    this.entries.set(k, e);
    return e;
  }

  /** 使った印を付けずに見る（描く前の確かめ）。 */
  peek(bin: number, tile: number): PeakTile | undefined {
    return this.entries.get(PeakTileCache.key(bin, tile))?.tile;
  }

  /** 置く（同じ鍵は替える）。上限を超えたら古いものから捨てる。 */
  put(bin: number, tile: number, rev: number, data: PeakTile): void {
    const k = PeakTileCache.key(bin, tile);
    const old = this.entries.get(k);
    if (old) {
      this.entries.delete(k);
      this.used -= old.bytes;
    }
    const bytes = data.data.byteLength + 64;
    this.entries.set(k, { rev, tile: data, bytes });
    this.used += bytes;
    for (const [key, e] of this.entries) {
      if (this.used <= this.maxBytes || key === k) break;
      this.entries.delete(key);
      this.used -= e.bytes;
    }
  }

  /** 変わった区間に掛かるタイルを捨てる（`all` なら全部）。 */
  invalidate(changed: EditSpansChangeDto, tileBins = TILE_BINS): void {
    if (changed.type === 'all') {
      this.clear();
      return;
    }
    if (changed.spans.length === 0) return;
    for (const [key, e] of this.entries) {
      const per = tileFrames(e.tile.bin, tileBins);
      const a = e.tile.tile * per;
      if (touches(changed, a, a + per)) {
        this.entries.delete(key);
        this.used -= e.bytes;
      }
    }
  }

  clear(): void {
    this.entries.clear();
    this.used = 0;
  }
}

/** 列を埋める条件。 */
export interface PeakColumnsInput {
  cache: PeakTileCache;
  /** 試す段のビンのフレーム数（先に試すものから。ふつうは選んだ段と、それより粗い段）。 */
  bins: readonly number[];
  channels: number;
  /** 段の出力の長さ（フレーム）。 */
  frames: number;
  /** 描く区間（小数のフレーム）と列の数。 */
  start: number;
  span: number;
  columns: number;
  tileBins?: number;
}

/**
 * 列ごと・チャンネルごとの [min, max]（−1〜1）を `out`（列 → チャンネル → [min, max]。長さは列 × チャンネル × 2）に、
 * 埋めたかを `has`（列ごと。1 なら埋めた）に書く。段の外・区間の外の列は 0 で埋めたことにする。埋めた列の数を返す。
 * 使い回す配列に書く（描くたびに作らない）。
 */
export function fillPeakColumns(
  input: PeakColumnsInput,
  out: Float32Array,
  has: Uint8Array,
): number {
  const { cache, bins, channels, frames, start, span, columns } = input;
  const tileBins = input.tileBins ?? TILE_BINS;
  const n = Math.max(0, Math.floor(columns));
  out.fill(0, 0, n * channels * 2);
  has.fill(0, 0, n);
  if (n === 0 || !(span > 0) || channels <= 0) return 0;
  let filled = 0;
  for (let c = 0; c < n; c++) {
    const a = start + (c * span) / n;
    const b = start + ((c + 1) * span) / n;
    const lo = Math.max(0, Math.floor(a));
    const hi = Math.min(frames, Math.ceil(b));
    if (hi <= lo) {
      has[c] = 1;
      filled++;
      continue;
    }
    for (const bin of bins) {
      const levelBins = Math.ceil(frames / bin);
      const b0 = Math.floor(lo / bin);
      const b1 = Math.min(levelBins, Math.max(b0 + 1, Math.ceil(hi / bin)));
      let ok = true;
      for (let ch = 0; ch < channels; ch++) {
        out[(c * channels + ch) * 2] = 1;
        out[(c * channels + ch) * 2 + 1] = -1;
      }
      for (let i = b0; i < b1 && ok; i++) {
        const tile = cache.peek(bin, Math.floor(i / tileBins));
        if (!tile || tile.channels !== channels) {
          ok = false;
          break;
        }
        const local = i - tile.tile * tileBins;
        if (local < 0 || local >= tile.count) {
          ok = false;
          break;
        }
        for (let ch = 0; ch < channels; ch++) {
          const at = (local * channels + ch) * 2;
          const o = (c * channels + ch) * 2;
          const mn = (tile.data[at] ?? 0) / 32768;
          const mx = (tile.data[at + 1] ?? 0) / 32768;
          if (mn < (out[o] ?? 0)) out[o] = mn;
          if (mx > (out[o + 1] ?? 0)) out[o + 1] = mx;
        }
      }
      if (ok) {
        has[c] = 1;
        filled++;
        break;
      }
    }
    if (!has[c]) {
      for (let ch = 0; ch < channels; ch++) {
        out[(c * channels + ch) * 2] = 0;
        out[(c * channels + ch) * 2 + 1] = 0;
      }
    }
  }
  return filled;
}
