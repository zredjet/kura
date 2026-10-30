/**
 * 波形の面を canvas に描く（M13 の段 6。design.md §3.1 の「音声エディタ」の案文、設計 §2.3）。`WaveController` が作った
 * 中身（`WaveScene`）をそのまま描くだけで、状態を持たない。
 *
 * - 上から目盛り（時刻の刻みは 11px tertiary、ループの旗とマーカーの刻みもここ）・全体図（最も粗い段で全体、見ている
 *   範囲を --fill-2 の地と 1px の --text-secondary の枠で）・チャンネルの段（区切りは 1px --stroke-1、左上に「L」「R」…）。
 * - 波形の線は --text-secondary（M11 と同じ）。ループは --loop-region の地と両端 1px --accent。選択は --wave-selection、
 *   切り詰めの外は --wave-outside。フェードは 1.5px --accent の曲線と、段の上の角の 10px の三角（--text-on-media）。
 *   カーソルは 1px --text-primary。再生位置は canvas の外の要素（描かない）。
 */

import { ja } from '../../../i18n/ja';
import type { WaveScene } from './waveController';
import { FADE_HANDLE_PX, frameToX } from './waveMath';

const t = ja.editor.audio;

/** フェードの曲線を描く点の数。 */
const CURVE_STEPS = 32;

/** 段の中の波形の高さの割合（上下に少し余白）。 */
const WAVE_FILL = 0.92;

export function drawWave(canvas: HTMLCanvasElement, s: WaveScene): void {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const d = s.dpr;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  const ix = Math.round(s.inner.x * d);
  const iy = Math.round(s.inner.y * d);
  const iw = s.columns;
  const x = (frame: number) => ix + frameToX(frame, s.window, s.inner.w) * d;
  const lanesTop = iy + (s.layout.lanes[0]?.y ?? 0) * d;
  const lastLane = s.layout.lanes[s.layout.lanes.length - 1];
  const lanesBottom = iy + (lastLane ? lastLane.y + lastLane.h : 0) * d;
  const lanesH = lanesBottom - lanesTop;
  const rulerY = iy + s.layout.ruler.y * d;
  const rulerH = s.layout.ruler.h * d;
  const ovY = iy + s.layout.overview.y * d;
  const ovH = s.layout.overview.h * d;
  const o = s.overlay;
  const c = s.colors;
  const clampX = (v: number) => Math.min(ix + iw, Math.max(ix, v));

  ctx.save();
  ctx.beginPath();
  ctx.rect(ix, iy, iw, Math.max(0, lanesBottom - iy));
  ctx.clip();

  // ループの区間（目盛りと段に地、両端に 1px）
  if (o.loop) {
    const a = clampX(x(o.loop.start));
    const b = clampX(x(o.loop.end));
    if (b > a) {
      ctx.fillStyle = c.loop;
      ctx.fillRect(a, rulerY, b - a, rulerH);
      ctx.fillRect(a, lanesTop, b - a, lanesH);
    }
  }

  // 選択
  if (o.selection && o.selection.end > o.selection.start) {
    const a = clampX(x(o.selection.start));
    const b = clampX(x(o.selection.end));
    ctx.fillStyle = c.selection;
    ctx.fillRect(a, lanesTop, Math.max(d, b - a), lanesH);
  }

  // チャンネルの段：波形
  ctx.fillStyle = c.wave;
  const ch = s.channels;
  for (const [i, lane] of s.layout.lanes.entries()) {
    const top = iy + lane.y * d;
    const h = lane.h * d;
    const mid = top + h / 2;
    const half = (h / 2) * WAVE_FILL;
    if (s.points && s.points[i]) {
      continue;
    }
    for (let col = 0; col < s.columns; col++) {
      if (!s.has[col]) continue;
      const at = (col * ch + i) * 2;
      const mn = s.lanes[at] ?? 0;
      const mx = s.lanes[at + 1] ?? 0;
      const y0 = Math.min(mid - mx * half, mid - d * 0.5);
      const y1 = Math.max(mid - mn * half, mid + d * 0.5);
      ctx.fillRect(ix + col, y0, 1, y1 - y0);
    }
  }

  // 標本の点（拡大して 1 フレームが 8 装置画素を超えたとき）：点を結ぶ線と小さな四角
  if (s.points) {
    ctx.strokeStyle = c.wave;
    ctx.lineWidth = Math.max(1, d);
    for (const [i, lane] of s.layout.lanes.entries()) {
      const pts = s.points[i];
      if (!pts) continue;
      const top = iy + lane.y * d;
      const h = lane.h * d;
      const mid = top + h / 2;
      const half = (h / 2) * WAVE_FILL;
      ctx.beginPath();
      for (let k = 0; k < pts.length; k++) {
        const px = x(s.pointsStart + k);
        const py = mid - (pts[k] ?? 0) * half;
        if (k === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.stroke();
      const r = 1.5 * d;
      for (let k = 0; k < pts.length; k++) {
        ctx.fillRect(x(s.pointsStart + k) - r, mid - (pts[k] ?? 0) * half - r, r * 2, r * 2);
      }
    }
  }

  // 段の区切りと「L」「R」
  ctx.fillStyle = c.separator;
  for (const [i, lane] of s.layout.lanes.entries()) {
    if (i > 0) ctx.fillRect(ix, Math.round(iy + lane.y * d), iw, Math.max(1, Math.round(d)));
  }
  ctx.fillStyle = c.label;
  ctx.font = scaledFont(s.metrics.font, d);
  ctx.textBaseline = 'top';
  for (const [i, lane] of s.layout.lanes.entries()) {
    ctx.fillText(t.channelLabel(i, ch), ix + 4 * d, iy + lane.y * d + 3 * d);
  }

  // 切り詰めの枠の外
  if (o.trim) {
    const a = clampX(x(o.trim.start));
    const b = clampX(x(o.trim.end));
    ctx.fillStyle = c.outside;
    if (a > ix) ctx.fillRect(ix, lanesTop, a - ix, lanesH);
    if (b < ix + iw) ctx.fillRect(b, lanesTop, ix + iw - b, lanesH);
    ctx.fillStyle = c.accent;
    ctx.fillRect(a - d, lanesTop, Math.max(1, 2 * d), lanesH);
    ctx.fillRect(b - d, lanesTop, Math.max(1, 2 * d), lanesH);
  }

  // フェードの曲線（段の下から上へ。等パワーは正弦）と三角のハンドル
  if (o.fade) {
    ctx.strokeStyle = c.accent;
    ctx.lineWidth = 1.5 * d;
    const curve = (u: number) => (o.fade?.curve === 'equalPower' ? Math.sin((u * Math.PI) / 2) : u);
    if (o.fade.inEnd > 0) {
      ctx.beginPath();
      for (let k = 0; k <= CURVE_STEPS; k++) {
        const u = k / CURVE_STEPS;
        const px = x(u * o.fade.inEnd);
        const py = lanesBottom - curve(u) * lanesH;
        if (k === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.stroke();
    }
    if (o.fade.outStart < s.total) {
      ctx.beginPath();
      for (let k = 0; k <= CURVE_STEPS; k++) {
        const u = k / CURVE_STEPS;
        const px = x(o.fade.outStart + u * (s.total - o.fade.outStart));
        const py = lanesBottom - curve(1 - u) * lanesH;
        if (k === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.stroke();
    }
    if (o.fade.handles) {
      const size = FADE_HANDLE_PX * d;
      ctx.fillStyle = c.handle;
      const xi = x(o.fade.inEnd);
      ctx.beginPath();
      ctx.moveTo(xi, lanesTop);
      ctx.lineTo(xi + size, lanesTop);
      ctx.lineTo(xi, lanesTop + size);
      ctx.closePath();
      ctx.fill();
      const xo = x(o.fade.outStart);
      ctx.beginPath();
      ctx.moveTo(xo, lanesTop);
      ctx.lineTo(xo - size, lanesTop);
      ctx.lineTo(xo, lanesTop + size);
      ctx.closePath();
      ctx.fill();
    }
  }

  // ループの両端の線（段と目盛りを通す）と旗
  if (o.loop) {
    ctx.fillStyle = c.accent;
    for (const f of [o.loop.start, o.loop.end]) {
      const px = Math.round(x(f));
      ctx.fillRect(px, rulerY, Math.max(1, Math.round(d)), lanesBottom - rulerY);
    }
    const flag = 6 * d;
    const sx = Math.round(x(o.loop.start));
    const ex = Math.round(x(o.loop.end));
    ctx.beginPath();
    ctx.moveTo(sx, rulerY);
    ctx.lineTo(sx + flag, rulerY + flag / 2);
    ctx.lineTo(sx, rulerY + flag);
    ctx.closePath();
    ctx.fill();
    ctx.beginPath();
    ctx.moveTo(ex, rulerY);
    ctx.lineTo(ex - flag, rulerY + flag / 2);
    ctx.lineTo(ex, rulerY + flag);
    ctx.closePath();
    ctx.fill();
  }

  // カーソル
  if (o.cursor !== null) {
    ctx.fillStyle = c.cursor;
    ctx.fillRect(Math.round(x(o.cursor)), lanesTop, Math.max(1, Math.round(d)), lanesH);
  }
  ctx.restore();

  // 目盛り：刻みと文字、マーカーの刻み
  ctx.fillStyle = c.label;
  ctx.font = scaledFont(s.metrics.font, d);
  ctx.textBaseline = 'middle';
  for (const tick of s.ticks) {
    const px = Math.round(ix + tick.x * d);
    if (px < ix || px > ix + iw) continue;
    ctx.fillRect(px, rulerY + rulerH - 4 * d, Math.max(1, Math.round(d)), 4 * d);
    ctx.fillText(tick.label, px + 3 * d, rulerY + rulerH / 2);
  }
  ctx.fillStyle = c.accent;
  for (const m of o.markers) {
    const px = Math.round(x(m.pos));
    if (px < ix || px > ix + iw) continue;
    ctx.fillRect(px, rulerY + rulerH - 8 * d, Math.max(1, Math.round(d)), 8 * d);
  }

  // 全体図：チャンネルをまとめた最も粗い段と、見ている範囲の枠
  ctx.fillStyle = c.wave;
  const ovMid = ovY + ovH / 2;
  const ovHalf = (ovH / 2) * WAVE_FILL;
  for (let col = 0; col < s.overviewColumns; col++) {
    if (!s.overviewHas[col]) continue;
    let mn = 0;
    let mx = 0;
    for (let k = 0; k < ch; k++) {
      mn = Math.min(mn, s.overview[(col * ch + k) * 2] ?? 0);
      mx = Math.max(mx, s.overview[(col * ch + k) * 2 + 1] ?? 0);
    }
    const y0 = Math.min(ovMid - mx * ovHalf, ovMid - d * 0.5);
    const y1 = Math.max(ovMid - mn * ovHalf, ovMid + d * 0.5);
    ctx.fillRect(ix + col, y0, 1, y1 - y0);
  }
  if (s.total > 0) {
    const a = ix + (s.window.start / s.total) * iw;
    const w = Math.max(2 * d, (s.window.span / s.total) * iw);
    ctx.fillStyle = c.overviewWindow;
    ctx.fillRect(a, ovY, w, ovH);
    ctx.strokeStyle = c.overviewFrame;
    ctx.lineWidth = Math.max(1, Math.round(d));
    ctx.strokeRect(a + 0.5, ovY + 0.5, w - 1, ovH - 1);
  }
}

/** 「11px sans-serif」を装置の画素に直す。 */
function scaledFont(font: string, d: number): string {
  return font.replace(/(\d+(?:\.\d+)?)px/, (_m, n: string) => `${Number(n) * d}px`);
}
