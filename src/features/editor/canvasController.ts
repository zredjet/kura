/**
 * エディタのキャンバスの制御（React の外。docs/editing.md §12.8・§12.13）。
 *
 * - 1 枚の `<canvas>` に、kura:// の 256 のタイルを解いた絵（ImageBitmap）を `drawImage` で描く。拡大は最近傍、
 *   装置の画素で 1 倍未満の縮小は補間。ドット絵は装置の画素の整数倍に吸着する（`viewMath`）。
 * - ズーム・パンはここで描き直し、Zustand には倍率の表示（と、絵が出そろったか）だけを間引いて送る（`onInfo`）。
 *   止まって 100ms たったら、表示する範囲と倍率を頼み直す（`onIdle`。段は Rust が決める）。それまでは今の段のタイルを
 *   広げて・縮めて見せ、段が替わった直後は前の段のタイルで足りない所を埋める。
 * - タイルは「最後に変わった rev」で指す（`TileTable`）。表示の結果を受けたら、見えていて、要求した範囲の中にある
 *   タイルのうち、その rev の絵を持っていないものだけを取りに行く。古い rev の URL が 404 なら（WebKit も覚えていない）、
 *   今の表示の rev で取り直す（変わっていないので中身は同じ）。
 * - 仮の筆（`stroke.ts`）を上に重ね、要求の結果のタイルを解き終えたら外す。
 * - 市松模様（透明を持てる形式のとき常に）は canvas の下の要素に敷く（画面に固定した模様を画像の矩形で切る）。
 *   消しゴム・置き換えの仮の筆は canvas を destination-out で抜くので、下の模様が見える。
 * - 描く関数・タイルを読む口・時計は差し込める（テスト）。閉じたら（`dispose`）絵をすべて close する。
 */

import type { EditFrameDto, EditViewDto, EditViewportDto } from '../../ipc/edit';
import { editTileUrl, type EditTileFormat } from '../../ipc/media';
import type { Point } from '../../lib/paint';
import {
  OverlayTracker,
  type OverlayChunk,
  type OverlayMode,
  type PendingStroke,
  StrokeBuilder,
  StrokeOverlay,
  type StrokeSpec,
  CHUNK,
  overlayMode,
  strokeAction,
} from './stroke';
import { type TileBitmap, TileTable, tileKey } from './tileTable';
import { loadTile, TILE_FORMAT, TileHttpError } from './tiles';
import {
  clampTransform,
  fitArea,
  fitTransform,
  type FrameRect,
  formatCanBeTransparent,
  imagePixel,
  imageScreenRect,
  inRange,
  type Insets,
  intersect,
  nextZoom,
  type Rect,
  roundRect,
  scaleRect,
  type Size,
  safeDpr,
  smoothingFor,
  snapOffset,
  snapZoom,
  type TileRange,
  tileImageRect,
  tileRange,
  tileSize,
  tilesIn,
  toScreen,
  type Transform,
  visibleImageRect,
  ZOOM_MAX,
  ZOOM_MIN,
  zoomAround,
  zoomPercent,
} from './viewMath';

/** 止まってから頼み直すまで（ms）。 */
export const IDLE_MS = 100;
/** 頼む範囲を面の外へ広げる量（CSS px。少し動かしてもすぐ見えるように）。 */
export const VIEW_MARGIN = 128;
/** 同時に読むタイルの数。 */
const MAX_LOADS = 6;

/** 描くタイル（装置の画素）。 */
export interface DrawTile {
  image: TileBitmap;
  dx: number;
  dy: number;
  dw: number;
  dh: number;
  smoothing: boolean;
  /** この矩形の中だけに描く（前の段のタイルで埋めるとき）。 */
  clip: Rect | null;
}

/** 描く仮の筆の区画（装置の画素）。 */
export interface DrawChunk {
  chunk: OverlayChunk;
  dx: number;
  dy: number;
  dw: number;
  dh: number;
}

export interface DrawOverlay {
  mode: OverlayMode;
  /** 縮めて見ているときだけ補間（細い線が消えないように）。 */
  smoothing: boolean;
  chunks: DrawChunk[];
}

/** 1 回の描画の中身。 */
export interface Scene {
  /** canvas の大きさ（装置の画素）。 */
  width: number;
  height: number;
  /** 画像の矩形（装置の画素。この中だけに描く）。 */
  clip: Rect | null;
  tiles: DrawTile[];
  overlays: DrawOverlay[];
  /** 市松模様を敷く矩形（CSS px。面に切ったもの。敷かなければ null）。 */
  checker: Rect | null;
}

/** 描く先（canvas と市松模様の要素）。 */
export interface Surfaces {
  canvas: HTMLCanvasElement;
  checker: HTMLElement | null;
}

export type DrawScene = (surfaces: Surfaces, scene: Scene) => void;
export type LoadTile = (
  url: string,
  format: EditTileFormat,
  w: number,
  h: number,
) => Promise<TileBitmap>;

/** 時計（描く時刻と、止まってからの待ち）。 */
export interface Scheduler {
  frame(cb: () => void): number;
  cancelFrame(id: number): void;
  later(cb: () => void, ms: number): number;
  cancelLater(id: number): void;
}

const realScheduler: Scheduler = {
  frame: (cb) => requestAnimationFrame(cb),
  cancelFrame: (id) => cancelAnimationFrame(id),
  later: (cb, ms) => window.setTimeout(cb, ms),
  cancelLater: (id) => window.clearTimeout(id),
};

/** Zustand へ送る表示の情報。 */
export interface ControllerInfo {
  zoom: number;
  level: number | null;
  /** 見えているタイルの絵が一度出そろったか（開いた直後の仮の絵を外す）。 */
  ready: boolean;
  /** 画像の画面の矩形（CSS px。仮の絵を置く）。 */
  imageRect: Rect | null;
}

export interface ControllerOptions {
  draw?: DrawScene;
  load?: LoadTile;
  scheduler?: Scheduler;
  format?: EditTileFormat;
  budget?: number;
  onInfo?: (info: ControllerInfo) => void;
  /** 止まってから 100ms：表示の範囲を頼み直す。 */
  onIdle?: (viewport: EditViewportDto) => void;
}

/** 画像の形。 */
export interface ImageModel {
  w: number;
  h: number;
  pixelArt: boolean;
}

/** 表示の結果のフレームの矩形。 */
export function frameOf(view: EditViewDto, frame: number): EditFrameDto | null {
  return (
    view.frames.find((f) => f.index === frame) ??
    (view.frames.length === 1 ? (view.frames[0] ?? null) : null)
  );
}

/** 筆を描き終えた結果（送る動作と、その仮の筆）。 */
export interface FinishedStroke {
  action: ReturnType<typeof strokeAction>;
  pending: PendingStroke;
}

export class CanvasController {
  private surfaces: Surfaces | null = null;
  private stage: Size = { w: 0, h: 0 };
  private dpr = 1;
  private insets: Insets = { top: 0, right: 0, bottom: 0, left: 0 };
  private image: ImageModel | null = null;
  private alpha = false;
  private view: EditViewDto | null = null;
  private shown: { level: number; frame: number; range: TileRange | null } | null = null;
  private fallbackLevel: number | null = null;
  private t: Transform = { zoom: 1, ox: 0, oy: 0 };
  private rawZoom = 1;
  private fitMode = true;
  private session: string | null = null;
  private builder: StrokeBuilder | null = null;
  private lastEnd: { frame: number; point: Point } | null = null;
  private loads = 0;
  private frameId: number | null = null;
  private idleId: number | null = null;
  private disposed = false;
  private ready = false;
  private infoKey = '';
  private readonly draw: DrawScene;
  private readonly load: LoadTile;
  private readonly scheduler: Scheduler;
  private readonly format: EditTileFormat;
  readonly tiles: TileTable<TileBitmap>;
  readonly overlays = new OverlayTracker();
  /** 表示しているフレーム。 */
  frame = 0;
  /** 編集中のノード（Rust のキャッシュに固定する）。 */
  focus: number | null = null;

  constructor(private readonly options: ControllerOptions = {}) {
    this.draw = options.draw ?? (() => undefined);
    this.load = options.load ?? loadTile;
    this.scheduler = options.scheduler ?? realScheduler;
    this.format = options.format ?? TILE_FORMAT;
    this.tiles = new TileTable(options.budget);
  }

  // ===== 面・画像 =====

  /** 描く先をつなぐ。 */
  attach(surfaces: Surfaces): void {
    this.surfaces = surfaces;
    this.sizeCanvas();
    this.invalidate();
  }

  /** 描く先を外す（同じ canvas のときだけ）。 */
  detach(canvas: HTMLCanvasElement): void {
    if (this.surfaces?.canvas === canvas) this.surfaces = null;
  }

  /** 面の大きさ（CSS px）と装置の画素の比。 */
  resize(w: number, h: number, dpr: number): void {
    const d = safeDpr(dpr);
    if (w === this.stage.w && h === this.stage.h && d === this.dpr) return;
    this.stage = { w: Math.max(0, w), h: Math.max(0, h) };
    this.dpr = d;
    this.sizeCanvas();
    this.layoutChanged();
  }

  /** 「全体を表示」であける量。 */
  setInsets(insets: Insets): void {
    const same =
      insets.top === this.insets.top &&
      insets.right === this.insets.right &&
      insets.bottom === this.insets.bottom &&
      insets.left === this.insets.left;
    if (same) return;
    this.insets = insets;
    if (this.fitMode) this.layoutChanged();
  }

  setSession(session: string | null): void {
    this.session = session;
  }

  /** 画像の形（大きさが変わったら、全体を表示しているときは収め直す）。 */
  setImage(model: ImageModel): void {
    const prev = this.image;
    this.image = model;
    if (!prev || this.fitMode) {
      this.applyFit();
    } else {
      this.t = this.settle(this.t);
    }
    this.emitInfo();
    this.invalidate();
  }

  /** 表示しているフレームを替える（すぐ頼み直す）。 */
  setFrame(frame: number): void {
    if (frame === this.frame) return;
    this.frame = frame;
    this.invalidate();
    this.options.onIdle?.(this.viewport());
  }

  // ===== ズーム・パン =====

  /** 全体を表示（⌘0）。 */
  fit(): void {
    this.fitMode = true;
    this.applyFit();
    this.transformed();
  }

  /** 100%（⌘1。画像の 1 画素を 1pt）。 */
  actualSize(): void {
    this.setZoom(1);
  }

  /** ⌘+（dir > 0）・⌘−（dir < 0）。 */
  zoomStep(dir: number): void {
    this.setZoom(nextZoom(this.t.zoom, dir, this.dpr, this.image?.pixelArt ?? false));
  }

  /** 枠の中央を動かさずに倍率を替える。 */
  setZoom(zoom: number): void {
    const area = fitArea(this.stage, this.insets);
    const z = snapZoom(zoom, this.dpr, this.image?.pixelArt ?? false);
    this.fitMode = false;
    this.t = zoomAround(this.t, z, area.x + area.w / 2, area.y + area.h / 2);
    this.rawZoom = z;
    this.transformed();
  }

  /** ピンチ・⌃ホイール：画面の点を動かさずに `factor` 倍する（ドット絵は吸着した倍率で見せる）。 */
  zoomAt(factor: number, sx: number, sy: number): void {
    if (!Number.isFinite(factor) || factor <= 0) return;
    this.rawZoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, this.rawZoom * factor));
    const z = snapZoom(this.rawZoom, this.dpr, this.image?.pixelArt ?? false);
    this.fitMode = false;
    if (z !== this.t.zoom) this.t = zoomAround(this.t, z, sx, sy);
    this.transformed();
  }

  /** ピンチを始めたときの倍率から `scale` 倍にする（Safari の GestureEvent）。 */
  zoomGesture(startZoom: number, scale: number, sx: number, sy: number): void {
    if (!Number.isFinite(scale) || scale <= 0) return;
    this.rawZoom = this.t.zoom;
    this.zoomAt((startZoom * scale) / this.t.zoom, sx, sy);
  }

  /** 動かす（CSS px）。 */
  panBy(dx: number, dy: number): void {
    this.fitMode = false;
    this.t = { zoom: this.t.zoom, ox: this.t.ox + dx, oy: this.t.oy + dy };
    this.transformed();
  }

  /** 今の倍率。 */
  get zoom(): number {
    return this.t.zoom;
  }

  /** 今の変換（テスト・座標の表示）。 */
  get transform(): Transform {
    return this.t;
  }

  /** 全体を表示しているか。 */
  get fitting(): boolean {
    return this.fitMode;
  }

  /** 画面の点の下の画素（整数。キャンバスの座標）。 */
  imagePoint(sx: number, sy: number): Point {
    return imagePixel(this.t, sx, sy);
  }

  /** 描ける範囲（フレームの列はそのフレームの矩形、ほかは画像全体。キャンバスの座標）。 */
  frameBounds(): Rect | null {
    const img = this.image;
    if (!img) return null;
    const v = this.view;
    if (v && v.frames.length > 1) {
      const f = frameOf(v, this.frame);
      return f ? { x: f.x, y: f.y, w: f.w, h: f.h } : null;
    }
    return { x: 0, y: 0, w: img.w, h: img.h };
  }

  /** 点が描ける範囲の中か。 */
  contains(p: Point): boolean {
    const b = this.frameBounds();
    return b !== null && p[0] >= b.x && p[1] >= b.y && p[0] < b.x + b.w && p[1] < b.y + b.h;
  }

  /** Rust へ送る表示の要求（見えている範囲を少し広げたもの。画像全体が入るなら null）。 */
  viewport(): EditViewportDto {
    const img = this.image;
    const rect = img ? visibleImageRect(this.t, this.stage, img, VIEW_MARGIN) : null;
    const whole =
      !img || !rect || (rect.x === 0 && rect.y === 0 && rect.w === img.w && rect.h === img.h);
    return {
      frame: this.frame,
      rect: whole || !rect ? null : { x: rect.x, y: rect.y, w: rect.w, h: rect.h },
      scale: this.t.zoom * this.dpr,
      interacting: false,
      focus: this.focus,
    };
  }

  // ===== 表示の結果 =====

  /**
   * 表示の結果を受ける（`requested` はその要求のビューポート）。rev の表を進め、見えているタイルを取りに行き、
   * その要求までの仮の筆に、解き終えるのを待つタイルを渡す。
   */
  applyView(view: EditViewDto, seq: number, requested: EditViewportDto): void {
    if (this.disposed) return;
    const prevLevel = this.view?.level ?? null;
    const levelChanged = prevLevel !== null && prevLevel !== view.level;
    if (levelChanged) this.fallbackLevel = prevLevel;
    if (view.changed.type === 'all') {
      this.tiles.markAll(view.level, view.rev);
      // 段を替えたとき、その段に前から残っている古い絵は出さない（前の段のタイルで埋める）
      if (levelChanged) this.tiles.dropStale(view.level, view.rev);
    } else {
      for (const f of view.changed.frames)
        this.tiles.markTiles(view.level, f.frame, f.tiles, view.rev);
    }
    this.tiles.clearFailures();
    this.view = view;
    this.alpha = formatCanBeTransparent(view.format);
    const fr = frameOf(view, requested.frame);
    const area = requested.rect ?? { x: 0, y: 0, w: view.fullWidth, h: view.fullHeight };
    this.shown = {
      level: view.level,
      frame: requested.frame,
      range: fr ? tileRange(area, view.level, fr, view.tile) : null,
    };
    const img = this.image;
    if (!img || img.w !== view.fullWidth || img.h !== view.fullHeight) {
      this.setImage({ w: view.fullWidth, h: view.fullHeight, pixelArt: img?.pixelArt ?? false });
    }
    const wait = this.pump();
    this.overlays.shown(seq, wait);
    this.emitInfo();
    this.invalidate();
  }

  /** 要求 `seq` の `index` 番目の手が断られた。 */
  onRejected(seq: number, index: number): void {
    if (this.overlays.rejected(seq, index)) this.invalidate();
  }

  /** 要求 `seq` が失敗した・描けなかった。 */
  onFailed(seq: number): void {
    if (this.overlays.failed(seq)) this.invalidate();
  }

  // ===== 筆 =====

  /** 描いている途中か。 */
  get drawing(): boolean {
    return this.builder !== null;
  }

  /** 筆を描き始める。 */
  beginStroke(spec: StrokeSpec, at: Point): void {
    this.cancelStroke();
    const overlay = new StrokeOverlay(
      spec.frame,
      spec.bounds,
      overlayMode(spec),
      spec.color?.rgba ?? [0, 0, 0, 0],
    );
    this.builder = new StrokeBuilder(spec, (x, y) => {
      overlay.plot(x, y);
    });
    this.overlays.begin(overlay);
    this.builder.add(at);
    this.invalidate();
  }

  /** 点を足す。 */
  extendStroke(points: readonly Point[]): void {
    const b = this.builder;
    if (!b) return;
    let n = 0;
    for (const p of points) n += b.add(p);
    if (n > 0) this.invalidate();
  }

  /** 描き終える（送る動作と仮の筆。描いていなければ null）。 */
  endStroke(): FinishedStroke | null {
    const b = this.builder;
    if (!b) return null;
    this.builder = null;
    b.finish();
    const pending = this.overlays.commit();
    this.invalidate();
    const last = b.points[b.points.length - 1];
    if (!pending || !last) return null;
    this.lastEnd = { frame: b.spec.frame, point: last };
    return { action: strokeAction(b.spec, b.points), pending };
  }

  /** 描いている筆をやめる（送らない）。やめたら true。 */
  cancelStroke(): boolean {
    if (!this.builder) return false;
    this.builder = null;
    this.overlays.cancel();
    this.invalidate();
    return true;
  }

  /** ⇧クリックの直線の始まり（そのフレームで最後に描いた筆の終わり）。 */
  lineStart(frame: number): Point | null {
    return this.lastEnd?.frame === frame ? this.lastEnd.point : null;
  }

  /** 仮の筆を送った。 */
  strokeSent(p: PendingStroke, seq: number, index: number): void {
    this.overlays.sent(p, seq, index);
  }

  /**
   * 自己点検（タイルの形式を測る。`src/dev/tileCheck.ts`）に渡す、今の表示の有効なタイル（要求した範囲の中。見えて
   * いるものを先に、`limit` 枚まで）。表示がまだなければ null。
   */
  probeTiles(limit = 24): {
    rev: number;
    level: number;
    frame: number;
    tiles: { tx: number; ty: number; w: number; h: number }[];
  } | null {
    const v = this.view;
    const s = this.shown;
    if (!v || !s || s.level !== v.level || !s.range) return null;
    const fr = frameOf(v, s.frame);
    if (!fr) return null;
    const img = this.image;
    const vis = img ? visibleImageRect(this.t, this.stage, img, 0) : null;
    const visible = vis ? tileRange(vis, v.level, fr, v.tile) : null;
    const all = tilesIn(s.range);
    const ordered = [
      ...all.filter(([tx, ty]) => inRange(visible, tx, ty)),
      ...all.filter(([tx, ty]) => !inRange(visible, tx, ty)),
    ];
    return {
      rev: v.rev,
      level: v.level,
      frame: s.frame,
      tiles: ordered
        .slice(0, limit)
        .map(([tx, ty]) => ({ tx, ty, ...tileSize(fr, tx, ty, v.tile) })),
    };
  }

  // ===== 片付け =====

  /** 閉じる（絵をすべて close。以後は何もしない）。 */
  dispose(): void {
    this.disposed = true;
    if (this.frameId !== null) this.scheduler.cancelFrame(this.frameId);
    if (this.idleId !== null) this.scheduler.cancelLater(this.idleId);
    this.frameId = null;
    this.idleId = null;
    this.tiles.clear();
    this.overlays.clear();
    this.builder = null;
    this.surfaces = null;
  }

  /** 次の描画を頼む（同じフレームの中では 1 回）。 */
  invalidate(): void {
    if (this.disposed || this.frameId !== null) return;
    this.frameId = this.scheduler.frame(() => {
      this.frameId = null;
      this.render();
    });
  }

  /** 今すぐ描く中身（テスト用）。 */
  scene(): Scene {
    return this.build();
  }

  // ===== 中身 =====

  private sizeCanvas(): void {
    const canvas = this.surfaces?.canvas;
    if (!canvas) return;
    const w = Math.max(1, Math.round(this.stage.w * this.dpr));
    const h = Math.max(1, Math.round(this.stage.h * this.dpr));
    if (canvas.width !== w) canvas.width = w;
    if (canvas.height !== h) canvas.height = h;
  }

  private layoutChanged(): void {
    if (this.fitMode) {
      this.applyFit();
    } else {
      this.t = this.settle(this.t);
    }
    this.transformed();
  }

  private applyFit(): void {
    const img = this.image;
    if (!img || this.stage.w <= 0 || this.stage.h <= 0) return;
    this.t = fitTransform(img.w, img.h, fitArea(this.stage, this.insets), this.dpr, img.pixelArt);
    this.rawZoom = this.t.zoom;
  }

  /** 位置を抑え、装置の画素にそろえる。 */
  private settle(t: Transform): Transform {
    const img = this.image;
    if (!img || this.stage.w <= 0) return t;
    return snapOffset(clampTransform(t, this.stage, img), this.dpr);
  }

  /** 変換が変わった：抑えて描き直し、止まったら頼み直す。 */
  private transformed(): void {
    this.t = this.settle(this.t);
    this.emitInfo();
    this.invalidate();
    this.pump();
    if (this.idleId !== null) this.scheduler.cancelLater(this.idleId);
    this.idleId = this.scheduler.later(() => {
      this.idleId = null;
      if (!this.disposed) this.options.onIdle?.(this.viewport());
    }, IDLE_MS);
  }

  /**
   * 見えていて要求した範囲の中にあり、最後に変わった rev の絵を持っていないタイルを取りに行く（同時に `MAX_LOADS` まで）。
   * まだ解き終えていないタイル（読み込み中・これから読む）の鍵を返す。
   */
  private pump(): Set<string> {
    const need = new Set<string>();
    const v = this.view;
    const s = this.shown;
    const img = this.image;
    if (!v || !s || !img || !this.session || s.frame !== this.frame || s.level !== v.level) {
      return need;
    }
    const fr = frameOf(v, this.frame);
    const vis = visibleImageRect(this.t, this.stage, img, 0);
    if (!fr || !vis) return need;
    const keep = new Set<string>();
    for (const [tx, ty] of tilesIn(tileRange(vis, v.level, fr, v.tile))) {
      const key = tileKey(v.level, this.frame, tx, ty);
      keep.add(key);
      if (!inRange(s.range, tx, ty)) continue;
      const rev = this.tiles.rev(v.level, this.frame, tx, ty);
      if (rev === null) continue;
      const loading = this.tiles.isLoading(key);
      if (!this.tiles.needsLoad(key, rev) && !loading) continue;
      need.add(key);
      if (this.tiles.needsLoad(key, rev) && this.loads < MAX_LOADS) {
        this.startLoad(v, fr, tx, ty, rev);
      }
    }
    this.tiles.evict(keep);
    return need;
  }

  private startLoad(v: EditViewDto, fr: FrameRect, tx: number, ty: number, rev: number): void {
    const session = this.session;
    if (!session) return;
    const level = v.level;
    const frame = this.frame;
    const key = tileKey(level, frame, tx, ty);
    const size = tileSize(fr, tx, ty, v.tile);
    const shownRev = v.rev;
    const fetchAt = (r: number) =>
      this.load(
        editTileUrl(session, r, level, frame, tx, ty, this.format),
        this.format,
        size.w,
        size.h,
      );
    this.tiles.startLoad(key, rev);
    this.loads++;
    // URL を組めない（あり得ない番号）ときも、読めなかったものとして扱う
    let first: Promise<TileBitmap>;
    try {
      first = fetchAt(rev);
    } catch (error) {
      first = Promise.reject(error);
    }
    first
      .catch((error: unknown) => {
        // 古い rev は Rust がもう配らない（WebKit も覚えていなかった）。変わっていないので今の rev で同じ中身を取る
        if (error instanceof TileHttpError && error.status === 404 && rev !== shownRev) {
          return fetchAt(shownRev);
        }
        throw error;
      })
      .then(
        (bitmap) => {
          this.loads--;
          if (this.disposed || session !== this.session) {
            bitmap.close();
            return;
          }
          this.tiles.finishLoad(key, rev, bitmap);
          this.arrived(key);
        },
        (error: unknown) => {
          this.loads--;
          if (this.disposed) return;
          if (!(error instanceof TileHttpError)) console.warn('タイルを解けませんでした', error);
          this.tiles.failLoad(key, rev);
          this.arrived(key);
        },
      );
  }

  private arrived(key: string): void {
    this.overlays.tileReady(key);
    this.invalidate();
    this.pump();
  }

  private render(): void {
    const surfaces = this.surfaces;
    if (!surfaces || this.disposed) return;
    this.draw(surfaces, this.build());
  }

  private build(): Scene {
    const d = this.dpr;
    const scene: Scene = {
      width: Math.max(1, Math.round(this.stage.w * d)),
      height: Math.max(1, Math.round(this.stage.h * d)),
      clip: null,
      tiles: [],
      overlays: [],
      checker: null,
    };
    const img = this.image;
    if (!img) return scene;
    const t = this.t;
    const imgRect = imageScreenRect(t, img);
    scene.clip = roundRect(scaleRect(imgRect, d));
    const v = this.view;
    if (v) {
      scene.checker = this.alpha
        ? intersect(imgRect, { x: 0, y: 0, w: this.stage.w, h: this.stage.h })
        : null;
      this.buildTiles(scene, v, img);
    }
    for (const o of this.overlays.overlays()) {
      if (o.frame !== this.frame) continue;
      const chunks: DrawChunk[] = [];
      for (const chunk of o.chunks.values()) {
        const r = roundRect(
          scaleRect(
            toScreen(t, { x: chunk.cx * CHUNK, y: chunk.cy * CHUNK, w: CHUNK, h: CHUNK }),
            d,
          ),
        );
        chunks.push({ chunk, dx: r.x, dy: r.y, dw: r.w, dh: r.h });
      }
      scene.overlays.push({ mode: o.mode, smoothing: smoothingFor(t.zoom, d, 0), chunks });
    }
    return scene;
  }

  private buildTiles(scene: Scene, v: EditViewDto, img: ImageModel): void {
    const t = this.t;
    const d = this.dpr;
    const fr = frameOf(v, this.frame);
    const vis = visibleImageRect(t, this.stage, img, 0);
    if (!fr || !vis) return;
    const range = tileRange(vis, v.level, fr, v.tile);
    const smoothing = smoothingFor(t.zoom, d, v.level);
    let missing = 0;
    for (const [tx, ty] of tilesIn(range)) {
      const dest = roundRect(scaleRect(toScreen(t, tileImageRect(v.level, fr, tx, ty, v.tile)), d));
      const got = this.tiles.get(tileKey(v.level, this.frame, tx, ty));
      if (got) {
        scene.tiles.push({
          image: got.bitmap,
          dx: dest.x,
          dy: dest.y,
          dw: dest.w,
          dh: dest.h,
          smoothing,
          clip: null,
        });
        continue;
      }
      missing++;
      this.fillFromFallback(scene, v, tx, ty, fr, dest);
    }
    if (!this.ready && range && missing === 0) {
      this.ready = true;
      this.emitInfo();
    }
  }

  /** 段が替わった直後：足りないタイルの所を、前の段のタイルで埋める（1 枚の画像のときだけ）。 */
  private fillFromFallback(
    scene: Scene,
    v: EditViewDto,
    tx: number,
    ty: number,
    fr: FrameRect,
    dest: Rect,
  ): void {
    const fl = this.fallbackLevel;
    if (fl === null || fl === v.level || v.frames.length !== 1) return;
    const s = 2 ** fl;
    const ffr: FrameRect = {
      x: 0,
      y: 0,
      w: Math.ceil(v.fullWidth / s),
      h: Math.ceil(v.fullHeight / s),
    };
    const full = tileImageRect(v.level, fr, tx, ty, v.tile);
    const smoothing = smoothingFor(this.t.zoom, this.dpr, fl);
    for (const [fx, fy] of tilesIn(tileRange(full, fl, ffr, v.tile))) {
      const got = this.tiles.get(tileKey(fl, this.frame, fx, fy));
      if (!got) continue;
      const r = roundRect(
        scaleRect(toScreen(this.t, tileImageRect(fl, ffr, fx, fy, v.tile)), this.dpr),
      );
      scene.tiles.push({
        image: got.bitmap,
        dx: r.x,
        dy: r.y,
        dw: r.w,
        dh: r.h,
        smoothing,
        clip: dest,
      });
    }
  }

  private emitInfo(): void {
    const img = this.image;
    const rect = img ? imageScreenRect(this.t, img) : null;
    const level = this.view?.level ?? null;
    const key = [
      zoomPercent(this.t.zoom),
      level,
      this.ready,
      // 仮の絵を置く矩形は、絵が出そろうまでだけ追う
      !this.ready && rect ? [rect.x, rect.y, rect.w, rect.h].map(Math.round).join(',') : '',
    ].join('|');
    if (key === this.infoKey) return;
    this.infoKey = key;
    this.options.onInfo?.({ zoom: this.t.zoom, level, ready: this.ready, imageRect: rect });
  }
}
