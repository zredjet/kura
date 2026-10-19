/**
 * アニメのプレビュー（aseprite・GIF・APNG・アニメの WebP・格子のスプライトシート）の時間軸と格子（docs/design.md §3.1）。
 * DOM に触れない純粋な関数。経過時間からコマを決めるので、タイマーの刻みに依らず全体の時間が正しい。
 */

/** aseprite のタグの向き（kura-engine の `TagDirection` と同じ綴り）。 */
export type Direction = 'forward' | 'reverse' | 'pingPong' | 'pingPongReverse';

/** アニメのタグ（kura-engine の `AnimTag` と同じ形）。`from`・`to` は両端を含む。 */
export interface AnimTag {
  name: string;
  from: number;
  to: number;
  direction: Direction;
  /** 0 は無限。プレビューでは使わない（ずっと回す）。 */
  repeat: number;
}

/** 表示時間が 0 のコマの長さ（GIF の慣習）。 */
export const ZERO_DURATION_MS = 100;

/** コマの数の上限（kura-engine の `Limits::max_frames`）。 */
export const MAX_FRAMES = 4096;

/**
 * 1 周分のコマの番号の列。ピンポンは端を重ねない（1..4 なら 1 2 3 4 3 2 → 1 …）。
 * タグの範囲はコマの数で切り、逆向きの範囲は入れ替える。範囲がコマの外なら先頭のコマだけ。
 */
export function sequence(frameCount: number, tag?: AnimTag): number[] {
  if (!(frameCount > 0)) return [];
  const lo = tag ? Math.max(0, Math.min(tag.from, tag.to)) : 0;
  const hi = tag ? Math.min(frameCount - 1, Math.max(tag.from, tag.to)) : frameCount - 1;
  if (lo > hi) return [0];
  const up: number[] = [];
  for (let i = lo; i <= hi; i++) up.push(i);
  const down = [...up].reverse();
  switch (tag?.direction ?? 'forward') {
    case 'forward':
      return up;
    case 'reverse':
      return down;
    case 'pingPong':
      return [...up, ...down.slice(1, -1)];
    case 'pingPongReverse':
      return [...down, ...up.slice(1, -1)];
  }
}

/** コマの表示時間（ms）。0 と読めない値は 100ms、ほかはファイルのまま。 */
export function effectiveDuration(ms: number): number {
  return Number.isFinite(ms) && ms > 0 ? ms : ZERO_DURATION_MS;
}

export interface Track {
  /** 再生するコマの番号の列（1 周分）。 */
  steps: number[];
  /** steps[i] の始まりの時刻（ms）。starts[0] = 0。 */
  starts: number[];
  /** 1 周の長さ（ms）。 */
  period: number;
}

/** コマごとの表示時間とタグから、1 周分の時刻の表を作る。 */
export function track(durations: readonly number[], tag?: AnimTag): Track {
  const steps = sequence(durations.length, tag);
  const starts: number[] = [];
  let t = 0;
  for (const f of steps) {
    starts.push(t);
    t += effectiveDuration(durations[f] ?? 0);
  }
  return { steps, starts, period: t };
}

/** 経過時間 `elapsedMs` に出すコマの番号と、列の中の位置（周期で回る。負の時間は前の周）。 */
export function frameAt(tr: Track, elapsedMs: number): { frame: number; step: number } {
  if (tr.steps.length === 0 || !(tr.period > 0) || !Number.isFinite(elapsedMs)) {
    return { frame: tr.steps[0] ?? 0, step: 0 };
  }
  const t = ((elapsedMs % tr.period) + tr.period) % tr.period;
  // starts[step] <= t となる最大の step（二分探索）
  let lo = 0;
  let hi = tr.starts.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if ((tr.starts[mid] ?? 0) <= t) lo = mid;
    else hi = mid - 1;
  }
  return { frame: tr.steps[lo] ?? 0, step: lo };
}

/** 列の位置 `step` の始まりの時刻（コマ送り。列の長さで回り、負の位置は後ろから）。 */
export function startOfStep(tr: Track, step: number): number {
  const n = tr.starts.length;
  if (n === 0) return 0;
  const i = ((Math.trunc(step) % n) + n) % n;
  return tr.starts[i] ?? 0;
}

/** 格子（kura-engine の `Grid` と同じ欄）。 */
export interface Grid {
  cellW: number;
  cellH: number;
  offsetX: number;
  offsetY: number;
  gapX: number;
  gapY: number;
}

export interface Cell {
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * 1 つの軸に収まるセルの数（kura-engine の `sprite::split_grid::cells_on_axis` と同じ式。端の欠けたセルは数えない）。
 * 共有のベクタ（crates/kura-engine/tests/vectors/sprite/grid-cells.json）で照らす。
 */
export function cellsOnAxis(len: number, offset: number, cell: number, gap: number): number {
  if (cell <= 0 || offset + cell > len) return 0;
  return Math.floor((len - offset - cell) / (cell + gap)) + 1;
}

/** セルの矩形（行ごとに左から。`sprite.split_grid` と同じ並び）。`limit` で先頭からの数を絞る。 */
export function gridCells(w: number, h: number, g: Grid, limit = MAX_FRAMES): Cell[] {
  const cols = cellsOnAxis(w, g.offsetX, g.cellW, g.gapX);
  const rows = cellsOnAxis(h, g.offsetY, g.cellH, g.gapY);
  const out: Cell[] = [];
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      if (out.length >= limit) return out;
      out.push({
        x: g.offsetX + c * (g.cellW + g.gapX),
        y: g.offsetY + r * (g.cellH + g.gapY),
        w: g.cellW,
        h: g.cellH,
      });
    }
  }
  return out;
}

/**
 * 帯（`/anim/{hash}/1` の PNG）の中のコマ f の左上。帯はキャンバスの大きさのセルを行ごとに左から `cols` 個ずつ、
 * 隙間なく並べたもの。
 */
export function stripCell(
  f: number,
  cols: number,
  canvasW: number,
  canvasH: number,
): { x: number; y: number } {
  if (!(cols > 0)) return { x: 0, y: 0 };
  return { x: (f % cols) * canvasW, y: Math.floor(f / cols) * canvasH };
}
