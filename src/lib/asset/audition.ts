/**
 * ループの試聴と効果音の即再生の決め方（docs/design.md §3.1）。純粋な関数。鳴らすのは src/lib/assetAudio.ts。
 *
 * WebKit で確かめたこと（M11 の試作）：
 * - AudioBufferSourceNode の loopStart・loopEnd を「フレーム ÷ レート」の秒で与えると、秒 × レートがちょうど整数に
 *   戻らない位置（44.1kHz で 1〜200,000 の 14%）で補間の道に落ち、継ぎ目の標本が合わない。近くの double でも直せない。
 * - ループの本体 [start, end) だけを 1 つのバッファにして loop = true（loopStart・loopEnd は既定の 0）で回すと、
 *   22.05〜96kHz のどれでも継ぎ目の食い違いは 0 標本。前奏は別のバッファにして、本体をそのあとに続けて置く。
 * - バッファと文脈が同じレートのときだけちょうど（違うレートは補間）。文脈は元のレートで作る。
 * だから：ループ点を秒で渡さない。バッファをループ点で切り分ける。
 */

import { MAX_RATE, MIN_RATE } from './wav';

/** 試聴に要る事実（解析の結果。/pcm の応答の `kura` チャンクで確かめ直す）。 */
export interface AuditionFacts {
  /** 音声の長さ（フレーム）。 */
  frames: number;
  rate: number;
  channels: number;
  /** ループの区間 [始まり, 終わり)（フレーム。`audio.loop` と同じ半開区間）。null なら全体をループで回す。 */
  loop: readonly [number, number] | null;
}

export type AuditionPlan =
  | {
      mode: 'loop';
      /** 前奏 [0, start)（はじめから聞くとき、本体が先頭からでなければ）。 */
      intro: [number, number] | null;
      /** ループの本体 [start, end)。loop = true で回す（loopStart・loopEnd は触らない）。 */
      body: [number, number];
      /** 本体を鳴らし始める位置（本体の中のフレーム。前奏があれば 0）。 */
      offset: number;
    }
  | {
      mode: 'seam';
      /** 継ぎ目の手前（ループの終わりの前）と、始まりから。続けて 1 回鳴らす。 */
      tail: [number, number];
      head: [number, number];
    };

export interface AuditionOptions {
  /** 鳴らすバッファ（浮動小数）のバイト数の上限。超えたら継ぎ目の前後だけ。 */
  capBytes: number;
  /** 継ぎ目の手前から鳴らす長さ（秒）。 */
  leadSeconds: number;
  /** 継ぎ目だけを聞くときの前後の長さ（秒）。 */
  seamSeconds: number;
  /** はじめから（前奏を含めて）鳴らす。 */
  fromStart: boolean;
}

export const DEFAULT_AUDITION: AuditionOptions = {
  capBytes: 64 * 1024 * 1024,
  leadSeconds: 3,
  seamSeconds: 4,
  fromStart: false,
};

const BYTES_PER_SAMPLE = 4;

function frameIndex(n: number): boolean {
  return Number.isSafeInteger(n) && n >= 0;
}

/** ループの試聴の計画。区間が壊れている・試聴できないレートなら null。 */
export function planAudition(
  facts: AuditionFacts,
  options: Partial<AuditionOptions> = {},
): AuditionPlan | null {
  const opts = { ...DEFAULT_AUDITION, ...options };
  const { frames, rate, channels } = facts;
  if (!frameIndex(frames) || !Number.isInteger(rate) || rate < MIN_RATE || rate > MAX_RATE)
    return null;
  if (!Number.isInteger(channels) || channels < 1) return null;
  const [start, end] = facts.loop ?? [0, frames];
  if (!frameIndex(start) || !frameIndex(end) || !(start < end) || end > frames) return null;
  const from = opts.fromStart ? 0 : start;
  if ((end - from) * channels * BYTES_PER_SAMPLE <= opts.capBytes) {
    const lead = Math.round(opts.leadSeconds * rate);
    return {
      mode: 'loop',
      intro: opts.fromStart && start > 0 ? [0, start] : null,
      body: [start, end],
      offset: opts.fromStart ? 0 : Math.max(0, end - start - lead),
    };
  }
  // 継ぎ目の前後も、2 つ合わせて上限に収める
  const fit = Math.floor(opts.capBytes / (2 * channels * BYTES_PER_SAMPLE));
  const seam = Math.min(Math.round(opts.seamSeconds * rate), end - start, fit);
  if (seam < 1) return null;
  return { mode: 'seam', tail: [end - seam, end], head: [start, start + seam] };
}

/** 計画が読む区間（取る順。/pcm の `{start}-{end}`）。 */
export function rangesOf(plan: AuditionPlan): [number, number][] {
  if (plan.mode === 'seam') return [plan.tail, plan.head];
  return plan.intro ? [plan.intro, plan.body] : [plan.body];
}

/** 選んだら鳴らす音声の長さの上限（ms。設定にはしない）。 */
export const SFX_MAX_MS = 10_000;

export interface SfxCandidate {
  kind: string;
  durationMs: number | null;
}

export interface SfxContext {
  /** 設定「選んだ短い音声をすぐ鳴らす」。 */
  enabled: boolean;
  /** プレーヤー（シアター・ミニプレーヤー）が再生中か（一時停止中は鳴らしてよい）。 */
  playerPlaying: boolean;
  /** クイックルックが開いているか（クイックルックは自分で鳴らす）。 */
  quickLook: boolean;
  /** 選んでいる数（1 件のときだけ鳴らす）。 */
  selectedCount: number;
}

/** 選択が動いたときに、選んだものをその場で鳴らすか。 */
export function shouldPlaySfx(item: SfxCandidate | undefined, cx: SfxContext): boolean {
  if (!cx.enabled || cx.playerPlaying || cx.quickLook || cx.selectedCount !== 1 || !item)
    return false;
  return (
    item.kind === 'audio' &&
    item.durationMs !== null &&
    item.durationMs > 0 &&
    item.durationMs <= SFX_MAX_MS
  );
}
