/**
 * 音声エディタの波形の面の制御（React の外。M13 の段 6。設計 §2.4、docs/editing.md §12.13）。
 *
 * - 1 枚の `<canvas>` に、上から目盛り・全体図・チャンネルの段を描く（描く関数は `drawWave.ts`。差し込める）。再生位置は
 *   canvas の上の要素を `transform` で動かすので、ここでは描かない（`xOf` で位置だけ出す）。
 * - 描く範囲：見ている区間（小数のフレーム）と、幅 × DPR の列の数から 1 列のフレーム数を出す。256 以上なら `pickBin` で
 *   選んだ段のタイルで描き、足りないタイルは粗い段で埋めてから取りに行く（鍵ごとに 1 本、今の `peaks.rev` の URL で）。
 *   256 未満は PCM で描く：見えている区間をチャンクにそろえてビューポートの `spans` に入れて頼み、結果の `valid` に
 *   入ったチャンクを `/edit/{s}/{rev}/pcm/{a}-{b}` で取る（`kura` チャンクの頭・長さ・デコーダ `kura-edit/1:{samplesId}`
 *   を確かめる）。届くまでは最も細かい段で描く。
 * - 表示の結果を受けたら：`changed` に掛かる PCM のチャンクを捨て、`peaks.changed` に掛かるタイルを捨てる。
 *   `peaks.pending` か `work` が真なら、手が止まって 150 ms たったら `work: true` で頼み直す（同じビューポートでも送る）。
 * - ズーム・パンで描き直すのは `requestAnimationFrame` の 1 回だけ（汚れたときだけ）。止まって 150 ms で今の区間を頼み直す。
 *   列ごとの [min, max] は使い回す型付きの配列に入れる。
 * - 公開されている段の長さが今の出力と違う間（尾を作り直している）は、重なる所だけ描く（段の長さで切る）。
 * - 描く関数・取得・時計は差し込める（テスト）。
 */

import type {
  EditAudioView,
  EditAudioViewport,
  EditMarkerDto,
  EditPeaksInfoDto,
  EditSpanDto,
} from '../../../ipc/edit';
import { editPcmUrl, editPeaksUrl } from '../../../ipc/media';
import { readPcmWav } from '../../../lib/asset/wav';
import {
  alignSpan,
  chunkOf,
  chunkSpan,
  covered,
  fillPcmColumns,
  type PcmChunk,
  PcmChunkCache,
} from './pcmChunks';
import {
  fillPeakColumns,
  parseEditPeaksTile as parseTile,
  PeakTileCache,
  TILE_BINS,
  tileRange,
} from './peakTiles';
import {
  clampWindow,
  fitView,
  followPlayhead,
  frameToX,
  type FrameSpan,
  hitTest,
  type HitScene,
  type HitTarget,
  laneLayout,
  type LaneLayout,
  minSpan,
  overviewStart,
  PCM_BELOW,
  panBy,
  pickBin,
  rulerTicks,
  SAMPLE_POINTS_PX,
  sameWindow,
  type Tick,
  type WaveWindow,
  xToFrame,
  zoomAround,
} from './waveMath';

/** 止まってから頼み直すまで（ms）。 */
export const WAVE_IDLE_MS = 150;
/** 同時に取るタイルの数。 */
const MAX_TILE_LOADS = 4;
/** 同時に取る PCM のチャンクの数。 */
const MAX_PCM_LOADS = 4;
/** ⌘+・⌘− の 1 回の倍率。 */
export const ZOOM_STEP = 2;
/** ビューポートの `spans` の数の上限（Rust の `MAX_VIEW_SPANS`）。 */
const MAX_SPANS = 4;
/** 出力の PCM の kura チャンクのデコーダの頭（Rust の `EDIT_DECODER_PREFIX`）。 */
export const EDIT_DECODER_PREFIX = 'kura-edit/1:';

/** 面の外側の余白（CSS px。上部と下のカプセルを避ける）。 */
export interface WaveInsets {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/** 描く色（CSS の色。canvas の `fillStyle` にそのまま渡す）。 */
export interface WaveColors {
  wave: string;
  loop: string;
  accent: string;
  selection: string;
  outside: string;
  handle: string;
  cursor: string;
  separator: string;
  label: string;
  overviewWindow: string;
  overviewFrame: string;
}

/** 描く大きさ（CSS px）と文字。 */
export interface WaveMetrics {
  ruler: number;
  overview: number;
  font: string;
}

/** 波形に重ねるもの（ストアが決める。出さないものは null）。 */
export interface WaveOverlay {
  cursor: number | null;
  selection: FrameSpan | null;
  /** 切り詰めの枠（外を暗くする）。 */
  trim: FrameSpan | null;
  /** フェードの曲線（長さ 0 の側は描かない）と、三角のハンドルを出すか。 */
  fade: {
    inEnd: number;
    outStart: number;
    curve: string;
    handles: boolean;
  } | null;
  loop: FrameSpan | null;
  /** ループの旗をつかめるか（ループの道具）。 */
  loopFlags: boolean;
  markers: readonly EditMarkerDto[];
}

export const EMPTY_OVERLAY: WaveOverlay = {
  cursor: null,
  selection: null,
  trim: null,
  fade: null,
  loop: null,
  loopFlags: false,
  markers: [],
};

/** 1 回の描画の中身。 */
export interface WaveScene {
  /** canvas の大きさ（装置の画素）と比。 */
  width: number;
  height: number;
  dpr: number;
  /** 描く領域（CSS px。面の中）。 */
  inner: { x: number; y: number; w: number; h: number };
  layout: LaneLayout;
  window: WaveWindow;
  total: number;
  rate: number;
  channels: number;
  /** 段の列（装置の画素 1 列ずつ。列 → チャンネル → [min, max]）と、埋めた列。 */
  columns: number;
  lanes: Float32Array;
  has: Uint8Array;
  /** 何で描いたか（段・PCM・まだない）。 */
  mode: 'peaks' | 'pcm' | 'empty';
  /** 標本の点（1 フレームが 8 装置画素を超えたとき。チャンネルごと、`pointsStart` からのフレーム）。 */
  points: Float32Array[] | null;
  pointsStart: number;
  /** 全体図の列（装置の画素 1 列ずつ）。 */
  overviewColumns: number;
  overview: Float32Array;
  overviewHas: Uint8Array;
  ticks: Tick[];
  overlay: WaveOverlay;
  colors: WaveColors;
  metrics: WaveMetrics;
}

export type DrawWave = (canvas: HTMLCanvasElement, scene: WaveScene) => void;

/** 取る口（kura:// の本体）。404 は `WaveHttpError`。 */
export type FetchBytes = (url: string) => Promise<ArrayBuffer>;

/** kura:// の応答が 2xx でなかった。 */
export class WaveHttpError extends Error {
  constructor(readonly status: number) {
    super(`HTTP ${status}`);
    this.name = 'WaveHttpError';
  }
}

async function fetchBytes(url: string): Promise<ArrayBuffer> {
  const res = await fetch(url);
  if (!res.ok) throw new WaveHttpError(res.status);
  return res.arrayBuffer();
}

/** 時計（描く時刻と、止まってからの待ち）。 */
export interface WaveScheduler {
  frame(cb: () => void): number;
  cancelFrame(id: number): void;
  later(cb: () => void, ms: number): number;
  cancelLater(id: number): void;
}

const realScheduler: WaveScheduler = {
  frame: (cb) =>
    typeof requestAnimationFrame === 'function'
      ? requestAnimationFrame(cb)
      : window.setTimeout(cb, 16),
  cancelFrame: (id) =>
    typeof cancelAnimationFrame === 'function' ? cancelAnimationFrame(id) : window.clearTimeout(id),
  later: (cb, ms) => window.setTimeout(cb, ms),
  cancelLater: (id) => window.clearTimeout(id),
};

/** ストアへ送る表示の情報。 */
export interface WaveInfo {
  window: WaveWindow;
  /** 波形の段を作り直している（古い段を出している・まだない）。 */
  pending: boolean;
  /** 段がまだ一度も届いていない。 */
  building: boolean;
}

export interface WaveOptions {
  draw?: DrawWave;
  fetch?: FetchBytes;
  scheduler?: WaveScheduler;
  /** 波形の拡大と再生が分け合う PCM の表。 */
  pcm?: PcmChunkCache;
  tiles?: PeakTileCache;
  colors?: WaveColors;
  metrics?: WaveMetrics;
  onInfo?(info: WaveInfo): void;
  /** 表示の範囲を頼む（`force` は同じビューポートでも送る：裏の仕事）。 */
  onIdle?(viewport: EditAudioViewport, force: boolean): void;
}

/** テストと、色を読めないときの既定（CSS の変数の名前。canvas はそのまま色にできないので、面がつなぐときに読む）。 */
export const DEFAULT_COLORS: WaveColors = {
  wave: 'gray',
  loop: 'orange',
  accent: 'orange',
  selection: 'white',
  outside: 'black',
  handle: 'white',
  cursor: 'white',
  separator: 'gray',
  label: 'gray',
  overviewWindow: 'gray',
  overviewFrame: 'gray',
};

export const DEFAULT_METRICS: WaveMetrics = { ruler: 20, overview: 24, font: '11px sans-serif' };

/** 出力の形（表示の結果か、開いたときの元）。 */
interface Shape {
  rate: number;
  channels: number;
  frames: number;
}

/** 読んでいるもの（鍵 → 世代）。 */
interface Loading {
  epoch: number;
}

export class WaveController {
  private canvas: HTMLCanvasElement | null = null;
  private size = { w: 0, h: 0 };
  private dpr = 1;
  private insets: WaveInsets = { top: 0, right: 0, bottom: 0, left: 0 };
  private session: string | null = null;
  private shape: Shape | null = null;
  private view: EditAudioView | null = null;
  private peaks: EditPeaksInfoDto | null = null;
  private w: WaveWindow = { start: 0, span: 1 };
  private fitMode = true;
  private overlay: WaveOverlay = EMPTY_OVERLAY;
  private extraSpans: EditSpanDto[] = [];
  private interacting = false;
  private focusNode: number | null = null;
  private frameId: number | null = null;
  private idleId: number | null = null;
  private workWanted = false;
  private tileLoads = 0;
  private pcmLoads = 0;
  private readonly loadingTiles = new Map<string, Loading>();
  private readonly failedTiles = new Set<string>();
  private readonly loadingPcm = new Map<number, Loading>();
  private readonly failedPcm = new Set<string>();
  /** 波形の段・PCM を捨てた回数（読んでいる途中のものが古くなったかを見る）。 */
  private tileEpoch = 0;
  private pcmEpoch = 0;
  private disposed = false;
  private infoKey = '';
  private lanes = new Float32Array(0);
  private has = new Uint8Array(0);
  private overviewCols = new Float32Array(0);
  private overviewHas = new Uint8Array(0);
  private lastMode: WaveScene['mode'] = 'empty';
  private readonly draw: DrawWave;
  private readonly fetch: FetchBytes;
  private readonly scheduler: WaveScheduler;
  readonly pcm: PcmChunkCache;
  readonly tiles: PeakTileCache;
  colors: WaveColors;
  metrics: WaveMetrics;

  constructor(private readonly options: WaveOptions = {}) {
    this.draw = options.draw ?? (() => undefined);
    this.fetch = options.fetch ?? fetchBytes;
    this.scheduler = options.scheduler ?? realScheduler;
    this.pcm = options.pcm ?? new PcmChunkCache();
    this.tiles = options.tiles ?? new PeakTileCache();
    this.colors = options.colors ?? DEFAULT_COLORS;
    this.metrics = options.metrics ?? DEFAULT_METRICS;
  }

  // ===== 面 =====

  /** 描く先をつなぐ。 */
  attach(canvas: HTMLCanvasElement): void {
    this.canvas = canvas;
    this.sizeCanvas();
    this.invalidate();
  }

  /** 描く先を外す（同じ canvas のときだけ）。 */
  detach(canvas: HTMLCanvasElement): void {
    if (this.canvas === canvas) this.canvas = null;
  }

  /** 面の大きさ（CSS px）と装置の画素の比。 */
  resize(w: number, h: number, dpr: number): void {
    const d = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
    if (w === this.size.w && h === this.size.h && d === this.dpr) return;
    this.size = { w: Math.max(0, w), h: Math.max(0, h) };
    this.dpr = d;
    this.sizeCanvas();
    if (this.fitMode) this.w = fitView(this.total);
    this.changed(false);
  }

  /** 上部・下のカプセルを避ける余白。 */
  setInsets(insets: WaveInsets): void {
    const s = this.insets;
    if (
      s.top === insets.top &&
      s.right === insets.right &&
      s.bottom === insets.bottom &&
      s.left === insets.left
    ) {
      return;
    }
    this.insets = insets;
    this.invalidate();
  }

  setSession(session: string | null): void {
    this.session = session;
  }

  /** 開いたときの形（最初の表示が届くまで。全体を表示する）。 */
  setShape(shape: Shape): void {
    this.shape = shape;
    if (this.fitMode) this.w = fitView(shape.frames);
    else this.w = clampWindow(this.w, shape.frames);
    this.emitInfo();
    this.invalidate();
  }

  /** 重ねるもの（変われば描き直す）。 */
  setOverlay(overlay: WaveOverlay): void {
    if (overlay === this.overlay) return;
    this.overlay = overlay;
    this.invalidate();
  }

  /** 再生の先読みに要る区間（ビューポートの `spans` の先頭に入れる）。 */
  setPlaybackSpans(spans: readonly EditSpanDto[]): void {
    const same =
      spans.length === this.extraSpans.length &&
      spans.every(
        (s, i) => s.start === this.extraSpans[i]?.start && s.end === this.extraSpans[i]?.end,
      );
    if (same) return;
    this.extraSpans = [...spans];
    this.options.onIdle?.(this.viewport(), false);
  }

  /** 操作中（ドラッグの途中。波形の段を直さない・裏の仕事をしない）。 */
  setInteracting(on: boolean): void {
    if (this.interacting === on) return;
    this.interacting = on;
    if (!on) this.scheduleIdle();
  }

  /** 編集中のノード（キャッシュに固定する）。 */
  setFocus(node: number | null): void {
    this.focusNode = node;
  }

  /** 出力の長さ（フレーム）。 */
  get total(): number {
    return this.shape?.frames ?? 0;
  }

  get rate(): number {
    return this.shape?.rate ?? 0;
  }

  get channels(): number {
    return this.shape?.channels ?? 0;
  }

  /** 見ている区間。 */
  get window(): WaveWindow {
    return this.w;
  }

  /** 全体を表示しているか。 */
  get fitting(): boolean {
    return this.fitMode;
  }

  /** 描く領域（CSS px。面の中）。 */
  inner(): { x: number; y: number; w: number; h: number } {
    const i = this.insets;
    return {
      x: i.left,
      y: i.top,
      w: Math.max(0, this.size.w - i.left - i.right),
      h: Math.max(0, this.size.h - i.top - i.bottom),
    };
  }

  /** 縦の割り付け（描く領域の中の CSS px）。 */
  layout(): LaneLayout {
    return laneLayout(
      this.inner().h,
      Math.max(1, this.channels),
      this.metrics.ruler,
      this.metrics.overview,
    );
  }

  /** フレームの面の中の x（CSS px）。 */
  xOf(frame: number): number {
    const inner = this.inner();
    return inner.x + frameToX(frame, this.w, inner.w);
  }

  /** 面の中の x（CSS px）のフレーム（全体の中に収めた整数）。 */
  frameAt(x: number): number {
    const inner = this.inner();
    const f = Math.round(xToFrame(x - inner.x, this.w, inner.w));
    return Math.min(this.total, Math.max(0, f));
  }

  /** 当たり判定（x・y は面の中の CSS px）。 */
  hit(x: number, y: number, scene: Omit<HitScene, 'layout' | 'window' | 'width'>): HitTarget {
    const inner = this.inner();
    return hitTest(x - inner.x, y - inner.y, {
      ...scene,
      layout: this.layout(),
      window: this.w,
      width: inner.w,
    });
  }

  // ===== ズーム・パン =====

  /** 全体を表示（⌘0）。 */
  fit(): void {
    this.fitMode = true;
    this.setWindow(fitView(this.total), true);
  }

  /** ⌘+（dir > 0）・⌘−（dir < 0）：見ている区間の中央を動かさずに 2 倍。 */
  zoomStep(dir: number): void {
    this.zoomAt(dir > 0 ? ZOOM_STEP : 1 / ZOOM_STEP, this.inner().x + this.inner().w / 2);
  }

  /** ⌘ホイール・ピンチ：x（面の中の CSS px）の下のフレームを動かさずに `factor` 倍に拡大する。 */
  zoomAt(factor: number, x: number): void {
    const inner = this.inner();
    const anchor = xToFrame(x - inner.x, this.w, inner.w);
    const next = zoomAround(this.w, factor, anchor, this.total, minSpan(inner.w, this.total));
    this.fitMode = next.span >= this.total;
    this.setWindow(next, true);
  }

  /** ホイール・横スワイプ：dx（CSS px。正で右の時刻へ）だけ動かす。 */
  panBy(dx: number): void {
    const inner = this.inner();
    if (!(inner.w > 0)) return;
    this.setWindow(panBy(this.w, (dx / inner.w) * this.w.span, this.total), true);
  }

  /** 全体図を押した・ドラッグした：その x を見ている区間の中央にする。 */
  panToOverview(x: number): void {
    const inner = this.inner();
    const start = overviewStart(x - inner.x, inner.w, this.w, this.total);
    this.setWindow({ start, span: this.w.span }, true);
  }

  /** 再生位置が見ている区間の外へ出たら 1 画面ぶん飛ぶ（動いたら true）。 */
  follow(pos: number): boolean {
    const next = followPlayhead(this.w, pos, this.total);
    if (next === this.w) return false;
    this.setWindow(next, true);
    return true;
  }

  /** 区間を替える（`user` なら止まってから頼み直す）。 */
  setWindow(w: WaveWindow, user: boolean): void {
    const next = clampWindow(w, this.total);
    if (sameWindow(next, this.w)) return;
    this.w = next;
    this.changed(user);
  }

  private changed(user: boolean): void {
    this.emitInfo();
    this.invalidate();
    this.pump();
    if (user) this.scheduleIdle();
  }

  // ===== 表示の結果 =====

  /** Rust へ送る表示の要求。 */
  viewport(): EditAudioViewport {
    const total = this.total;
    const visible =
      total > 0
        ? {
            start: Math.max(0, Math.floor(this.w.start)),
            end: Math.min(total, Math.ceil(this.w.start + this.w.span)),
          }
        : null;
    const spans: EditSpanDto[] = [...this.extraSpans];
    if (visible && this.pcmMode()) {
      const aligned = alignSpan(visible.start, visible.end, total);
      if (aligned) spans.push(aligned);
    }
    return {
      type: 'audio',
      visible: visible && visible.end > visible.start ? visible : null,
      spans: spans.slice(0, MAX_SPANS),
      interacting: this.interacting,
      focus: this.focusNode,
      work: false,
    };
  }

  /** 表示の結果を受ける。 */
  applyView(view: EditAudioView): void {
    if (this.disposed) return;
    const shape = { rate: view.rate, channels: view.channels, frames: view.frames };
    const reshaped =
      !this.shape ||
      this.shape.rate !== shape.rate ||
      this.shape.channels !== shape.channels ||
      this.shape.frames !== shape.frames;
    this.shape = shape;
    this.view = view;
    // 変わった区間の PCM を捨てる（読んでいる途中のものも古い）
    if (view.changed.type === 'all' || view.changed.spans.length > 0) {
      this.pcm.invalidate(view.changed);
      this.pcmEpoch++;
      this.loadingPcm.clear();
      this.failedPcm.clear();
    }
    // 波形の段：前に返した段から変わった区間のタイルを捨てる
    const peaks = view.peaks;
    if (peaks.changed.type === 'all' || peaks.changed.spans.length > 0) {
      this.tiles.invalidate(peaks.changed, peaks.tileBins || TILE_BINS);
      this.tileEpoch++;
      this.loadingTiles.clear();
    }
    if (peaks.rev !== this.peaks?.rev) this.failedTiles.clear();
    this.peaks = peaks;
    if (reshaped) {
      this.w = this.fitMode ? fitView(shape.frames) : clampWindow(this.w, shape.frames);
    }
    this.workWanted = view.work || peaks.pending;
    this.emitInfo();
    this.pump();
    this.invalidate();
    if (this.workWanted) this.scheduleIdle();
  }

  /** 止まって 150 ms で頼み直す（裏の仕事が残っていれば `work: true` で、同じでも送る）。 */
  private scheduleIdle(): void {
    if (this.idleId !== null) this.scheduler.cancelLater(this.idleId);
    this.idleId = this.scheduler.later(() => {
      this.idleId = null;
      if (this.disposed) return;
      if (this.interacting) return;
      const vp = this.viewport();
      if (this.workWanted) {
        this.options.onIdle?.({ ...vp, work: true }, true);
      } else {
        this.options.onIdle?.(vp, false);
      }
    }, WAVE_IDLE_MS);
  }

  // ===== 読み込み =====

  /** 1 列が 256 フレームより細かく、PCM で描くか。 */
  private pcmMode(): boolean {
    const columns = Math.max(1, Math.round(this.inner().w * this.dpr));
    return this.w.span / columns < PCM_BELOW;
  }

  /** 選んだ段（PCM で描く・段がなければ null）と、試す順の段。 */
  private binsToTry(): { chosen: number | null; order: number[] } {
    const bins = this.peaks?.bins ?? [];
    const columns = Math.max(1, Math.round(this.inner().w * this.dpr));
    const chosen = pickBin(bins, this.w.span, columns);
    const at = chosen === null ? 0 : Math.max(0, bins.indexOf(chosen));
    return { chosen, order: bins.slice(at) };
  }

  /** 足りないタイル・PCM を取りに行く（全体図の最も粗い段 → 見ている区間の段 → PCM の順）。 */
  private pump(): void {
    if (this.disposed || !this.session) return;
    const peaks = this.peaks;
    const total = this.total;
    if (peaks && peaks.rev !== null && peaks.frames > 0) {
      const tileBins = peaks.tileBins || TILE_BINS;
      const coarsest = peaks.bins[peaks.bins.length - 1];
      const wants: [number, number][] = [];
      if (coarsest !== undefined) {
        const [a, b] = tileRange(0, peaks.frames, coarsest, peaks.frames, tileBins);
        for (let t = a; t < b; t++) wants.push([coarsest, t]);
      }
      const { chosen, order } = this.binsToTry();
      // PCM で描くときも、届くまで埋める最も細かい段を取る
      const bin = chosen ?? order[0];
      if (bin !== undefined && bin !== coarsest) {
        const [a, b] = tileRange(
          this.w.start,
          this.w.start + this.w.span,
          bin,
          peaks.frames,
          tileBins,
        );
        for (let t = a; t < b; t++) wants.push([bin, t]);
      }
      for (const [bin, tile] of wants) {
        if (this.tileLoads >= MAX_TILE_LOADS) break;
        this.loadTile(bin, tile, peaks.rev);
      }
    }
    const view = this.view;
    if (view && total > 0 && this.pcmMode()) {
      const first = chunkOf(this.w.start);
      const last = chunkOf(Math.max(this.w.start, this.w.start + this.w.span - 1));
      for (let i = first; i <= last; i++) {
        if (this.pcmLoads >= MAX_PCM_LOADS) break;
        this.loadPcm(i, view);
      }
    }
  }

  private loadTile(bin: number, tile: number, rev: number): void {
    const key = `${bin}/${tile}`;
    if (this.tiles.peek(bin, tile) || this.loadingTiles.has(key)) return;
    if (this.failedTiles.has(`${key}@${rev}`)) return;
    const session = this.session;
    if (!session) return;
    let url: string;
    try {
      url = editPeaksUrl(session, rev, bin, tile);
    } catch {
      return;
    }
    const mark: Loading = { epoch: this.tileEpoch };
    this.loadingTiles.set(key, mark);
    this.tileLoads++;
    this.fetch(url)
      .then((buf) => {
        if (this.disposed || this.loadingTiles.get(key) !== mark) return;
        // 読んでいる間に段が替わった・捨てられたら使わない（今の rev で取り直す）
        if (mark.epoch !== this.tileEpoch || this.peaks?.rev !== rev) return;
        const parsed = parseTile(buf);
        if (!parsed || parsed.bin !== bin || parsed.tile !== tile) {
          this.failedTiles.add(`${key}@${rev}`);
          return;
        }
        this.tiles.put(bin, tile, rev, parsed);
        this.invalidate();
        this.emitInfo();
      })
      .catch(() => {
        if (!this.disposed) this.failedTiles.add(`${key}@${rev}`);
      })
      .finally(() => {
        if (this.loadingTiles.get(key) === mark) this.loadingTiles.delete(key);
        this.tileLoads--;
        if (!this.disposed) this.pump();
      });
  }

  private loadPcm(index: number, view: EditAudioView): void {
    if (this.pcm.has(index) || this.loadingPcm.has(index)) return;
    const span = chunkSpan(index, view.frames);
    const session = this.session;
    if (!span || !session) return;
    const [a, b] = span;
    if (!covered(view.valid, a, b)) return;
    const failKey = `${index}@${view.rev}`;
    if (this.failedPcm.has(failKey)) return;
    let url: string;
    try {
      url = editPcmUrl(session, view.rev, a, b);
    } catch {
      return;
    }
    const mark: Loading = { epoch: this.pcmEpoch };
    this.loadingPcm.set(index, mark);
    this.pcmLoads++;
    const decoder = EDIT_DECODER_PREFIX + view.samplesId;
    this.fetch(url)
      .then((buf) => {
        if (this.disposed || this.loadingPcm.get(index) !== mark) return;
        if (mark.epoch !== this.pcmEpoch) return;
        const pcm = readPcmWav(buf);
        if (
          !pcm ||
          pcm.info.start !== a ||
          pcm.frames !== b - a ||
          pcm.info.decoder !== decoder ||
          pcm.channels !== view.channels
        ) {
          this.failedPcm.add(failKey);
          return;
        }
        this.pcm.put(index, { rev: view.rev, start: a, frames: b - a, planes: pcm.planes });
        this.invalidate();
      })
      .catch(() => {
        if (!this.disposed) this.failedPcm.add(failKey);
      })
      .finally(() => {
        if (this.loadingPcm.get(index) === mark) this.loadingPcm.delete(index);
        this.pcmLoads--;
        if (!this.disposed) this.pump();
      });
  }

  // ===== 描く =====

  /** 描き直しを頼む（次の描く時刻に 1 回）。 */
  invalidate(): void {
    if (this.disposed || this.frameId !== null) return;
    this.frameId = this.scheduler.frame(() => {
      this.frameId = null;
      this.render();
    });
  }

  private sizeCanvas(): void {
    const c = this.canvas;
    if (!c) return;
    const w = Math.max(1, Math.round(this.size.w * this.dpr));
    const h = Math.max(1, Math.round(this.size.h * this.dpr));
    if (c.width !== w) c.width = w;
    if (c.height !== h) c.height = h;
  }

  /** 今の描画の中身（テストでも呼ぶ）。 */
  scene(): WaveScene {
    const inner = this.inner();
    const layout = this.layout();
    const channels = Math.max(1, this.channels);
    const columns = Math.max(0, Math.round(inner.w * this.dpr));
    if (this.lanes.length < columns * channels * 2)
      this.lanes = new Float32Array(columns * channels * 2);
    if (this.has.length < columns) this.has = new Uint8Array(columns);
    if (this.overviewCols.length < columns * channels * 2) {
      this.overviewCols = new Float32Array(columns * channels * 2);
    }
    if (this.overviewHas.length < columns) this.overviewHas = new Uint8Array(columns);
    const peaks = this.peaks;
    const total = this.total;
    let mode: WaveScene['mode'] = 'empty';
    let points: Float32Array[] | null = null;
    let pointsStart = 0;
    this.lanes.fill(0);
    this.has.fill(0);
    this.overviewCols.fill(0);
    this.overviewHas.fill(0);
    const peaksOk =
      peaks !== null && peaks.rev !== null && peaks.channels === channels && peaks.frames > 0;
    if (peaksOk && peaks) {
      const { chosen, order } = this.binsToTry();
      // 公開されている段の長さが今の出力と違う間は、重なる所だけ描く
      const frames = Math.min(peaks.frames, total);
      if (chosen === null && this.view) {
        mode = 'pcm';
        const filled = fillPcmColumns(
          {
            get: (i) => this.pcm.peek(i),
            channels,
            frames: total,
            start: this.w.start,
            span: this.w.span,
            columns,
          },
          this.lanes,
          this.has,
        );
        // 届くまでは最も細かい段で描く
        if (filled < columns) this.fillMissingFromPeaks(order, frames, columns, channels);
      } else {
        mode = 'peaks';
        fillPeakColumns(
          {
            cache: this.tiles,
            bins: order,
            channels,
            frames,
            start: this.w.start,
            span: this.w.span,
            columns,
            tileBins: peaks.tileBins || TILE_BINS,
          },
          this.lanes,
          this.has,
        );
      }
      fillPeakColumns(
        {
          cache: this.tiles,
          bins: [...peaks.bins].reverse(),
          channels,
          frames,
          start: 0,
          span: Math.max(1, total),
          columns,
          tileBins: peaks.tileBins || TILE_BINS,
        },
        this.overviewCols,
        this.overviewHas,
      );
    } else if (this.view && total > 0 && this.w.span / Math.max(1, columns) < PCM_BELOW) {
      // 段がまだない短い音声の拡大：PCM で描く
      mode = 'pcm';
      fillPcmColumns(
        {
          get: (i) => this.pcm.peek(i),
          channels,
          frames: total,
          start: this.w.start,
          span: this.w.span,
          columns,
        },
        this.lanes,
        this.has,
      );
    }
    // 1 フレームが 8 装置画素を超えたら標本の点
    if (mode === 'pcm' && columns > 0 && columns / this.w.span > SAMPLE_POINTS_PX) {
      const pts = this.samplePoints(channels);
      if (pts) {
        points = pts.points;
        pointsStart = pts.start;
      }
    }
    this.lastMode = mode;
    return {
      width: Math.round(this.size.w * this.dpr),
      height: Math.round(this.size.h * this.dpr),
      dpr: this.dpr,
      inner,
      layout,
      window: this.w,
      total,
      rate: this.rate,
      channels,
      columns,
      lanes: this.lanes,
      has: this.has,
      mode,
      points,
      pointsStart,
      overviewColumns: columns,
      overview: this.overviewCols,
      overviewHas: this.overviewHas,
      ticks: rulerTicks(this.w, inner.w, this.rate),
      overlay: this.overlay,
      colors: this.colors,
      metrics: this.metrics,
    };
  }

  /** PCM で埋められなかった列を段で埋める。 */
  private fillMissingFromPeaks(
    order: number[],
    frames: number,
    columns: number,
    channels: number,
  ): void {
    const peaks = this.peaks;
    if (!peaks) return;
    const out = new Float32Array(columns * channels * 2);
    const has = new Uint8Array(columns);
    fillPeakColumns(
      {
        cache: this.tiles,
        bins: order,
        channels,
        frames,
        start: this.w.start,
        span: this.w.span,
        columns,
        tileBins: peaks.tileBins || TILE_BINS,
      },
      out,
      has,
    );
    for (let c = 0; c < columns; c++) {
      if (this.has[c] || !has[c]) continue;
      this.has[c] = 1;
      for (let k = 0; k < channels * 2; k++)
        this.lanes[c * channels * 2 + k] = out[c * channels * 2 + k] ?? 0;
    }
  }

  /** 見ている区間の標本（チャンネルごと。チャンクがそろっていなければ null）。 */
  private samplePoints(channels: number): { start: number; points: Float32Array[] } | null {
    const start = Math.max(0, Math.floor(this.w.start));
    const end = Math.min(this.total, Math.ceil(this.w.start + this.w.span) + 1);
    if (end <= start) return null;
    const points = Array.from({ length: channels }, () => new Float32Array(end - start));
    for (let f = start; f < end;) {
      const chunk: PcmChunk | undefined = this.pcm.peek(chunkOf(f));
      if (!chunk || chunk.planes.length !== channels) return null;
      const stop = Math.min(end, chunk.start + chunk.frames);
      for (let ch = 0; ch < channels; ch++) {
        const plane = chunk.planes[ch];
        if (plane) points[ch]?.set(plane.subarray(f - chunk.start, stop - chunk.start), f - start);
      }
      if (stop <= f) return null;
      f = stop;
    }
    return { start, points };
  }

  private render(): void {
    if (this.disposed) return;
    const scene = this.scene();
    if (this.canvas) this.draw(this.canvas, scene);
  }

  /** 何で描いたか（最後の描画。テスト）。 */
  get mode(): WaveScene['mode'] {
    return this.lastMode;
  }

  private emitInfo(): void {
    const peaks = this.peaks;
    const info: WaveInfo = {
      window: this.w,
      pending: peaks ? peaks.pending || peaks.rev === null : true,
      building: !peaks || peaks.rev === null,
    };
    const key = `${info.window.start}:${info.window.span}:${info.pending}:${info.building}`;
    if (key === this.infoKey) return;
    this.infoKey = key;
    this.options.onInfo?.(info);
  }

  /** 捨てる。 */
  dispose(): void {
    this.disposed = true;
    if (this.frameId !== null) this.scheduler.cancelFrame(this.frameId);
    if (this.idleId !== null) this.scheduler.cancelLater(this.idleId);
    this.frameId = null;
    this.idleId = null;
    this.canvas = null;
    this.loadingTiles.clear();
    this.loadingPcm.clear();
  }
}
