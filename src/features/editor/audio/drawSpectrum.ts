/**
 * スペクトルの面を描く（M17 段 5。状態を持たない。docs/design.md「音声エディタのスペクトルとダイナミクス」）。
 *
 * 横は対数の周波数、縦は左にイコライザの ±dB、右にスペクトルの dBFS。アナライザは後（塗り＋縁）・前（実線）・最大の保持
 * （点線）を線の形で分ける（§7「色だけで状態を伝えない」）。イコライザの曲線・点は段 4 の手で重ねる（`eq`）。
 */

import {
  type DbRange,
  type FreqAxis,
  FREQ_TICKS,
  type SpecBandEdge,
  dbToY,
  freqToX,
  tickLabel,
} from './spectrumMath';

/** 描く色（CSS の変数を計算した色。`SpectrumStage` が読む）。 */
export interface SpectrumColors {
  post: string;
  postLine: string;
  pre: string;
  hold: string;
  grid: string;
  label: string;
  zero: string;
  accent: string;
  band: string;
  bandFill: string;
  point: string;
  pointLine: string;
}

export const DEFAULT_SPECTRUM_COLORS: SpectrumColors = {
  post: 'gray',
  postLine: 'gray',
  pre: 'gray',
  hold: 'white',
  grid: 'gray',
  label: 'gray',
  zero: 'gray',
  accent: 'orange',
  band: 'gray',
  bandFill: 'orange',
  point: 'white',
  pointLine: 'black',
};

/** イコライザの曲線と点（段 4 の手。なければ描かない）。 */
export interface SpectrumEq {
  /** 曲線の周波数（Hz。昇順）と合成の dB。 */
  freqs: readonly (number | null)[];
  total: readonly (number | null)[];
  /** 選んだバンドの曲線と効く帯の塗り（dB）。 */
  selected: readonly (number | null)[] | null;
  /** ポインタを載せたバンドの曲線（dB）。 */
  hovered: readonly (number | null)[] | null;
  /** バンドの点（面の中の CSS px）。 */
  points: readonly SpectrumPoint[];
}

/** バンドの点。 */
export interface SpectrumPoint {
  x: number;
  y: number;
  selected: boolean;
  enabled: boolean;
  /** 選んだ点の Q の印（左右の x。なければ null）。 */
  q: { left: number; right: number } | null;
}

/** 描く中身。座標は CSS px（`dpr` を掛けて描く）。 */
export interface SpectrumScene {
  /** canvas の大きさ（装置の画素）。 */
  width: number;
  height: number;
  dpr: number;
  /** 描く領域（CSS px）。 */
  inner: { x: number; y: number; w: number; h: number };
  axis: FreqAxis;
  /** 縦の範囲：スペクトル（dBFS）とイコライザ（±dB）。 */
  spec: DbRange;
  eq: DbRange;
  bands: readonly SpecBandEdge[];
  /** 後・前・最大の保持（帯ごとの dB。傾きの補正を当てたもの。なければ null）。 */
  post: Float32Array | null;
  pre: Float32Array | null;
  hold: Float32Array | null;
  /** 後を薄く出す（ドラッグ中の古い後・平均を計っている間）。 */
  dimPost: boolean;
  eqLayer: SpectrumEq | null;
  colors: SpectrumColors;
  font: string;
  /** 山の印（面の中の CSS px。なければ null）。 */
  peak: { x: number; y: number } | null;
}

type Ctx = Pick<
  CanvasRenderingContext2D,
  | 'save'
  | 'restore'
  | 'setTransform'
  | 'clearRect'
  | 'beginPath'
  | 'moveTo'
  | 'lineTo'
  | 'closePath'
  | 'stroke'
  | 'fill'
  | 'fillText'
  | 'setLineDash'
  | 'arc'
  | 'fillRect'
> & {
  fillStyle: CanvasRenderingContext2D['fillStyle'];
  strokeStyle: CanvasRenderingContext2D['strokeStyle'];
  lineWidth: number;
  font: string;
  textAlign: CanvasTextAlign;
  textBaseline: CanvasTextBaseline;
  globalAlpha: number;
};

/** 帯ごとの値の折れ線（帯の中心の x。有限でない値は線を切る）。`close` なら下へ閉じて塗る形にする。 */
function bandPath(ctx: Ctx, s: SpectrumScene, values: Float32Array, close: boolean): boolean {
  const { x, y, w, h } = s.inner;
  let started = false;
  let firstX = 0;
  let lastX = 0;
  ctx.beginPath();
  for (let k = 0; k < s.bands.length; k++) {
    const v = values[k];
    const b = s.bands[k];
    if (v === undefined || !b || !Number.isFinite(v)) continue;
    const px = x + freqToX(b.center, s.axis, w);
    const py = y + Math.min(h, Math.max(0, dbToY(v, s.spec, h)));
    if (!started) {
      ctx.moveTo(px, close ? y + h : py);
      if (close) ctx.lineTo(px, py);
      firstX = px;
      started = true;
    } else {
      ctx.lineTo(px, py);
    }
    lastX = px;
  }
  if (started && close) {
    ctx.lineTo(lastX, y + h);
    ctx.lineTo(firstX, y + h);
    ctx.closePath();
  }
  return started;
}

/** 曲線の折れ線（イコライザの ±dB）。 */
function curvePath(
  ctx: Ctx,
  s: SpectrumScene,
  freqs: readonly (number | null)[],
  db: readonly (number | null)[],
): boolean {
  const { x, y, w, h } = s.inner;
  let started = false;
  ctx.beginPath();
  for (let i = 0; i < Math.min(freqs.length, db.length); i++) {
    const f = freqs[i];
    const v = db[i];
    if (f == null || v == null || !Number.isFinite(v)) {
      started = false;
      continue;
    }
    const px = x + freqToX(f, s.axis, w);
    const py = y + dbToY(v, s.eq, h);
    if (started) ctx.lineTo(px, py);
    else ctx.moveTo(px, py);
    started = true;
  }
  return started;
}

/** 面を描く。 */
export function drawSpectrum(ctx: Ctx, s: SpectrumScene): void {
  ctx.save();
  ctx.setTransform(s.dpr, 0, 0, s.dpr, 0, 0);
  ctx.clearRect(0, 0, s.width / s.dpr, s.height / s.dpr);
  const { x, y, w, h } = s.inner;
  if (w <= 0 || h <= 0) {
    ctx.restore();
    return;
  }
  const c = s.colors;
  ctx.font = s.font;
  ctx.lineWidth = 1;
  // 周波数の目盛り（縦線と下の数）
  ctx.strokeStyle = c.grid;
  ctx.fillStyle = c.label;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'top';
  for (const f of FREQ_TICKS) {
    if (f < s.axis.lo || f > s.axis.hi) continue;
    const px = x + freqToX(f, s.axis, w);
    ctx.beginPath();
    ctx.moveTo(px, y);
    ctx.lineTo(px, y + h);
    ctx.stroke();
    ctx.fillText(tickLabel(f), px, y + h + 4);
  }
  // イコライザの dB（左）とスペクトルの dBFS（右）の横線。0 dB の線は濃く
  const eqStep = s.eq.top <= 6 ? 3 : s.eq.top <= 12 ? 6 : 10;
  ctx.textBaseline = 'middle';
  for (let v = s.eq.bottom; v <= s.eq.top + 1e-9; v += eqStep) {
    const py = y + dbToY(v, s.eq, h);
    ctx.strokeStyle = Math.abs(v) < 1e-9 ? c.zero : c.grid;
    ctx.beginPath();
    ctx.moveTo(x, py);
    ctx.lineTo(x + w, py);
    ctx.stroke();
    ctx.textAlign = 'right';
    ctx.fillText(v > 0 ? `+${v}` : v < 0 ? `−${-v}` : '0', x - 6, py);
  }
  const specStep = (s.spec.top - s.spec.bottom) / 3;
  ctx.textAlign = 'left';
  for (let i = 0; i <= 3; i++) {
    const v = s.spec.top - i * specStep;
    const py = y + dbToY(v, s.spec, h);
    ctx.fillText(v < 0 ? `−${Math.round(-v)}` : String(Math.round(v)), x + w + 6, py);
  }
  // アナライザ：後（塗り＋縁）・前（実線）・最大の保持（点線）
  if (s.post) {
    ctx.globalAlpha = s.dimPost ? 0.45 : 1;
    if (bandPath(ctx, s, s.post, true)) {
      ctx.fillStyle = c.post;
      ctx.fill();
    }
    if (bandPath(ctx, s, s.post, false)) {
      ctx.strokeStyle = c.postLine;
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
  }
  if (s.pre && bandPath(ctx, s, s.pre, false)) {
    ctx.strokeStyle = c.pre;
    ctx.stroke();
  }
  if (s.hold && bandPath(ctx, s, s.hold, false)) {
    ctx.strokeStyle = c.hold;
    ctx.lineWidth = 1.2;
    ctx.setLineDash([1.5, 3]);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.lineWidth = 1;
  }
  // イコライザ（段 4）
  const eq = s.eqLayer;
  if (eq) {
    if (eq.selected && curvePath(ctx, s, eq.freqs, eq.selected)) {
      ctx.strokeStyle = c.band;
      ctx.stroke();
      // 効く帯の塗り：0 dB の線との間
      const zero = y + dbToY(0, s.eq, h);
      ctx.beginPath();
      let started = false;
      let firstX = 0;
      let lastX = 0;
      for (let i = 0; i < Math.min(eq.freqs.length, eq.selected.length); i++) {
        const f = eq.freqs[i];
        const v = eq.selected[i];
        if (f == null || v == null || !Number.isFinite(v)) continue;
        const px = x + freqToX(f, s.axis, w);
        const py = y + dbToY(v, s.eq, h);
        if (!started) {
          ctx.moveTo(px, zero);
          firstX = px;
          started = true;
        }
        ctx.lineTo(px, py);
        lastX = px;
      }
      if (started) {
        ctx.lineTo(lastX, zero);
        ctx.lineTo(firstX, zero);
        ctx.closePath();
        ctx.fillStyle = c.bandFill;
        ctx.fill();
      }
    }
    if (eq.hovered && curvePath(ctx, s, eq.freqs, eq.hovered)) {
      ctx.strokeStyle = c.band;
      ctx.stroke();
    }
    if (curvePath(ctx, s, eq.freqs, eq.total)) {
      ctx.strokeStyle = c.accent;
      ctx.lineWidth = 2;
      ctx.stroke();
      ctx.lineWidth = 1;
    }
    for (const p of eq.points) {
      const r = p.selected ? 6 : 5;
      ctx.beginPath();
      ctx.arc(p.x, p.y, r, 0, Math.PI * 2);
      if (p.enabled) {
        ctx.fillStyle = p.selected ? c.accent : c.point;
        ctx.fill();
        ctx.strokeStyle = c.pointLine;
        ctx.stroke();
      } else {
        ctx.setLineDash([2, 2]);
        ctx.strokeStyle = c.point;
        ctx.stroke();
        ctx.setLineDash([]);
      }
      if (p.q) {
        ctx.fillStyle = c.point;
        for (const qx of [p.q.left, p.q.right]) {
          ctx.fillRect(qx - 2, p.y - 2, 4, 4);
        }
      }
    }
  }
  if (s.peak) {
    ctx.beginPath();
    ctx.arc(s.peak.x, s.peak.y, 4, 0, Math.PI * 2);
    ctx.strokeStyle = c.accent;
    ctx.stroke();
  }
  ctx.restore();
}
