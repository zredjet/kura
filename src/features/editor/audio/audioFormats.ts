/**
 * 音声の形式の表と、保存で失うもの（M13 の段 6。設計 §2.1・§2.3）。
 *
 * - 形式の ID・拡張子（先頭が代表）・書ける印（可逆・高いビット・ループ点・マーカー）は、エンジンの形式の表
 *   （`crates/kura-engine/src/codec/formats.rs` の音声の行の `MetaCaps`）と同じ。bindings に形式の表の定数がないので、
 *   エディタが使う音声の行だけをここに写す。
 * - 保存で書ける形式は wav・aiff・flac・ogg（Vorbis）・mp3（ffmpeg）・m4a（ffmpeg）。元の形式に書けないときは
 *   Ogg Vorbis（利用者の決定）。ffmpeg がないときに書けない形式は、段 4C の `edit_formats` が来たら差し替える。
 * - 失うもの（`lossesFor`）：ループ点・マーカーを書けない形式、非可逆の形式、高いビットを書けない可逆の形式。
 */

import { formatTable } from '../formats';

/** 音声の形式の ID → 拡張子（先頭が代表）。 */
export const AUDIO_FORMAT_EXTS: Readonly<Record<string, readonly string[]>> = {
  wav: ['wav'],
  aiff: ['aif', 'aiff'],
  flac: ['flac'],
  mp3: ['mp3'],
  m4a: ['m4a'],
  aac: ['aac'],
  ogg: ['ogg'],
  opus: ['opus'],
};

/** 保存で書ける音声の形式（並べる順）。 */
export const WRITABLE_AUDIO_FORMATS: readonly string[] = [
  'wav',
  'aiff',
  'flac',
  'ogg',
  'mp3',
  'm4a',
];

/** 元の形式に書けないときに使う形式。 */
export const AUDIO_FALLBACK_FORMAT = 'ogg';

/** 音声の形式の表。 */
export const AUDIO_FORMATS = formatTable({
  exts: AUDIO_FORMAT_EXTS,
  writable: WRITABLE_AUDIO_FORMATS,
  fallback: AUDIO_FALLBACK_FORMAT,
});

/** 形式が書けるもの（エンジンの `MetaCaps` のうち音声に効くもの）。 */
export interface AudioCaps {
  lossless: boolean;
  highBitDepth: boolean;
  loopPoints: boolean;
  markers: boolean;
}

const NONE: AudioCaps = { lossless: false, highBitDepth: false, loopPoints: false, markers: false };

/** 形式ごとの書けるもの。 */
export const AUDIO_CAPS: Readonly<Record<string, AudioCaps>> = {
  wav: { lossless: true, highBitDepth: true, loopPoints: true, markers: true },
  aiff: { lossless: true, highBitDepth: true, loopPoints: true, markers: true },
  flac: { lossless: true, highBitDepth: true, loopPoints: true, markers: false },
  mp3: NONE,
  m4a: NONE,
  aac: NONE,
  ogg: { ...NONE, loopPoints: true },
  opus: { ...NONE, loopPoints: true },
};

/** 形式の書けるもの（知らない形式は null）。 */
export function capsOf(format: string | null | undefined): AudioCaps | null {
  return (format && AUDIO_CAPS[format]) || null;
}

/** 保存で失うもの。 */
export type AudioLoss = 'loop' | 'markers' | 'lossy' | 'bits';

/** 保存したときに失うもの（並べる順）。`view` は今の出力のループ・マーカーと元のビット数。 */
export function lossesFor(
  caps: AudioCaps | null,
  view: { loop: unknown; markers: readonly unknown[]; bits: number | null },
): AudioLoss[] {
  if (!caps) return [];
  const out: AudioLoss[] = [];
  if (view.loop && !caps.loopPoints) out.push('loop');
  if (view.markers.length > 0 && !caps.markers) out.push('markers');
  if (!caps.lossless) out.push('lossy');
  else if (!caps.highBitDepth && (view.bits ?? 0) > 16) out.push('bits');
  return out;
}
