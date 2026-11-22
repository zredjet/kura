/**
 * 音声エディタの自己点検（`VITE_KURA_CHECK=1` で起動・ビルドしたときだけ読み込む。通常のビルドには入らない。M13 の段 6、
 * 設計 §2.13、docs/architecture.md §13）。結果は console.warn と `bench_report` に出す。
 *
 * 1. 継ぎ目（`edit-seam`）：続けて置いた区間（ループの折り返しも）を OfflineAudioContext で描き、PCM をつないだものと
 *    1 標本ずつ照らす（`seamCheck.ts` の `runEditorSeamCheck`）。食い違いが 0 であること。44.1・48・96 kHz の音声で。
 * 2. 交差フェード（`edit-crossfade`）：手のあとの差し替えで、前後 20 ms の標本の跳びが T で切り替えたときより小さい。
 * 3. 60 分の描画（`edit-wave`）：続けてパン・ズームしたときの 1 フレームの描く時間（p95 ≤ 16.7 ms）・取ったタイルの数・
 *    手元の表の大きさ。30 分以上の音声を開いて段が届いたら 1 回、ほかは devtools で `__kuraWaveCheck()`。WebContent の
 *    メモリは WebKit から読めないので、測っている間に `footprint -p <WebContent の pid>` か アクティビティモニタで見る。
 * 4. 往復（`edit-roundtrip`）：手を含む `edit_step` を送ってから表示が届くまでと、PCM 1 チャンクの取得（NFR-11 の 50 ms）。
 *    20 回ごとにまとめて出す（gain・fade・ループ点の手を試す）。
 * 5. 文脈（`edit-context`）：鳴らしている間に devtools で `__kuraContextCheck()`：効果音・試聴がほかのレートで文脈を
 *    作っても（3 つ作る）、エディタの文脈が閉じられない（使用中の印がある・プールに残っている）。
 */

import { audioRuntime } from '../features/asset/audioRuntime';
import type { AudioCheck } from '../features/editor/audio/audioStore';
import type {
  CrossfadeRecord,
  EditorPlayer,
  PlacedRecord,
} from '../features/editor/audio/playback';
import type { WaveController } from '../features/editor/audio/waveController';
import { commands } from '../ipc/bindings';
import type { ContextLike } from '../lib/assetAudio';
import type { EditStepDto } from '../ipc/edit';
import { runCrossfadeCheck, runEditorSeamCheck } from './seamCheck';
import { stats } from './tileCheck';

/** 1 回のまとめに使う数。 */
const BATCH = 20;
/** 継ぎ目を照らす回数の上限（1 回の起動で）。 */
const MAX_SEAM_CHECKS = 6;
/** 1 つの窓にためる区間の数。 */
const RUN_SEGMENTS = 6;
/** 描画を測るフレームの数。 */
export const BENCH_FRAMES = 240;
/** 自動で描画を測る長さ（秒）。 */
const AUTO_BENCH_SECONDS = 30 * 60;

function send(name: string, value: unknown): void {
  const json = JSON.stringify(value);
  console.warn(`[check] ${name} ${json}`);
  void commands.benchReport(name, json).catch(() => undefined);
}

/** 文書を変える手か（往復を測る手）。 */
function docStep(step: EditStepDto): boolean {
  return step.type !== 'seal' && step.type !== 'preview' && step.type !== 'cancelPreview';
}

/** 手の種類の名前（往復のまとめの内訳）。 */
export function stepKind(steps: readonly EditStepDto[]): string {
  const s = steps.find(docStep) ?? steps[0];
  if (!s) return 'view';
  if (s.type === 'handle') return `handle:${s.handle.type}`;
  if (s.type === 'commit') return `commit:${s.edit.type}`;
  return s.type;
}

/** 往復の時間をためてまとめる（純粋な部分）。 */
export class RoundTrips {
  private readonly started = new Map<number, { t: number; kind: string }>();
  private samples: { kind: string; ms: number }[] = [];

  /** 手を含む要求を送った。 */
  sent(seq: number, kind: string, t: number): void {
    this.started.set(seq, { t, kind });
  }

  /** 表示が届いた。`BATCH` 回たまったらまとめを返す。 */
  shown(
    seq: number,
    t: number,
  ): {
    count: number;
    all: ReturnType<typeof stats>;
    kinds: Record<string, ReturnType<typeof stats>>;
  } | null {
    const s = this.started.get(seq);
    if (!s) return null;
    this.started.delete(seq);
    this.samples.push({ kind: s.kind, ms: t - s.t });
    if (this.samples.length < BATCH) return null;
    const all = stats(this.samples.map((x) => x.ms));
    const kinds: Record<string, ReturnType<typeof stats>> = {};
    for (const kind of new Set(this.samples.map((x) => x.kind))) {
      kinds[kind] = stats(this.samples.filter((x) => x.kind === kind).map((x) => x.ms));
    }
    const count = this.samples.length;
    this.samples = [];
    return { count, all, kinds };
  }
}

/** 描画を測るときの操作の並び（パンとズームを交互に。純粋な関数）。 */
export function benchSteps(
  n: number,
): ({ kind: 'pan'; dx: number } | { kind: 'zoom'; factor: number })[] {
  const out: ({ kind: 'pan'; dx: number } | { kind: 'zoom'; factor: number })[] = [];
  for (let i = 0; i < n; i++) {
    const phase = Math.floor(i / 30) % 4;
    if (phase === 0) out.push({ kind: 'pan', dx: 40 });
    else if (phase === 1) out.push({ kind: 'zoom', factor: 1.08 });
    else if (phase === 2) out.push({ kind: 'pan', dx: -40 });
    else out.push({ kind: 'zoom', factor: 1 / 1.08 });
  }
  return out;
}

/** 次の描く時刻を待つ。 */
function nextFrame(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

/** 続けてパン・ズームして、1 フレームの描く時間と取った数を測る。 */
export async function runWaveBench(c: WaveController, frames = BENCH_FRAMES): Promise<unknown> {
  const times: number[] = [];
  const before = { ...c.stats };
  const start = c.window;
  const center = c.inner().x + c.inner().w / 2;
  c.renderProbe = (ms) => times.push(ms);
  const started = performance.now();
  for (const step of benchSteps(frames)) {
    if (step.kind === 'pan') c.panBy(step.dx);
    else c.zoomAt(step.factor, center);
    await nextFrame();
  }
  c.renderProbe = null;
  c.setWindow(start, true);
  const memory = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory;
  return {
    total: c.total,
    rate: c.rate,
    frames: times.length,
    draw: stats(times),
    over16ms: times.filter((t) => t > 16.7).length,
    tileLoads: c.stats.tileLoads - before.tileLoads,
    pcmLoads: c.stats.pcmLoads - before.pcmLoads,
    tiles: c.tiles.size,
    tileBytes: c.tiles.bytes,
    pcmBytes: c.pcm.bytes,
    jsHeap: memory?.usedJSHeapSize ?? null,
    ms: Math.round(performance.now() - started),
  };
}

/**
 * 鳴らしている間に、ほかのレートで文脈を作っても、エディタの文脈が閉じられないか。判定はプールに同じ文脈が残っているか
 * で行う（追い出した文脈の close は待たないので、直後の `state` はまだ 'closed' にならない。WebKit で確かめた）。
 */
export function contextCheck(player: EditorPlayer): unknown {
  const ctx = player.context as ({ sampleRate: number; state?: string } & ContextLike) | null;
  if (!ctx || !player.playing) return { playing: false };
  const pool = audioRuntime().pool;
  const rate = ctx.sampleRate;
  // 効果音（装置のレート）・試聴（ほかのレート）が文脈を作るまね
  for (const r of [22_050, 96_000, undefined]) {
    if (r === rate) continue;
    try {
      pool.get(r);
    } catch {
      // そのレートの文脈を作れない
    }
  }
  return {
    playing: true,
    rate,
    held: pool.isHeld(rate),
    state: ctx.state ?? null,
    ok: pool.peek(rate) === ctx,
  };
}

/** 音声エディタの自己点検を作る（`audioStore.ts` が開くたびに `attach` する）。 */
export function createAudioCheck(): AudioCheck {
  const trips = new RoundTrips();
  const fetches = { pcm: [] as number[], peaks: [] as number[] };
  let run: PlacedRecord[] = [];
  let seamChecks = 0;
  let benched = false;
  let parts: { player: EditorPlayer; controller: WaveController } | null = null;

  const flushRun = () => {
    const records = run;
    run = [];
    if (records.length < 2 || seamChecks >= MAX_SEAM_CHECKS) return;
    seamChecks++;
    void runEditorSeamCheck(records).then(
      (report) => {
        if (report) send('edit-seam', report);
      },
      (error: unknown) =>
        send('edit-seam', { error: error instanceof Error ? error.message : String(error) }),
    );
  };

  return {
    attach(p) {
      parts = p;
      benched = false;
      run = [];
      const w = window as unknown as Record<string, unknown>;
      w.__kuraWaveCheck = () => runWaveBench(p.controller).then((r) => send('edit-wave', r));
      w.__kuraContextCheck = () => send('edit-context', contextCheck(p.player));
    },
    sent(seq, steps) {
      if (steps.some(docStep)) trips.sent(seq, stepKind(steps), performance.now());
    },
    shown(seq, view) {
      const done = trips.shown(seq, performance.now());
      if (done) send('edit-roundtrip', done);
      // 長い音声を開いて段が届いたら 1 回、描画を測る（面の大きさが決まるのを待つ）
      if (
        !benched &&
        parts &&
        view.peaks.rev !== null &&
        view.frames / Math.max(1, view.rate) >= AUTO_BENCH_SECONDS
      ) {
        benched = true;
        const c = parts.controller;
        setTimeout(() => void runWaveBench(c).then((r) => send('edit-wave', r)), 2000);
      }
    },
    placed(record: PlacedRecord) {
      const last = run[run.length - 1];
      const contiguous = last !== undefined && record.abs === last.abs + (last.end - last.start);
      if (record.fadeIn || (last && !contiguous)) flushRun();
      if (!record.fadeIn) run.push(record);
      if (run.length >= RUN_SEGMENTS) flushRun();
    },
    crossfade(record: CrossfadeRecord) {
      flushRun();
      void runCrossfadeCheck(record).then(
        (report) => {
          if (report) send('edit-crossfade', report);
        },
        (error: unknown) =>
          send('edit-crossfade', { error: error instanceof Error ? error.message : String(error) }),
      );
    },
    fetched(url, ms) {
      const list = url.includes('/pcm/')
        ? fetches.pcm
        : url.includes('/peaks/')
          ? fetches.peaks
          : null;
      if (!list) return;
      list.push(ms);
      if (list.length >= BATCH) {
        send('edit-fetch', { kind: list === fetches.pcm ? 'pcm' : 'peaks', ...stats(list) });
        list.length = 0;
      }
    },
  };
}
