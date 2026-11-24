/**
 * スペクトルの面の制御（M17 段 5。React の外。docs/design.md「音声エディタのスペクトルとダイナミクス」、docs/editing.md §18.4）。
 *
 * - 見るポート：イコライザのノード（選んだもの・なければ最後のもの）の入力（前）と出力（後）。ノードがなければ文書の出力（後）
 *   だけ。表示の頼みの `spec`（[前, 後] の順）を組み（`specRequests`）、WaveController がすべての頼みに載せる。
 * - 再生中：鳴っている位置（出力のフレーム）をポートの写し（`spec[].map`）でポートのフレームに写し、そのフレームのタイルの列を
 *   出す。描く時刻が 1 hop より遅れたら、前に出したフレームから今のフレームまでの最大を出す（過渡を落とさない）。表示は
 *   落ちる速さで下げ、最大の保持を溜める。
 * - 止まっているとき：選択があれば区間の平均（`edit_spec_average`。前・後）。頼みとそのときのポートの鍵を覚え、表示の
 *   `specAverage` と今の鍵を比べて頼み直す（同じ組は 1 回）。計っている間は前の平均を薄く出す。選択がなければカーソルの
 *   位置のフレーム。
 * - タイル：`/edit/{s}/spec/{key}/{ch}/{chunk}`（置き場にあるもの＝`ready` だけを取る）。404 は表示を頼み直す。
 */

import type {
  EditAudioView,
  EditSpanDto,
  EditSpecAverageDto,
  EditSpecAverageRequestDto,
  EditSpecChannelDto,
  EditSpecPortDto,
  EditSpecRequestDto,
  EditSpecSideDto,
} from '../../../ipc/edit';
import { editSpecUrl } from '../../../ipc/media';
import { FailureMarks } from './pcmChunks';
import {
  DEFAULT_SPECTRUM_COLORS,
  type SpectrumColors,
  type SpectrumEq,
  type SpectrumScene,
} from './drawSpectrum';
import {
  type SpecBandEdge,
  decayToward,
  freqAxis,
  holdMax,
  specBands,
  tiltDb,
} from './spectrumMath';
import { SPEC_CHUNK_FRAMES, SpecTileCache, isReady, parseEditSpecTile, specDb } from './specTiles';
import {
  type FetchBytes,
  type WaveInsets,
  type WaveScheduler,
  WaveHttpError,
  fetchBytes,
  realScheduler,
} from './waveController';

/** 表示の設定（表示だけ。文書に入らない）。 */
export interface SpectrumSettings {
  /** 落ちる速さ（dB/s）。 */
  fall: number;
  /** 傾きの補正（1 kHz を中心に dB/oct。0 で切る）。 */
  tilt: number;
  channel: EditSpecChannelDto;
  /** スペクトルの範囲の幅（dB。上は 0 dBFS）。 */
  range: number;
  /** イコライザの目盛り（±dB）。 */
  eqScale: number;
}

export const DEFAULT_SPECTRUM_SETTINGS: SpectrumSettings = {
  fall: 20,
  tilt: 4.5,
  channel: { type: 'sum' },
  range: 90,
  eqScale: 12,
};

/** 表示の設定の選べる値。 */
export const SPECTRUM_CHOICES = {
  fall: [10, 20, 40],
  tilt: [0, 4.5],
  range: [60, 90, 120],
  eqScale: [6, 12, 30],
} as const;

/** 止まっているときの選択（出力のフレーム）とカーソル。 */
export interface SpectrumStill {
  cursor: number;
  selection: { start: number; end: number } | null;
}

export type DrawSpectrum = (canvas: HTMLCanvasElement, scene: SpectrumScene) => void;

export interface SpectrumOptions {
  fetch?: FetchBytes;
  scheduler?: WaveScheduler;
  now?(): number;
  /** 鳴っている位置（出力のフレーム。止まっていれば null）。 */
  playhead?(): number | null;
  average?(session: string, request: EditSpecAverageRequestDto): Promise<EditSpecAverageDto>;
  draw?: DrawSpectrum;
  tiles?: SpecTileCache;
  /** タイルが置き場から消えていた：表示を頼み直す（同じビューポートでも送る）。 */
  onStale?(): void;
  /** 区間の平均を計っている（裏の仕事の頼みを起こす）。 */
  onWork?(on: boolean): void;
}

/** 区間の平均の覚え（向き 1 つ）。 */
interface AverageSlot {
  request: EditSpecAverageRequestDto;
  /** 頼んだときのポートの鍵（null は鍵がまだ分からない）。 */
  key: string | null;
  state: 'measuring' | 'ready' | 'failed';
  mean: Float32Array | null;
  max: Float32Array | null;
}

/** 出力のフレームをポートのフレームへ（写しの区分の中。外なら null）。 */
export function toPortFrame(port: EditSpecPortDto, out: number): number | null {
  for (const p of port.map) {
    if (out >= p.outStart && out < p.outEnd) {
      return p.inStart + Math.round(((out - p.outStart) * p.inRate) / Math.max(1, p.outRate));
    }
  }
  return null;
}

/** 同じ頼みか。 */
function sameRequest(a: EditSpecAverageRequestDto, b: EditSpecAverageRequestDto): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** チャンクに揃えた区間（出力のフレーム）。 */
function chunkSpan(frame: number, total: number): EditSpanDto | null {
  if (!(total > 0)) return null;
  const c = Math.floor(Math.min(Math.max(0, frame), total - 1) / SPEC_CHUNK_FRAMES);
  return { start: c * SPEC_CHUNK_FRAMES, end: Math.min(total, (c + 1) * SPEC_CHUNK_FRAMES) };
}

export class SpectrumController {
  private canvas: HTMLCanvasElement | null = null;
  private size = { w: 0, h: 0 };
  private dpr = 1;
  private insets: WaveInsets = { top: 0, right: 0, bottom: 0, left: 0 };
  colors: SpectrumColors = DEFAULT_SPECTRUM_COLORS;
  font = '11px sans-serif';
  private session: string | null = null;
  private view: EditAudioView | null = null;
  private target: number | null = null;
  private settings: SpectrumSettings = DEFAULT_SPECTRUM_SETTINGS;
  private still: SpectrumStill = { cursor: 0, selection: null };
  private playing = false;
  private playbackSpans: EditSpanDto[] = [];
  private bands: SpecBandEdge[] = [];
  private bandsRate = 0;
  /** 出している値（dB。傾きの補正を当てたもの）と最大の保持。 */
  private postShown: Float32Array | null = null;
  private preShown: Float32Array | null = null;
  private hold: Float32Array | null = null;
  private dimPost = false;
  /** 再生中に前に出したポートのフレーム（後・前）と時刻。 */
  private lastFrame: { post: number | null; pre: number | null } = { post: null, pre: null };
  private lastTime: number | null = null;
  private averages: { pre: AverageSlot | null; post: AverageSlot | null } = {
    pre: null,
    post: null,
  };
  private averageEpoch = 0;
  private eq: SpectrumEq | null = null;
  private peak: { x: number; y: number } | null = null;
  private frameId: number | null = null;
  private loopId: number | null = null;
  private readonly loading = new Set<string>();
  private readonly failed: FailureMarks;
  private readonly tiles: SpecTileCache;
  private readonly fetch: FetchBytes;
  private readonly scheduler: WaveScheduler;
  private readonly now: () => number;
  private disposed = false;

  constructor(private readonly options: SpectrumOptions = {}) {
    this.fetch = options.fetch ?? fetchBytes;
    this.scheduler = options.scheduler ?? realScheduler;
    this.now = options.now ?? (() => performance.now());
    this.tiles = options.tiles ?? new SpecTileCache();
    this.failed = new FailureMarks(this.now);
  }

  // ===== つなぐ =====

  attach(canvas: HTMLCanvasElement): void {
    this.canvas = canvas;
    this.invalidate();
  }

  detach(): void {
    this.canvas = null;
    this.stopLoop();
  }

  resize(w: number, h: number, dpr: number): void {
    this.size = { w, h };
    this.dpr = dpr > 0 ? dpr : 1;
    this.invalidate();
  }

  setInsets(insets: WaveInsets): void {
    this.insets = insets;
    this.invalidate();
  }

  dispose(): void {
    this.disposed = true;
    this.stopLoop();
    if (this.frameId !== null) this.scheduler.cancelFrame(this.frameId);
    this.frameId = null;
    this.tiles.clear();
  }

  // ===== 入力 =====

  setSession(session: string | null): void {
    if (this.session === session) return;
    this.session = session;
    this.tiles.clear();
    this.loading.clear();
    this.averages = { pre: null, post: null };
    this.resetShown(true);
  }

  /** 見るイコライザのノード（なければ文書の出力だけ）。 */
  setTarget(node: number | null): void {
    if (this.target === node) return;
    this.target = node;
    this.averages = { pre: null, post: null };
    this.resetShown(true);
    this.refreshStill();
  }

  get targetNode(): number | null {
    return this.target;
  }

  setSettings(settings: SpectrumSettings): void {
    const prev = this.settings;
    this.settings = settings;
    if (JSON.stringify(prev.channel) !== JSON.stringify(settings.channel)) {
      this.averages = { pre: null, post: null };
      this.resetShown(true);
      this.refreshStill();
    } else if (prev.tilt !== settings.tilt) {
      // 傾きを替えたら出している値を作り直す
      this.resetShown(false);
      this.refreshStill();
    }
    this.invalidate();
  }

  get currentSettings(): SpectrumSettings {
    return this.settings;
  }

  /** 止まっているときのカーソルと選択（選択は手を離したときに渡す）。 */
  setStill(still: SpectrumStill): void {
    const same =
      still.cursor === this.still.cursor &&
      JSON.stringify(still.selection) === JSON.stringify(this.still.selection);
    this.still = still;
    if (same) return;
    this.refreshStill();
  }

  /** 再生しているか（始めたら最大の保持を消す）。 */
  setPlaying(on: boolean): void {
    if (this.playing === on) return;
    this.playing = on;
    this.lastFrame = { post: null, pre: null };
    this.lastTime = null;
    if (on) {
      this.hold = null;
      this.startLoop();
    } else {
      this.stopLoop();
      this.refreshStill();
    }
  }

  /** 再生の先読みの区間（出力のフレーム。spec の区間に使う）。 */
  setPlaybackSpans(spans: readonly EditSpanDto[]): void {
    this.playbackSpans = [...spans];
  }

  /** イコライザの重ね（段 4 の手）。 */
  setEq(eq: SpectrumEq | null, peak: { x: number; y: number } | null = null): void {
    this.eq = eq;
    this.peak = peak;
    this.invalidate();
  }

  /** 最大の保持を消す（札の［リセット］）。 */
  resetHold(): void {
    this.hold = null;
    this.invalidate();
  }

  // ===== 表示の結果 =====

  applyView(view: EditAudioView): void {
    if (this.disposed) return;
    this.view = view;
    if (this.bandsRate !== this.portRate()) {
      this.bandsRate = this.portRate();
      this.bands = specBands(this.bandsRate);
      this.resetShown(true);
    }
    this.checkAverages();
    this.pump();
    this.invalidate();
  }

  /** 表示の頼みの `spec`（[前, 後]。区間は再生の区間か、カーソル・選択の始めのチャンク）。 */
  specRequests(): EditSpecRequestDto[] {
    const total = this.view?.frames ?? 0;
    const spans: EditSpanDto[] = [];
    if (this.playing) {
      spans.push(...this.playbackSpans);
    } else {
      const at = chunkSpan(this.still.cursor, total);
      if (at) spans.push(at);
      const sel = this.still.selection ? chunkSpan(this.still.selection.start, total) : null;
      if (sel && (!at || sel.start !== at.start)) spans.push(sel);
    }
    const channel = this.settings.channel;
    const port = (node: number | null, side: EditSpecSideDto): EditSpecRequestDto => ({
      node,
      side,
      channel,
      spans,
    });
    return this.target === null
      ? [port(null, 'output')]
      : [port(this.target, 'input'), port(this.target, 'output')];
  }

  // ===== ポート =====

  private port(side: 'pre' | 'post'): EditSpecPortDto | null {
    const spec = this.view?.spec ?? [];
    const ch = JSON.stringify(this.settings.channel);
    return (
      spec.find(
        (p) =>
          JSON.stringify(p.channel) === ch &&
          (side === 'pre'
            ? this.target !== null && p.node === this.target && p.side === 'input'
            : p.node === this.target && p.side === 'output'),
      ) ?? null
    );
  }

  /** 帯の表を決めるレート（後のポート、なければ出力）。 */
  private portRate(): number {
    return this.port('post')?.rate ?? this.port('pre')?.rate ?? this.view?.rate ?? 0;
  }

  /** 出している値を捨てる（`hold` なら最大の保持も）。 */
  private resetShown(hold: boolean): void {
    this.postShown = null;
    this.preShown = null;
    if (hold) this.hold = null;
    this.lastFrame = { post: null, pre: null };
    this.lastTime = null;
    this.invalidate();
  }

  // ===== タイル =====

  private tileKey(port: EditSpecPortDto, chunk: number): string {
    return `${port.key}/${JSON.stringify(port.channel)}/${chunk}`;
  }

  /** 要るタイルを取る（再生中は今と次のチャンク、止まっていればカーソルのチャンク）。 */
  private pump(): void {
    const session = this.session;
    if (!session || this.disposed) return;
    for (const side of ['pre', 'post'] as const) {
      const port = this.port(side);
      if (!port) continue;
      const out = this.playing
        ? (this.options.playhead?.() ?? this.still.cursor)
        : this.still.cursor;
      const frame = toPortFrame(port, out);
      if (frame === null) continue;
      const c = Math.floor(frame / SPEC_CHUNK_FRAMES);
      for (const chunk of this.playing ? [c, c + 1] : [c]) {
        if (!isReady(port.ready, chunk)) continue;
        if (this.tiles.get(port.key, port.channel, chunk)) continue;
        const key = this.tileKey(port, chunk);
        if (this.loading.has(key) || this.failed.blocked(key)) continue;
        this.loading.add(key);
        const url = editSpecUrl(session, port.key, port.channel, chunk);
        this.fetch(url)
          .then((buf) => {
            this.loading.delete(key);
            if (this.disposed || this.session !== session) return;
            const tile = parseEditSpecTile(buf);
            if (!tile) {
              this.failed.fail(key);
              return;
            }
            this.tiles.put(port.key, tile);
            this.refreshStill();
            this.invalidate();
          })
          .catch((err: unknown) => {
            this.loading.delete(key);
            if (this.disposed) return;
            this.failed.fail(key);
            // 置き場から追い出された：表示を頼み直す（作り直したら ready に戻る）
            const status =
              err instanceof WaveHttpError ? err.status : (err as { status?: unknown }).status;
            if (status === 404) this.options.onStale?.();
          });
      }
    }
  }

  /** ポートのフレーム `j` の帯の dB（タイルがなければ null）。 */
  private column(port: EditSpecPortDto, j: number): Float32Array | null {
    const per = SPEC_CHUNK_FRAMES / port.hop;
    const chunk = Math.floor(j / per);
    const tile = this.tiles.get(port.key, port.channel, chunk);
    if (!tile) return null;
    const col = j - tile.firstFrame;
    if (col < 0 || col >= tile.frames) return null;
    const out = new Float32Array(tile.bands);
    const base = col * tile.bands;
    for (let k = 0; k < tile.bands; k++) out[k] = specDb(tile.data[base + k] ?? 0);
    return out;
  }

  /** 出力のフレーム `out` の帯の dB（前に出したフレームから今までの最大。傾きの補正を当てる）。 */
  private valuesAt(side: 'pre' | 'post', out: number): Float32Array | null {
    const port = this.port(side);
    if (!port) return null;
    const pf = toPortFrame(port, out);
    if (pf === null) return null;
    const total = Math.ceil(port.frames / port.hop);
    const j = Math.min(Math.max(0, Math.round(pf / port.hop)), Math.max(0, total - 1));
    const last = this.lastFrame[side];
    const per = SPEC_CHUNK_FRAMES / port.hop;
    const from = last !== null && j > last && j - last <= per ? last + 1 : j;
    let acc: Float32Array | null = null;
    for (let f = from; f <= j; f++) {
      const col = this.column(port, f);
      if (!col) continue;
      if (!acc) acc = col;
      else holdMax(acc, col);
    }
    if (acc) this.lastFrame[side] = j;
    return acc ? this.tilted(acc) : null;
  }

  /** 傾きの補正を当てる。 */
  private tilted(values: Float32Array): Float32Array {
    const tilt = this.settings.tilt;
    if (tilt === 0) return values;
    for (let k = 0; k < values.length; k++) {
      const b = this.bands[k];
      if (b) values[k] = (values[k] ?? 0) + tiltDb(b.center, tilt);
    }
    return values;
  }

  // ===== 止まっているとき =====

  /** 止まっているときの値を作り直す（平均か、カーソルのフレーム）。 */
  private refreshStill(): void {
    if (this.playing || this.disposed) return;
    if (this.still.selection) {
      this.requestAverages();
      const post = this.averages.post;
      const pre = this.averages.pre;
      this.postShown = post?.mean ? this.tilted(Float32Array.from(post.mean)) : this.postShown;
      this.preShown = pre?.mean ? this.tilted(Float32Array.from(pre.mean)) : this.preShown;
      this.hold = post?.max ? this.tilted(Float32Array.from(post.max)) : null;
      this.dimPost = post?.state === 'measuring';
    } else {
      this.lastFrame = { post: null, pre: null };
      this.postShown = this.valuesAt('post', this.still.cursor);
      this.preShown = this.valuesAt('pre', this.still.cursor);
      this.hold = null;
      this.dimPost = false;
      this.pump();
    }
    this.invalidate();
  }

  /** 前・後の区間の平均を頼む（同じ頼みと鍵の組は 1 回）。 */
  private requestAverages(): void {
    const session = this.session;
    const sel = this.still.selection;
    const average = this.options.average;
    if (!session || !sel || !average) return;
    const sides: ('pre' | 'post')[] = this.target === null ? ['post'] : ['pre', 'post'];
    for (const side of sides) {
      const request: EditSpecAverageRequestDto = {
        node: this.target,
        side: side === 'pre' ? 'input' : 'output',
        channel: this.settings.channel,
        start: sel.start,
        end: sel.end,
      };
      const key = this.port(side)?.key ?? null;
      const slot = this.averages[side];
      if (slot && sameRequest(slot.request, request) && (slot.key === key || key === null)) {
        continue;
      }
      const next: AverageSlot = {
        request,
        key,
        state: 'measuring',
        mean: slot?.mean ?? null,
        max: slot?.max ?? null,
      };
      this.averages[side] = next;
      this.fetchAverage(session, side, next);
    }
  }

  private fetchAverage(session: string, side: 'pre' | 'post', slot: AverageSlot): void {
    const average = this.options.average;
    if (!average) return;
    const epoch = ++this.averageEpoch;
    average(session, slot.request)
      .then((r) => {
        if (this.disposed || this.averages[side] !== slot) return;
        if (r.type === 'ready') {
          slot.state = 'ready';
          slot.mean = Float32Array.from(r.mean.map((v) => v ?? Number.NEGATIVE_INFINITY));
          slot.max = Float32Array.from(r.max.map((v) => v ?? Number.NEGATIVE_INFINITY));
          if (r.frames === 0) {
            // 選択が hop より短い：カーソルのフレームに落とす
            slot.mean = null;
            slot.max = null;
          }
        } else if (r.type === 'measuring') {
          slot.state = 'measuring';
          this.options.onWork?.(true);
        } else {
          slot.state = 'failed';
        }
        if (epoch === this.averageEpoch && !this.anyMeasuring()) this.options.onWork?.(false);
        this.refreshStill();
      })
      .catch(() => {
        if (this.averages[side] === slot) slot.state = 'failed';
      });
  }

  private anyMeasuring(): boolean {
    return this.averages.pre?.state === 'measuring' || this.averages.post?.state === 'measuring';
  }

  /** 表示の `specAverage` を見て、終えたもの・捨てられたもの・鍵の替わったものを頼み直す。 */
  private checkAverages(): void {
    const session = this.session;
    if (!session || this.playing || !this.still.selection) return;
    const statuses = this.view?.specAverage ?? [];
    for (const side of ['pre', 'post'] as const) {
      const slot = this.averages[side];
      if (!slot) continue;
      const key = this.port(side)?.key ?? null;
      // 鍵が替わった（引数を変えた・ノードを替えた）：頼み直す
      if (key !== null && slot.key !== null && slot.key !== key) {
        slot.key = key;
        slot.state = 'measuring';
        this.fetchAverage(session, side, slot);
        continue;
      }
      if (slot.state !== 'measuring') continue;
      const st = statuses.find((s) => sameRequest(s.request, slot.request));
      if (!st) continue;
      if (st.phase.type === 'ready' || st.phase.type === 'idle')
        this.fetchAverage(session, side, slot);
      else if (st.phase.type === 'failed') slot.state = 'failed';
    }
  }

  // ===== 再生中 =====

  private startLoop(): void {
    if (this.loopId !== null || this.disposed) return;
    const tick = () => {
      this.loopId = null;
      if (!this.playing || this.disposed) return;
      this.tickPlaying();
      this.loopId = this.scheduler.frame(tick);
    };
    this.loopId = this.scheduler.frame(tick);
  }

  private stopLoop(): void {
    if (this.loopId !== null) this.scheduler.cancelFrame(this.loopId);
    this.loopId = null;
  }

  /** 描く時刻ごと：鳴っている位置の値に向けて落とし、保持を溜めて描く。 */
  tickPlaying(): void {
    const pos = this.options.playhead?.();
    if (pos === null || pos === undefined) return;
    const now = this.now();
    const dt = this.lastTime === null ? 0 : Math.max(0, (now - this.lastTime) / 1_000);
    this.lastTime = now;
    const fall = this.settings.fall;
    for (const side of ['pre', 'post'] as const) {
      const target = this.valuesAt(side, pos);
      const shown = side === 'post' ? this.postShown : this.preShown;
      if (target) {
        if (!shown || shown.length !== target.length) {
          if (side === 'post') this.postShown = Float32Array.from(target);
          else this.preShown = Float32Array.from(target);
        } else {
          decayToward(shown, target, fall, dt);
        }
        if (side === 'post') {
          if (!this.hold || this.hold.length !== target.length)
            this.hold = Float32Array.from(target);
          else holdMax(this.hold, target);
        }
      } else if (shown) {
        // タイルがまだない：出している値を落ちる速さで下げる
        decayToward(shown, new Float32Array(shown.length).fill(Number.NEGATIVE_INFINITY), fall, dt);
      }
    }
    this.dimPost = false;
    this.pump();
    this.draw();
  }

  // ===== 描く =====

  invalidate(): void {
    if (this.frameId !== null || this.disposed) return;
    this.frameId = this.scheduler.frame(() => {
      this.frameId = null;
      this.draw();
    });
  }

  /** 描く領域（CSS px）。 */
  inner(): { x: number; y: number; w: number; h: number } {
    const i = this.insets;
    return {
      x: i.left,
      y: i.top,
      w: Math.max(0, this.size.w - i.left - i.right),
      h: Math.max(0, this.size.h - i.top - i.bottom),
    };
  }

  scene(): SpectrumScene {
    const rate = this.portRate();
    const s = this.settings;
    return {
      width: Math.round(this.size.w * this.dpr),
      height: Math.round(this.size.h * this.dpr),
      dpr: this.dpr,
      inner: this.inner(),
      axis: freqAxis(rate > 0 ? rate : 48_000),
      spec: { top: 0, bottom: -s.range },
      eq: { top: s.eqScale, bottom: -s.eqScale },
      bands: this.bands,
      post: this.postShown,
      pre: this.preShown,
      hold: this.hold,
      dimPost: this.dimPost,
      eqLayer: this.eq,
      colors: this.colors,
      font: this.font,
      peak: this.peak,
    };
  }

  private draw(): void {
    const canvas = this.canvas;
    if (!canvas || this.disposed) return;
    const w = Math.round(this.size.w * this.dpr);
    const h = Math.round(this.size.h * this.dpr);
    if (canvas.width !== w) canvas.width = w;
    if (canvas.height !== h) canvas.height = h;
    this.options.draw?.(canvas, this.scene());
  }
}
