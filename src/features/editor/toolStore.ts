/**
 * エディタの道具の設定（道具・筆・色・最近の 8 色。M12 の決定 5：描画の色は 1 色＋最近の 8 色＋スポイト）。
 *
 * 見た目の設定なので localStorage に置く（読めない・書けないときは既定のまま。ライブラリのデータは置かない）。
 */

import { create } from 'zustand';

import type { BrushShape } from '../../lib/paint';
import type { Blend, PaintColor } from './stroke';

/** 道具（切り抜きは段 5b）。 */
export type EditorTool = 'hand' | 'pen' | 'eraser' | 'fill' | 'eyedropper';

export const EDITOR_TOOLS: readonly EditorTool[] = ['hand', 'pen', 'eraser', 'fill', 'eyedropper'];

/** 最近の色の数。 */
export const RECENT_COLORS = 8;
/** 筆の大きさの範囲（エンジンの `Brush.size`）。 */
export const BRUSH_MIN = 1;
export const BRUSH_MAX = 255;

const STORAGE_KEY = 'kura.editor.tools';

export interface ToolSettings {
  tool: EditorTool;
  shape: BrushShape;
  size: number;
  pixelPerfect: boolean;
  blend: Blend;
  /** 塗りつぶしの許容差（0〜255）。 */
  tolerance: number;
  connectivity: 'four' | 'eight';
  /** つながった範囲だけを塗る（偽なら画像全体の同じ色）。 */
  contiguous: boolean;
  color: PaintColor;
  recent: PaintColor[];
}

interface ToolState extends ToolSettings {
  setTool: (tool: EditorTool) => void;
  setShape: (shape: BrushShape) => void;
  setSize: (size: number) => void;
  stepSize: (delta: number) => void;
  setPixelPerfect: (on: boolean) => void;
  setBlend: (blend: Blend) => void;
  setTolerance: (tolerance: number) => void;
  setConnectivity: (c: 'four' | 'eight') => void;
  setContiguous: (on: boolean) => void;
  setColor: (color: PaintColor) => void;
  /** 描いた色を最近の色の先頭に置く（同じ色は 1 つ）。 */
  rememberColor: (color: PaintColor) => void;
}

export const DEFAULT_TOOLS: ToolSettings = {
  tool: 'pen',
  shape: 'square',
  size: 1,
  pixelPerfect: false,
  blend: 'over',
  tolerance: 0,
  connectivity: 'four',
  contiguous: true,
  color: { rgba: [0, 0, 0, 255], index: null },
  recent: [],
};

function clampInt(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, Math.round(Number.isFinite(v) ? v : lo)));
}

function byte(v: unknown): number {
  return typeof v === 'number' ? clampInt(v, 0, 255) : 0;
}

/** 色を確かめて直す（読めなければ null）。 */
export function normalizeColor(value: unknown): PaintColor | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as { rgba?: unknown; index?: unknown };
  if (!Array.isArray(v.rgba) || v.rgba.length !== 4) return null;
  const [r, g, b, a] = v.rgba.map(byte) as [number, number, number, number];
  const index = typeof v.index === 'number' ? clampInt(v.index, 0, 255) : null;
  return { rgba: [r, g, b, a], index };
}

/** 同じ色か（RGBA と番号）。 */
export function sameColor(a: PaintColor, b: PaintColor): boolean {
  return a.index === b.index && a.rgba.every((v, i) => v === b.rgba[i]);
}

/** 最近の色の先頭に置く（同じ色は 1 つ。8 色まで）。 */
export function pushRecent(recent: readonly PaintColor[], color: PaintColor): PaintColor[] {
  return [color, ...recent.filter((c) => !sameColor(c, color))].slice(0, RECENT_COLORS);
}

/** 保存した設定を読む（項目ごとに確かめ、だめなものは既定）。 */
export function parseSettings(text: string | null): ToolSettings {
  if (!text) return DEFAULT_TOOLS;
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return DEFAULT_TOOLS;
  }
  if (!raw || typeof raw !== 'object') return DEFAULT_TOOLS;
  const r = raw as Partial<Record<keyof ToolSettings, unknown>>;
  const d = DEFAULT_TOOLS;
  return {
    tool: EDITOR_TOOLS.includes(r.tool as EditorTool) ? (r.tool as EditorTool) : d.tool,
    shape: r.shape === 'circle' || r.shape === 'square' ? r.shape : d.shape,
    size: typeof r.size === 'number' ? clampInt(r.size, BRUSH_MIN, BRUSH_MAX) : d.size,
    pixelPerfect: typeof r.pixelPerfect === 'boolean' ? r.pixelPerfect : d.pixelPerfect,
    blend: r.blend === 'replace' || r.blend === 'over' ? r.blend : d.blend,
    tolerance: typeof r.tolerance === 'number' ? clampInt(r.tolerance, 0, 255) : d.tolerance,
    connectivity:
      r.connectivity === 'eight' || r.connectivity === 'four' ? r.connectivity : d.connectivity,
    contiguous: typeof r.contiguous === 'boolean' ? r.contiguous : d.contiguous,
    color: normalizeColor(r.color) ?? d.color,
    recent: Array.isArray(r.recent)
      ? r.recent
          .map(normalizeColor)
          .filter((c): c is PaintColor => c !== null)
          .slice(0, RECENT_COLORS)
      : d.recent,
  };
}

function load(): ToolSettings {
  try {
    return parseSettings(window.localStorage.getItem(STORAGE_KEY));
  } catch {
    return DEFAULT_TOOLS;
  }
}

function save(s: ToolSettings): void {
  try {
    const out: ToolSettings = {
      tool: s.tool,
      shape: s.shape,
      size: s.size,
      pixelPerfect: s.pixelPerfect,
      blend: s.blend,
      tolerance: s.tolerance,
      connectivity: s.connectivity,
      contiguous: s.contiguous,
      color: s.color,
      recent: s.recent,
    };
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(out));
  } catch {
    // 書けなければ（プライベートな窓など）このセッションの間だけ覚える
  }
}

export const useToolStore = create<ToolState>((set, get) => {
  const update = (patch: Partial<ToolSettings>) => {
    set(patch);
    save(get());
  };
  return {
    ...load(),
    setTool: (tool) => update({ tool }),
    setShape: (shape) => update({ shape }),
    setSize: (size) => update({ size: clampInt(size, BRUSH_MIN, BRUSH_MAX) }),
    stepSize: (delta) => update({ size: clampInt(get().size + delta, BRUSH_MIN, BRUSH_MAX) }),
    setPixelPerfect: (pixelPerfect) => update({ pixelPerfect }),
    setBlend: (blend) => update({ blend }),
    setTolerance: (tolerance) => update({ tolerance: clampInt(tolerance, 0, 255) }),
    setConnectivity: (connectivity) => update({ connectivity }),
    setContiguous: (contiguous) => update({ contiguous }),
    setColor: (color) => update({ color }),
    rememberColor: (color) => update({ recent: pushRecent(get().recent, color) }),
  };
});
