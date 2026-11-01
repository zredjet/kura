/**
 * 音声エディタの波形の面の純粋な関数（M13 の段 6。設計 §2.1・§2.3）。DOM に触れない。
 *
 * - 見ている区間（`WaveWindow`）は出力のフレームの小数の区間 `[start, start + span)`。x は面の中の CSS px。
 * - ズームはポインタ（の下のフレーム）を中心にし、全体より広げない・1 フレームを `MAX_PX_PER_FRAME` px より大きくしない。
 * - 再生位置が見ている区間の外へ出たら、そこを頭にして 1 画面ぶん飛ぶ（滑らかなスクロールはしない）。
 * - 目盛りは 1 ms〜10 分のきりのよい刻みから、文字が重ならない最も細かいものを選ぶ。
 * - 段（ビン）は M11 の `pickLevel`（`src/lib/asset/peaks.ts`）と同じ規則で、1 列に 1〜4 ビン。1 列が最も細かい段の
 *   ビン（256 フレーム）より細かければ PCM で描く（null）。
 * - 当たり判定の順：ループの旗 > フェードのハンドル > 切り詰めの端 > 選択の端 > 全体図 > 目盛り > 空き。
 */

/** 見ている区間（出力のフレーム。小数）。 */
export interface WaveWindow {
  start: number;
  span: number;
}

/** 1 フレームを描く大きさの上限（CSS px）。 */
export const MAX_PX_PER_FRAME = 64;
/** 1 フレームの大きさがこれを超えたら標本の点も描く（装置の画素）。 */
export const SAMPLE_POINTS_PX = 8;
/** 目盛りの刻みの間隔の下限（CSS px）。 */
export const TICK_MIN_PX = 72;
/** 目盛りの刻み（秒）。 */
export const TICK_STEPS: readonly number[] = [
  0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600,
];
/** チャンネルの段の高さの下限（CSS px）。 */
export const MIN_LANE = 24;
/** 当たり判定の幅（CSS px。端・旗）。 */
export const HIT_PX = 6;
/** フェードの三角のハンドルの大きさ（CSS px）。 */
export const FADE_HANDLE_PX = 10;

/** フェードの曲線の 1 点（出力のフレームとゲイン 0〜1）。 */
export interface FadePoint {
  frame: number;
  gain: number;
}

/**
 * フェードの曲線の点。フェードは見かけの始まり `from` から見かけの終わり `to`（出力のフレーム。下流で頭・終わりを
 * 切られていれば出力の範囲の外もある）に掛かり、`rising`（フェードイン）なら from で 0・to で 1、そうでなければ
 * from で 1・to で 0。形は `equalPower` なら sin(πu/2)、ほかは直線（u は見かけの区間で測る）。出力の範囲 [0, total] で
 * 切った区間を `steps` 等分した点を返す。端が null（写せない）・切った区間が空なら空（曲線を描かない）。
 */
export function fadeCurvePoints(
  from: number | null,
  to: number | null,
  rising: boolean,
  total: number,
  curve: string,
  steps: number,
): FadePoint[] {
  if (from === null || to === null || !(to > from) || steps < 1) return [];
  const a = Math.max(0, from);
  const b = Math.min(total, to);
  if (!(b > a)) return [];
  const shape = (u: number) => (curve === 'equalPower' ? Math.sin((u * Math.PI) / 2) : u);
  const points: FadePoint[] = [];
  for (let k = 0; k <= steps; k++) {
    const frame = k === steps ? b : a + ((b - a) * k) / steps;
    const u = (frame - from) / (to - from);
    points.push({ frame, gain: shape(rising ? u : 1 - u) });
  }
  return points;
}

/** フレームを x にする。 */
export function frameToX(frame: number, w: WaveWindow, width: number): number {
  return w.span > 0 ? ((frame - w.start) / w.span) * width : 0;
}

/** x をフレームにする（小数）。 */
export function xToFrame(x: number, w: WaveWindow, width: number): number {
  return width > 0 ? w.start + (x / width) * w.span : w.start;
}

/** 区間の最小の長さ（1 フレームが `MAX_PX_PER_FRAME` px）。全体より長くはしない。 */
export function minSpan(width: number, total: number): number {
  return Math.min(Math.max(1, total), Math.max(2, width / MAX_PX_PER_FRAME));
}

/** 全体を表示。 */
export function fitView(total: number): WaveWindow {
  return { start: 0, span: Math.max(1, total) };
}

/** 区間を全体の中に収める（長さは全体まで、頭は 0 以上で尾が全体を越えない）。 */
export function clampWindow(w: WaveWindow, total: number): WaveWindow {
  const t = Math.max(1, total);
  const span = Number.isFinite(w.span) && w.span > 0 ? Math.min(w.span, t) : t;
  const start = Number.isFinite(w.start) ? Math.min(Math.max(0, w.start), t - span) : 0;
  return { start, span };
}

/**
 * フレーム `anchor` を同じ位置に置いたまま `factor` 倍に拡大する（1 より大きいと拡大、小さいと縮小）。長さは
 * `[min, max]`（max は全体）に収める。
 */
export function zoomAround(
  w: WaveWindow,
  factor: number,
  anchor: number,
  total: number,
  min: number,
  max: number = Math.max(1, total),
): WaveWindow {
  if (!Number.isFinite(factor) || factor <= 0 || !(w.span > 0)) return clampWindow(w, total);
  const lo = Math.min(min, max);
  const span = Math.min(max, Math.max(lo, w.span / factor));
  const rel = (anchor - w.start) / w.span;
  return clampWindow({ start: anchor - rel * span, span }, total);
}

/** `frames` だけ動かす（端で止まる）。 */
export function panBy(w: WaveWindow, frames: number, total: number): WaveWindow {
  return clampWindow(
    { start: w.start + (Number.isFinite(frames) ? frames : 0), span: w.span },
    total,
  );
}

/** 同じ区間か。 */
export function sameWindow(a: WaveWindow, b: WaveWindow): boolean {
  return a.start === b.start && a.span === b.span;
}

/** 再生位置 `pos` が区間の外なら、そこを頭にして 1 画面ぶん飛ぶ（中なら同じものを返す）。 */
export function followPlayhead(w: WaveWindow, pos: number, total: number): WaveWindow {
  if (pos >= w.start && pos < w.start + w.span) return w;
  return clampWindow({ start: pos, span: w.span }, total);
}

/** ミリ秒を「1:02.345」「1:02」「1:02:03」の形にする（`fraction` で小数 3 桁）。 */
export function msText(ms: number, fraction: boolean): string {
  const v = Math.max(0, Math.round(ms));
  const h = Math.floor(v / 3_600_000);
  const m = Math.floor((v % 3_600_000) / 60_000);
  const s = Math.floor((v % 60_000) / 1000);
  const sec = String(s).padStart(2, '0');
  const rest = fraction ? `${sec}.${String(v % 1000).padStart(3, '0')}` : sec;
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${rest}` : `${m}:${rest}`;
}

/** 目盛り 1 つ。 */
export interface Tick {
  /** 出力のフレーム（小数）。 */
  frame: number;
  x: number;
  label: string;
}

/** 目盛りの刻み（秒）：間隔が `TICK_MIN_PX` 以上になる最も細かいもの（なければ最も粗いもの）。 */
export function tickStep(w: WaveWindow, width: number, rate: number): number {
  const pxPerSecond = w.span > 0 && rate > 0 ? (width * rate) / w.span : 0;
  for (const step of TICK_STEPS) {
    if (step * pxPerSecond >= TICK_MIN_PX) return step;
  }
  return TICK_STEPS[TICK_STEPS.length - 1] ?? 600;
}

/** 目盛り（見ている区間の中の刻み。文字は 1 秒以上の刻みなら秒まで、細かければミリ秒まで）。 */
export function rulerTicks(w: WaveWindow, width: number, rate: number): Tick[] {
  if (!(rate > 0) || !(w.span > 0) || !(width > 0)) return [];
  const step = tickStep(w, width, rate);
  const startSec = w.start / rate;
  const endSec = (w.start + w.span) / rate;
  const first = Math.ceil(startSec / step - 1e-9);
  const out: Tick[] = [];
  for (let k = first; k * step < endSec && out.length < 1000; k++) {
    // 刻みの積で誤差が出ないよう、ミリ秒の整数に丸めてから
    const ms = Math.round(k * step * 1000);
    const frame = (ms / 1000) * rate;
    out.push({ frame, x: frameToX(frame, w, width), label: msText(ms, step < 1) });
  }
  return out;
}

/** 1 列がこのフレーム数より細かければ PCM で描く（段の最も細かいビン。Rust の `PEAK_BINS[0]`）。 */
export const PCM_BELOW = 256;

/**
 * 描く段のビンのフレーム数：区間 `span` を `columns` 列に描くとき、ビンの数が列の数以上の段のうち最も粗いもの（なければ
 * 持っている最も細かい段）。1 列が `pcmBelow`（256）フレームより細かければ null（PCM で描く）。`bins` は細かい順で、
 * 結果の `peaks.bins` にある段だけから選ぶ（長い音声をライブラリの段で仮に出している間は粗い段しかないことがある）。
 */
export function pickBin(
  bins: readonly number[],
  span: number,
  columns: number,
  pcmBelow = PCM_BELOW,
): number | null {
  const finest = bins[0];
  if (finest === undefined || !(columns > 0) || !(span > 0)) return null;
  if (span / columns < pcmBelow) return null;
  let best = finest;
  for (const b of bins) {
    if (Math.ceil(span / b) >= columns) best = b;
  }
  return best;
}

/** 縦の帯（CSS px）。 */
export interface Band {
  y: number;
  h: number;
}

/** 面の縦の割り付け：上から目盛り・全体図・チャンネルの段。 */
export interface LaneLayout {
  ruler: Band;
  overview: Band;
  lanes: Band[];
}

/**
 * 縦の割り付け（`height` は面の中の描く高さ）。チャンネルの段は残りを等分し、1 段は `MIN_LANE` px 以上（入らなければ
 * 下にはみ出す）。
 */
export function laneLayout(
  height: number,
  channels: number,
  ruler: number,
  overview: number,
): LaneLayout {
  const top = Math.max(0, ruler) + Math.max(0, overview);
  const n = Math.max(1, Math.floor(channels));
  const h = Math.max(MIN_LANE, Math.floor(Math.max(0, height - top) / n));
  return {
    ruler: { y: 0, h: Math.max(0, ruler) },
    overview: { y: Math.max(0, ruler), h: Math.max(0, overview) },
    lanes: Array.from({ length: n }, (_, i) => ({ y: top + i * h, h })),
  };
}

/** 区間（出力のフレーム。終わりは含まない）。 */
export interface FrameSpan {
  start: number;
  end: number;
}

/** 当たり判定に使う、今つかめるもの（出していないものは null）。 */
export interface HitScene {
  layout: LaneLayout;
  window: WaveWindow;
  width: number;
  loop: FrameSpan | null;
  /** フェードのハンドル（フェードインの終わり・フェードアウトの始め）。 */
  fade: { inEnd: number; outStart: number } | null;
  trim: FrameSpan | null;
  selection: FrameSpan | null;
}

export type HitTarget =
  | { kind: 'loop'; edge: 'start' | 'end' }
  | { kind: 'fade'; edge: 'in' | 'out' }
  | { kind: 'trim'; edge: 'start' | 'end' }
  | { kind: 'selection'; edge: 'start' | 'end' }
  | { kind: 'overview' }
  | { kind: 'ruler' }
  | { kind: 'lanes' }
  | { kind: 'empty' };

function inBand(y: number, b: Band): boolean {
  return y >= b.y && y < b.y + b.h;
}

/** 端の近いほう（どちらも `HIT_PX` の外なら null）。重なれば近いほう、同じなら終わり（右へ広げられる）。 */
function nearEdge(
  x: number,
  span: FrameSpan,
  w: WaveWindow,
  width: number,
  tol = HIT_PX,
): 'start' | 'end' | null {
  const ds = Math.abs(x - frameToX(span.start, w, width));
  const de = Math.abs(x - frameToX(span.end, w, width));
  if (ds > tol && de > tol) return null;
  return de <= ds ? 'end' : 'start';
}

/** 当たり判定（x・y は面の中の CSS px）。 */
export function hitTest(x: number, y: number, s: HitScene): HitTarget {
  const { layout, window: w, width } = s;
  const first = layout.lanes[0];
  const last = layout.lanes[layout.lanes.length - 1];
  const lanesTop = first?.y ?? layout.overview.y + layout.overview.h;
  const lanesBottom = last ? last.y + last.h : lanesTop;
  const inLanes = y >= lanesTop && y < lanesBottom;
  if (s.loop && inBand(y, layout.ruler)) {
    const edge = nearEdge(x, s.loop, w, width);
    if (edge) return { kind: 'loop', edge };
  }
  if (s.fade && y >= lanesTop && y < lanesTop + FADE_HANDLE_PX * 1.5) {
    const di = x - frameToX(s.fade.inEnd, w, width);
    const dout = frameToX(s.fade.outStart, w, width) - x;
    // 三角は内側へ向く（フェードインは右、フェードアウトは左）。端の外側も少しつかめる
    const inHit = di >= -HIT_PX && di <= FADE_HANDLE_PX + HIT_PX / 2;
    const outHit = dout >= -HIT_PX && dout <= FADE_HANDLE_PX + HIT_PX / 2;
    if (inHit || outHit) {
      if (inHit && outHit)
        return { kind: 'fade', edge: Math.abs(di) <= Math.abs(dout) ? 'in' : 'out' };
      return { kind: 'fade', edge: inHit ? 'in' : 'out' };
    }
  }
  if (s.trim && inLanes) {
    const edge = nearEdge(x, s.trim, w, width);
    if (edge) return { kind: 'trim', edge };
  }
  if (s.selection && inLanes) {
    const edge = nearEdge(x, s.selection, w, width);
    if (edge) return { kind: 'selection', edge };
  }
  if (inBand(y, layout.overview)) return { kind: 'overview' };
  if (inBand(y, layout.ruler)) return { kind: 'ruler' };
  if (inLanes) return { kind: 'lanes' };
  return { kind: 'empty' };
}

/** 全体図の x（0〜width）を、見ている区間の中央がそこに来る頭のフレームにする。 */
export function overviewStart(x: number, width: number, w: WaveWindow, total: number): number {
  const center = width > 0 ? (Math.min(Math.max(0, x), width) / width) * total : 0;
  return clampWindow({ start: center - w.span / 2, span: w.span }, total).start;
}
