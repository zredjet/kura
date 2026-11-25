/**
 * ループの試聴の継ぎ目の自己点検（`VITE_KURA_CHECK=1` で起動・ビルドしたときだけ読み込む。通常のビルドには入らない。
 * docs/architecture.md §13）。
 *
 * 試聴を始めるたびに、同じバッファ（/pcm から読んだ PCM）と同じ組み立て（本体を loop で回す・前奏のあとに本体を置く・
 * 継ぎ目の前後を続けて鳴らす。始める時刻はフレームの境、バッファの中の始まりは「フレーム ÷ レート」の秒）を
 * OfflineAudioContext で描き、期待の標本（PCM を計画の順につないだもの）と 1 標本ずつ照らす。
 * 結果は `bench_report('audition-seam', …)` でログに出す（完了条件「ループの試聴に継ぎ目がない」は mismatches = 0。違いは
 * 聞こえない差（`SEAM_TOLERANCE`）を超えたものだけを数え、1 標本ずつ一致しなかった数 `inexact` と差の最大 `maxDiff` も出す）。
 *
 * 本物の試聴との違い：OfflineAudioContext は 0 秒から描くので、始める時刻は 0 を起点にする（本物は文脈の今の時刻の
 * 少し先のフレームの境）。前奏と本体が長くて描く長さが上限を超えるときは、前奏の終わりの手前から始める窓と、本体の
 * 継ぎ目の手前から始める窓に分ける。
 *
 * 音声エディタ（M13 の段 6）：エディタの置き方（チャンクの区間ごとのバッファを続けて置く・ループの折り返し）を同じく
 * 照らす（`runEditorSeamCheck`。`bench_report('edit-seam', …)`）。手のあとの交差フェードは、前後 20 ms の標本の跳びを
 * T で切り替えたときと比べる（`runCrossfadeCheck`。`bench_report('edit-crossfade', …)`）。呼ぶのは `waveCheck.ts`。
 */

import type { CrossfadeRecord, PlacedRecord } from '../features/editor/audio/playback';
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

/**
 * 聞こえない差として許す幅（2^-24 ≈ 6.0e-8。−144 dBFS）。書き出す整数の形式のいちばん細かい 24 ビットの 1 段（2^-23）より
 * 小さく、どの形式に書いても区別できない。WebKit の OfflineAudioContext は、始める時刻（秒）が標本の境に乗らないと標本の途中
 * から補間して鳴らすので、0 ちょうどの標本に 1e-14 ほどの残りが出る（0 でない標本は float32 に丸めると元に戻る）。区間を秒で
 * 置く限りこれは避けられないので、1 標本ずつの一致ではなくこの幅で判定し、一致しなかった数と差の最大は別に報告する
 * （docs/architecture.md §13・ADR-179）。
 */
export const SEAM_TOLERANCE = 2 ** -24;

/** 照らした結果。 */
export interface Comparison {
  frames: number;
  /** 1 チャンネルでも許す幅（`SEAM_TOLERANCE`）を超えて違ったフレームの数（合格は 0）。 */
  mismatches: number;
  /** 1 チャンネルでも 1 標本ずつは一致しなかったフレームの数（幅の内の差を含む。見張りのための数）。 */
  inexact: number;
  /** 差の最大（NaN の食い違い・足りない標本は Infinity）。 */
  maxDiff: number;
  /** 許す幅を超えた最初の違い。 */
  first: { frame: number; channel: number; expected: number; got: number } | null;
}

/** 2 つの標本の差（どちらも NaN なら 0、片方だけなら Infinity）。 */
function diff(a: number, b: number): number {
  if (Number.isNaN(a) || Number.isNaN(b)) return Number.isNaN(a) && Number.isNaN(b) ? 0 : Infinity;
  return Math.abs(a - b);
}

/**
 * 描いた標本と期待の標本を 1 標本ずつ照らす（足りないチャンネル・フレームは違いとして数える）。違いは許す幅で判定し、
 * 幅の内でも一致しなかったフレームは `inexact` に数える。
 */
export function compareSamples(
  expected: readonly Float32Array[],
  got: readonly Float32Array[],
  tolerance = SEAM_TOLERANCE,
): Comparison {
  const frames = expected[0]?.length ?? 0;
  let mismatches = 0;
  let inexact = 0;
  let maxDiff = 0;
  let first: Comparison['first'] = null;
  for (let k = 0; k < frames; k++) {
    let bad = false;
    let off = false;
    for (const [ch, want] of expected.entries()) {
      const e = want[k] ?? 0;
      const g = got[ch]?.[k];
      const d = g === undefined ? Infinity : diff(e, g);
      if (d > maxDiff) maxDiff = d;
      if (d > 0) off = true;
      if (d > tolerance) {
        bad = true;
        first ??= { frame: k, channel: ch, expected: e, got: g ?? Number.NaN };
      }
    }
    if (bad) mismatches++;
    if (off) inexact++;
  }
  return { frames, mismatches, inexact, maxDiff, first };
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
  inexact: number;
  /** 差の最大（JSON では Infinity が null になる）。 */
  maxDiff: number;
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
  let inexact = 0;
  let maxDiff = 0;
  let first: Comparison['first'] = null;
  for (const w of windows) {
    const got = await render(w, pcms, rate, channels);
    const result = compareSamples(expectedSamples(w, pcms, channels), got);
    seams += w.seams.length;
    frames += result.frames;
    mismatches += result.mismatches;
    inexact += result.inexact;
    maxDiff = Math.max(maxDiff, result.maxDiff);
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
    inexact,
    maxDiff,
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

// ===== 音声エディタの編集中の試聴（M13 の段 6。設計 §2.13 の 1・2） =====

/** 区間の標本を PCM の形にする（`expectedSamples`・`offlineRender` に渡す）。 */
function asPcm(r: PlacedRecord): Pcm {
  const frames = r.end - r.start;
  return {
    rate: r.rate,
    channels: r.planes.length,
    frames,
    format: 'f32',
    planes: r.planes,
    info: { start: r.start, total: r.end, loop: null, decoder: 'kura-edit' },
  };
}

/**
 * エディタが置いた区間の列から、照らす窓を作る（純粋な関数）：先頭から、続けて置いたもの（次の区間がちょうど前の区間の
 * 終わりに置かれた）だけを、上限の長さまで。交差フェードで入れた区間（0→1）と、間の空いた所（遅らせて置いた）で切る。
 * 継ぎ目が 1 つもなければ null。
 */
export function editorSeamWindow(
  records: readonly PlacedRecord[],
  maxSeconds = MAX_WINDOW_SECONDS,
): { window: SeamWindow; pcms: Pcm[]; rate: number; channels: number } | null {
  const first = records[0];
  if (!first || first.fadeIn) return null;
  const rate = first.rate;
  const max = Math.round(maxSeconds * rate);
  const parts: WindowPart[] = [];
  const pcms: Pcm[] = [];
  const seams: number[] = [];
  let at = 0;
  let prevEnd = first.abs;
  for (const r of records) {
    const len = r.end - r.start;
    if (r.rate !== rate || r.fadeIn || r.abs !== prevEnd || len <= 0) break;
    if (parts.length > 0 && at + len > max) break;
    if (parts.length > 0) seams.push(at);
    parts.push({ pcm: pcms.length, loop: false, at, offset: 0 });
    pcms.push(asPcm(r));
    at += len;
    prevEnd = r.abs + len;
  }
  if (seams.length === 0) return null;
  return { window: { parts, frames: at, seams }, pcms, rate, channels: first.planes.length };
}

/** エディタの継ぎ目の報告（bench_report('edit-seam', …)）。 */
export interface EditorSeamReport {
  rate: number;
  channels: number;
  segments: number;
  seams: number;
  frames: number;
  mismatches: number;
  inexact: number;
  /** 差の最大（JSON では Infinity が null になる）。 */
  maxDiff: number;
  first: Comparison['first'];
  ms: number;
}

/**
 * エディタの置き方（区間ごとのバッファを、前の区間の終わりのフレームに続けて置く。ループの折り返しも同じ）を
 * OfflineAudioContext で描き、PCM をつないだものと 1 標本ずつ照らす（違いは聞こえない差 `SEAM_TOLERANCE` で判定）。
 * 照らす窓がなければ null。
 */
export async function runEditorSeamCheck(
  records: readonly PlacedRecord[],
  render: RenderWindow = offlineRender,
  now: () => number = () => performance.now(),
): Promise<EditorSeamReport | null> {
  const built = editorSeamWindow(records);
  if (!built) return null;
  const started = now();
  const { window, pcms, rate, channels } = built;
  const got = await render(window, pcms, rate, channels);
  const result = compareSamples(expectedSamples(window, pcms, channels), got);
  return {
    rate,
    channels,
    segments: window.parts.length,
    seams: window.seams.length,
    frames: result.frames,
    mismatches: result.mismatches,
    inexact: result.inexact,
    maxDiff: result.maxDiff,
    first: result.first,
    ms: Math.round(now() - started),
  };
}

/** 区間 `[from, to)` の、隣り合う標本の差の最大（チャンネルのうち最大）。 */
export function maxJump(samples: readonly Float32Array[], from: number, to: number): number {
  let max = 0;
  for (const plane of samples) {
    const a = Math.max(0, from);
    const b = Math.min(plane.length - 1, to);
    for (let k = a; k < b; k++) {
      const d = Math.abs((plane[k + 1] ?? 0) - (plane[k] ?? 0));
      if (d > max) max = d;
    }
  }
  return max;
}

/** 交差フェードの前後を描く口（`fade` が偽なら、T で切り替える）。描く長さ `frames`、窓の頭は文脈のフレーム `base`。 */
export type RenderCrossfade = (
  record: CrossfadeRecord,
  base: number,
  frames: number,
  fade: boolean,
) => Promise<Float32Array[]>;

/**
 * 窓の中で区間を鳴らし始める時刻（秒）と、バッファの中の始まり（秒）。窓の頭 `base` より前に置いた区間（T をまたいで
 * 鳴っていた古い音）は、窓の 0 秒から `base − abs` フレーム目を鳴らす（純粋な関数）。
 */
export function crossfadePlacement(
  r: Pick<PlacedRecord, 'abs'>,
  base: number,
  rate: number,
): { when: number; offset: number } {
  const w = (r.abs - base) / rate;
  return { when: Math.max(0, w), offset: Math.max(0, -w) };
}

/** エディタと同じ組み立て（止める音は T から 1→0、入れる音は 0→1。どちらも 10 ms）で OfflineAudioContext に描く。 */
export const offlineCrossfade: RenderCrossfade = async (record, base, frames, fade) => {
  const channels = record.fresh?.planes.length ?? record.old[0]?.planes.length ?? 1;
  const rate = record.rate;
  const ctx = new OfflineAudioContext(channels, frames, rate);
  const t = (record.t - base) / rate;
  const xf = 0.01;
  const put = (r: PlacedRecord, role: 'old' | 'fresh') => {
    const buffer = ctx.createBuffer(r.planes.length, r.end - r.start, rate);
    r.planes.forEach((plane, ch) => buffer.copyToChannel(plane, ch));
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    const gain = ctx.createGain();
    src.connect(gain);
    gain.connect(ctx.destination);
    // 先に start（stop を先に呼ぶと InvalidStateError）。窓の頭より前に始まった区間はバッファの中をずらして鳴らす
    const { when, offset } = crossfadePlacement(r, base, rate);
    src.start(when, offset);
    // 区間の利得（音量を合わせて聞く。M17 段 6）もエディタと同じに当てる
    const level = r.level;
    if (role === 'old') {
      if (fade) {
        gain.gain.setValueAtTime(level, t);
        gain.gain.linearRampToValueAtTime(0, t + xf);
        src.stop(Math.max(when, t + xf));
      } else {
        gain.gain.value = level;
        src.stop(Math.max(when, t));
      }
    } else if (fade) {
      gain.gain.setValueAtTime(0, when);
      gain.gain.linearRampToValueAtTime(level, when + xf);
    } else {
      gain.gain.value = level;
    }
  };
  for (const r of record.old) put(r, 'old');
  if (record.fresh) put(record.fresh, 'fresh');
  const rendered = await ctx.startRendering();
  return Array.from({ length: channels }, (_, ch) => rendered.getChannelData(ch));
};

/** 交差フェードの報告（bench_report('edit-crossfade', …)）。 */
export interface CrossfadeReport {
  rate: number;
  /** T の前後 20 ms の、隣り合う標本の差の最大（交差フェードあり・T で切り替えたとき）。 */
  withFade: number;
  hardCut: number;
  /** 交差フェードのほうが跳びが小さい（同じ中身で、どちらもほとんど跳ばなければ良しとする）。 */
  ok: boolean;
  ms: number;
}

/** 交差フェードの前後 20 ms の跳びを、T で切り替えたときと比べる。 */
export async function runCrossfadeCheck(
  record: CrossfadeRecord,
  render: RenderCrossfade = offlineCrossfade,
  now: () => number = () => performance.now(),
): Promise<CrossfadeReport | null> {
  if (!record.fresh || record.old.length === 0) return null;
  const started = now();
  const rate = record.rate;
  const around = Math.round(0.02 * rate);
  const base = record.t - around * 2;
  const frames = around * 4;
  const center = record.t - base;
  const [a, b] = await Promise.all([
    render(record, base, frames, true),
    render(record, base, frames, false),
  ]);
  const withFade = maxJump(a, center - around, center + around);
  const hardCut = maxJump(b, center - around, center + around);
  return {
    rate,
    withFade,
    hardCut,
    ok: withFade <= hardCut + 1e-6 || hardCut < 1e-3,
    ms: Math.round(now() - started),
  };
}
