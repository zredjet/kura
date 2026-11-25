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
 * - 取れなかった PCM のチャンクは印を付け、期限（1 秒から倍々で 8 秒まで）が来たら取り直す（503 の Busy もある）。
 * - 表示の結果を受けたら：`changed` に掛かる PCM のチャンクを捨て、`peaks.changed` に掛かるタイルを捨てる。
 *   `peaks.pending` か `work` が真なら（ラウドネスを計っている間も：`setExtraWork`）、手が止まって 150 ms たったら
 *   `work: true` で頼み直す（同じビューポートでも送る）。そのあと仕事が残っている間は、その頼みの応えが届いたら次のタスク
 *   で次を頼む（連鎖。1 本ずつ）。手・ズーム・パン・再生の頼み・ドラッグが来たら連鎖をやめ、また手が止まって 150 ms から
 *   始める（`noteSent`・`setWindow`・`setPlaybackSpans`・`setInteracting`）。
 *   応えが「進まなかった」（`workStalled`。源が読めない・予算が空いていない）なら連鎖をやめて待ち（`stallWaitMs`。150 ms、
 *   続けば 1 秒から倍々に 8 秒まで）、進めば連鎖と待ちを元に戻す。
 * - ズーム・パンで描き直すのは `requestAnimationFrame` の 1 回だけ（汚れたときだけ）。止まって 150 ms で今の区間を頼み直す。
 *   列ごとの [min, max] は使い回す型付きの配列に入れる。
 * - 公開されている段の長さが今の出力と違う間（尾を作り直している）は、重なる所だけ描く（段の長さで切る）。レートが
 *   違う間（レートを変えた直後）は、段を時刻で写して描き、タイルも時刻で写した区間で選ぶ。
 * - 描く関数・取得・時計は差し込める（テスト）。
 */

import type {
  EditAudioView,
  EditAudioViewport,
  EditMarkerDto,
  EditPeaksInfoDto,
  EditSpanDto,
  EditSpecRequestDto,
} from '../../../ipc/edit';
import { editPcmUrl, editPeaksUrl } from '../../../ipc/media';
import { readPcmWav } from '../../../lib/asset/wav';
import {
  alignSpan,
  CHUNK,
  chunkOf,
  chunkSpan,
  covered,
  FAIL_RETRY_MAX_MS,
  FAIL_RETRY_MS,
  FailureMarks,
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

/**
 * 裏の仕事が `count` 回続けて進まなかった応えのあとの待ち（ms）：1 回目は手が止まったときと同じ 150 ms、続けば PCM の取り直しと
 * 同じく 1 秒から倍々に 8 秒まで。
 */
export function stallWaitMs(count: number): number {
  if (count <= 1) return WAVE_IDLE_MS;
  return Math.min(FAIL_RETRY_MAX_MS, FAIL_RETRY_MS * 2 ** (count - 2));
}

/** 同時に取るタイルの数。 */
const MAX_TILE_LOADS = 4;
/** 同時に取る PCM のチャンクの数。 */
const MAX_PCM_LOADS = 4;
/** ⌘+・⌘− の 1 回の倍率。 */
export const ZOOM_STEP = 2;
/** PCM の表に再生の先読みのために空けておくチャンクの数（再生位置から 2 つ・ループの始めから 2 つ）。 */
const PLAYBACK_RESERVE_CHUNKS = 4;
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
  /**
   * フェードの三角の位置（`inEnd`・`outStart`）と、曲線の見かけの始まり・終わり（`inStart`・`outEnd`。出力の範囲の外も
   * ある。null なら曲線を描かない）、三角のハンドルを出すか。長さ 0 の側は描かない。
   */
  fade: {
    inStart: number | null;
    inEnd: number;
    outStart: number;
    outEnd: number | null;
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

/** kura:// のバイト列を取る（2xx でなければ `WaveHttpError`）。 */
export async function fetchBytes(url: string): Promise<ArrayBuffer> {
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

/** 本物の時計（requestAnimationFrame と setTimeout）。 */
export const realScheduler: WaveScheduler = {
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
  /** 時計（ms。取れなかった PCM を取り直すまでの期限）。 */
  now?(): number;
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
  /** 頭の文書の流し解きを裏で進めている（表示は頭より古い。M17 段 3）：操作中でも、手が止まったら裏の仕事を頼む。 */
  private solving = false;
  private focusNode: number | null = null;
  private frameId: number | null = null;
  private idleId: number | null = null;
  /** 連鎖の次の頼み（次のタスク）。 */
  private chainId: number | null = null;
  /**
   * 連鎖している（手が止まって 150 ms で `work: true` を頼み、そのあとほかの頼み・手が出ていない）。真の間は、`work: true`
   * の頼みの応えが届いたら次のタスクで次を頼む。
   */
  private chaining = false;
  private workWanted = false;
  /** 裏の仕事の頼みの応えが続けて進まなかった数（進めば 0）。 */
  private stalls = 0;
  /** 表示の結果のほかに残っている裏の仕事の理由（長い音声のラウドネス・スペクトルの区間の平均を計っている）。 */
  private readonly extraReasons = new Set<string>();
  /** スペクトルの面の頼み（M17 段 5。null なら送らない：波形のときは今までと同じ頼み）。 */
  private spec: EditSpecRequestDto[] | null = null;
  /** 一時的に外して聞くノード（M17 段 6）。 */
  private auditionBypass: number | null = null;
  /** 細い波形の帯（スペクトルの面の上。曲の全体・1 段にまとめる・ズームしない）と、入る前の区間。 */
  private strip = false;
  private beforeStrip: { w: WaveWindow; fit: boolean } | null = null;
  private stripLanes = new Float32Array(0);
  private stripHas = new Uint8Array(0);
  private tileLoads = 0;
  private pcmLoads = 0;
  private readonly loadingTiles = new Map<string, Loading>();
  private readonly failedTiles = new Set<string>();
  private readonly loadingPcm = new Map<number, Loading>();
  /** 取れなかった PCM のチャンク（`{番号}@{rev}`。期限が来たら取り直す）。 */
  private readonly failedPcm: FailureMarks;
  private retryId: number | null = null;
  private retryAt = Number.POSITIVE_INFINITY;
  private readonly now: () => number;
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
  /** 描いた時間（ms）を受ける（波形の描画の自己点検 `VITE_KURA_CHECK`）。 */
  renderProbe: ((ms: number) => void) | null = null;
  /** 取りに行った数（自己点検）。 */
  readonly stats = { tileLoads: 0, pcmLoads: 0 };

  constructor(private readonly options: WaveOptions = {}) {
    this.draw = options.draw ?? (() => undefined);
    this.fetch = options.fetch ?? fetchBytes;
    this.scheduler = options.scheduler ?? realScheduler;
    this.pcm = options.pcm ?? new PcmChunkCache();
    this.tiles = options.tiles ?? new PeakTileCache();
    this.colors = options.colors ?? DEFAULT_COLORS;
    this.metrics = options.metrics ?? DEFAULT_METRICS;
    this.now = options.now ?? (() => Date.now());
    this.failedPcm = new FailureMarks(this.now);
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
    this.stalls = 0;
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

  /**
   * 一時的に外して聞くノード（M17 段 6）：頼みに `audition` を載せる（外した音を再生の先読みの区間で描いてもらう。表示の
   * 区間はそのまま：外した音を解いている間は表示の音を鳴らす）。替わったらすぐ頼む。
   */
  setAudition(bypass: number | null): void {
    if (this.auditionBypass === bypass) return;
    this.auditionBypass = bypass;
    this.breakChain();
    this.options.onIdle?.({ ...this.viewport(), work: this.solving }, false);
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
    // 再生の頼みを先にする（裏の仕事の続きは、また 150 ms から）。流し解きの間は、再生の頼みでも流し解きを進める（再生が
    // 続くと連鎖が切れ続けるため）
    this.breakChain();
    this.options.onIdle?.({ ...this.viewport(), work: this.solving }, false);
  }

  /** 操作中（ドラッグの途中。波形の段を直さない・裏の仕事をしない）。 */
  setInteracting(on: boolean): void {
    if (this.interacting === on) return;
    this.interacting = on;
    if (on) {
      this.stopChain();
      // 流し解きは操作中でも進める（押したまま止めて聴けば新しい音に替わる）
      if (this.solving) this.scheduleIdle();
    } else this.scheduleIdle();
  }

  /**
   * 表示の結果のほかに残っている裏の仕事（長い音声のラウドネスを計っている。計りは edit_view { work: true } の区切りで
   * だけ進む）。真の間は、表示の work が偽でも、手が止まって 150 ms で work: true を頼み、応えのたびに続ける。
   */
  setExtraWork(on: boolean, reason = 'loudness'): void {
    if (this.extraReasons.has(reason) === on) return;
    if (on) this.extraReasons.add(reason);
    else this.extraReasons.delete(reason);
    this.workWanted =
      (this.view?.work ?? false) || (this.peaks?.pending ?? false) || this.extraWork;
    // 連鎖している間は、届く応えが続ける（同時に 2 本頼まない）
    if (this.workWanted && !this.chaining) this.scheduleIdle();
  }

  /**
   * 要求を送った（核の `sent`。`work` は、手を含まない `work: true` の頼み＝裏の仕事の頼みか）。ほかの要求（手・再生・
   * 表示の頼み）が出たら、そちらが先なので連鎖をやめ、また手が止まって 150 ms から始める。
   */
  noteSent(work: boolean): void {
    if (this.disposed || work) return;
    this.breakChain();
  }

  /** 表示できなかった・要求が通らなかった：連鎖をやめる（次の手・ズームなどのあとに 150 ms から始める）。 */
  viewFailed(): void {
    this.stopChain();
  }

  /** 表示の結果のほかに残っている裏の仕事があるか。 */
  private get extraWork(): boolean {
    return this.extraReasons.size > 0;
  }

  /** 編集中のノード（キャッシュに固定する）。 */
  setFocus(node: number | null): void {
    this.focusNode = node;
  }

  /**
   * スペクトルの面の頼み（M17 段 5）：手・再生・裏の仕事のすべての頼みに載せる（Rust は頼みに入った spec でしかタイルを
   * 作らない）。変わったら止まって 150 ms で頼む。null なら載せない。
   */
  setSpec(spec: EditSpecRequestDto[] | null): void {
    if (JSON.stringify(spec) === JSON.stringify(this.spec)) return;
    this.spec = spec === null ? null : spec.map((r) => ({ ...r, spans: [...r.spans] }));
    if (!this.interacting || this.solving) this.scheduleIdle();
  }

  /**
   * 細い波形の帯にする（スペクトルの面の上。M17 段 5）：曲の全体を 1 段にまとめて描き、ズーム・パンしない。見えている区間は
   * 全体を頼み、PCM の区間は頼まない。戻すと前の区間に戻る。
   */
  setStrip(on: boolean): void {
    if (this.strip === on) return;
    this.strip = on;
    if (on) {
      this.beforeStrip = { w: this.w, fit: this.fitMode };
      this.fitMode = true;
      this.w = fitView(this.total);
    } else {
      const back = this.beforeStrip;
      this.beforeStrip = null;
      this.fitMode = back?.fit ?? true;
      this.w = this.fitMode ? fitView(this.total) : clampWindow(back?.w ?? this.w, this.total);
    }
    this.changed(true);
  }

  /** 細い波形の帯か。 */
  get stripMode(): boolean {
    return this.strip;
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
      this.strip ? 1 : Math.max(1, this.channels),
      this.metrics.ruler,
      this.strip ? 0 : this.metrics.overview,
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
    if (this.strip) return;
    this.fitMode = true;
    this.setWindow(fitView(this.total), true);
  }

  /** ⌘+（dir > 0）・⌘−（dir < 0）：見ている区間の中央を動かさずに 2 倍。 */
  zoomStep(dir: number): void {
    this.zoomAt(dir > 0 ? ZOOM_STEP : 1 / ZOOM_STEP, this.inner().x + this.inner().w / 2);
  }

  /** ⌘ホイール・ピンチ：x（面の中の CSS px）の下のフレームを動かさずに `factor` 倍に拡大する。 */
  zoomAt(factor: number, x: number): void {
    if (this.strip) return;
    const inner = this.inner();
    const anchor = xToFrame(x - inner.x, this.w, inner.w);
    const next = zoomAround(this.w, factor, anchor, this.total, minSpan(inner.w, this.total));
    this.fitMode = next.span >= this.total;
    this.setWindow(next, true);
  }

  /** ホイール・横スワイプ：dx（CSS px。正で右の時刻へ）だけ動かす。 */
  panBy(dx: number): void {
    const inner = this.inner();
    if (this.strip || !(inner.w > 0)) return;
    this.setWindow(panBy(this.w, (dx / inner.w) * this.w.span, this.total), true);
  }

  /** 全体図を押した・ドラッグした：その x を見ている区間の中央にする。 */
  panToOverview(x: number): void {
    if (this.strip) return;
    const inner = this.inner();
    const start = overviewStart(x - inner.x, inner.w, this.w, this.total);
    this.setWindow({ start, span: this.w.span }, true);
  }

  /** 再生位置が見ている区間の外へ出たら 1 画面ぶん飛ぶ（動いたら true）。 */
  follow(pos: number): boolean {
    if (this.strip) return false;
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
    if (user) {
      // ズーム・パンを先にする（止まって 150 ms で今の区間を頼み、裏の仕事もそこから続ける）
      this.stopChain();
      this.scheduleIdle();
    }
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
    // 細い波形の帯（スペクトルの面）では、拡大の PCM の区間は頼まない
    if (visible && this.pcmMode() && !this.strip) {
      const aligned = alignSpan(visible.start, visible.end, total);
      if (aligned) spans.push(aligned);
    }
    const vp: EditAudioViewport = {
      type: 'audio',
      visible: visible && visible.end > visible.start ? visible : null,
      spans: spans.slice(0, MAX_SPANS),
      interacting: this.interacting,
      focus: this.focusNode,
      work: false,
    };
    // spec・audition は頼むときだけ載せる（今までと同じ頼み：同じものを送らない比べが崩れない）
    const withSpec = this.spec ? { ...vp, spec: this.spec } : vp;
    return this.auditionBypass === null
      ? withSpec
      : {
          ...withSpec,
          audition: { bypass: this.auditionBypass, spans: this.extraSpans.slice(0, MAX_SPANS) },
        };
  }

  /**
   * 表示の結果を受ける（`requestedWork` は、その結果の要求が裏の仕事の頼み（`work: true`）だったか。連鎖を続けるのは
   * その応えだけ）。
   */
  applyView(view: EditAudioView, requestedWork = false): void {
    if (this.disposed) return;
    const shape = { rate: view.rate, channels: view.channels, frames: view.frames };
    const reshaped =
      !this.shape ||
      this.shape.rate !== shape.rate ||
      this.shape.channels !== shape.channels ||
      this.shape.frames !== shape.frames;
    this.shape = shape;
    this.view = view;
    this.solving = view.solving !== null;
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
    this.workWanted = view.work || peaks.pending || this.extraWork;
    // 裏の仕事の頼みの応えだけが、進んだかを知らせる（ほかの要求の応えでは数えない）
    const stalled = requestedWork && view.workStalled;
    if (requestedWork) this.stalls = stalled ? this.stalls + 1 : 0;
    this.emitInfo();
    this.pump();
    this.invalidate();
    if (!this.workWanted) {
      // 仕事がなくなった（手の応えの中で直し切ったときも）：進まなかった数も戻す（次の最初の待ちは 150 ms）
      this.stalls = 0;
      this.stopChain();
    } else if (this.chaining) {
      if (stalled) {
        // 進まない間は、応えの速さで頼み続けない（待ってから頼み直す。手・ズーム・再生が来たらそちらを先にする）
        this.stopChain();
        this.scheduleIdle(stallWaitMs(this.stalls));
      } else if (requestedWork) {
        // 連鎖の頼みの応えなら、次のタスクで次を頼む（応えの直後に核が送る、ためていた手・表示の頼みを先にする。それが
        // 出たら `noteSent` が連鎖をやめる）。ほかの要求の応えなら、そのあとに出る連鎖の頼みの応えを待つ
        this.scheduleChain();
      }
    } else {
      this.scheduleIdle();
    }
  }

  /** 止まって `ms`（既定は 150 ms）で頼み直す（裏の仕事が残っていれば `work: true` で、同じでも送り、連鎖を始める）。 */
  private scheduleIdle(ms: number = WAVE_IDLE_MS): void {
    if (this.idleId !== null) this.scheduler.cancelLater(this.idleId);
    this.idleId = this.scheduler.later(() => {
      this.idleId = null;
      if (this.disposed) return;
      if (this.interacting && !this.solving) return;
      const vp = this.viewport();
      if (this.workWanted) {
        // 送るとすぐ核の `sent` が `noteSent` を呼ぶので、先に連鎖に入る
        this.chaining = true;
        this.options.onIdle?.({ ...vp, work: true }, true);
      } else {
        this.options.onIdle?.(vp, false);
      }
    }, ms);
  }

  /** 連鎖の次の頼みを次のタスクで出す（それまでに仕事がなくなった・ドラッグが始まったら出さない）。 */
  private scheduleChain(): void {
    if (this.chainId !== null) return;
    this.chainId = this.scheduler.later(() => {
      this.chainId = null;
      if (this.disposed || !this.chaining) return;
      if (!this.workWanted || (this.interacting && !this.solving)) {
        this.chaining = false;
        return;
      }
      this.options.onIdle?.({ ...this.viewport(), work: true }, true);
    }, 0);
  }

  /** 連鎖をやめる（待っている次の頼みも出さない）。 */
  private stopChain(): void {
    this.chaining = false;
    if (this.chainId !== null) this.scheduler.cancelLater(this.chainId);
    this.chainId = null;
  }

  /** ほかの頼みが先に出た：連鎖をやめ、仕事が残っていれば手が止まって 150 ms から始め直す。 */
  private breakChain(): void {
    this.stopChain();
    if (this.workWanted) this.scheduleIdle();
  }

  // ===== 読み込み =====

  /** 1 列が 256 フレームより細かく、見えているチャンクが PCM の表に収まるので PCM で描くか。 */
  private pcmMode(): boolean {
    const columns = Math.max(1, Math.round(this.inner().w * this.dpr));
    return this.w.span / columns < PCM_BELOW && this.pcmFits();
  }

  /**
   * 見えているチャンク（と再生の先読みの分）が PCM の表の上限に収まるか。収まらなければ（多チャンネルを広い画面で深く
   * 拡大した）、追い出しと取り直しが止まらなくなるので段で描く。
   */
  private pcmFits(): boolean {
    const first = chunkOf(this.w.start);
    const last = chunkOf(Math.max(this.w.start, this.w.start + this.w.span - 1));
    const chunkBytes = CHUNK * Math.max(1, this.channels) * Float32Array.BYTES_PER_ELEMENT;
    return (last - first + 1 + PLAYBACK_RESERVE_CHUNKS) * chunkBytes <= this.pcm.limit;
  }

  /**
   * 段のフレーム ÷ 出力のフレーム。公開されている段のレートが今の出力と違う間（レートを変えた直後、長い音声の段を作り
   * 直している）は、段を時刻で写して描く・選ぶ（フレームの番号のまま描くと縮んで尾が平らになる）。同じなら 1。
   */
  private peaksScale(): number {
    const peaks = this.peaks;
    const rate = this.rate;
    return peaks && peaks.rate > 0 && rate > 0 && peaks.rate !== rate ? peaks.rate / rate : 1;
  }

  /** 選んだ段（PCM で描く・段がなければ null）と、試す順の段。 */
  private binsToTry(): { chosen: number | null; order: number[] } {
    const bins = this.peaks?.bins ?? [];
    const columns = Math.max(1, Math.round(this.inner().w * this.dpr));
    // PCM で描くかは出力のフレームで決め（表に収まらない拡大は段）、段は段のフレーム（時刻で写した区間）で選ぶ
    const chosen = this.pcmMode()
      ? null
      : pickBin(bins, this.w.span * this.peaksScale(), columns, 0);
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
        const k = this.peaksScale();
        const [a, b] = tileRange(
          this.w.start * k,
          (this.w.start + this.w.span) * k,
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
    this.stats.tileLoads++;
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
    if (this.failedPcm.blocked(failKey)) return;
    let url: string;
    try {
      url = editPcmUrl(session, view.rev, a, b);
    } catch {
      return;
    }
    const mark: Loading = { epoch: this.pcmEpoch };
    this.loadingPcm.set(index, mark);
    this.pcmLoads++;
    this.stats.pcmLoads++;
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
          this.pcmFailed(failKey);
          return;
        }
        this.failedPcm.ok(failKey);
        this.pcm.put(index, { rev: view.rev, start: a, frames: b - a, planes: pcm.planes });
        this.invalidate();
      })
      .catch(() => {
        if (!this.disposed && mark.epoch === this.pcmEpoch) this.pcmFailed(failKey);
      })
      .finally(() => {
        if (this.loadingPcm.get(index) === mark) this.loadingPcm.delete(index);
        this.pcmLoads--;
        if (!this.disposed) this.pump();
      });
  }

  /** PCM を取れなかった：印を付け、期限が来たら取り直す（503 の Busy のような一時的な失敗があるため）。 */
  private pcmFailed(key: string): void {
    this.failedPcm.fail(key);
    this.retryIn(this.failedPcm.waitOf(key) ?? FAIL_RETRY_MS);
  }

  /** `ms` 後に足りないものを取りに行く（もっと早く頼んであればそのまま）。 */
  private retryIn(ms: number): void {
    const at = this.now() + ms;
    if (this.retryId !== null) {
      if (this.retryAt <= at) return;
      this.scheduler.cancelLater(this.retryId);
    }
    this.retryAt = at;
    this.retryId = this.scheduler.later(() => {
      this.retryId = null;
      this.retryAt = Number.POSITIVE_INFINITY;
      if (!this.disposed) this.pump();
    }, ms);
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
      // 段のフレームで読む（レートが違う間は時刻で写す）。公開されている段の長さが今の出力と違う間は、重なる所だけ描く
      const k = this.peaksScale();
      const frames = Math.min(peaks.frames, Math.round(total * k));
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
        if (filled < columns) this.fillMissingFromPeaks(order, frames, columns, channels, k);
      } else {
        mode = 'peaks';
        fillPeakColumns(
          {
            cache: this.tiles,
            bins: order,
            channels,
            frames,
            start: this.w.start * k,
            span: this.w.span * k,
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
          span: Math.max(1, total) * k,
          columns,
          tileBins: peaks.tileBins || TILE_BINS,
        },
        this.overviewCols,
        this.overviewHas,
      );
    } else if (this.view && total > 0 && this.pcmMode()) {
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
    // 細い波形の帯：チャンネルを 1 段にまとめる（列ごとに最小・最大）
    let lanes = this.lanes;
    let has = this.has;
    let shownChannels = channels;
    if (this.strip) {
      if (this.stripLanes.length < columns * 2) this.stripLanes = new Float32Array(columns * 2);
      if (this.stripHas.length < columns) this.stripHas = new Uint8Array(columns);
      for (let col = 0; col < columns; col++) {
        let lo = Number.POSITIVE_INFINITY;
        let hi = Number.NEGATIVE_INFINITY;
        for (let ch = 0; ch < channels; ch++) {
          const i = (col * channels + ch) * 2;
          lo = Math.min(lo, this.lanes[i] ?? 0);
          hi = Math.max(hi, this.lanes[i + 1] ?? 0);
        }
        this.stripLanes[col * 2] = Number.isFinite(lo) ? lo : 0;
        this.stripLanes[col * 2 + 1] = Number.isFinite(hi) ? hi : 0;
        this.stripHas[col] = this.has[col] ?? 0;
      }
      lanes = this.stripLanes;
      has = this.stripHas;
      shownChannels = 1;
      points = null;
    }
    return {
      width: Math.round(this.size.w * this.dpr),
      height: Math.round(this.size.h * this.dpr),
      dpr: this.dpr,
      inner,
      layout,
      window: this.w,
      total,
      rate: this.rate,
      channels: shownChannels,
      columns,
      lanes,
      has,
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

  /** PCM で埋められなかった列を段で埋める（`k` は段のフレーム ÷ 出力のフレーム）。 */
  private fillMissingFromPeaks(
    order: number[],
    frames: number,
    columns: number,
    channels: number,
    k: number,
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
        start: this.w.start * k,
        span: this.w.span * k,
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
    const probe = this.renderProbe;
    const t0 = probe ? performance.now() : 0;
    const scene = this.scene();
    if (this.canvas) this.draw(this.canvas, scene);
    if (probe) probe(performance.now() - t0);
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
    if (this.retryId !== null) this.scheduler.cancelLater(this.retryId);
    this.stopChain();
    this.frameId = null;
    this.idleId = null;
    this.retryId = null;
    this.canvas = null;
    this.loadingTiles.clear();
    this.loadingPcm.clear();
  }
}
