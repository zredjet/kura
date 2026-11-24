/**
 * 音声エディタのスペクトルのタイル（M17 段 4。docs/editing.md §18.4、Rust は `crates/kura-core/src/edit/editor/audio/spec.rs`、
 * 式は `crates/kura-engine/src/audio/spec.rs` の `kura-spec/1`）。画面（アナライザ・スペクトログラム）は段 5・8。
 *
 * - タイルは `/edit/{session}/spec/{key}/{channel}/{chunk}`（immutable。`src/ipc/media.ts` の `editSpecUrl`）。並びは
 *   リトルエンディアンで `"kst1"`・u16 式の番号（1）・u16 チャンネルの符号（番号・M 0xFFFD・S 0xFFFE・和 0xFFFF）・u32 レート・
 *   u32 N・u32 hop・u16 帯の数・u16 1 列のフレーム数（1）・u64 最初のフレーム・u32 列の数・u32 0、続けて 列 × 帯 の u8
 *   （列が外。dB = q/2 − 120）。共有のベクタ（`crates/kura-core/tests/vectors/edit/spec_tile.json`）で Rust と照らす。
 * - 手元の表（`SpecTileCache`）は (鍵, チャンネル, チャンク) → タイル。鍵はポートの指紋なので中身は変わらない。上限を
 *   超えたら長く使っていないものから捨てる。置き場から追い出されたタイルは 404 になるので、表示の頼み（`spec`）を出し直す。
 */

import type { EditSpecChannelDto } from '../../../ipc/edit';

/** タイルの頭の長さ。 */
export const SPEC_TILE_HEADER = 40;
/** 式の番号（Rust の `SPEC_VERSION`）。 */
export const SPEC_VERSION = 1;
/** チャンクのフレーム数（Rust の `AUDIO_CHUNK`）。 */
export const SPEC_CHUNK_FRAMES = 65_536;
/** u8 の 0 の dB と刻み。 */
export const SPEC_DB_FLOOR = -120;
export const SPEC_DB_STEP = 0.5;
/** 手元に持つタイルの上限（バイト）。 */
export const SPEC_CACHE_BYTES = 32 * 1024 * 1024;

const MAGIC = [0x6b, 0x73, 0x74, 0x31]; // "kst1"
const SAFE = BigInt(Number.MAX_SAFE_INTEGER);
const MIN_N = 256;
const MAX_N = 16_384;

/** スペクトルのタイル 1 枚。 */
export interface SpecTile {
  channel: EditSpecChannelDto;
  rate: number;
  /** 窓の大きさ・hop（フレーム）。 */
  n: number;
  hop: number;
  bands: number;
  /** チャンクの番号と、最初のフレームの番号（中心 = フレーム × hop）。 */
  chunk: number;
  firstFrame: number;
  /** 列（フレーム）の数（最後のチャンクは少ない）。 */
  frames: number;
  /** 列 × 帯の u8（列が外）。 */
  data: Uint8Array;
}

/** チャンネルの符号を読む。 */
function channelOf(code: number): EditSpecChannelDto {
  switch (code) {
    case 0xfffd:
      return { type: 'mid' };
    case 0xfffe:
      return { type: 'side' };
    case 0xffff:
      return { type: 'sum' };
    default:
      return { type: 'index', index: code };
  }
}

/** u8 の値の dB。 */
export function specDb(q: number): number {
  return q * SPEC_DB_STEP + SPEC_DB_FLOOR;
}

/**
 * タイルを読む。外から来たバイト列として、magic・式の番号・予約の 0・N（2 の冪で 256〜16,384）・hop（N/4）・1 列のフレーム数（1）・
 * 帯の数・列の数（1〜チャンクのフレーム数）・最初のフレームがチャンクの頭・長さがちょうどを確かめる。読めなければ null。
 */
export function parseEditSpecTile(buf: ArrayBuffer): SpecTile | null {
  if (buf.byteLength < SPEC_TILE_HEADER) return null;
  const bytes = new Uint8Array(buf);
  if (MAGIC.some((b, i) => bytes[i] !== b)) return null;
  const v = new DataView(buf);
  if (v.getUint16(4, true) !== SPEC_VERSION) return null;
  const code = v.getUint16(6, true);
  const rate = v.getUint32(8, true);
  const n = v.getUint32(12, true);
  const hop = v.getUint32(16, true);
  const bands = v.getUint16(20, true);
  const perColumn = v.getUint16(22, true);
  const firstBig = v.getBigUint64(24, true);
  const frames = v.getUint32(32, true);
  if (v.getUint32(36, true) !== 0 || perColumn !== 1 || rate < 1 || bands < 1) return null;
  if (n < MIN_N || n > MAX_N || (n & (n - 1)) !== 0 || hop * 4 !== n) return null;
  const perChunk = SPEC_CHUNK_FRAMES / hop;
  if (frames < 1 || frames > perChunk || firstBig > SAFE) return null;
  const firstFrame = Number(firstBig);
  if (firstFrame % perChunk !== 0) return null;
  if (buf.byteLength !== SPEC_TILE_HEADER + frames * bands) return null;
  return {
    channel: channelOf(code),
    rate,
    n,
    hop,
    bands,
    chunk: firstFrame / perChunk,
    firstFrame,
    frames,
    data: bytes.slice(SPEC_TILE_HEADER),
  };
}

/** フレーム j の中心（ポートのフレーム）。 */
export function frameCenter(tile: Pick<SpecTile, 'hop'>, frame: number): number {
  return frame * tile.hop;
}

/** ポートのフレームの区間 `[start, end)` に掛かるチャンクの番号の範囲 `[first, last)`（長さ `frames` の内）。 */
export function specChunkRange(start: number, end: number, frames: number): [number, number] {
  const total = Math.ceil(Math.max(0, frames) / SPEC_CHUNK_FRAMES);
  const a = Math.min(total, Math.max(0, Math.floor(Math.max(0, start) / SPEC_CHUNK_FRAMES)));
  const b = Math.min(total, Math.max(a, Math.ceil(Math.min(end, frames) / SPEC_CHUNK_FRAMES)));
  return [a, b];
}

/** 置き場にあるチャンクの区間（表示の結果の `spec[].ready`）に、チャンク `c` が入るか。 */
export function isReady(ready: readonly { start: number; end: number }[], c: number): boolean {
  return ready.some((r) => r.start <= c && c < r.end);
}

interface Entry {
  tile: SpecTile;
  bytes: number;
}

function channelKey(c: EditSpecChannelDto): string {
  return c.type === 'index' ? String(c.index) : c.type;
}

/** 手元のタイルの表（(鍵, チャンネル, チャンク) → タイル。長く使っていないものから捨てる）。 */
export class SpecTileCache {
  private readonly entries = new Map<string, Entry>();
  private used = 0;

  constructor(private readonly maxBytes = SPEC_CACHE_BYTES) {}

  private static key(key: string, channel: EditSpecChannelDto, chunk: number): string {
    return `${key}/${channelKey(channel)}/${chunk}`;
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
  get(key: string, channel: EditSpecChannelDto, chunk: number): SpecTile | undefined {
    const k = SpecTileCache.key(key, channel, chunk);
    const e = this.entries.get(k);
    if (!e) return undefined;
    this.entries.delete(k);
    this.entries.set(k, e);
    return e.tile;
  }

  /** 入れて、上限を超えたら古いものから捨てる。 */
  put(key: string, tile: SpecTile): void {
    const k = SpecTileCache.key(key, tile.channel, tile.chunk);
    const old = this.entries.get(k);
    if (old) {
      this.entries.delete(k);
      this.used -= old.bytes;
    }
    const bytes = tile.data.byteLength + SPEC_TILE_HEADER;
    this.entries.set(k, { tile, bytes });
    this.used += bytes;
    for (const [oldKey, e] of this.entries) {
      if (this.used <= this.maxBytes || oldKey === k) break;
      this.entries.delete(oldKey);
      this.used -= e.bytes;
    }
  }

  /** すべて捨てる（エディタを閉じたとき）。 */
  clear(): void {
    this.entries.clear();
    this.used = 0;
  }
}
