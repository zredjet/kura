/**
 * ループの試聴の継ぎ目の自己点検（`VITE_KURA_CHECK=1` で起動・ビルドしたときだけ読み込む。通常のビルドには入らない。
 * docs/architecture.md §13）。
 *
 * 試聴を始めるたびに、同じバッファ（/pcm から読んだ PCM）と同じ組み立て（本体を loop で回す・前奏のあとに本体を置く・
 * 継ぎ目の前後を続けて鳴らす。始める時刻はフレームの境、バッファの中の始まりは「フレーム ÷ レート」の秒）を
 * OfflineAudioContext で描き、期待の標本（PCM を計画の順につないだもの）と 1 標本ずつ照らす。
 * 結果は `bench_report('audition-seam', …)` でログに出す（完了条件「ループの試聴に継ぎ目がない」は mismatches = 0）。
 *
 * 本物の試聴との違い：OfflineAudioContext は 0 秒から描くので、始める時刻は 0 を起点にする（本物は文脈の今の時刻の
 * 少し先のフレームの境）。前奏と本体が長くて描く長さが上限を超えるときは、前奏の終わりの手前から始める窓と、本体の
 * 継ぎ目の手前から始める窓に分ける。
 */

import { commands } from '../ipc/bindings';
import type { AuditionPlan } from '../lib/asset/audition';
import type { Pcm } from '../lib/asset/wav';
import type { ScheduledAudition } from '../lib/assetAudio';

/** 継ぎ目のあとに照らす長さ（秒）。 */
const AFTER_SECONDS = 1;
/** 1 つの窓で描く長さの上限（秒）。 */
const MAX_WINDOW_SECONDS = 30;

/** 窓の中の 1 つのバッファの置き方（pcm は読んだ順の番号）。 */
export interface WindowPart {
  pcm: number;
  loop: boolean;
  /** 始める時刻（窓の始まりからのフレーム）。 */
  at: number;
  /** バッファの中の始まり（フレーム）。 */
  offset: number;
}

/** 描いて照らす窓。 */
export interface SeamWindow {
  parts: WindowPart[];
  /** 描く長さ（フレーム）。 */
  frames: number;
  /** 窓の中の継ぎ目（バッファのつなぎ目・ループの折り返し）の位置。 */
  seams: number[];
}

/** 継ぎ目の位置：`first` から `step` ごとに、`frames` の手前まで（step が 0 なら first だけ）。 */
function seamsFrom(first: number, step: number, frames: number): number[] {
  const out: number[] = [];
  for (let at = first; at < frames; at += step) {
    out.push(at);
    if (step <= 0) break;
  }
  return out;
}

/** 本体を loop で回す窓（`offset` から始めて、最初の継ぎ目のあと `after` フレームまで）。 */
function loopWindow(pcm: number, len: number, offset: number, after: number): SeamWindow {
  const first = len - offset;
  const frames = first + after;
  return {
    parts: [{ pcm, loop: true, at: 0, offset }],
    frames,
    seams: seamsFrom(first, len, frames),
  };
}

/**
 * 計画と読んだ PCM の長さから、照らす窓を作る。純粋な関数。
 * - 本体だけ：本物と同じ始まり（継ぎ目の 3 秒前）から、継ぎ目のあと 1 秒まで。
 * - 前奏つき：前奏の始まりから、本体の最初の継ぎ目のあと 1 秒まで（上限を超えれば、前奏の終わりの 1 秒前からの窓と、
 *   本体の継ぎ目の 1 秒前からの窓）。
 * - 継ぎ目だけ：手前と始まりを続けて 1 回。
 */
export function seamWindows(
  plan: AuditionPlan,
  lengths: readonly number[],
  rate: number,
): SeamWindow[] {
  const after = Math.max(1, Math.round(AFTER_SECONDS * rate));
  const max = Math.round(MAX_WINDOW_SECONDS * rate);
  const [a, b] = lengths;
  if (a === undefined || a <= 0) return [];
  if (plan.mode === 'seam') {
    if (b === undefined || b <= 0) return [];
    return [
      {
        parts: [
          { pcm: 0, loop: false, at: 0, offset: 0 },
          { pcm: 1, loop: false, at: a, offset: 0 },
        ],
        frames: a + b,
        seams: [a],
      },
    ];
  }
  if (b === undefined) {
    return [loopWindow(0, a, Math.min(plan.offset, a - 1), after)];
  }
  if (b <= 0) return [];
  if (a + b + after <= max) {
    const frames = a + b + after;
    return [
      {
        parts: [
          { pcm: 0, loop: false, at: 0, offset: 0 },
          { pcm: 1, loop: true, at: a, offset: 0 },
        ],
        frames,
        seams: [a, ...seamsFrom(a + b, b, frames)],
      },
    ];
  }
  const lead = Math.min(after, a);
  const junction: SeamWindow = {
    parts: [
      { pcm: 0, loop: false, at: 0, offset: a - lead },
      { pcm: 1, loop: true, at: lead, offset: 0 },
    ],
    frames: lead + Math.min(after, b),
    seams: [lead],
  };
  return [junction, loopWindow(1, b, Math.max(0, b - after), after)];
}

/** 窓の期待の標本（チャンネルごと）。どのバッファも鳴っていない所は 0。 */
export function expectedSamples(
  window: SeamWindow,
  pcms: readonly Pcm[],
  channels: number,
): Float32Array[] {
  const out = Array.from({ length: channels }, () => new Float32Array(window.frames));
  for (let k = 0; k < window.frames; k++) {
    let part: WindowPart | undefined;
    for (const p of window.parts) {
      if (p.at <= k) part = p;
    }
    const pcm = part ? pcms[part.pcm] : undefined;
    if (!part || !pcm || pcm.frames <= 0) continue;
    let local = part.offset + (k - part.at);
    if (part.loop) local %= pcm.frames;
    if (local >= pcm.frames) continue;
    for (let ch = 0; ch < channels; ch++) {
      const plane = out[ch];
      if (plane) plane[k] = pcm.planes[ch]?.[local] ?? 0;
    }
  }
  return out;
}

/** 照らした結果。 */
export interface Comparison {
  frames: number;
  /** 1 チャンネルでも違ったフレームの数。 */
  mismatches: number;
  first: { frame: number; channel: number; expected: number; got: number } | null;
}

function same(a: number, b: number): boolean {
  return a === b || (Number.isNaN(a) && Number.isNaN(b));
}

/** 描いた標本と期待の標本を 1 標本ずつ照らす（足りないチャンネル・フレームは違いとして数える）。 */
export function compareSamples(
  expected: readonly Float32Array[],
  got: readonly Float32Array[],
): Comparison {
  const frames = expected[0]?.length ?? 0;
  let mismatches = 0;
  let first: Comparison['first'] = null;
  for (let k = 0; k < frames; k++) {
    let bad = false;
    for (const [ch, want] of expected.entries()) {
      const e = want[k] ?? 0;
      const g = got[ch]?.[k];
      if (g === undefined || !same(e, g)) {
        bad = true;
        first ??= { frame: k, channel: ch, expected: e, got: g ?? Number.NaN };
      }
    }
    if (bad) mismatches++;
  }
  return { frames, mismatches, first };
}

/** 窓を描く口（本物は OfflineAudioContext。テストでは偽物）。 */
export type RenderWindow = (
  window: SeamWindow,
  pcms: readonly Pcm[],
  rate: number,
  channels: number,
) => Promise<Float32Array[]>;

/** 本物の試聴と同じ組み立て（loop・start(フレーム ÷ レート, フレーム ÷ レート)）で OfflineAudioContext に描く。 */
export const offlineRender: RenderWindow = async (window, pcms, rate, channels) => {
  const ctx = new OfflineAudioContext(channels, window.frames, rate);
  for (const part of window.parts) {
    const pcm = pcms[part.pcm];
    if (!pcm) continue;
    const buffer = ctx.createBuffer(pcm.channels, pcm.frames, rate);
    pcm.planes.forEach((plane, ch) => buffer.copyToChannel(plane, ch));
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.loop = part.loop;
    source.connect(ctx.destination);
    source.start(part.at / rate, part.offset / rate);
  }
  const rendered = await ctx.startRendering();
  return Array.from({ length: channels }, (_, ch) => rendered.getChannelData(ch));
};

/** 自己点検の報告（bench_report の中身）。 */
export interface SeamReport {
  hash: string;
  mode: AuditionPlan['mode'];
  rate: number;
  channels: number;
  windows: number;
  seams: number;
  frames: number;
  mismatches: number;
  first: Comparison['first'];
  ms: number;
}

/** 試聴 1 回分を照らす。 */
export async function runSeamCheck(
  scheduled: ScheduledAudition,
  render: RenderWindow = offlineRender,
  now: () => number = () => performance.now(),
): Promise<SeamReport> {
  const started = now();
  const { plan, rate, pcms } = scheduled;
  const channels = pcms[0]?.channels ?? 0;
  const windows = seamWindows(
    plan,
    pcms.map((p) => p.frames),
    rate,
  );
  let seams = 0;
  let frames = 0;
  let mismatches = 0;
  let first: Comparison['first'] = null;
  for (const w of windows) {
    const got = await render(w, pcms, rate, channels);
    const result = compareSamples(expectedSamples(w, pcms, channels), got);
    seams += w.seams.length;
    frames += result.frames;
    mismatches += result.mismatches;
    first ??= result.first;
  }
  return {
    hash: scheduled.hash,
    mode: plan.mode,
    rate,
    channels,
    windows: windows.length,
    seams,
    frames,
    mismatches,
    first,
    ms: Math.round(now() - started),
  };
}

/** 試聴を始めたときに呼ぶ（待たない）。結果は console.warn と Rust のログ（bench_report）に出す。 */
export function checkSeam(scheduled: ScheduledAudition): void {
  void runSeamCheck(scheduled).then(
    (report) => send(report),
    (error: unknown) =>
      send({ hash: scheduled.hash, error: error instanceof Error ? error.message : String(error) }),
  );
}

function send(value: unknown): void {
  const json = JSON.stringify(value);
  console.warn(`[check] audition-seam ${json}`);
  void commands.benchReport('audition-seam', json).catch(() => undefined);
}
