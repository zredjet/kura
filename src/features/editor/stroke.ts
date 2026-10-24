/**
 * 描いている最中の仮の筆（docs/editing.md §10）。
 *
 * - `StrokeBuilder`：ポインタの点（画像の整数座標）を足すたびに、`src/lib/paint.ts` の `path`・`footprint` で道を求め直し、
 *   増えた画素だけを `plot` に渡す。道は前から伸びるだけ（同じ点は 1 つ）。ピクセルパーフェクトでは最後の点だけが
 *   次の点で消えうるので、最後の点は次の点が来るか描き終えるまで置かない。置いた画素の和はエンジンの
 *   `strokePixels` と同じ（stroke.test.ts が照らす）。
 * - `StrokeOverlay`：仮の画素を 256 の区画ごとの RGBA の帯（色）と覆い（アルファ 255）に持つ。描き方は重ねる（色を
 *   source-over）・置き換え（覆いで destination-out してから色）・消しゴム（覆いで destination-out）。
 * - `OverlayTracker`：仮の筆の寿命。描き終えたら送った要求の通し番号と手の位置を覚え、その要求以降の表示が届いて、
 *   見えている変わったタイルを解き終えたら外す（`superseded` なら次の表示を待つ）。断られた・描けなかったらすぐ外す。
 * - 塗りつぶしは仮描きしない（Rust の結果を待つ。エンジンで 4096² の全面が 7 ms）。
 */

import { type Brush, footprint, packPoints, path, type Point } from '../../lib/paint';
import type { Rect } from './viewMath';

/** 描画の色（RGBA8 のストレート。パレットの画像はスポイトで拾った番号）。 */
export interface PaintColor {
  rgba: [number, number, number, number];
  index: number | null;
}

export type StrokeTool = 'pen' | 'eraser';
export type Blend = 'over' | 'replace';

/** 1 本の筆の条件。 */
export interface StrokeSpec {
  tool: StrokeTool;
  /** ペンの色（消しゴムは null）。 */
  color: PaintColor | null;
  blend: Blend;
  brush: Brush;
  pixelPerfect: boolean;
  /** フレームの番号（画像は 0）。 */
  frame: number;
  /** 描ける範囲（フレームの矩形。キャンバスの座標）。外の画素は仮描きしない。 */
  bounds: Rect;
  /** パレットの画像（色は番号で送り、置き換えだけ）。 */
  indexed: boolean;
}

/** 点を足しながら道と画素を求める（モジュールの説明）。 */
export class StrokeBuilder {
  readonly points: Point[] = [];
  private placed = 0;
  private trail: Point[] = [];
  private readonly stamp: Point[];

  constructor(
    readonly spec: StrokeSpec,
    private readonly plot: (x: number, y: number) => void,
  ) {
    this.stamp = footprint(spec.brush);
  }

  /** 点を足す（前と同じ点なら何もしない）。新しく置いた道の点の数を返す。 */
  add(p: Point): number {
    const last = this.points[this.points.length - 1];
    if (last && last[0] === p[0] && last[1] === p[1]) return 0;
    this.points.push([p[0], p[1]]);
    return this.place(false);
  }

  /** 描き終える（最後の点も置く）。 */
  finish(): number {
    return this.place(true);
  }

  /** 求め直した道（テスト用）。 */
  get pathPoints(): readonly Point[] {
    return this.trail;
  }

  private place(final: boolean): number {
    const next = path(this.points, this.spec.pixelPerfect);
    // 置いた点は変わらない（道は前から伸びるだけ。変わっていたら作りの誤り：置き直さず、残りだけ置く）
    const keep = Math.min(this.placed, next.length);
    this.trail = next;
    const upto = final || !this.spec.pixelPerfect ? next.length : next.length - 1;
    let n = 0;
    for (let i = keep; i < upto; i++) {
      const c = next[i];
      if (!c) continue;
      for (const [dx, dy] of this.stamp) this.plot(c[0] + dx, c[1] + dy);
      n++;
    }
    this.placed = Math.max(keep, upto);
    return n;
  }
}

/** 区画の 1 辺（タイルと同じ）。 */
export const CHUNK = 256;

/** 仮の画素の区画（キャンバスの座標の 256 の格子）。 */
export interface OverlayChunk {
  readonly cx: number;
  readonly cy: number;
  /** 色（RGBA のストレート。置いた画素だけ）。 */
  readonly color: Uint8ClampedArray;
  /** 覆い（置いた画素はアルファ 255）。 */
  readonly mask: Uint8ClampedArray;
  /** 中身が変わるたびに増える（描く側が貼り直す）。 */
  version: number;
}

/** 仮の筆の描き方。 */
export type OverlayMode = 'over' | 'replace' | 'erase';

/** 筆の条件から描き方を決める。 */
export function overlayMode(spec: Pick<StrokeSpec, 'tool' | 'blend' | 'indexed'>): OverlayMode {
  if (spec.tool === 'eraser') return 'erase';
  return spec.blend === 'replace' || spec.indexed ? 'replace' : 'over';
}

/** 仮の画素（モジュールの説明）。 */
export class StrokeOverlay {
  readonly chunks = new Map<string, OverlayChunk>();
  readonly mode: OverlayMode;
  private readonly rgba: readonly [number, number, number, number];

  constructor(
    readonly frame: number,
    readonly bounds: Rect,
    mode: OverlayMode,
    rgba: readonly [number, number, number, number],
  ) {
    this.mode = mode;
    this.rgba = rgba;
  }

  /** 画素を 1 つ置く（範囲の外・置いたものは何もしない）。置いたら true。 */
  plot(x: number, y: number): boolean {
    const b = this.bounds;
    if (x < b.x || y < b.y || x >= b.x + b.w || y >= b.y + b.h) return false;
    const cx = Math.floor(x / CHUNK);
    const cy = Math.floor(y / CHUNK);
    const key = `${cx},${cy}`;
    let chunk = this.chunks.get(key);
    if (!chunk) {
      chunk = {
        cx,
        cy,
        color: new Uint8ClampedArray(CHUNK * CHUNK * 4),
        mask: new Uint8ClampedArray(CHUNK * CHUNK * 4),
        version: 0,
      };
      this.chunks.set(key, chunk);
    }
    const i = ((y - cy * CHUNK) * CHUNK + (x - cx * CHUNK)) * 4;
    if (chunk.mask[i + 3] === 255) return false;
    chunk.mask[i + 3] = 255;
    chunk.color[i] = this.rgba[0];
    chunk.color[i + 1] = this.rgba[1];
    chunk.color[i + 2] = this.rgba[2];
    chunk.color[i + 3] = this.rgba[3];
    chunk.version++;
    return true;
  }

  /** 置いた画素の数（テスト用）。 */
  get pixelCount(): number {
    let n = 0;
    for (const c of this.chunks.values()) {
      for (let i = 3; i < c.mask.length; i += 4) if (c.mask[i] === 255) n++;
    }
    return n;
  }
}

/** 送った・送る仮の筆。 */
export interface PendingStroke {
  readonly overlay: StrokeOverlay;
  /** 送った要求の通し番号（まだ送っていなければ null）。 */
  seq: number | null;
  /** 要求の中の手の位置。 */
  index: number | null;
  /** 解き終えるのを待つタイルの鍵（表示が届く前は null）。 */
  waiting: Set<string> | null;
}

/** 仮の筆の寿命（モジュールの説明）。どれも描き直しが要れば true を返す。 */
export class OverlayTracker {
  active: StrokeOverlay | null = null;
  readonly pending: PendingStroke[] = [];

  begin(overlay: StrokeOverlay): void {
    this.active = overlay;
  }

  /** 描いている筆をやめる（送らない）。 */
  cancel(): boolean {
    const had = this.active !== null;
    this.active = null;
    return had;
  }

  /** 描き終えた筆を、送るのを待つものにする。 */
  commit(): PendingStroke | null {
    if (!this.active) return null;
    const p: PendingStroke = { overlay: this.active, seq: null, index: null, waiting: null };
    this.active = null;
    this.pending.push(p);
    return p;
  }

  /** 送った（要求の通し番号と手の位置）。 */
  sent(p: PendingStroke, seq: number, index: number): void {
    p.seq = seq;
    p.index = index;
  }

  /** 要求 `seq` の `index` 番目の手で断られた（それより後の手は当たっていない）。 */
  rejected(seq: number, index: number): boolean {
    return this.drop((p) => p.seq === seq && (p.index ?? 0) >= index);
  }

  /** 要求 `seq` が失敗した・表示が描けなかった（その要求までに送った筆を外す）。 */
  failed(seq: number): boolean {
    return this.drop((p) => p.seq !== null && p.seq <= seq);
  }

  /** 要求 `seq` の表示が届いた。`waitKeys` は解き終えるのを待つタイル（空ならすぐ外す）。 */
  shown(seq: number, waitKeys: ReadonlySet<string>): boolean {
    for (const p of this.pending) {
      if (p.seq !== null && p.seq <= seq) p.waiting = new Set(waitKeys);
    }
    return this.drop((p) => p.waiting !== null && p.waiting.size === 0);
  }

  /** タイルを解き終えた（か、読めなかった）。 */
  tileReady(key: string): boolean {
    for (const p of this.pending) p.waiting?.delete(key);
    return this.drop((p) => p.waiting !== null && p.waiting.size === 0);
  }

  /** 描く順の仮の筆（送ったもの・描いているもの）。 */
  overlays(): StrokeOverlay[] {
    const out = this.pending.map((p) => p.overlay);
    if (this.active) out.push(this.active);
    return out;
  }

  clear(): void {
    this.active = null;
    this.pending.length = 0;
  }

  private drop(pred: (p: PendingStroke) => boolean): boolean {
    let changed = false;
    for (let i = this.pending.length - 1; i >= 0; i--) {
      const p = this.pending[i];
      if (p && pred(p)) {
        this.pending.splice(i, 1);
        changed = true;
      }
    }
    return changed;
  }
}

/** 描画の動作の色の JSON（`{"rgba":[..]}` か `{"index":n}`）。 */
function colorJson(color: PaintColor, indexed: boolean): { rgba: number[] } | { index: number } {
  return indexed && color.index !== null ? { index: color.index } : { rgba: [...color.rgba] };
}

/** 筆 1 本の動作（`paint.actions` の要素。docs/editing.md §10）。 */
export function strokeAction(spec: StrokeSpec, points: readonly Point[]) {
  const pen = spec.tool === 'pen';
  return {
    type: 'stroke',
    tool: spec.tool,
    color: pen && spec.color ? colorJson(spec.color, spec.indexed) : null,
    blend: pen && !spec.indexed ? spec.blend : 'replace',
    brush: { shape: spec.brush.shape, size: spec.brush.size },
    pixelPerfect: spec.pixelPerfect,
    frame: spec.frame,
    points: packPoints(points),
  };
}

/** 塗りつぶしの条件。 */
export interface FillSpec {
  color: PaintColor;
  tolerance: number;
  connectivity: 'four' | 'eight';
  contiguous: boolean;
  frame: number;
  indexed: boolean;
}

/** 塗りつぶし 1 つの動作（パレットの画像は許容差 0）。 */
export function fillAction(spec: FillSpec, at: Point) {
  return {
    type: 'fill',
    at: { x: at[0], y: at[1] },
    color: colorJson(spec.color, spec.indexed),
    tolerance: spec.indexed ? 0 : spec.tolerance,
    connectivity: spec.connectivity,
    contiguous: spec.contiguous,
    frame: spec.frame,
  };
}
