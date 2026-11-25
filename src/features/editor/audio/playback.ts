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
 * - 聞き比べ（`switchTo`）：レートが同じなら、替える時刻 T の位置を写して同じ交差フェードで替える（T のチャンクが
 *   届くまでは古い音を鳴らし続ける）。違えば止めて始め直す。
 * - ループの試聴の区間を替えたら（旗のドラッグ）、まだ鳴っていない区間から新しい区間で置き直す。
 */

import type { EditSpanDto, EditSpansChangeDto } from '../../../ipc/edit';
import { editAuditionPcmUrl, editInputPcmUrl, editPcmUrl } from '../../../ipc/media';
import { MAX_RATE, MIN_RATE, readPcmWav } from '../../../lib/asset/wav';
import type {
  AudioParamLike,
  BiquadLike,
  ContextLike,
  ContextPool,
  GainLike,
  SourceLike,
} from '../../../lib/assetAudio';
import {
  CHUNK,
  chunkOf,
  chunkSpan,
  covered,
  FailureMarks,
  type PcmChunk,
  PcmChunkCache,
} from './pcmChunks';
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
/** これより短いループは、1 周ずつではなく、つないだバッファ（`LOOP_BLOCK_FRAMES` 程度）を 1 区間として置く。 */
export const SHORT_LOOP_FRAMES = CHUNK / 8;
/** 短いループをつなぐバッファの長さの目安（フレーム）。 */
export const LOOP_BLOCK_FRAMES = CHUNK;
/** 1 回の足し（`fill`）で置く区間の数の上限（ノードを一度に作りすぎない。足りない分は次の tick で）。 */
export const MAX_PLACE_PER_FILL = 32;
/** 待っているチャンクがこの回数続けて取れなければ、止めて知らせる。 */
export const MAX_CHUNK_FAILURES = 3;

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
  /**
   * 鳴らす音の id（M17 段 6。重ねがなければ `samplesId`。違えば、編集後の音は試聴の PCM を `audition.valid` の区間で取る）。
   * 省けば `samplesId`。
   */
  auditionId?: string;
  audition?: { valid: readonly EditSpanDto[] } | null;
}

/** 鳴らす音が試聴の値か（`auditionId` が `samplesId` と違う）。 */
export function playsAudition(view: FeedView): boolean {
  return view.auditionId !== undefined && view.auditionId !== view.samplesId;
}

/**
 * 表示の結果が替わったとき、鳴らしている編集後の音のどこが替わったか（M17 段 6）。鳴らす音が前も今も表示の音なら表示の
 * `changed`、どちらかが試聴の値で id が替われば全体、同じ試聴の値なら替わらない（null）。
 */
export function playbackChange(
  prev: FeedView | null,
  view: FeedView & { changed: EditSpansChangeDto },
): EditSpansChangeDto | null {
  if (!prev) return view.changed;
  const before = playsAudition(prev);
  const now = playsAudition(view);
  if (!before && !now) return view.changed;
  const prevId = prev.auditionId ?? prev.samplesId;
  const nowId = view.auditionId ?? view.samplesId;
  return prevId === nowId ? null : { type: 'all' };
}

/**
 * 試聴の PCM のチャンクを取る（出力は表示の `valid` に覆われるものだけ。元はいつでも）。出力の表は波形の拡大と分け合い
 * （捨てるのは波形の面）、元の表はここで持つ（元は変わらない）。届いたら `onLoaded`。取れなかったチャンクは印を付け、
 * 期限（1 秒から倍々で 8 秒まで。`FailureMarks`）が過ぎたら取り直す（503 の Busy のような一時的な失敗があるため）。
 * 取れなかったら `onFailed` に続けて失敗した回数を渡す。
 */
export class PcmFeed {
  private session: string | null = null;
  private view: FeedView | null = null;
  private input: SourceInfo | null = null;
  private readonly loading = new Set<string>();
  private readonly failures: FailureMarks;
  private epoch = 0;
  /** 試聴の値の表の世代（鳴らす音の id が替わったら進める）。 */
  private auditionEpoch = 0;
  private disposed = false;
  readonly inputCache: PcmChunkCache;
  /** 試聴の値の表（M17 段 6。波形の拡大とは分ける：波形は文書の値のまま）。 */
  readonly auditionCache: PcmChunkCache;

  constructor(
    private readonly deps: {
      fetch: FetchPcm;
      output: PcmChunkCache;
      input?: PcmChunkCache;
      onLoaded?(): void;
      /** 取れなかった（`count` は続けて失敗した回数）。 */
      onFailed?(source: PlaySource, index: number, count: number): void;
      /** 時計（ms。取り直すまでの期限）。 */
      now?(): number;
    },
  ) {
    this.inputCache = deps.input ?? new PcmChunkCache();
    this.auditionCache = new PcmChunkCache();
    this.failures = new FailureMarks(deps.now);
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
    const prevAudition = this.view && playsAudition(this.view) ? this.view.auditionId : null;
    this.view = view;
    if (changed && (changed.type === 'all' || changed.spans.length > 0)) {
      this.epoch++;
      for (const k of [...this.loading]) if (k.startsWith('e')) this.loading.delete(k);
      this.failures.clear('e');
    }
    // 鳴らす試聴の値が替わった（外す・やめる・外した音が変わった）：試聴の表を空にし、読んでいる途中のものを捨てる
    const audition = playsAudition(view) ? view.auditionId : null;
    if (audition !== prevAudition) {
      this.auditionEpoch++;
      this.auditionCache.clear();
      for (const k of [...this.loading]) if (k.startsWith('a')) this.loading.delete(k);
      this.failures.clear('a');
    }
  }

  /** 手元のチャンク（編集後の音は、外して聞いている間は試聴の値）。 */
  peek(source: PlaySource, index: number): PcmChunk | undefined {
    if (source === 'source') return this.inputCache.peek(index);
    return this.view && playsAudition(this.view)
      ? this.auditionCache.peek(index)
      : this.deps.output.peek(index);
  }

  /** チャンクを取りに行く（手元にある・読んでいる・取れない区間・取り直す期限の前は何もしない）。 */
  want(source: PlaySource, index: number): void {
    if (this.disposed || !this.session || this.peek(source, index)) return;
    const audition = source === 'edited' && !!this.view && playsAudition(this.view);
    const key = `${source === 'source' ? 'i' : audition ? 'a' : 'e'}${index}`;
    if (this.loading.has(key)) return;
    if (audition) this.loadAudition(index, key);
    else if (source === 'edited') this.loadOutput(index, key);
    else this.loadInput(index, key);
  }

  /** 試聴の値のチャンク（`/edit/{s}/audition/{auditionId}/pcm`。`audition.valid` に覆われるものだけ）。 */
  private loadAudition(index: number, key: string): void {
    const view = this.view;
    const session = this.session;
    const id = view?.auditionId;
    if (!view || !session || !id) return;
    const span = chunkSpan(index, view.frames);
    if (!span || !covered(view.audition?.valid ?? [], span[0], span[1])) return;
    const failKey = `${key}@${id}`;
    if (this.failures.blocked(failKey)) return;
    const [a, b] = span;
    const epoch = this.auditionEpoch;
    const decoder = DECODER_PREFIX + id;
    this.loading.add(key);
    this.deps
      .fetch(editAuditionPcmUrl(session, id, a, b))
      .then((buf) => {
        if (this.disposed || epoch !== this.auditionEpoch || !this.loading.has(key)) return;
        const pcm = readPcmWav(buf);
        if (!pcm || pcm.info.start !== a || pcm.frames !== b - a || pcm.info.decoder !== decoder) {
          this.failed(failKey, 'edited', index);
          return;
        }
        this.failures.ok(failKey);
        this.auditionCache.put(index, {
          rev: view.rev,
          start: a,
          frames: b - a,
          planes: pcm.planes,
        });
        this.deps.onLoaded?.();
      })
      .catch(() => {
        if (!this.disposed && epoch === this.auditionEpoch) this.failed(failKey, 'edited', index);
      })
      .finally(() => {
        if (epoch === this.auditionEpoch) this.loading.delete(key);
      });
  }

  /** 取れなかった印を付けて知らせる。 */
  private failed(failKey: string, source: PlaySource, index: number): void {
    const count = this.failures.fail(failKey);
    this.deps.onFailed?.(source, index, count);
  }

  private loadOutput(index: number, key: string): void {
    const view = this.view;
    const session = this.session;
    if (!view || !session) return;
    const span = chunkSpan(index, view.frames);
    if (!span || !covered(view.valid, span[0], span[1])) return;
    const failKey = `${key}@${view.rev}`;
    if (this.failures.blocked(failKey)) return;
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
          this.failed(failKey, 'edited', index);
          return;
        }
        this.failures.ok(failKey);
        this.deps.output.put(index, { rev: view.rev, start: a, frames: b - a, planes: pcm.planes });
        this.deps.onLoaded?.();
      })
      .catch(() => {
        if (!this.disposed && epoch === this.epoch) this.failed(failKey, 'edited', index);
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
    if (!span || this.failures.blocked(key)) return;
    const [a, b] = span;
    this.loading.add(key);
    this.deps
      .fetch(editInputPcmUrl(session, a, b))
      .then((buf) => {
        if (this.disposed) return;
        const pcm = readPcmWav(buf);
        if (!pcm || pcm.info.start !== a || pcm.frames !== b - a || pcm.rate !== info.rate) {
          this.failed(key, 'source', index);
          return;
        }
        this.failures.ok(key);
        this.inputCache.put(index, { rev: 0, start: a, frames: b - a, planes: pcm.planes });
        this.deps.onLoaded?.();
      })
      .catch(() => {
        if (!this.disposed) this.failed(key, 'source', index);
      })
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
  /**
   * 音ごとの利得（倍率。M17 段 6 の音量を合わせて聞く：大きいほうを下げる。省けば 1）。値が替わったら `relevel` を呼ぶ。
   */
  level?(source: PlaySource): number;
  /** 文脈を使っている印（`holdAudio`）。戻り値で外す。 */
  hold?(): () => void;
  timer?: PlayerTimer;
  /** 時計（ms。交差フェードの間隔）。 */
  now?(): number;
  /** 終わりまで鳴った（選択の終わり・音の終わり）。`pos` は止まった位置（鳴らしていた音 `source` のフレーム）。 */
  onEnded?(pos: number, source: PlaySource): void;
  /** 区間を置いた（継ぎ目の自己点検 `VITE_KURA_CHECK` が、同じ標本と同じ置き方で照らす）。 */
  onPlaced?(record: PlacedRecord): void;
  /** 交差フェードで替えた（自己点検が、替える前後の標本の跳びを照らす）。 */
  onCrossfade?(record: CrossfadeRecord): void;
  /**
   * 待っていた聞き比べの替えをやめた（今の音を鳴らし続ける）。`unmappable` は替える時刻の位置を写せなくなった、
   * `unavailable` は替える先のチャンクを取れなかった。
   */
  onSwitchFailed?(reason: 'unmappable' | 'unavailable'): void;
  /** 待っているチャンクを取れないので止めた。 */
  onFailed?(): void;
}

/** 置いた区間の記録（自己点検）。 */
export interface PlacedRecord {
  source: PlaySource;
  rate: number;
  /** 文脈のフレームで鳴り始める位置。 */
  abs: number;
  /** 鳴らす音のフレーム `[start, end)`。短いループをつないだバッファでは `end − start` はバッファの長さ。 */
  start: number;
  end: number;
  /** 1 周の長さ（短いループをつないだバッファ。ほかは `end − start`）。 */
  cycle: number;
  /** バッファに写した標本（チャンネルごと。チャンクの切り出し、短いループはつないだもの）。 */
  planes: Float32Array<ArrayBuffer>[];
  fadeIn: boolean;
  /** 区間の利得（倍率。音量を合わせて聞くとき 1 より小さい。M17 段 6）。 */
  level: number;
}

/** 交差フェードの記録（自己点検）：替える位置 T（文脈のフレーム）と、止めた音・入れた音。 */
export interface CrossfadeRecord {
  rate: number;
  t: number;
  old: PlacedRecord[];
  fresh: PlacedRecord | null;
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

/** 聞き比べで替える先（位置は、替える時刻 T に鳴っている位置を `map` で写して決める）。 */
export interface SwitchRequest {
  source: PlaySource;
  info: SourceInfo;
  /** ここで止める（選択の終わり。替える先のフレーム）。 */
  end?: number | null;
  /** ループを試聴する区間（替える先のフレーム）。 */
  loop?: FrameSpan | null;
  /** 今鳴らしている音の位置を、替える先の位置にする（写せなければ null）。替える時刻に呼ぶ。 */
  map(pos: number): number | null;
}

/** 聞き比べの替えの結果（`unmappable` は今の位置を写せないので替えなかった）。 */
export type SwitchOutcome = PlayOutcome | 'unmappable';

/** 置いた区間 1 つ。 */
interface Placed {
  /** 鳴らす音のフレーム（1 周分。短いループをつないだバッファは、この区間を繰り返す）。 */
  start: number;
  end: number;
  /** 文脈のフレームで鳴り始める位置。 */
  abs: number;
  /** 鳴らす長さ（フレーム。つないだバッファは 1 周の長さ × 回数。途中で止めたら縮める）。 */
  len: number;
  src: SourceLike;
  gain: GainLike;
  /** 交差フェードで止めた（`stopAt` のあとは位置の計算に使わない）。 */
  fading: boolean;
  /** 交差フェードで止める文脈のフレーム。 */
  stopAt: number | null;
  /** 記録（自己点検）。 */
  record: PlacedRecord;
  /** 置いたときの利得（交差フェードで止めるとき、この値から 0 へ）。 */
  level: number;
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

/** 置いた区間が文脈のフレーム `f` で鳴らしている位置（つないだバッファは 1 周の中へ折り返す）。 */
function placedPos(p: Placed, f: number): number {
  const cycle = Math.max(1, p.end - p.start);
  return p.start + (Math.floor(f - p.abs) % cycle);
}

/**
 * ループを `loop` に替えたとき、文脈のフレーム `now` に鳴っている区間 `p` をどこで止めるか（止める文脈のフレームと、
 * そこから続ける鳴らす音の位置。止めなくてよければ null）：もう新しい終わりを過ぎていれば今止めて始めから、この周の中で
 * 新しい終わりに着くならそこで止めて始めから、ほかは周の終わり（つないだバッファの 1 周の境）で止めて周の終わりから。
 */
export function loopCut(
  p: { start: number; end: number; abs: number; len: number },
  now: number,
  loop: FrameSpan,
): { abs: number; pos: number } | null {
  const cycle = Math.max(1, p.end - p.start);
  const cycleAbs = p.abs + Math.floor((now - p.abs) / cycle) * cycle;
  const pos = p.start + (now - cycleAbs);
  let cut: { abs: number; pos: number };
  if (pos >= loop.end) cut = { abs: now, pos: loop.start };
  else if (loop.end < p.end) cut = { abs: cycleAbs + (loop.end - p.start), pos: loop.start };
  else cut = { abs: cycleAbs + cycle, pos: p.end };
  return cut.abs < p.abs + p.len ? cut : null;
}

/** ループの 1 周の標本を `reps` 回つなぐ（チャンネルごと）。 */
function repeatPlanes(planes: readonly Float32Array[], reps: number): Float32Array<ArrayBuffer>[] {
  return planes.map((one) => {
    const out = new Float32Array(one.length * reps);
    out.set(one, 0);
    // 倍々に写す
    for (let filled = one.length; filled < out.length; filled *= 2) {
      out.copyWithin(filled, 0, Math.min(filled, out.length - filled));
    }
    return out;
  });
}

/** ソロで鳴らす帯（バンドパスの中心の周波数と Q。M17 段 6）。 */
export interface SoloBand {
  freq: number;
  q: number;
}

/** ソロの中心の周波数（文脈のナイキストの 0.49 倍まで）。 */
function soloFreq(freq: number, rate: number): number {
  return Math.max(10, Math.min(freq, rate * 0.49));
}

/** 編集中の試聴（一度に 1 つ）。 */
export class EditorPlayer {
  private ctx: ContextLike | null = null;
  private master: GainLike | null = null;
  /** ソロ（M17 段 6）：鳴らしている音をその帯だけにする（null なら全体）。 */
  private solo: SoloBand | null = null;
  /**
   * 続けて置く区間の利得（M17 段 6）。替えを待っている間（`refreshWanted`）は前の利得のまま置き、替える時刻 T からの交差
   * フェードで新しい利得にする（区間の境で利得が跳ばない）。
   */
  private placingLevel: number | null = null;
  /** ソロの道（全体の音量のあと：素通しとバンドパスを 10 ms で替える。初めてソロにしたときに作る）。 */
  private soloNodes: { dry: GainLike; filter: BiquadLike; wet: GainLike } | null = null;
  private plan: Plan | null = null;
  private placed: Placed[] = [];
  private timerId: number | null = null;
  private releases: (() => void)[] = [];
  private refreshWanted = false;
  /** 替える先のチャンクを待っている聞き比べ。 */
  private pendingSwitch: SwitchRequest | null = null;
  /** 置くのに待っているチャンク・聞き比べで替える先の待っているチャンク（取れなければ止める・替えをやめる）。 */
  private waiting: { source: PlaySource; index: number } | null = null;
  private switchWaiting: { source: PlaySource; index: number } | null = null;
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

  /**
   * ソロ（M17 段 6）：鳴らしている音（編集後でも元でも）を帯 `band` だけにする（null で全体に戻す）。替えは 10 ms のランプで、
   * 帯を動かしても付いて行く。鳴らしていなければ覚えておき、鳴らし始めたら当てる（鳴らし始めはしない）。
   */
  setSolo(band: SoloBand | null): void {
    const now = this.solo;
    if (band === null ? now === null : now !== null && now.freq === band.freq && now.q === band.q) {
      return;
    }
    this.solo = band ? { freq: band.freq, q: band.q } : null;
    this.applySolo();
  }

  /** ソロの帯（なければ null。テスト）。 */
  get soloing(): SoloBand | null {
    return this.solo;
  }

  private applySolo(): void {
    const ctx = this.ctx;
    const master = this.master;
    if (!ctx || !master) return;
    const band = this.solo;
    if (!band && !this.soloNodes) return;
    const nodes = this.soloNodes ?? this.buildSolo(ctx, master, band);
    const t = ctx.currentTime;
    const end = t + CROSSFADE_SECONDS;
    // 前の替えの途中でも、予定を消して今の値から替える（古い目標へ寄らない）
    const ramp = (p: AudioParamLike, v: number) => {
      p.cancelScheduledValues(t);
      p.setValueAtTime(p.value, t);
      p.linearRampToValueAtTime(v, end);
    };
    if (band) {
      ramp(nodes.filter.frequency, soloFreq(band.freq, ctx.sampleRate));
      ramp(nodes.filter.Q, band.q);
    }
    ramp(nodes.dry.gain, band ? 0 : 1);
    ramp(nodes.wet.gain, band ? 1 : 0);
  }

  /** ソロの道を作る：全体の音量 → 素通し（1）→ 出口、全体の音量 → バンドパス → 通した音（0）→ 出口。 */
  private buildSolo(
    ctx: ContextLike,
    master: GainLike,
    band: SoloBand | null,
  ): { dry: GainLike; filter: BiquadLike; wet: GainLike } {
    const dry = ctx.createGain();
    dry.gain.value = 1;
    const filter = ctx.createBiquadFilter();
    filter.type = 'bandpass';
    if (band) {
      filter.frequency.value = soloFreq(band.freq, ctx.sampleRate);
      filter.Q.value = band.q;
    }
    const wet = ctx.createGain();
    wet.gain.value = 0;
    master.disconnect();
    master.connect(dry);
    dry.connect(ctx.destination);
    master.connect(filter);
    filter.connect(wet);
    wet.connect(ctx.destination);
    this.soloNodes = { dry, filter, wet };
    return this.soloNodes;
  }

  /** 鳴らしている音（していなければ null）。 */
  get source(): PlaySource | null {
    return this.plan?.source ?? null;
  }

  /** ループを試聴しているか。 */
  get looping(): boolean {
    return this.plan?.loop != null;
  }

  /** 試聴しているループの区間（鳴らしている音のフレーム。していなければ null）。 */
  get loop(): FrameSpan | null {
    return this.plan?.loop ?? null;
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
    this.soloNodes = null;
    this.placingLevel = null;
    if (this.solo) this.applySolo();
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
    if (this.soloNodes) {
      this.soloNodes.dry.disconnect();
      this.soloNodes.filter.disconnect();
      this.soloNodes.wet.disconnect();
      this.soloNodes = null;
    }
    this.plan = null;
    this.ctx = null;
    this.refreshWanted = false;
    this.pendingSwitch = null;
    this.waiting = null;
    this.switchWaiting = null;
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
      if (f >= p.abs && f < p.abs + p.len) return placedPos(p, f);
      if (p.abs > f && (after === null || p.abs < after.abs)) after = p;
    }
    // 交差フェードで替える前（T より前）は、止める前の音の位置
    for (const p of this.placed) {
      if (!p.fading) continue;
      const end = Math.min(p.abs + p.len, p.stopAt ?? Number.POSITIVE_INFINITY);
      if (f >= p.abs && f < end) return placedPos(p, f);
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
    this.recutLoop(next);
    this.tick();
  }

  /**
   * 音ごとの利得（`level`）が替わった（M17 段 6）：同じ音を新しい利得で置き直す（今 + 60 ms から 10 ms の交差フェード。
   * 元を鳴らしていても）。
   */
  relevel(): void {
    const plan = this.plan;
    if (!plan || !this.ctx) return;
    const level = this.deps.level?.(plan.source) ?? 1;
    if (this.placed.every((p) => p.fading || p.level === level)) return;
    this.refreshWanted = true;
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
      (p) => !p.fading && p.abs + p.len > nowFrame && touches(changed, p.start, p.end),
    );
    if (hit || changed.type === 'all') {
      this.refreshWanted = true;
      this.tick();
    }
  }

  /**
   * 聞き比べ：鳴らす音を替える。レートが同じなら、替える時刻 T（今 + 60 ms）に鳴っている位置を `map` で写した位置から
   * 交差フェードで替える。替える先の T のチャンクが手元に来るまでは古い音を鳴らし続けて待つ（手のあとの道と同じ）。
   * レートが違えば、今の位置を写して止めてから始め直す。今の位置を写せなければ替えない（`unmappable`）。
   */
  switchTo(req: SwitchRequest): SwitchOutcome {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx) return 'failed';
    if (req.source === plan.source) {
      // 待っている替えを取り消す（同じ音のまま）
      this.pendingSwitch = null;
      this.switchWaiting = null;
      return 'playing';
    }
    if (plan.info.rate !== req.info.rate) {
      const pos = this.position();
      const from = pos === null ? null : req.map(pos);
      if (from === null || from >= req.info.frames) return 'unmappable';
      return this.play({
        from,
        end: req.end,
        loop: req.loop,
        source: req.source,
        info: req.info,
      });
    }
    const at = this.positionAt(this.switchFrame());
    const to = at === null ? null : req.map(at);
    if (to === null || to >= req.info.frames) return 'unmappable';
    this.pendingSwitch = req;
    this.tick();
    return 'playing';
  }

  /** 待っている聞き比べの替えを試す（替えたら true）。写せなくなったらやめて知らせる。 */
  private trySwitch(): boolean {
    const req = this.pendingSwitch;
    if (!req) return false;
    const t = this.switchFrame();
    const at = this.positionAt(t);
    const to = at === null ? null : req.map(at);
    if (to === null || to >= req.info.frames) {
      this.pendingSwitch = null;
      this.switchWaiting = null;
      this.deps.onSwitchFailed?.('unmappable');
      return false;
    }
    const loop = validLoop(req.loop, req.info.frames);
    const chunk = this.deps.peek(req.source, chunkOf(to));
    if (!chunk || chunk.planes.length !== req.info.channels) {
      this.switchWaiting = { source: req.source, index: chunkOf(to) };
      this.deps.need?.(req.source, neededSpans(to, req.info.frames, loop));
      this.deps.want(req.source, chunkOf(to));
      return false;
    }
    this.pendingSwitch = null;
    this.switchWaiting = null;
    this.refreshWanted = false;
    this.crossfade({
      source: req.source,
      info: req.info,
      end: req.end ?? null,
      loop,
      nextPos: to,
    });
    return true;
  }

  /**
   * チャンクを取れなかった（`count` は続けて失敗した回数）。置くのに待っているチャンクが `MAX_CHUNK_FAILURES` 回続けて
   * 取れなければ止めて知らせる。聞き比べで替える先なら、替えをやめて今の音を鳴らし続ける。
   */
  chunkFailed(source: PlaySource, index: number, count: number): void {
    if (!this.plan || count < MAX_CHUNK_FAILURES) return;
    const same = (w: { source: PlaySource; index: number } | null) =>
      w !== null && w.source === source && w.index === index;
    if (this.pendingSwitch && same(this.switchWaiting)) {
      this.pendingSwitch = null;
      this.switchWaiting = null;
      this.deps.onSwitchFailed?.('unavailable');
      return;
    }
    if (!same(this.waiting)) return;
    this.stop();
    this.deps.onFailed?.();
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
    // 聞き比べ：替える先の T のチャンクが届いたら替える
    if (this.trySwitch()) return;
    // 手のあと：替える位置のチャンクが（今の形のチャンネル数で）届いていて、前に替えてから 150 ms たったら交差フェード
    if (this.refreshWanted && this.now() - this.lastSwitch >= SWITCH_MIN_MS) {
      const t = this.switchFrame();
      const at = this.positionAt(t);
      const chunk = at === null ? undefined : this.deps.peek(plan.source, chunkOf(at));
      if (
        at !== null &&
        at < plan.info.frames &&
        chunk &&
        chunk.planes.length === plan.info.channels
      ) {
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
      const endAbs = last ? last.abs + last.len : plan.nextAbs;
      if (nowFrame >= endAbs) {
        const at = plan.nextPos;
        const source = plan.source;
        this.stop();
        this.deps.onEnded?.(at, source);
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
    for (let count = 0; plan.nextAbs < until && count < MAX_PLACE_PER_FILL; count++) {
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
      // 短いループの頭：1 周をつないだバッファを 1 区間に（区間ごとにノードを作りすぎない）
      const loop = plan.loop;
      const short =
        loop !== null && seg.start === loop.start && loop.end - loop.start <= SHORT_LOOP_FRAMES;
      const planes = short && loop ? this.loopBlock(loop) : this.segmentPlanes(seg);
      if (!planes) return;
      // 間に合わなかった（時刻が過ぎた）なら、今から少し先へ遅らせて置く
      const earliest = this.leadFrame();
      if (plan.nextAbs < earliest - Math.round(LEAD_SECONDS * rate)) plan.nextAbs = earliest;
      const end = short && loop ? loop.end : seg.end;
      const len = planes[0]?.length ?? 0;
      if (len <= 0) return;
      this.waiting = null;
      this.place(seg.start, end, planes, plan.nextAbs, plan.fadeInNext);
      plan.fadeInNext = false;
      plan.nextAbs += len;
      plan.nextPos = end;
    }
  }

  /** 区間の標本（チャンクの切り出し。チャンクが手元になければ取りに行って null）。 */
  private segmentPlanes(seg: Segment): Float32Array<ArrayBuffer>[] | null {
    const plan = this.plan;
    if (!plan) return null;
    const chunk = this.deps.peek(plan.source, chunkOf(seg.start));
    if (!chunk || chunk.planes.length !== plan.info.channels) {
      this.waiting = { source: plan.source, index: chunkOf(seg.start) };
      this.deps.want(plan.source, chunkOf(seg.start));
      return null;
    }
    return chunk.planes.map((plane) =>
      plane.subarray(seg.start - chunk.start, seg.end - chunk.start),
    );
  }

  /**
   * 短いループの 1 周を、チャンク程度の長さ（`LOOP_BLOCK_FRAMES`）までつないだ標本（1 周がチャンクの境をまたいでも
   * よい。チャンクが手元になければ取りに行って null）。
   */
  private loopBlock(loop: FrameSpan): Float32Array<ArrayBuffer>[] | null {
    const plan = this.plan;
    if (!plan) return null;
    const cycle = loop.end - loop.start;
    const one = Array.from({ length: plan.info.channels }, () => new Float32Array(cycle));
    let ready = true;
    for (let i = chunkOf(loop.start); i * CHUNK < loop.end; i++) {
      const chunk = this.deps.peek(plan.source, i);
      if (!chunk || chunk.planes.length !== plan.info.channels) {
        if (ready) this.waiting = { source: plan.source, index: i };
        this.deps.want(plan.source, i);
        ready = false;
        continue;
      }
      const a = Math.max(loop.start, chunk.start);
      const b = Math.min(loop.end, chunk.start + chunk.frames);
      if (b <= a) continue;
      chunk.planes.forEach((plane, ch) => {
        one[ch]?.set(plane.subarray(a - chunk.start, b - chunk.start), a - loop.start);
      });
    }
    if (!ready) return null;
    return repeatPlanes(one, Math.max(1, Math.floor(LOOP_BLOCK_FRAMES / cycle)));
  }

  /**
   * 区間を置く（その標本だけのバッファ）。`[start, end)` は 1 周分の鳴らす音のフレームで、`planes` がそれより長ければ
   * つないだ短いループ。`fadeIn` なら 0→1 の 10 ms で入れる。
   */
  private place(
    start: number,
    end: number,
    planes: Float32Array<ArrayBuffer>[],
    abs: number,
    fadeIn: boolean,
  ): void {
    const ctx = this.ctx;
    const plan = this.plan;
    const master = this.master;
    if (!ctx || !plan || !master) return;
    const rate = plan.info.rate;
    const len = planes[0]?.length ?? 0;
    const buffer = ctx.createBuffer(planes.length, len, rate);
    planes.forEach((plane, ch) => buffer.copyToChannel(plane, ch));
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    const gain = ctx.createGain();
    const when = abs / rate;
    const wanted = this.deps.level?.(plan.source) ?? 1;
    const level = this.refreshWanted && this.placingLevel !== null ? this.placingLevel : wanted;
    this.placingLevel = level;
    if (fadeIn) {
      gain.gain.setValueAtTime(0, when);
      gain.gain.linearRampToValueAtTime(level, when + CROSSFADE_SECONDS);
    } else {
      gain.gain.value = level;
    }
    src.connect(gain);
    gain.connect(master);
    src.start(when);
    const record: PlacedRecord = {
      source: plan.source,
      rate,
      abs,
      start,
      end: start + len,
      cycle: end - start,
      planes,
      fadeIn,
      level,
    };
    this.placed.push({
      start,
      end,
      abs,
      len,
      src,
      gain,
      fading: false,
      stopAt: null,
      record,
      level,
    });
    this.deps.onPlaced?.(record);
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
      const endAbs = p.abs + p.len;
      if (p.abs >= t) {
        this.kill(p);
      } else if (endAbs > t) {
        p.gain.gain.setValueAtTime(p.level, when);
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
    const rate = plan.info.rate;
    const old = keep.filter((p) => p.fading && p.stopAt === t).map((p) => p.record);
    const before = this.placed.length;
    Object.assign(plan, next, { nextAbs: t, done: false, fadeInNext: true });
    this.fill(Math.ceil(ctx.currentTime * plan.info.rate));
    this.deps.onCrossfade?.({ rate, t, old, fresh: this.placed[before]?.record ?? null });
  }

  /**
   * ループの区間を替えた：まだ鳴っていない区間を捨て、鳴っている区間は次の折り返し（新しい終わり・つないだバッファの
   * 1 周の境）で止めて、そこから新しい区間で置き直す（新しい終わりをもう過ぎていれば、すぐ止めて始めから）。
   */
  private recutLoop(loop: FrameSpan): void {
    const plan = this.plan;
    const ctx = this.ctx;
    if (!plan || !ctx) return;
    const rate = plan.info.rate;
    const nowFrame = Math.ceil(ctx.currentTime * rate) + Math.round(0.01 * rate);
    const keep: Placed[] = [];
    let first: Placed | null = null;
    let playing: Placed | null = null;
    for (const p of this.placed) {
      if (!p.fading && p.abs > nowFrame) {
        if (first === null || p.abs < first.abs) first = p;
        this.kill(p);
      } else {
        keep.push(p);
        if (!p.fading && nowFrame >= p.abs && nowFrame < p.abs + p.len) playing = p;
      }
    }
    this.placed = keep;
    plan.done = false;
    const cut = playing ? loopCut(playing, nowFrame, loop) : null;
    if (playing && cut) {
      try {
        playing.src.stop(cut.abs / rate);
      } catch {
        // もう止まっている
      }
      playing.len = cut.abs - playing.abs;
      plan.nextAbs = cut.abs;
      plan.nextPos = cut.pos;
    } else if (first) {
      plan.nextAbs = first.abs;
      plan.nextPos = first.start;
    }
  }

  /** 鳴り終えた区間を外す（最後の 1 つは位置のために残す）。 */
  private prune(nowFrame: number): void {
    if (this.placed.length <= 1) return;
    const keep: Placed[] = [];
    const lastLive = this.placed.filter((p) => !p.fading).at(-1);
    for (const p of this.placed) {
      const endAbs = p.abs + p.len;
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
