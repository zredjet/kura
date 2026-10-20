/**
 * 素材の音を鳴らす口（docs/design.md §3.1）：ループの試聴（Audition）と、選んだ短い音声の即再生（SfxPlayer）。
 * Web Audio には差し替えられる口（ContextLike）の向こうで触る（jsdom に AudioContext がないので、テストは偽物で回す）。
 *
 * ループの試聴の約束（src/lib/asset/audition.ts の注。あとで「簡単にする」直しで崩されやすいので、テストで守る）：
 * - loopStart・loopEnd には触れない（SourceLike に持たせない）。本体だけのバッファを loop = true で回す。
 * - 文脈は元のレートで作る（違うレートは補間になる）。作れなければ試聴しない。
 * - 始める時刻はフレームの境にそろえ、前奏のあとの本体は「開始のフレーム + 前奏のフレーム数」÷ レートに置く。
 * - ループ点・長さは /pcm の応答（同じ読み取りの値）を正とし、DB の値と違えば計画し直して 1 回だけ取り直す。
 */

import {
  type AuditionFacts,
  type AuditionOptions,
  type AuditionPlan,
  auditionPosition,
  DEFAULT_AUDITION,
  planAudition,
  rangesOf,
} from './asset/audition';
import { type Pcm, readPcmWav } from './asset/wav';

/** AudioParam のうち使う所。 */
export interface AudioParamLike {
  value: number;
  setValueAtTime(value: number, when: number): unknown;
  linearRampToValueAtTime(value: number, when: number): unknown;
}

/** つなぐ先（AudioNode のうち使う所）。 */
export interface AudioNodeLike {
  disconnect(): void;
}

export interface GainLike extends AudioNodeLike {
  readonly gain: AudioParamLike;
  connect(dest: AudioNodeLike): unknown;
}

/** AudioBuffer のうち使う所。 */
export interface BufferLike {
  readonly length: number;
  readonly sampleRate: number;
  readonly numberOfChannels: number;
  copyToChannel(source: Float32Array<ArrayBuffer>, channel: number): void;
}

/** AudioBufferSourceNode のうち使う所。loopStart・loopEnd は持たせない（使うと標本がずれる）。 */
export interface SourceLike extends AudioNodeLike {
  buffer: BufferLike | null;
  loop: boolean;
  onended: ((ev: Event) => unknown) | null;
  connect(dest: AudioNodeLike): unknown;
  start(when?: number, offset?: number): void;
  stop(when?: number): void;
}

/** AudioContext のうち使う所。 */
export interface ContextLike {
  readonly sampleRate: number;
  readonly currentTime: number;
  readonly destination: AudioNodeLike;
  createBuffer(channels: number, frames: number, rate: number): BufferLike;
  createBufferSource(): SourceLike;
  createGain(): GainLike;
  decodeAudioData(data: ArrayBuffer): Promise<BufferLike>;
  resume(): Promise<void>;
  close(): Promise<void>;
}

/** 本物の文脈を作る（`rate` を省くと装置のレート）。型の上で AudioContext が ContextLike を満たすことも確かめる。 */
export function browserContext(rate?: number): ContextLike {
  const ctx: AudioContext = new AudioContext(rate === undefined ? undefined : { sampleRate: rate });
  return ctx;
}

/** 文脈の鍵：元のレート、または装置のレート。 */
type ContextKey = number | 'device';

/**
 * 文脈を元のレートごとに 1 つだけ持って使い回す（新しく使ったものを後ろへ）。`max` を超えたら、いちばん長く使っていない
 * ものを閉じる。持ち主（ライブラリの画面）を離れるときに `closeAll` で全部閉じる。
 */
export class ContextPool {
  private readonly contexts = new Map<ContextKey, ContextLike>();

  constructor(
    private readonly create: (rate?: number) => ContextLike,
    private readonly max = 3,
  ) {
    if (!(max >= 1)) throw new RangeError('文脈の数の上限は 1 以上です');
  }

  /** 持っている文脈の数。 */
  get size(): number {
    return this.contexts.size;
  }

  /** レート `rate`（省けば装置のレート）の文脈。 */
  get(rate?: number): ContextLike {
    const key: ContextKey = rate ?? 'device';
    const found = this.contexts.get(key);
    if (found) {
      this.contexts.delete(key);
      this.contexts.set(key, found);
      return found;
    }
    while (this.contexts.size >= this.max) {
      const oldest = this.contexts.keys().next();
      if (oldest.done) break;
      void this.contexts.get(oldest.value)?.close();
      this.contexts.delete(oldest.value);
    }
    const ctx = this.create(rate);
    this.contexts.set(key, ctx);
    return ctx;
  }

  /** 全部を閉じる。 */
  closeAll(): void {
    for (const ctx of this.contexts.values()) void ctx.close();
    this.contexts.clear();
  }
}

/** 鳴らし始めた試聴（継ぎ目の自己点検 `VITE_KURA_CHECK` が、同じバッファと同じ組み立てで照らす）。 */
export interface ScheduledAudition {
  hash: string;
  plan: AuditionPlan;
  rate: number;
  /** 計画の区間の順（`rangesOf`）に読んだ PCM。 */
  pcms: readonly Pcm[];
}

export interface AuditionDeps {
  pool: ContextPool;
  /** `kura://localhost/pcm/{hash}/{from}-{to}` のバイト列。 */
  fetchPcm(hash: string, from: number, to: number): Promise<ArrayBuffer>;
  /** 音量（0〜1。再生の設定）。 */
  volume(): number;
  /** 止まったとき（止めた・継ぎ目だけの試聴が終わった）。 */
  onStop?(): void;
  /** 鳴らし始めたとき（dev の継ぎ目の自己点検だけが使う）。 */
  onScheduled?(scheduled: ScheduledAudition): void;
}

/** 長さ・ループ点を知らずに始めるときに分かっていること（解析の事実）。 */
export interface AuditionHint {
  rate: number;
  channels: number;
}

export type AuditionOutcome =
  | { status: 'playing'; plan: AuditionPlan; facts: AuditionFacts }
  /** 待つ間に止めた・次を始めた。 */
  | { status: 'superseded' }
  /** 試聴できない（レート・区間・応答が合わない）。 */
  | { status: 'unsupported' }
  /** 取れなかった（混んでいる 503 など）。 */
  | { status: 'failed'; error: unknown };

/** 文脈を始めてから最初の音までの余裕（秒）。 */
const LEAD_IN_SECONDS = 0.05;
/** 取得を待つ間に止めた・次を始めた印。 */
const SUPERSEDED = Symbol('superseded');

/** 文脈を取る（そのレートの文脈を作れない環境では NotSupportedError になるので null）。 */
function contextOf(pool: ContextPool, rate?: number): ContextLike | null {
  try {
    return pool.get(rate);
  } catch {
    return null;
  }
}

/** resume を始める（キー・クリックの処理の中で）。待たずに終わる道でも、失敗を未処理のまま残さない。 */
function resumeOf(ctx: ContextLike): Promise<void> {
  const p = ctx.resume();
  p.catch(() => undefined);
  return p;
}

function sameLoop(
  a: readonly [number, number] | null,
  b: readonly [number, number] | null,
): boolean {
  return a === b || (a !== null && b !== null && a[0] === b[0] && a[1] === b[1]);
}

/** ループの試聴：一度に 1 つだけ。始めると前のものを止める。 */
export class Audition {
  private sources: SourceLike[] = [];
  private gain: GainLike | null = null;
  private token = 0;
  /** 鳴らしている試聴の時刻の基準（位置を求める）。 */
  private timing: {
    ctx: ContextLike;
    plan: AuditionPlan;
    rate: number;
    startFrame: number;
  } | null = null;

  constructor(private readonly deps: AuditionDeps) {}

  get playing(): boolean {
    return this.sources.length > 0;
  }

  /** 鳴っている元のファイルの位置（フレーム）。鳴らしていなければ null。 */
  position(): number | null {
    const t = this.timing;
    if (!t || !this.playing) return null;
    return auditionPosition(t.plan, Math.round(t.ctx.currentTime * t.rate) - t.startFrame);
  }

  /**
   * 試聴を始める。キー・クリックの処理の中から呼ぶ（文脈の resume を、取得を待つ前に呼ぶため）。
   * 本体が上限を超えるものは継ぎ目の前後だけを鳴らす。
   */
  async start(
    hash: string,
    facts: AuditionFacts,
    options: Partial<AuditionOptions> = {},
  ): Promise<AuditionOutcome> {
    this.stop();
    const token = ++this.token;
    const opts = { ...DEFAULT_AUDITION, ...options };
    const plan = planAudition(facts, opts);
    if (!plan) return { status: 'unsupported' };
    const ctx = contextOf(this.deps.pool, facts.rate);
    if (!ctx || ctx.sampleRate !== facts.rate) return { status: 'unsupported' };
    const resumed = resumeOf(ctx);
    return this.run(ctx, resumed, token, hash, facts, plan, opts);
  }

  /**
   * 長さ・ループ点を知らずに試聴を始める（DB は長さをフレームで持たない）。先頭の 1 フレームを取り、応答の `kura`
   * チャンク（同じ読み取りの長さ・ループ点）で計画してから取る。応答のレート・チャンネルが `hint` と違えば試聴しない。
   * キー・クリックの処理の中から呼ぶ（resume は取る前）。
   */
  async startProbed(
    hash: string,
    hint: AuditionHint,
    options: Partial<AuditionOptions> = {},
  ): Promise<AuditionOutcome> {
    this.stop();
    const token = ++this.token;
    const opts = { ...DEFAULT_AUDITION, ...options };
    const ctx = contextOf(this.deps.pool, hint.rate);
    if (!ctx || ctx.sampleRate !== hint.rate) return { status: 'unsupported' };
    const resumed = resumeOf(ctx);
    let probe: Pcm | null;
    try {
      probe = readPcmWav(await this.deps.fetchPcm(hash, 0, 1), opts.capBytes);
    } catch (error) {
      return token === this.token ? { status: 'failed', error } : { status: 'superseded' };
    }
    if (token !== this.token) return { status: 'superseded' };
    if (!probe || probe.rate !== hint.rate || probe.channels !== hint.channels) {
      return { status: 'unsupported' };
    }
    const facts: AuditionFacts = {
      frames: probe.info.total,
      rate: hint.rate,
      channels: hint.channels,
      loop: probe.info.loop,
    };
    const plan = planAudition(facts, opts);
    if (!plan) return { status: 'unsupported' };
    return this.run(ctx, resumed, token, hash, facts, plan, opts);
  }

  /** 計画の区間を取って鳴らす（start・startProbed の続き）。 */
  private async run(
    ctx: ContextLike,
    resumed: Promise<void>,
    token: number,
    hash: string,
    facts: AuditionFacts,
    plan: AuditionPlan,
    opts: AuditionOptions,
  ): Promise<AuditionOutcome> {
    let loaded: Awaited<ReturnType<Audition['load']>>;
    try {
      loaded = await this.load(hash, facts, plan, opts, token);
    } catch (error) {
      return token === this.token ? { status: 'failed', error } : { status: 'superseded' };
    }
    if (loaded === SUPERSEDED || token !== this.token) return { status: 'superseded' };
    if (!loaded) return { status: 'unsupported' };
    try {
      await resumed;
    } catch (error) {
      return token === this.token ? { status: 'failed', error } : { status: 'superseded' };
    }
    if (token !== this.token) return { status: 'superseded' };
    this.schedule(ctx, loaded.plan, loaded.facts.rate, loaded.pcms, token);
    this.deps.onScheduled?.({
      hash,
      plan: loaded.plan,
      rate: loaded.facts.rate,
      pcms: loaded.pcms,
    });
    return { status: 'playing', plan: loaded.plan, facts: loaded.facts };
  }

  /**
   * 計画の区間を取って読む。応答のループ点・長さ・デコーダが DB の値と違えば、応答の値で計画し直して 1 回だけ取り直す。
   */
  private async load(
    hash: string,
    facts: AuditionFacts,
    plan: AuditionPlan,
    opts: AuditionOptions,
    token: number,
  ): Promise<{ plan: AuditionPlan; facts: AuditionFacts; pcms: Pcm[] } | null | typeof SUPERSEDED> {
    let want = facts;
    let current = plan;
    for (let attempt = 0; attempt < 2; attempt++) {
      const ranges = rangesOf(current);
      const bufs = await Promise.all(ranges.map(([a, b]) => this.deps.fetchPcm(hash, a, b)));
      if (token !== this.token) return SUPERSEDED;
      const pcms: Pcm[] = [];
      for (const [i, buf] of bufs.entries()) {
        const pcm = readPcmWav(buf, opts.capBytes);
        const range = ranges[i];
        if (!pcm || !range) return null;
        const [a, b] = range;
        const exact =
          pcm.rate === want.rate &&
          pcm.channels === want.channels &&
          pcm.info.start === a &&
          pcm.frames === b - a;
        if (!exact) return null;
        pcms.push(pcm);
      }
      const first = pcms[0]?.info;
      if (!first) return null;
      const agree = pcms.every(
        (p) =>
          p.info.total === first.total &&
          sameLoop(p.info.loop, first.loop) &&
          p.info.decoder === first.decoder,
      );
      if (agree && first.total === want.frames && sameLoop(first.loop, want.loop)) {
        return { plan: current, facts: want, pcms };
      }
      if (attempt === 1) return null;
      want = { ...want, frames: first.total, loop: first.loop };
      const next = planAudition(want, opts);
      if (!next) return null;
      current = next;
    }
    return null;
  }

  private schedule(
    ctx: ContextLike,
    plan: AuditionPlan,
    rate: number,
    pcms: Pcm[],
    token: number,
  ): void {
    const gain = ctx.createGain();
    gain.gain.value = this.deps.volume();
    gain.connect(ctx.destination);
    this.gain = gain;
    // フレームの境にそろえた開始のフレーム。前奏・本体の時刻は整数のフレームから求める（丸めを同じ向きにする）
    const startFrame = Math.ceil((ctx.currentTime + LEAD_IN_SECONDS) * rate);
    this.timing = { ctx, plan, rate, startFrame };
    const make = (pcm: Pcm, looping: boolean): SourceLike => {
      const buffer = ctx.createBuffer(pcm.channels, pcm.frames, rate);
      pcm.planes.forEach((plane, ch) => buffer.copyToChannel(plane, ch));
      const s = ctx.createBufferSource();
      s.buffer = buffer;
      s.loop = looping;
      s.connect(gain);
      this.sources.push(s);
      return s;
    };
    const [first, second] = pcms;
    if (!first) return;
    if (plan.mode === 'loop') {
      if (second) {
        make(first, false).start(startFrame / rate);
        make(second, true).start((startFrame + first.frames) / rate);
      } else {
        make(first, true).start(startFrame / rate, plan.offset / rate);
      }
    } else if (second) {
      make(first, false).start(startFrame / rate);
      const head = make(second, false);
      head.onended = () => {
        if (token === this.token) this.stop();
      };
      head.start((startFrame + first.frames) / rate);
    }
  }

  /** 止める（鳴らしていなければ何もしない）。待っている試聴も取り消す。 */
  stop(): void {
    this.token++;
    this.timing = null;
    if (this.sources.length === 0) return;
    for (const s of this.sources) {
      s.onended = null;
      try {
        s.stop();
      } catch {
        // 始まる前に止めた
      }
      s.disconnect();
    }
    this.sources = [];
    this.gain?.disconnect();
    this.gain = null;
    this.deps.onStop?.();
  }

  /** 使い終えたとき（インスペクタを離れた）。文脈を閉じるのは ContextPool の持ち主。 */
  dispose(): void {
    this.stop();
  }
}

export interface SfxDeps {
  pool: ContextPool;
  /** 鳴らす音声のバイト列（`playUrl` を fetch したもの）。 */
  fetchAudio(hash: string): Promise<ArrayBuffer>;
  /** 音量（0〜1。再生の設定）。 */
  volume(): number;
  /** 解いた音を持つ上限（バイト。浮動小数の大きさ）。既定 32MiB。 */
  maxCacheBytes?: number;
}

/** 前の音を下げて止める長さ（秒）。 */
const SFX_FADE_SECONDS = 0.01;

/**
 * 選んだ短い音声をすぐ鳴らす（装置のレートの文脈で decodeAudioData。効果音では時間軸のずれは問題にならない）。
 * 一度に 1 つだけで、次の音は前の音を 10ms で下げて止める。解いた音は上限まで持つ（古いものから捨てる）。
 */
export class SfxPlayer {
  private readonly cache = new Map<string, BufferLike>();
  private cacheBytes = 0;
  private current: { source: SourceLike; gain: GainLike; ctx: ContextLike } | null = null;
  private token = 0;

  constructor(private readonly deps: SfxDeps) {}

  async play(hash: string): Promise<'playing' | 'superseded' | 'failed'> {
    const token = ++this.token;
    this.stop();
    const ctx = contextOf(this.deps.pool);
    if (!ctx) return 'failed';
    const resumed = resumeOf(ctx);
    let buffer = this.cache.get(hash);
    try {
      if (buffer) {
        this.cache.delete(hash);
        this.cache.set(hash, buffer);
      } else {
        const data = await this.deps.fetchAudio(hash);
        if (token !== this.token) return 'superseded';
        buffer = await ctx.decodeAudioData(data);
        this.remember(hash, buffer);
      }
      await resumed;
    } catch {
      return token === this.token ? 'failed' : 'superseded';
    }
    if (token !== this.token) return 'superseded';
    const gain = ctx.createGain();
    gain.gain.value = this.deps.volume();
    gain.connect(ctx.destination);
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(gain);
    source.onended = () => {
      if (this.current?.source === source) {
        source.disconnect();
        gain.disconnect();
        this.current = null;
      }
    };
    source.start(0);
    this.current = { source, gain, ctx };
    return 'playing';
  }

  /** 鳴らしている音を 10ms で下げて止める。 */
  stop(): void {
    const cur = this.current;
    if (!cur) return;
    this.current = null;
    const end = cur.ctx.currentTime + SFX_FADE_SECONDS;
    cur.gain.gain.setValueAtTime(cur.gain.gain.value, cur.ctx.currentTime);
    cur.gain.gain.linearRampToValueAtTime(0, end);
    try {
      cur.source.stop(end);
    } catch {
      // もう止まっている
    }
  }

  private remember(hash: string, buffer: BufferLike): void {
    const max = this.deps.maxCacheBytes ?? 32 * 1024 * 1024;
    const bytes = (b: BufferLike) => b.length * b.numberOfChannels * 4;
    this.cache.set(hash, buffer);
    this.cacheBytes += bytes(buffer);
    for (const [key, b] of this.cache) {
      if (this.cacheBytes <= max || key === hash) break;
      this.cache.delete(key);
      this.cacheBytes -= bytes(b);
    }
  }
}
