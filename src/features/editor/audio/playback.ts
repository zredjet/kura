/**
 * 音声エディタの編集中の試聴（M13 の段 6。設計 §2.5、利用者の決定：交差フェード・試聴だけのループ・聞き比べ）。
 *
 * - 文脈は `ContextPool` から鳴らす音のレートで取る（3〜384 kHz の外は鳴らさない）。鳴らしている間は `pool.hold(rate)`
 *   と `holdAudio()` で使用中の印を立て、効果音・試聴に文脈を閉じられないようにする。
 * - 計画（`planSegments`。純粋な関数）：鳴らす区間をチャンクの境で切った `[a, b)` の列。選択があれば選択の終わりで
 *   止め、ループを試聴するときは終わりのあとに始めから続ける。置く時刻はフレームで決める。
 * - 置き方：始めの時刻を `startFrame = ceil((ctx.currentTime + 0.05) × rate)` にし、区間ごとにその標本だけのバッファの
 *   `AudioBufferSourceNode`（自分の `GainNode` → 全体の音量）を `(startFrame + 累積フレーム) / rate` に置く（M11 の試聴で
 *   継ぎ目に食い違いがないと確かめた置き方と同じ）。`loop`・`loopStart`・`loopEnd` は使わない。先読みは 3 秒（ループの
 *   試聴は 1 秒）で、100 ms ごとに足す。チャンクがまだなければ取りに行き、間に合わなければ遅らせて置く（隙ができる）。
 * - 手のあと（`refresh`）：鳴らしている・置いた区間が変わった区間に掛かれば、その位置の新しい PCM が届いてから、
 *   T = 今 + 60 ms（フレームの境）で新しい音を 0→1、古い音を 1→0 の 10 ms の直線で替え、古い音は T + 10 ms に止める
 *   （置いてまだ鳴っていないものは捨てて置き直す）。替えるのは 150 ms に 1 回まで。
 * - 聞き比べ（`switchTo`）：レートが同じなら同じ交差フェードで替え、違えば止めて始め直す。
 * - ループの試聴の区間を替えたら（旗のドラッグ）、まだ鳴っていない区間から新しい区間で置き直す。
 */

import type { EditSpanDto, EditSpansChangeDto } from '../../../ipc/edit';
import { editInputPcmUrl, editPcmUrl } from '../../../ipc/media';
import { MAX_RATE, MIN_RATE, readPcmWav } from '../../../lib/asset/wav';
import type { ContextLike, ContextPool, GainLike, SourceLike } from '../../../lib/assetAudio';
import { CHUNK, chunkOf, chunkSpan, covered, type PcmChunk, PcmChunkCache } from './pcmChunks';
import { touches } from './peakTiles';
import type { FrameSpan } from './waveMath';

/** 文脈を始めてから最初の音までの余裕（秒）。 */
export const LEAD_SECONDS = 0.05;
/** 先読みの幅（秒）。 */
export const HORIZON_SECONDS = 3;
/** ループを試聴するときの先読みの幅（秒。旗を動かしたらすぐ替わるよう短く）。 */
export const LOOP_HORIZON_SECONDS = 1;
/** 先読みを足す間隔（ms）。 */
export const TICK_MS = 100;
/** 手のあと、新しい音に替えるまでの余裕（秒）。 */
export const SWITCH_AHEAD_SECONDS = 0.06;
/** 交差フェードの長さ（秒）。 */
export const CROSSFADE_SECONDS = 0.01;
/** 交差フェードで替える間隔の下限（ms）。 */
export const SWITCH_MIN_MS = 150;
/** 再生位置から先に頼むチャンクの数。 */
export const PREFETCH_CHUNKS = 2;

/** 鳴らす音：編集後（出力）か元（文書の入力）。 */
export type PlaySource = 'edited' | 'source';

/** 鳴らす音の形。 */
export interface SourceInfo {
  rate: number;
  channels: number;
  frames: number;
}

/** 計画の区間 1 つ（`[start, end)` は鳴らす音のフレーム、`at` は計画の頭からの累積フレーム）。 */
export interface Segment {
  start: number;
  end: number;
  at: number;
}

/** 計画の条件。 */
export interface PlanInput {
  /** 鳴らし始める位置。 */
  from: number;
  /** 鳴らす音の長さ。 */
  frames: number;
  /** ここで止める（選択の終わり）。null なら最後まで。 */
  end?: number | null;
  /** ループを試聴する区間（終わりのあとに始めから続ける）。 */
  loop?: FrameSpan | null;
  /** 計画する長さ（フレーム。累積がこれに届いたら打ち切る）。 */
  horizon: number;
  /** 区間を切る格子（既定はチャンク）。 */
  chunk?: number;
}

/** 正しいループの区間か（長さが 1 以上で、音の中）。 */
function validLoop(loop: FrameSpan | null | undefined, frames: number): FrameSpan | null {
  if (!loop) return null;
  const start = Math.max(0, Math.floor(loop.start));
  const end = Math.min(frames, Math.floor(loop.end));
  return end > start ? { start, end } : null;
}

/**
 * 鳴らす区間の計画（純粋な関数）：チャンクの境・止める位置・ループの終わりで切った区間の列。ループを試聴するときは、
 * 始めの位置がループの外ならループの始めから鳴らす。累積が `horizon` に届くか、止める位置に着いたら終わる。
 */
export function planSegments(input: PlanInput): Segment[] {
  const chunk = input.chunk ?? CHUNK;
  const frames = Math.max(0, Math.floor(input.frames));
  const loop = validLoop(input.loop, frames);
  const stop = loop
    ? loop.end
    : Math.min(frames, input.end === null || input.end === undefined ? frames : input.end);
  let pos = Math.max(0, Math.floor(input.from));
  if (loop && (pos < loop.start || pos >= loop.end)) pos = loop.start;
  const out: Segment[] = [];
  let at = 0;
  const horizon = Math.max(0, input.horizon);
  for (let guard = 0; at < horizon && guard < 100_000; guard++) {
    if (pos >= stop) {
      if (!loop) break;
      pos = loop.start;
    }
    const chunkEnd = (Math.floor(pos / chunk) + 1) * chunk;
    const end = Math.min(chunkEnd, stop);
    if (end <= pos) break;
    out.push({ start: pos, end, at });
    at += end - pos;
    pos = end;
  }
  return out;
}

/**
 * 先読みに要るチャンクの区間（チャンクの境にそろえた `[a, b)`）：鳴っている位置から `count` チャンクと、ループを試聴して
 * いればループの始めから `count` チャンク（ループの終わりまで）。
 */
export function neededSpans(
  from: number,
  frames: number,
  loop: FrameSpan | null,
  count = PREFETCH_CHUNKS,
): EditSpanDto[] {
  const at = (start: number, limit: number): EditSpanDto | null => {
    const a = chunkOf(start) * CHUNK;
    const b = Math.min(frames, a + count * CHUNK, Math.ceil(limit / CHUNK) * CHUNK);
    return b > a ? { start: a, end: b } : null;
  };
  const out: EditSpanDto[] = [];
  const first = at(from, frames);
  if (first) out.push(first);
  const l = validLoop(loop, frames);
  if (l) {
    const second = at(l.start, l.end);
    if (second && !(first && second.start >= first.start && second.end <= first.end)) {
      out.push(second);
    }
  }
  return out;
}

// ===== PCM を取る =====

/** 取る口（kura:// の本体）。 */
export type FetchPcm = (url: string) => Promise<ArrayBuffer>;

/** 出力の PCM の kura チャンクのデコーダの頭（Rust の `EDIT_DECODER_PREFIX`）。 */
const DECODER_PREFIX = 'kura-edit/1:';

/** 出力の表示のうち、PCM を取るのに要るもの。 */
export interface FeedView {
  rev: number;
  samplesId: string;
  valid: readonly EditSpanDto[];
  frames: number;
  channels: number;
  rate: number;
}

/**
 * 試聴の PCM のチャンクを取る（出力は表示の `valid` に覆われるものだけ。元はいつでも）。出力の表は波形の拡大と分け合い
 * （捨てるのは波形の面）、元の表はここで持つ（元は変わらない）。届いたら `onLoaded`。
 */
export class PcmFeed {
  private session: string | null = null;
  private view: FeedView | null = null;
  private input: SourceInfo | null = null;
  private readonly loading = new Set<string>();
  private readonly failed = new Set<string>();
  private epoch = 0;
  private disposed = false;
  readonly inputCache: PcmChunkCache;

  constructor(
    private readonly deps: {
      fetch: FetchPcm;
      output: PcmChunkCache;
      input?: PcmChunkCache;
      onLoaded?(): void;
    },
  ) {
    this.inputCache = deps.input ?? new PcmChunkCache();
  }

  setSession(session: string | null): void {
    this.session = session;
  }

  /** 元の形（開いたときの `source.audio`）。 */
  setInput(info: SourceInfo | null): void {
    this.input = info;
  }

  /** 表示の結果（出力が変わったら読んでいる途中のものは古い）。 */
  setView(view: FeedView, changed: EditSpansChangeDto | null): void {
    this.view = view;
    if (changed && (changed.type === 'all' || changed.spans.length > 0)) {
      this.epoch++;
      for (const k of [...this.loading]) if (k.startsWith('e')) this.loading.delete(k);
      for (const k of [...this.failed]) if (k.startsWith('e')) this.failed.delete(k);
    }
  }

  /** 手元のチャンク。 */
  peek(source: PlaySource, index: number): PcmChunk | undefined {
    return source === 'edited' ? this.deps.output.peek(index) : this.inputCache.peek(index);
  }

  /** チャンクを取りに行く（手元にある・読んでいる・取れない区間は何もしない）。 */
  want(source: PlaySource, index: number): void {
    if (this.disposed || !this.session || this.peek(source, index)) return;
    const key = `${source === 'edited' ? 'e' : 'i'}${index}`;
    if (this.loading.has(key)) return;
    if (source === 'edited') this.loadOutput(index, key);
    else this.loadInput(index, key);
  }

  private loadOutput(index: number, key: string): void {
    const view = this.view;
    const session = this.session;
    if (!view || !session) return;
    const span = chunkSpan(index, view.frames);
    if (!span || !covered(view.valid, span[0], span[1])) return;
    const failKey = `${key}@${view.rev}`;
    if (this.failed.has(failKey)) return;
    const [a, b] = span;
    const epoch = this.epoch;
    const decoder = DECODER_PREFIX + view.samplesId;
    this.loading.add(key);
    this.deps
      .fetch(editPcmUrl(session, view.rev, a, b))
      .then((buf) => {
        if (this.disposed || epoch !== this.epoch || !this.loading.has(key)) return;
        const pcm = readPcmWav(buf);
        if (!pcm || pcm.info.start !== a || pcm.frames !== b - a || pcm.info.decoder !== decoder) {
          this.failed.add(failKey);
          return;
        }
        this.deps.output.put(index, { rev: view.rev, start: a, frames: b - a, planes: pcm.planes });
        this.deps.onLoaded?.();
      })
      .catch(() => {
        if (epoch === this.epoch) this.failed.add(failKey);
      })
      .finally(() => {
        if (epoch === this.epoch) this.loading.delete(key);
      });
  }

  private loadInput(index: number, key: string): void {
    const info = this.input;
    const session = this.session;
    if (!info || !session) return;
    const span = chunkSpan(index, info.frames);
    if (!span || this.failed.has(key)) return;
    const [a, b] = span;
    this.loading.add(key);
    this.deps
      .fetch(editInputPcmUrl(session, a, b))
      .then((buf) => {
        if (this.disposed) return;
        const pcm = readPcmWav(buf);
        if (!pcm || pcm.info.start !== a || pcm.frames !== b - a || pcm.rate !== info.rate) {
          this.failed.add(key);
          return;
        }
        this.inputCache.put(index, { rev: 0, start: a, frames: b - a, planes: pcm.planes });
        this.deps.onLoaded?.();
      })
      .catch(() => this.failed.add(key))
      .finally(() => this.loading.delete(key));
  }

  dispose(): void {
    this.disposed = true;
    this.loading.clear();
  }
}

// ===== 鳴らす =====

/** 先読みを足す時計。 */
export interface PlayerTimer {
  every(cb: () => void, ms: number): number;
  clear(id: number): void;
}

const realTimer: PlayerTimer = {
  every: (cb, ms) => window.setInterval(cb, ms),
  clear: (id) => window.clearInterval(id),
};

export interface PlayerDeps {
  pool: Pick<ContextPool, 'get' | 'hold'>;
  /** 手元のチャンク（なければ undefined）。 */
  peek(source: PlaySource, index: number): PcmChunk | undefined;
  /** チャンクを取りに行く。 */
  want(source: PlaySource, index: number): void;
  /** 先読みに要る区間（出力ならビューポートの `spans` の先頭に入れる）。 */
  need?(source: PlaySource, spans: EditSpanDto[]): void;
  /** 音量（0〜1）。 */
  volume(): number;
  /** 文脈を使っている印（`holdAudio`）。戻り値で外す。 */
  hold?(): () => void;
  timer?: PlayerTimer;
  /** 時計（ms。交差フェードの間隔）。 */
  now?(): number;
  /** 終わりまで鳴った（選択の終わり・音の終わり）。`pos` は止まった位置。 */
  onEnded?(pos: number): void;
}

/** 鳴らす頼み。 */
export interface PlayRequest {
  from: number;
  /** ここで止める（選択の終わり）。 */
  end?: number | null;
  /** ループを試聴する区間。 */
  loop?: FrameSpan | null;
  source: PlaySource;
  info: SourceInfo;
}

export type PlayOutcome = 'playing' | 'rateUnsupported' | 'failed';

/** 置いた区間 1 つ。 */
interface Placed {
  start: number;
  end: number;
  /** 文脈のフレームで鳴り始める位置。 */
  abs: number;
  src: SourceLike;
  gain: GainLike;
  /** 交差フェードで止めた（`stopAt` のあとは位置の計算に使わない）。 */
  fading: boolean;
  /** 交差フェードで止める文脈のフレーム。 */
  stopAt: number | null;
}

/** 鳴らしている計画。 */
interface Plan {
  source: PlaySource;
  info: SourceInfo;
  end: number | null;
  loop: FrameSpan | null;
  /** 次に置く位置（鳴らす音のフレーム）と、文脈のフレーム。 */
  nextPos: number;
  nextAbs: number;
  /** 止める位置に着いた（置き終えた）。 */
  done: boolean;
  /** 次に置く区間を 0→1 で入れる（交差フェード）。 */
  fadeInNext: boolean;
}

/** 編集中の試聴（一度に 1 つ）。 */
export class EditorPlayer {
  private ctx: ContextLike | null = null;
  private master: GainLike | null = null;
  private plan: Plan | null = null;
  private placed: Placed[] = [];
  private timerId: number | null = null;
  private releases: (() => void)[] = [];
  private refreshWanted = false;
  private lastSwitch = Number.NEGATIVE_INFINITY;
  private readonly timer: PlayerTimer;
  private readonly now: () => number;

  constructor(private readonly deps: PlayerDeps) {
    this.timer = deps.timer ?? realTimer;
    this.now = deps.now ?? (() => performance.now());
  }

  /** 鳴らしているか。 */
  get playing(): boolean {
    return this.plan !== null;
  }

  /** 鳴らしている音（していなければ null）。 */
  get source(): PlaySource | null {
    return this.plan?.source ?? null;
  }

  /** ループを試聴しているか。 */
  get looping(): boolean {
    return this.plan?.loop != null;
  }

  /** 鳴らしている文脈（テスト・自己点検）。 */
  get context(): ContextLike | null {
    return this.ctx;
  }

  /**
   * 鳴らし始める（キー・クリックの処理の中から呼ぶ：文脈の resume を先に）。前のものは止める。レートが 3〜384 kHz の外
   * なら鳴らさない。
   */
  play(req: PlayRequest): PlayOutcome {
    this.stop();
    const rate = req.info.rate;
    if (!Number.isInteger(rate) || rate < MIN_RATE || rate > MAX_RATE) return 'rateUnsupported';
    let ctx: ContextLike;
    try {
      ctx = this.deps.pool.get(rate);
    } catch {
      return 'rateUnsupported';
    }
    if (ctx.sampleRate !== rate) return 'rateUnsupported';
    ctx.resume().catch(() => undefined);
    this.releases.push(this.deps.pool.hold(rate));
    const held = this.deps.hold?.();
    if (held) this.releases.push(held);
    this.ctx = ctx;
    const master = ctx.createGain();
    master.gain.value = this.deps.volume();
    master.connect(ctx.destination);
    this.master = master;
    this.plan = {
      source: req.source,
      info: req.info,
      end: req.end ?? null,
      loop: validLoop(req.loop, req.info.frames),
      nextPos: Math.max(0, Math.min(req.info.frames, Math.floor(req.from))),
      nextAbs: this.leadFrame(),
      done: false,
      fadeInNext: false,
    };
    this.refreshWanted = false;
    this.timerId = this.timer.every(() => this.tick(), TICK_MS);
    this.tick();
    return 'playing';
  }

  /** 止める（鳴らしていなければ何もしない）。止まった位置を返す。 */
  stop(): number | null {
    const pos = this.position();
    if (this.timerId !== null) this.timer.clear(this.timerId);
    this.timerId = null;
    for (const p of this.placed) this.kill(p);
    this.placed = [];
    this.master?.disconnect();
    this.master = null;
    this.plan = null;
    this.ctx = null;
    this.refreshWanted = false;
    const releases = this.releases;
    this.releases = [];
    for (const r of releases) r();
    return pos;
  }

  /** 鳴っている位置（鳴らしている音のフレーム。鳴らしていなければ null）。 */
  position(): number | null {
    const ctx = this.ctx;
    const plan = this.plan;
    if (!ctx || !plan) return null;
    return this.positionAt(ctx.currentTime * plan.info.rate);
  }

  /** 文脈のフレーム `f` で鳴っている位置。 */
  private positionAt(f: number): number | null {
    const plan = this.plan;
    if (!plan) return null;
    const live = this.placed.filter((p) => !p.fading);
    let after: Placed | null = null;
    for (const p of live) {
      const len = p.end - p.start;
      if (f >= p.abs && f < p.abs + len) return p.start + Math.floor(f - p.abs);
      if (p.abs > f && (after === null || p.abs < after.abs)) after = p;
    }
    // 交差フェードで替える前（T より前）は、止める前の音の位置
    for (const p of this.placed) {
      if (!p.fading) continue;
      const end = Math.min(p.abs + (p.end - p.start), p.stopAt ?? Number.POSITIVE_INFINITY);
      if (f >= p.abs && f < end) return p.start + Math.floor(f - p.abs);
    }
    if (after) return after.start;
    const last = live[live.length - 1];
    if (last) return last.end;
    return plan.nextPos;
  }

  /** ループの試聴の区間を替える（まだ鳴っていない区間から新しい区間で置き直す）。 */
  setLoop(loop: FrameSpan | null): void {
    const plan = this.plan;
    if (!plan || !plan.loop) return;
    const next = validLoop(loop, plan.info.frames);
    if (!next) return;
    if (next.start === plan.loop.start && next.end === plan.loop.end) return;
    plan.loop = next;
    this.dropUnstarted();
    this.tick();
  }

  /** 出力が変わった（表示の結果の `changed`）。鳴らしている・置いた区間に掛かれば、新しい音に替える。 */
  refresh(changed: EditSpansChangeDto): void {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx || plan.source !== 'edited') return;
    if (changed.type === 'spans' && changed.spans.length === 0) return;
    const nowFrame = ctx.currentTime * plan.info.rate;
    const hit = this.placed.some(
      (p) => !p.fading && p.abs + (p.end - p.start) > nowFrame && touches(changed, p.start, p.end),
    );
    if (hit || changed.type === 'all') {
      this.refreshWanted = true;
      this.tick();
    }
  }

  /**
   * 聞き比べ：鳴らす音を替える（`from` は替えた音の位置）。レートが同じなら交差フェードで、違えば止めて始め直す。
   */
  switchTo(req: PlayRequest): PlayOutcome {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx || plan.info.rate !== req.info.rate) {
      return this.play(req);
    }
    this.crossfade({
      source: req.source,
      info: req.info,
      end: req.end ?? null,
      loop: validLoop(req.loop, req.info.frames),
      nextPos: Math.max(0, Math.min(req.info.frames, Math.floor(req.from))),
    });
    return 'playing';
  }

  /** 先読みを足す（100 ms ごと・取ったチャンクが届いたとき）。 */
  tick(): void {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx) return;
    const rate = plan.info.rate;
    const nowFrame = Math.ceil(ctx.currentTime * rate);
    // 要るチャンクを頼む（鳴っている位置から）
    const pos = this.positionAt(ctx.currentTime * rate) ?? plan.nextPos;
    const spans = neededSpans(pos, plan.info.frames, plan.loop);
    this.deps.need?.(plan.source, spans);
    for (const s of spans) {
      for (let i = chunkOf(s.start); i * CHUNK < s.end; i++) this.deps.want(plan.source, i);
    }
    // 手のあと：替える位置のチャンクが届いていて、前に替えてから 150 ms たったら交差フェード
    if (this.refreshWanted && this.now() - this.lastSwitch >= SWITCH_MIN_MS) {
      const t = this.switchFrame();
      const at = this.positionAt(t);
      if (at !== null && at < plan.info.frames && this.deps.peek(plan.source, chunkOf(at))) {
        this.refreshWanted = false;
        this.crossfade({
          source: plan.source,
          info: plan.info,
          end: plan.end,
          loop: plan.loop,
          nextPos: at,
        });
        return;
      }
    }
    this.fill(nowFrame);
    // 置き終えて、最後の区間が鳴り終えたら止める
    if (plan.done) {
      const last = this.placed.filter((p) => !p.fading).at(-1);
      const endAbs = last ? last.abs + (last.end - last.start) : plan.nextAbs;
      if (nowFrame >= endAbs) {
        const at = plan.nextPos;
        this.stop();
        this.deps.onEnded?.(at);
        return;
      }
    }
    this.prune(nowFrame);
  }

  /** 先読みの幅まで区間を置く。 */
  private fill(nowFrame: number): void {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx || plan.done) return;
    const rate = plan.info.rate;
    const horizon = Math.round((plan.loop ? LOOP_HORIZON_SECONDS : HORIZON_SECONDS) * rate);
    const until = nowFrame + horizon;
    while (plan.nextAbs < until) {
      const segs = planSegments({
        from: plan.nextPos,
        frames: plan.info.frames,
        end: plan.end,
        loop: plan.loop,
        horizon: 1,
      });
      const seg = segs[0];
      if (!seg) {
        plan.done = true;
        return;
      }
      const chunk = this.deps.peek(plan.source, chunkOf(seg.start));
      if (!chunk || chunk.planes.length !== plan.info.channels) {
        this.deps.want(plan.source, chunkOf(seg.start));
        return;
      }
      // 間に合わなかった（時刻が過ぎた）なら、今から少し先へ遅らせて置く
      const earliest = this.leadFrame();
      if (plan.nextAbs < earliest - Math.round(LEAD_SECONDS * rate)) plan.nextAbs = earliest;
      this.place(seg, chunk, plan.nextAbs, plan.fadeInNext);
      plan.fadeInNext = false;
      plan.nextAbs += seg.end - seg.start;
      plan.nextPos = seg.end;
    }
  }

  /** 区間を置く（その標本だけのバッファ）。`fadeIn` なら 0→1 の 10 ms で入れる。 */
  private place(seg: Segment, chunk: PcmChunk, abs: number, fadeIn: boolean): void {
    const ctx = this.ctx;
    const plan = this.plan;
    const master = this.master;
    if (!ctx || !plan || !master) return;
    const rate = plan.info.rate;
    const len = seg.end - seg.start;
    const buffer = ctx.createBuffer(chunk.planes.length, len, rate);
    chunk.planes.forEach((plane, ch) =>
      buffer.copyToChannel(plane.subarray(seg.start - chunk.start, seg.end - chunk.start), ch),
    );
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    const gain = ctx.createGain();
    const when = abs / rate;
    if (fadeIn) {
      gain.gain.setValueAtTime(0, when);
      gain.gain.linearRampToValueAtTime(1, when + CROSSFADE_SECONDS);
    } else {
      gain.gain.value = 1;
    }
    src.connect(gain);
    gain.connect(master);
    src.start(when);
    this.placed.push({
      start: seg.start,
      end: seg.end,
      abs,
      src,
      gain,
      fading: false,
      stopAt: null,
    });
  }

  /** 交差フェードで替える位置（今 + 60 ms をフレームの境に）。 */
  private switchFrame(): number {
    const ctx = this.ctx;
    const plan = this.plan;
    if (!ctx || !plan) return 0;
    return Math.ceil((ctx.currentTime + SWITCH_AHEAD_SECONDS) * plan.info.rate);
  }

  /** 文脈の今の時刻の少し先のフレーム。 */
  private leadFrame(): number {
    const ctx = this.ctx;
    const plan = this.plan;
    const rate = plan?.info.rate ?? ctx?.sampleRate ?? 48_000;
    return Math.ceil(((ctx?.currentTime ?? 0) + LEAD_SECONDS) * rate);
  }

  /**
   * T = 今 + 60 ms で、置いた音を 1→0、新しい計画の音を 0→1 の 10 ms で替える（T より後に置いたものは捨てる）。
   */
  private crossfade(next: Omit<Plan, 'nextAbs' | 'done' | 'fadeInNext'>): void {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx) return;
    const t = this.switchFrame();
    const when = t / plan.info.rate;
    const keep: Placed[] = [];
    for (const p of this.placed) {
      if (p.fading) {
        keep.push(p);
        continue;
      }
      const endAbs = p.abs + (p.end - p.start);
      if (p.abs >= t) {
        this.kill(p);
      } else if (endAbs > t) {
        p.gain.gain.setValueAtTime(1, when);
        p.gain.gain.linearRampToValueAtTime(0, when + CROSSFADE_SECONDS);
        try {
          p.src.stop(when + CROSSFADE_SECONDS);
        } catch {
          // もう止まっている
        }
        p.fading = true;
        p.stopAt = t;
        keep.push(p);
      } else {
        keep.push(p);
      }
    }
    this.placed = keep;
    this.lastSwitch = this.now();
    Object.assign(plan, next, { nextAbs: t, done: false, fadeInNext: true });
    this.fill(Math.ceil(ctx.currentTime * plan.info.rate));
  }

  /** まだ鳴っていない区間を捨て、そこから置き直す。 */
  private dropUnstarted(): void {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx) return;
    const nowFrame =
      Math.ceil(ctx.currentTime * plan.info.rate) + Math.round(0.01 * plan.info.rate);
    const keep: Placed[] = [];
    let first: Placed | null = null;
    for (const p of this.placed) {
      if (!p.fading && p.abs > nowFrame) {
        if (first === null || p.abs < first.abs) first = p;
        this.kill(p);
      } else {
        keep.push(p);
      }
    }
    this.placed = keep;
    if (first) {
      plan.nextAbs = first.abs;
      plan.nextPos = first.start;
      plan.done = false;
    } else {
      const last = keep.filter((p) => !p.fading).at(-1);
      if (last) plan.done = false;
    }
  }

  /** 鳴り終えた区間を外す（最後の 1 つは位置のために残す）。 */
  private prune(nowFrame: number): void {
    if (this.placed.length <= 1) return;
    const keep: Placed[] = [];
    const lastLive = this.placed.filter((p) => !p.fading).at(-1);
    for (const p of this.placed) {
      const endAbs = p.abs + (p.end - p.start);
      if (
        endAbs + Math.round(CROSSFADE_SECONDS * 4 * (this.plan?.info.rate ?? 48_000)) < nowFrame &&
        p !== lastLive
      ) {
        p.src.disconnect();
        p.gain.disconnect();
      } else {
        keep.push(p);
      }
    }
    this.placed = keep;
  }

  private kill(p: Placed): void {
    p.src.onended = null;
    try {
      p.src.stop();
    } catch {
      // 始まる前に止めた
    }
    p.src.disconnect();
    p.gain.disconnect();
  }

  dispose(): void {
    this.stop();
  }
}
