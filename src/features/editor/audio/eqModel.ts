/**
 * イコライザ（`audio.eq` v1）の引数を画面で扱う（M17 段 5。純粋な関数。式は Rust にだけあり、ここは引数の読み書きと点の
 * 置き方・範囲の収め方。範囲と既定は bindings の `EQ_LIMITS` を読む：手で写さない）。
 */

import { EQ_LIMITS, type EditSpecChannelDto, type JsonValue } from '../../../ipc/edit';
import { freqParts } from './spectrumMath';

export type BandType =
  'bell' | 'lowShelf' | 'highShelf' | 'lowCut' | 'highCut' | 'notch' | 'bandPass' | 'tilt';
export type BandChannel = 'all' | 'left' | 'right' | 'mid' | 'side';

export const BAND_TYPES: readonly BandType[] = [
  'bell',
  'lowShelf',
  'highShelf',
  'lowCut',
  'highCut',
  'notch',
  'bandPass',
  'tilt',
];
export const BAND_CHANNELS: readonly BandChannel[] = ['all', 'left', 'right', 'mid', 'side'];

/** バンド 1 つ（引数の JSON と同じ名前）。 */
export interface EqBand {
  type: BandType;
  freq: number;
  gainDb: number;
  q: number;
  slope: number;
  channel: BandChannel;
  enabled: boolean;
}

/** イコライザの引数。 */
export interface EqParams {
  bands: EqBand[];
  phase: 'minimum' | 'linear';
  gainDb: number;
}

/** 利得を使う型（ベル・シェルフ・チルト）。 */
export function hasGain(t: BandType): boolean {
  return t === 'bell' || t === 'lowShelf' || t === 'highShelf' || t === 'tilt';
}

/** Q を使う型（ベル・ノッチ・バンドパス。シェルフは肩の形）。 */
export function hasQ(t: BandType): boolean {
  return t !== 'lowCut' && t !== 'highCut' && t !== 'tilt';
}

/** 傾きを使う型（カット）。 */
export function hasSlope(t: BandType): boolean {
  return t === 'lowCut' || t === 'highCut';
}

const num = (v: unknown, d: number): number =>
  typeof v === 'number' && Number.isFinite(v) ? v : d;

/** 引数（JSON）を読む（足りない欄は既定）。 */
export function readEq(params: JsonValue | undefined): EqParams {
  const p = (
    params && typeof params === 'object' && !Array.isArray(params) ? params : {}
  ) as Record<string, unknown>;
  const list = Array.isArray(p.bands) ? p.bands : [];
  const bands = list.map((raw): EqBand => {
    const b = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
    const type = BAND_TYPES.includes(b.type as BandType) ? (b.type as BandType) : 'bell';
    const channel = BAND_CHANNELS.includes(b.channel as BandChannel)
      ? (b.channel as BandChannel)
      : 'all';
    return {
      type,
      freq: num(b.freq, 1_000),
      gainDb: num(b.gainDb, 0),
      q: num(b.q, EQ_LIMITS.defaultQ),
      slope: num(b.slope, EQ_LIMITS.defaultSlope),
      channel,
      enabled: typeof b.enabled === 'boolean' ? b.enabled : true,
    };
  });
  return {
    bands,
    phase: p.phase === 'linear' ? 'linear' : 'minimum',
    gainDb: num(p.gainDb, 0),
  };
}

/** バンドの列を引数の `bands` の JSON に。 */
export function bandsJson(bands: readonly EqBand[]): JsonValue {
  return bands.map((b) => ({ ...b })) as unknown as JsonValue;
}

/** 引数全体の JSON（曲線を求める口に渡す）。 */
export function eqJson(p: EqParams): JsonValue {
  return { bands: bandsJson(p.bands), phase: p.phase, gainDb: p.gainDb } as unknown as JsonValue;
}

/** 周波数をレートで使える範囲に収める。 */
export function clampFreq(f: number, rate: number): number {
  const hi = Math.min(
    EQ_LIMITS.freqMax,
    rate > 0 ? rate * EQ_LIMITS.nyquistGuard : EQ_LIMITS.freqMax,
  );
  return Math.min(hi, Math.max(EQ_LIMITS.freqMin, f));
}

export function clampGain(g: number): number {
  return Math.min(EQ_LIMITS.gainMax, Math.max(-EQ_LIMITS.gainMax, g));
}

export function clampQ(q: number, t: BandType): number {
  const hi = t === 'lowShelf' || t === 'highShelf' ? EQ_LIMITS.shelfQMax : EQ_LIMITS.qMax;
  return Math.min(hi, Math.max(EQ_LIMITS.qMin, q));
}

/** 傾きを選べる値の最も近いものに。 */
export function nearestSlope(s: number): number {
  let best: number = EQ_LIMITS.slopes[0] ?? 12;
  for (const v of EQ_LIMITS.slopes) if (Math.abs(v - s) < Math.abs(best - s)) best = v;
  return best;
}

/** 点の位置（周波数と ±dB。ベル・シェルフ・チルトは利得、カット・ノッチ・バンドパスは 0 dB の線。P7）。 */
export function bandPoint(b: EqBand): { freq: number; db: number } {
  return { freq: b.freq, db: hasGain(b.type) ? b.gainDb : 0 };
}

/** 新しいベル。 */
export function newBell(freq: number, gainDb: number, q: number = EQ_LIMITS.defaultQ): EqBand {
  return {
    type: 'bell',
    freq,
    gainDb,
    q,
    slope: EQ_LIMITS.defaultSlope,
    channel: 'all',
    enabled: true,
  };
}

/** Q の帯の幅（オクターブ）。 */
export function bandwidthOct(q: number): number {
  return (2 / Math.LN2) * Math.asinh(1 / (2 * Math.max(q, 1e-6)));
}

/** 帯の幅（オクターブ）の Q。 */
export function qOfBandwidth(oct: number): number {
  return 1 / (2 * Math.sinh((Math.LN2 / 2) * Math.max(oct, 1e-6)));
}

/**
 * ソロで鳴らす帯（M17 段 6）：ベル・ノッチ・バンドパスは中心と Q、シェルフ・カットは境の周波数を中心に 1 オクターブ、
 * チルトはソロなし（null）。
 */
export function soloBandOf(b: EqBand): { freq: number; q: number } | null {
  switch (b.type) {
    case 'bell':
    case 'notch':
    case 'bandPass':
      return { freq: b.freq, q: b.q };
    case 'lowShelf':
    case 'highShelf':
    case 'lowCut':
    case 'highCut':
      return { freq: b.freq, q: qOfBandwidth(1) };
    case 'tilt':
      return null;
  }
}

/** 選んだ点の左右の Q の印の周波数（Q を使わない型は null）。 */
export function qHandles(b: EqBand): { lo: number; hi: number } | null {
  if (!hasQ(b.type)) return null;
  const half = bandwidthOct(b.q) / 2;
  return { lo: b.freq / Math.pow(2, half), hi: b.freq * Math.pow(2, half) };
}

/** Q の印を周波数 `f` へ動かしたときの Q（中心からの距離の 2 倍が帯の幅）。 */
export function qFromHandle(b: EqBand, f: number): number {
  const oct = 2 * Math.abs(Math.log2(Math.max(f, 1e-9) / b.freq));
  return clampQ(qOfBandwidth(oct), b.type);
}

/** 表示のチャンネルとバンドのチャンネルが合うか（「前 ＋ 曲線」で後を描けるか。P9）。 */
export function approxCompatible(bands: readonly EqBand[], channel: EditSpecChannelDto): boolean {
  const want: BandChannel | null =
    channel.type === 'mid'
      ? 'mid'
      : channel.type === 'side'
        ? 'side'
        : channel.type === 'index'
          ? channel.index === 0
            ? 'left'
            : channel.index === 1
              ? 'right'
              : null
          : null;
  return bands
    .filter((b) => b.enabled)
    .every((b) => b.channel === 'all' || (want !== null && b.channel === want));
}

/** 周波数の短い書き方（「2.80 kHz」）。 */
export function freqText(f: number): string {
  const p = freqParts(f);
  return `${p.value} ${p.unit}`;
}

/** dB の書き方（負は「−」。「−5.0 dB」）。 */
export function dbText(db: number): string {
  const v = Math.abs(db).toFixed(1);
  return `${db < 0 && v !== '0.0' ? '−' : ''}${v} dB`;
}
