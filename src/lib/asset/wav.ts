/**
 * 試聴の PCM：`kura://localhost/pcm/{hash}/{start}-{end}` が返す WAV を自分で読む（と、照らし合わせ用に書く）。
 * 並びは crates/kura-core/tests/pcm_vectors.rs の説明のとおり（RIFF・`fmt `・`kura`・`data`）。共有のベクタ
 * （crates/kura-core/tests/vectors/pcm/wav.json）で Rust の書き方と同じバイト列になることを照らす。
 *
 * decodeAudioData を使わないのは、文脈のレートに直されて（44.1k → 48k）ループ点が標本で合わなくなるのと、WebKit の
 * Ogg Vorbis の読み取りがエンジンと 128 フレームずれるため。`kura` チャンクの開始フレーム・フレーム数・ループ点・
 * デコーダの識別子は、この PCM を読んだのと同じ読み取りの値で、試聴は DB の値ではなくこれを使う。
 */

/** 標本の形式：16 ビットの整数か、32 ビットの浮動小数。 */
export type PcmFormat = 'i16' | 'f32';

/** `kura` チャンクの中身。 */
export interface PcmInfo {
  /** 応答の先頭が、元の音声の何フレーム目か。 */
  start: number;
  /** 元の音声のフレーム数（同じ読み取りで数えたもの）。 */
  total: number;
  /** ループの区間 [始まり, 終わり)（フレーム）。なければ null。 */
  loop: readonly [number, number] | null;
  /** デコーダの識別子（例 `wav-kura-dec/1`）。 */
  decoder: string;
}

export interface Pcm {
  rate: number;
  channels: number;
  frames: number;
  format: PcmFormat;
  /** チャンネルごとの標本（−1〜1。AudioBuffer.copyToChannel にそのまま渡す）。 */
  planes: Float32Array<ArrayBuffer>[];
  info: PcmInfo;
}

/** WebKit の AudioContext が受け付けるレート（768kHz は NotSupportedError）。 */
export const MIN_RATE = 3_000;
export const MAX_RATE = 384_000;
/** チャンネル数の上限（Web Audio が必ず扱える数）。 */
export const MAX_CHANNELS = 32;
/** 読む PCM の上限（浮動小数にしたときのバイト数）。試聴の本体の上限と同じ。 */
export const PCM_MAX_BYTES = 64 * 1024 * 1024;

const KURA_VERSION = 1;
const KURA_FIXED = 40;
const FLAG_LOOP = 1;
const MAX_DECODER_BYTES = 255;
const SAFE = BigInt(Number.MAX_SAFE_INTEGER);

function tagAt(bytes: Uint8Array, at: number): string {
  return String.fromCharCode(
    bytes[at] ?? 0,
    bytes[at + 1] ?? 0,
    bytes[at + 2] ?? 0,
    bytes[at + 3] ?? 0,
  );
}

function safe(n: bigint): number | null {
  return n <= SAFE ? Number(n) : null;
}

const utf8 = new TextDecoder('utf-8', { fatal: true });

/** `kura` チャンクを読む（読めなければ null）。 */
function readInfo(v: DataView, at: number, len: number): PcmInfo | null {
  if (len < KURA_FIXED) return null;
  if (v.getUint16(at, true) !== KURA_VERSION) return null;
  const decoderLen = v.getUint16(at + 2, true);
  if (decoderLen > MAX_DECODER_BYTES || KURA_FIXED + decoderLen !== len) return null;
  const flags = v.getUint32(at + 4, true);
  if ((flags & ~FLAG_LOOP) !== 0) return null;
  const start = safe(v.getBigUint64(at + 8, true));
  const total = safe(v.getBigUint64(at + 16, true));
  const loopStart = safe(v.getBigUint64(at + 24, true));
  const loopEnd = safe(v.getBigUint64(at + 32, true));
  if (start === null || total === null || loopStart === null || loopEnd === null) return null;
  let loop: [number, number] | null = null;
  if (flags & FLAG_LOOP) {
    if (!(loopStart < loopEnd && loopEnd <= total)) return null;
    loop = [loopStart, loopEnd];
  } else if (loopStart !== 0 || loopEnd !== 0) {
    return null;
  }
  let decoder: string;
  try {
    decoder = utf8.decode(new Uint8Array(v.buffer, v.byteOffset + at + KURA_FIXED, decoderLen));
  } catch {
    return null;
  }
  return { start, total, loop, decoder };
}

/**
 * /pcm の WAV を読む。外から来たバイト列として扱い、チャンクの長さ・形式・レート・`kura` チャンクの値を確かめ、
 * 浮動小数にしたときの大きさを `maxBytes` と照らしてから確保する。読めなければ null。
 */
export function readPcmWav(buf: ArrayBuffer, maxBytes = PCM_MAX_BYTES): Pcm | null {
  const bytes = new Uint8Array(buf);
  if (bytes.length < 12) return null;
  const v = new DataView(buf);
  if (tagAt(bytes, 0) !== 'RIFF' || tagAt(bytes, 8) !== 'WAVE') return null;
  if (v.getUint32(4, true) !== bytes.length - 8) return null;
  let fmt: { format: PcmFormat; channels: number; rate: number; block: number } | null = null;
  let info: PcmInfo | null = null;
  let data: { at: number; len: number } | null = null;
  let seenFmt = false;
  let seenKura = false;
  let at = 12;
  while (at < bytes.length) {
    if (at + 8 > bytes.length) return null;
    const id = tagAt(bytes, at);
    const len = v.getUint32(at + 4, true);
    const body = at + 8;
    if (body + len > bytes.length) return null;
    if (id === 'fmt ') {
      if (seenFmt || len !== 16) return null;
      seenFmt = true;
      const tag = v.getUint16(body, true);
      const channels = v.getUint16(body + 2, true);
      const rate = v.getUint32(body + 4, true);
      const byteRate = v.getUint32(body + 8, true);
      const block = v.getUint16(body + 12, true);
      const bits = v.getUint16(body + 14, true);
      const format: PcmFormat | null =
        tag === 1 && bits === 16 ? 'i16' : tag === 3 && bits === 32 ? 'f32' : null;
      if (!format || channels < 1 || channels > MAX_CHANNELS) return null;
      if (rate < MIN_RATE || rate > MAX_RATE) return null;
      if (block !== channels * (bits / 8) || byteRate !== rate * block) return null;
      fmt = { format, channels, rate, block };
    } else if (id === 'kura') {
      if (seenKura) return null;
      seenKura = true;
      info = readInfo(v, body, len);
      if (!info) return null;
    } else if (id === 'data') {
      if (data) return null;
      data = { at: body, len };
    }
    at = body + len + (len & 1);
  }
  if (!fmt || !info || !data || data.len % fmt.block !== 0) return null;
  const frames = data.len / fmt.block;
  if (frames * fmt.channels * 4 > maxBytes) return null;
  if (info.start + frames > info.total) return null;
  const planes = Array.from({ length: fmt.channels }, () => new Float32Array(frames));
  const size = fmt.format === 'i16' ? 2 : 4;
  for (let f = 0; f < frames; f++) {
    for (let ch = 0; ch < fmt.channels; ch++) {
      const p = data.at + f * fmt.block + ch * size;
      const plane = planes[ch];
      if (plane)
        plane[f] = fmt.format === 'i16' ? v.getInt16(p, true) / 32768 : v.getFloat32(p, true);
    }
  }
  return { rate: fmt.rate, channels: fmt.channels, frames, format: fmt.format, planes, info };
}

/** −1〜1 の標本を 16 ビットの整数にする（Rust の `codec::pcm::quantize(x, 16)`：偶数への丸め・範囲で止める・NaN は 0）。 */
export function quantize16(x: number): number {
  if (Number.isNaN(x)) return 0;
  const s = x * 32768;
  if (s >= 32767) return 32767;
  if (s <= -32768) return -32768;
  const r = Math.round(s);
  // Math.round は .5 を +∞ 向きに丸めるので、ちょうど .5 なら偶数へ
  const v = r - s === 0.5 && r % 2 !== 0 ? r - 1 : r;
  return v === 0 ? 0 : v;
}

/**
 * /pcm の WAV を書く（Rust の書き手と同じバイト列。共有のベクタで照らす）。チャンネルの長さがそろわない・
 * レートやチャンネル数が範囲の外・識別子が長すぎるものは RangeError。
 */
export function writePcmWav(p: {
  rate: number;
  format: PcmFormat;
  planes: readonly Float32Array[];
  info: PcmInfo;
}): ArrayBuffer {
  const channels = p.planes.length;
  const frames = p.planes[0]?.length ?? 0;
  if (channels < 1 || channels > MAX_CHANNELS || p.planes.some((x) => x.length !== frames)) {
    throw new RangeError('チャンネルの数・長さが正しくありません');
  }
  if (!Number.isInteger(p.rate) || p.rate < MIN_RATE || p.rate > MAX_RATE) {
    throw new RangeError('レートが範囲の外です');
  }
  const decoder = new TextEncoder().encode(p.info.decoder);
  if (decoder.length > MAX_DECODER_BYTES) throw new RangeError('デコーダの識別子が長すぎます');
  const size = p.format === 'i16' ? 2 : 4;
  const block = channels * size;
  const kuraLen = KURA_FIXED + decoder.length;
  const pad = kuraLen & 1;
  const dataLen = frames * block;
  const total = 12 + 8 + 16 + 8 + kuraLen + pad + 8 + dataLen;
  const buf = new ArrayBuffer(total);
  const v = new DataView(buf);
  const bytes = new Uint8Array(buf);
  const put = (at: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(at + i, s.charCodeAt(i));
  };
  put(0, 'RIFF');
  v.setUint32(4, total - 8, true);
  put(8, 'WAVE');
  put(12, 'fmt ');
  v.setUint32(16, 16, true);
  v.setUint16(20, p.format === 'i16' ? 1 : 3, true);
  v.setUint16(22, channels, true);
  v.setUint32(24, p.rate, true);
  v.setUint32(28, p.rate * block, true);
  v.setUint16(32, block, true);
  v.setUint16(34, size * 8, true);
  put(36, 'kura');
  v.setUint32(40, kuraLen, true);
  const k = 44;
  v.setUint16(k, KURA_VERSION, true);
  v.setUint16(k + 2, decoder.length, true);
  v.setUint32(k + 4, p.info.loop ? FLAG_LOOP : 0, true);
  v.setBigUint64(k + 8, BigInt(p.info.start), true);
  v.setBigUint64(k + 16, BigInt(p.info.total), true);
  v.setBigUint64(k + 24, BigInt(p.info.loop?.[0] ?? 0), true);
  v.setBigUint64(k + 32, BigInt(p.info.loop?.[1] ?? 0), true);
  bytes.set(decoder, k + KURA_FIXED);
  const d = k + kuraLen + pad;
  put(d, 'data');
  v.setUint32(d + 4, dataLen, true);
  for (let f = 0; f < frames; f++) {
    for (let ch = 0; ch < channels; ch++) {
      const at = d + 8 + f * block + ch * size;
      const x = p.planes[ch]?.[f] ?? 0;
      if (p.format === 'i16') v.setInt16(at, quantize16(x), true);
      else v.setFloat32(at, x, true);
    }
  }
  return buf;
}
