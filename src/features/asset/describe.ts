/**
 * 素材の欄の値の書き方（design.md §3.1「素材の欄」）。DOM に触れない純粋な関数。
 * 負の数は U+2212（−）で書く（検索の字句解析が `-` に直すので、写して入力できる）。値のないものは「—」。
 */

import { ja } from '../../i18n/ja';
import type {
  AnimDto,
  AssetAudioFactsDto,
  AssetFontDto,
  AssetGridDto,
  AssetImageFactsDto,
  AssetModelDto,
} from '../../ipc/asset';
import { isPowerOfTwo } from '../../lib/asset/pixel';
import { effectiveDuration } from '../../lib/asset/timeline';
import { formatCount } from '../../lib/format';

const t = ja.asset;

/** U+2212（マイナス記号）。 */
export const MINUS = '−';

/** 小数 `digits` 桁に丸めて、負なら U+2212 を付ける（丸めて 0 なら符号を付けない）。 */
export function signed(value: number, digits: number): string {
  const text = Math.abs(value).toFixed(digits);
  return value < 0 && Number(text) !== 0 ? `${MINUS}${text}` : text;
}

/** 大きさ「64 × 64（2 のべき乗）」。 */
export function sizeText(width: number | null, height: number | null): string {
  if (!width || !height || width <= 0 || height <= 0) {
    return t.none;
  }
  const text = t.sizeValue(formatCount(width), formatCount(height));
  return isPowerOfTwo(width, height) ? `${text}${t.powerOfTwo}` : text;
}

/** 透過「なし／あり（二値）／半透明あり」。 */
export function alphaText(image: AssetImageFactsDto | undefined): string {
  return image?.alpha ? t.alphaValues[image.alpha] : t.none;
}

/** 色数「16 色／256 色より多い」（JPEG とアルファのない HEIC・HEIF は計らないので「—」）。 */
export function colorsText(image: AssetImageFactsDto | undefined): string {
  const n = image?.colors;
  if (n === null || n === undefined || n <= 0) {
    return t.none;
  }
  return n > 256 ? t.colorsMany : t.colorsValue(formatCount(n));
}

/** 自動の判定の倍率（「4 倍」「4 × 2 倍」「等倍」）。 */
function scaleText(image: AssetImageFactsDto): string {
  const kx = image.pixelKx ?? 1;
  const ky = image.pixelKy ?? 1;
  if (kx <= 1 && ky <= 1) {
    return t.pixelSameSize;
  }
  return kx === ky ? t.pixelScale(String(kx)) : t.pixelScaleXY(String(kx), String(ky));
}

/** ドット絵の自動の判定（「自動の判定：はい（4 倍）」）。 */
export function pixelAutoText(image: AssetImageFactsDto | undefined): string {
  if (!image || image.pixelArt === null) {
    return t.pixelAuto(t.pixelAutoPending);
  }
  return t.pixelAuto(image.pixelArt ? t.pixelAutoYes(scaleText(image)) : t.pixelAutoNo);
}

/** 秒を「0.8」「1.25」「65」の形にする（小数 2 桁まで、末尾の 0 を省く）。 */
export function secondsText(ms: number): string {
  return String(Number((Math.max(0, ms) / 1000).toFixed(2)));
}

/** アニメ「8 コマ · 0.8 秒」（帯の長さがあれば秒も。1 周の長さで、0ms のコマは 100ms）。1 コマなら「なし」。 */
export function animText(frameCount: number | null, anim: AnimDto | null): string {
  const frames = anim?.frameCount ?? frameCount;
  if (frames === null || frames === undefined || frames < 2) {
    return frames === 1 ? t.animStill : t.none;
  }
  if (!anim) {
    return t.animFrames(formatCount(frames));
  }
  const total = anim.durations.reduce((sum, ms) => sum + effectiveDuration(ms), 0);
  return t.animFramesSeconds(formatCount(frames), secondsText(total));
}

/** 格子「16 × 16 · 12 コマ」か「なし」。`frames` は実効のコマの数（格子があれば帯で使う数）。 */
export function gridText(grid: AssetGridDto | null, frames: number | null): string {
  if (!grid) {
    return t.gridNone;
  }
  const count = grid.count ?? frames;
  const size = t.sizeValue(formatCount(grid.cellW), formatCount(grid.cellH));
  return count === null
    ? size
    : t.gridValue(formatCount(grid.cellW), formatCount(grid.cellH), formatCount(count));
}

/** kHz の値（「48」「44.1」「22.05」）。 */
export function khz(rate: number): string {
  return String(Number((rate / 1000).toFixed(3)));
}

/** チャンネル（「モノラル」「ステレオ」「6 チャンネル」）。 */
export function channelsText(channels: number): string {
  if (channels === 1) return t.mono;
  if (channels === 2) return t.stereo;
  return t.channelsValue(formatCount(channels));
}

/** 形式「48 kHz · 24 ビット · ステレオ」（非可逆の形式はビットが null なので出さない）。 */
export function audioFormatText(audio: AssetAudioFactsDto | undefined): string {
  if (!audio) {
    return t.none;
  }
  const parts: string[] = [];
  if (audio.sampleRate) parts.push(t.rateValue(khz(audio.sampleRate)));
  if (audio.bits) parts.push(t.bitsValue(String(audio.bits)));
  if (audio.channels) parts.push(channelsText(audio.channels));
  return parts.length > 0 ? parts.join(' · ') : t.none;
}

/** ラウドネス「−14.2 LUFS（LRA 6.1 LU）」。計れなければ理由（「短くて計れません」）。 */
export function loudnessText(audio: AssetAudioFactsDto | undefined): string {
  if (!audio) {
    return t.none;
  }
  if (audio.loudness === null) {
    if (!audio.loudnessReason) return t.none;
    return t.loudnessReasons[audio.loudnessReason] ?? t.loudnessUnknown;
  }
  const lufs = signed(audio.loudness, 1);
  return audio.loudnessRange === null
    ? t.loudnessValue(lufs)
    : t.loudnessWithRange(lufs, signed(audio.loudnessRange, 1));
}

/** ピーク「−1.0 dBTP」。 */
export function peakText(audio: AssetAudioFactsDto | undefined): string {
  return audio?.truePeak === null || audio?.truePeak === undefined
    ? t.none
    : t.peakValue(signed(audio.truePeak, 1));
}

/** フレームを「0:04.200」の形にする（1 時間を超えれば「1:02:03.456」）。 */
export function clockText(frames: number, rate: number): string {
  const ms = Math.max(0, Math.round((frames * 1000) / rate));
  const h = Math.floor(ms / 3_600_000);
  const m = Math.floor((ms % 3_600_000) / 60_000);
  const s = Math.floor((ms % 60_000) / 1000);
  const rest = `${String(s).padStart(2, '0')}.${String(ms % 1000).padStart(3, '0')}`;
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${rest}` : `${m}:${rest}`;
}

/** ループの時刻と標本の位置（なければ null）。 */
export function loopTexts(
  audio: AssetAudioFactsDto | undefined,
): { time: string; frames: string } | null {
  const range = audio?.loopRange;
  if (!range || !audio?.sampleRate) {
    return null;
  }
  return {
    time: t.loopRange(
      clockText(range.start, audio.sampleRate),
      clockText(range.end, audio.sampleRate),
    ),
    frames: t.loopRange(formatCount(range.start), formatCount(range.end)),
  };
}

/** フォントの形式（「OpenType（CFF）」「WOFF2」）。codec_info になければ拡張子から。 */
export function fontFormatText(font: AssetFontDto | null, ext: string): string {
  const key = (font?.format ?? ext).toLowerCase();
  return t.fontFormats[key] ?? (key ? key.toUpperCase() : t.none);
}

/** 数（「12,480」）。なければ「—」。 */
export function countText(n: number | null | undefined): string {
  return n === null || n === undefined ? t.none : formatCount(n);
}

/** 3D の大きさ「1.20 × 0.80 × 0.45」。 */
export function modelSizeText(size: AssetModelDto['size']): string {
  if (!size) {
    return t.none;
  }
  const [x, y, z] = size.map((v) => (v === null ? t.none : v.toFixed(2)));
  return t.modelSizeValue(x ?? t.none, y ?? t.none, z ?? t.none);
}

/** 描けない拡張「未対応：Draco 圧縮・スキン」（なければ null）。 */
export function unsupportedText(names: readonly string[]): string | null {
  if (names.length === 0) {
    return null;
  }
  const labels = [...new Set(names.map((n) => t.unsupportedNames[n] ?? n))];
  return t.unsupported(labels.join('・'));
}

/** 解析できなかった理由（media_facts.error_code）。 */
export function failureText(errorCode: string | null | undefined): string {
  return (errorCode && t.failureReasons[errorCode]) || t.failureUnknown;
}
