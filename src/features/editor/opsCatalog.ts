/**
 * 操作のカタログから右パネルを組む純粋な関数（M12 の段 5b。docs/editing.md §7.2）。
 *
 * - 引数パネルは `ParamSchema` から組む（`controlFor`）：整数・小数は数の欄とスライダー、真偽はスイッチ、列挙は 4 個以下が
 *   セグメント・多ければ選択、矩形・点・大きさ（鎖）・長さ（px｜%｜自動）・色・格子は専用の欄。描いたもの・色の対応表は数だけ。
 * - 表示の条件（`ParamSpec.visible`）：同じ操作の列挙の引数の値が `values` のどれかのときだけ出す。値の検証・既定値には
 *   効かない（出さない引数も文書に残る）。
 * - 文言は `ja.editor.ops[id]`（名前・引数・列挙の値）。
 */

import { ja } from '../../i18n/ja';
import {
  EDIT_CATALOG,
  type EditNodeDto,
  type JsonValue,
  type OpDescriptor,
  type ParamSpec,
} from '../../ipc/edit';
import { formatCount } from '../../lib/format';
import type { EditorDoc } from './editorStore';
import { toHex } from './image/color';
import { type EditorKind, opInputsOf } from './kindTraits';

const t = ja.editor;

/** 引数の値の入れ物（ノードの `params`）。 */
export type ParamValues = Record<string, JsonValue>;

/** 引数の欄の種類。 */
export type ParamControl =
  | 'number'
  | 'switch'
  | 'segmented'
  | 'select'
  | 'rect'
  | 'point'
  | 'size'
  | 'length'
  | 'color'
  | 'grid'
  | 'ratio'
  | 'count'
  | 'frameRange'
  /** 余白 1 辺（`image.pad`。px｜%）。M14 段 7。 */
  | 'margin'
  /** キャンバスの 1 辺（`image.fit_canvas`。元のまま｜px｜%｜倍数｜2 の冪｜長い辺）。M14 段 7。 */
  | 'canvasLength'
  /** 9 点の揃え（左上〜右下の 3×3 のボタン）。M14 段 7。 */
  | 'anchor'
  | 'readonly';

/** 列挙をセグメントにする数の上限（それより多ければ選択）。 */
export const SEGMENTED_MAX = 4;

/** 「操作を追加」の分類。 */
export type OpCategory = keyof typeof ja.editor.opCategories;

const CATEGORY_ORDER: readonly OpCategory[] = [
  'geometry',
  'convert',
  'sprite',
  'paint',
  'audioTime',
  'audioSignal',
  'meta',
  'other',
];

/** 音声の時間の操作（切り詰め・フェード・ループ点）。 */
const AUDIO_TIME_OPS: readonly string[] = ['audio.trim', 'audio.fade', 'audio.set_loop'];

/** 操作の分類（docs/editing.md §7.4 の表。音声は「時間」と「音量・チャンネル・レート」）。 */
export function opCategory(id: string): OpCategory {
  if (AUDIO_TIME_OPS.includes(id)) return 'audioTime';
  if (id.startsWith('audio.')) return 'audioSignal';
  if (id === 'image.convert' || id.startsWith('palette.')) return 'convert';
  if (id.startsWith('image.')) return 'geometry';
  if (id.startsWith('sprite.')) return 'sprite';
  if (id.startsWith('paint.')) return 'paint';
  if (id.startsWith('meta.')) return 'meta';
  return 'other';
}

/** 操作の記述（今の版。`v` を渡すと版も比べる）。なければ null。 */
export function opSpec(
  id: string,
  v?: number,
  catalog: readonly OpDescriptor[] = EDIT_CATALOG,
): OpDescriptor | null {
  const d = catalog.find((o) => o.id === id);
  if (!d || (v !== undefined && d.version !== v)) return null;
  return d;
}

/**
 * その種類のエディタで足せる操作（最初の入力が種類の入力のどれかを受けるもの。画像は画像かフレームの列）を分類ごとに。
 * 相対の指定（余白・キャンバスを合わせる・9 点のピボット）も、段 7 で欄を作ったので足せる。
 */
export function addableOps(
  kind: EditorKind,
  catalog: readonly OpDescriptor[] = EDIT_CATALOG,
): { category: OpCategory; ops: OpDescriptor[] }[] {
  const inputs = opInputsOf(kind);
  const ops = catalog.filter((o) => o.signature.inputs[0]?.accepts.some((k) => inputs.includes(k)));
  return CATEGORY_ORDER.map((category) => ({
    category,
    ops: ops.filter((o) => opCategory(o.id) === category),
  })).filter((g) => g.ops.length > 0);
}

/** 操作の名前（文言がなければ ID）。 */
export function opName(id: string): string {
  return t.ops[id]?.name ?? id;
}

/** 引数の名前。 */
export function paramLabel(op: string, key: string): string {
  return t.ops[op]?.params[key] ?? key;
}

/** 列挙の値の名前。 */
export function enumLabel(op: string, key: string, value: string): string {
  return t.ops[op]?.values?.[key]?.[value] ?? value;
}

/** 単位の表記（なしは空）。 */
export function unitLabel(unit: string): string {
  return t.units[unit] ?? '';
}

/** 引数の今の値（ノードになければ既定値）。 */
export function paramValue(params: ParamValues, spec: ParamSpec): JsonValue {
  return Object.prototype.hasOwnProperty.call(params, spec.key)
    ? (params[spec.key] ?? null)
    : spec.default;
}

/** 表示の条件を満たすか（条件がなければいつも出す）。 */
export function isParamVisible(
  spec: ParamSpec,
  params: ParamValues,
  all: readonly ParamSpec[],
): boolean {
  const cond = spec.visible;
  if (!cond) return true;
  const target = all.find((p) => p.key === cond.key);
  const value = target ? paramValue(params, target) : params[cond.key];
  return typeof value === 'string' && cond.values.includes(value);
}

/** 出す引数（ふだんの欄と、「詳細」に畳む欄）。 */
export function visibleParams(
  desc: OpDescriptor,
  params: ParamValues,
): { basic: ParamSpec[]; advanced: ParamSpec[] } {
  const shown = desc.schema.params.filter((p) => isParamVisible(p, params, desc.schema.params));
  return { basic: shown.filter((p) => !p.advanced), advanced: shown.filter((p) => p.advanced) };
}

/** 引数の欄の種類。 */
export function controlFor(spec: ParamSpec): ParamControl {
  const k = spec.kind;
  switch (k.type) {
    case 'int':
    case 'float':
      return 'number';
    case 'bool':
      return 'switch';
    case 'enum':
      if (isAnchorEnum(k.values)) return 'anchor';
      return k.values.length <= SEGMENTED_MAX ? 'segmented' : 'select';
    case 'margin':
      return 'margin';
    case 'canvasLength':
      return 'canvasLength';
    case 'rect':
      return 'rect';
    case 'point':
      return 'point';
    case 'size':
      return 'size';
    case 'length':
      return 'length';
    case 'color':
      return 'color';
    case 'grid':
      return 'grid';
    case 'ratio':
      return 'ratio';
    case 'strokes':
    case 'paletteEntries':
      return 'count';
    case 'frameRange':
      return 'frameRange';
    default:
      return 'readonly';
  }
}

/** 数の範囲（スライダーを出せるのは両端が分かり、刻みの数がほどほどのとき）。 */
export function numberRange(spec: ParamSpec): {
  min: number | null;
  max: number | null;
  step: number;
  slider: boolean;
  integer: boolean;
} {
  const k = spec.kind;
  if (k.type === 'int') {
    return {
      min: k.min,
      max: k.max,
      step: k.step || 1,
      slider: k.max - k.min <= 65_535,
      integer: true,
    };
  }
  if (k.type === 'float') {
    const step = k.step ?? 0.01;
    const slider = k.min !== null && k.max !== null && (k.max - k.min) / step <= 100_000;
    return { min: k.min, max: k.max, step, slider, integer: false };
  }
  return { min: null, max: null, step: 1, slider: false, integer: true };
}

/** 数を範囲と刻みに収める（読めなければ null）。 */
export function clampNumber(spec: ParamSpec, v: number): number | null {
  if (!Number.isFinite(v)) return null;
  const r = numberRange(spec);
  let out = r.integer ? Math.round(v) : v;
  if (r.min !== null) out = Math.max(r.min, out);
  if (r.max !== null) out = Math.min(r.max, out);
  return out;
}

// ===== 値の形 =====

/** 矩形・点・大きさ・格子の値（オブジェクト）を数の表として読む。 */
export function numbers(value: JsonValue, keys: readonly string[]): Record<string, number> | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const out: Record<string, number> = {};
  for (const key of keys) {
    const v = value[key];
    out[key] = typeof v === 'number' ? v : 0;
  }
  return out;
}

export const RECT_KEYS = ['x', 'y', 'w', 'h'] as const;
export const POINT_KEYS = ['x', 'y'] as const;
export const SIZE_KEYS = ['w', 'h'] as const;
export const RATIO_KEYS = ['num', 'den'] as const;
export const GRID_KEYS = ['cellW', 'cellH', 'offsetX', 'offsetY', 'gapX', 'gapY'] as const;

/** 9 点の揃えの値（左上から行の順）。 */
export const ANCHOR_VALUES = [
  'topLeft',
  'top',
  'topRight',
  'left',
  'center',
  'right',
  'bottomLeft',
  'bottom',
  'bottomRight',
] as const;

/** 列挙の値がちょうど 9 点の揃えか。 */
export function isAnchorEnum(values: readonly string[]): boolean {
  return values.length === ANCHOR_VALUES.length && ANCHOR_VALUES.every((v) => values.includes(v));
}

/** 長さの単位。 */
export type LengthMode = 'px' | 'percent' | 'auto';

/** 長さ（`{"px": n}`・`{"scale": {num, den}}`・`"auto"`）の単位と数（% は 100 倍して丸める）。 */
export function readLength(value: JsonValue): { mode: LengthMode; n: number } {
  if (value === 'auto') return { mode: 'auto', n: 0 };
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    if (typeof value.px === 'number') return { mode: 'px', n: value.px };
    const scale = value.scale;
    if (scale && typeof scale === 'object' && !Array.isArray(scale)) {
      const num = typeof scale.num === 'number' ? scale.num : 0;
      const den = typeof scale.den === 'number' && scale.den > 0 ? scale.den : 1;
      return { mode: 'percent', n: Math.round((num * 10_000) / den) / 100 };
    }
  }
  return { mode: 'auto', n: 0 };
}

/** 長さの値を作る（% は 100 分の 1 の比。小数の % は 1 万分の 1 まで）。 */
export function makeLength(mode: LengthMode, n: number): JsonValue {
  if (mode === 'auto') return 'auto';
  if (mode === 'px') return { px: Math.max(1, Math.round(n)) };
  const hundredths = Math.max(1, Math.round(n * 100));
  return hundredths % 100 === 0
    ? { scale: { num: hundredths / 100, den: 100 } }
    : { scale: { num: hundredths, den: 10_000 } };
}

/** 比（`{num, den}`）を % にする（100 分の 1 まで）。 */
function ratioPercent(scale: JsonValue): number {
  if (!scale || typeof scale !== 'object' || Array.isArray(scale)) return 0;
  const num = typeof scale.num === 'number' ? scale.num : 0;
  const den = typeof scale.den === 'number' && scale.den > 0 ? scale.den : 1;
  return Math.round((num * 10_000) / den) / 100;
}

/** % を比にする（整数の % は 100 分の 1、小数は 1 万分の 1。0 以上）。 */
function percentRatio(n: number): JsonValue {
  const hundredths = Math.max(0, Math.round(n * 100));
  return hundredths % 100 === 0
    ? { num: hundredths / 100, den: 100 }
    : { num: hundredths, den: 10_000 };
}

/** 余白の単位。 */
export type MarginMode = 'px' | 'percent';

/** 余白 1 辺（`{"px": n}`（負で削る）・`{"scale": {num, den}}`）の単位と数。 */
export function readMargin(value: JsonValue): { mode: MarginMode; n: number } {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    if (typeof value.px === 'number') return { mode: 'px', n: value.px };
    if (value.scale !== undefined) return { mode: 'percent', n: ratioPercent(value.scale) };
  }
  return { mode: 'px', n: 0 };
}

/** 余白 1 辺の値を作る（px は整数で負も可、% は 0 以上）。 */
export function makeMargin(mode: MarginMode, n: number): JsonValue {
  return mode === 'px' ? { px: Math.round(n) } : { scale: percentRatio(n) };
}

/** キャンバスの 1 辺の決め方。 */
export type CanvasMode = 'keep' | 'px' | 'percent' | 'multipleOf' | 'pow2' | 'longest';

/** キャンバスの 1 辺の決め方の並び（選択の順）。 */
export const CANVAS_MODES: readonly CanvasMode[] = [
  'keep',
  'px',
  'percent',
  'multipleOf',
  'pow2',
  'longest',
];

/** キャンバスの 1 辺（`"keep"`・`{"px"}`・`{"scale"}`・`{"multipleOf"}`・`"pow2"`・`"longest"`）の決め方と数。 */
export function readCanvasLength(value: JsonValue): { mode: CanvasMode; n: number } {
  if (value === 'pow2' || value === 'longest' || value === 'keep') return { mode: value, n: 0 };
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    if (typeof value.px === 'number') return { mode: 'px', n: value.px };
    if (typeof value.multipleOf === 'number') return { mode: 'multipleOf', n: value.multipleOf };
    if (value.scale !== undefined) return { mode: 'percent', n: ratioPercent(value.scale) };
  }
  return { mode: 'keep', n: 0 };
}

/** キャンバスの 1 辺の値を作る（px・倍数は 1 以上の整数、% は 1 万分の 1 まで）。 */
export function makeCanvasLength(mode: CanvasMode, n: number): JsonValue {
  switch (mode) {
    case 'px':
      return { px: Math.max(1, Math.round(n)) };
    case 'multipleOf':
      return { multipleOf: Math.max(1, Math.round(n)) };
    case 'percent':
      return { scale: percentRatio(Math.max(0.01, n)) };
    default:
      return mode;
  }
}

/** 決め方を替えたときの既定の数（px は今の大きさ、% は 100、倍数は 8）。 */
export function canvasDefault(mode: CanvasMode, size: number | null): number {
  if (mode === 'px') return Math.max(1, size ?? 1);
  if (mode === 'percent') return 100;
  if (mode === 'multipleOf') return 8;
  return 0;
}

/** 単位を替えたときの既定の数（px は今の大きさ、% は 100）。 */
export function lengthDefault(mode: LengthMode, size: number | null): number {
  return mode === 'px' ? Math.max(1, size ?? 1) : mode === 'percent' ? 100 : 0;
}

/** 色の値（`[r, g, b]` か `[r, g, b, a]`）を RGBA にする（null・読めなければ null）。 */
export function readColor(value: JsonValue): [number, number, number, number] | null {
  if (!Array.isArray(value) || (value.length !== 3 && value.length !== 4)) return null;
  const n = value.map((v) =>
    typeof v === 'number' ? Math.min(255, Math.max(0, Math.round(v))) : 0,
  );
  return [n[0] ?? 0, n[1] ?? 0, n[2] ?? 0, value.length === 4 ? (n[3] ?? 255) : 255];
}

/** RGBA を色の値にする（アルファを持たない引数は 3 つ）。 */
export function makeColor(rgba: readonly number[], alpha: boolean): JsonValue {
  const [r = 0, g = 0, b = 0, a = 255] = rgba;
  return alpha ? [r, g, b, a] : [r, g, b];
}

/**
 * 引数の既定の値が null のとき、「指定する」にしたときの初めの値。`frames` は音声のノードの入力の長さ（区間・標本の
 * 位置は全体にする）。
 */
export function initialValue(
  spec: ParamSpec,
  size: { w: number; h: number } | null,
  frames: number | null = null,
): JsonValue {
  switch (spec.kind.type) {
    case 'frameRange':
      return { start: 0, end: Math.max(1, frames ?? 1) };
    case 'color':
      return spec.kind.alpha ? [0, 0, 0, 255] : [0, 0, 0];
    case 'grid':
      return { cellW: 16, cellH: 16, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0 };
    case 'point':
      return size ? { x: Math.floor(size.w / 2), y: Math.floor(size.h / 2) } : { x: 0, y: 0 };
    case 'rect':
      return { x: 0, y: 0, w: size?.w ?? 1, h: size?.h ?? 1 };
    case 'size':
      return { w: size?.w ?? 1, h: size?.h ?? 1 };
    case 'length':
      return 'auto';
    case 'int':
    case 'float': {
      const r = numberRange(spec);
      if (spec.unit === 'samples' && frames !== null) {
        return Math.min(r.max ?? frames, Math.max(r.min ?? 0, frames));
      }
      return r.min ?? 0;
    }
    case 'bool':
      return false;
    case 'enum':
      return spec.kind.values[0] ?? null;
    default:
      return null;
  }
}

/**
 * 操作を足すときの引数（既定値。矩形・幅は今の出力の大きさ `size` から：切り抜き・キャンバスは全体、大きさを変えるは
 * 今の幅。末尾に足すときに合う）。
 */
export function defaultParams(
  desc: OpDescriptor,
  size: { w: number; h: number } | null,
): ParamValues {
  const out: ParamValues = {};
  for (const p of desc.schema.params) out[p.key] = p.default;
  if (size) {
    if ((desc.id === 'image.crop' || desc.id === 'image.canvas') && 'rect' in out) {
      out.rect = { x: 0, y: 0, w: size.w, h: size.h };
    }
    if (desc.id === 'image.resize' && 'width' in out) out.width = { px: size.w };
  }
  return out;
}

// ===== 要約 =====

function numberText(v: JsonValue): string | null {
  return typeof v === 'number' ? formatCount(v) : null;
}

/** 引数 1 つの短い表記（出さないものは null）。 */
export function valueSummary(op: string, spec: ParamSpec, value: JsonValue): string | null {
  const k = spec.kind;
  if (value === null) return k.type === 'color' ? t.transparent : t.none;
  switch (k.type) {
    case 'int':
    case 'float': {
      const n = numberText(value);
      const unit = unitLabel(spec.unit);
      return n === null ? null : unit ? `${n} ${unit}` : n;
    }
    case 'bool':
      return value === true ? paramLabel(op, spec.key) : null;
    case 'enum':
      return typeof value === 'string' ? enumLabel(op, spec.key, value) : null;
    case 'rect': {
      const r = numbers(value, RECT_KEYS);
      return r ? t.size(formatCount(r.w ?? 0), formatCount(r.h ?? 0)) : null;
    }
    case 'size': {
      const r = numbers(value, SIZE_KEYS);
      return r ? t.size(formatCount(r.w ?? 0), formatCount(r.h ?? 0)) : null;
    }
    case 'point': {
      const r = numbers(value, POINT_KEYS);
      return r ? `${formatCount(r.x ?? 0)}, ${formatCount(r.y ?? 0)}` : null;
    }
    case 'grid': {
      const r = numbers(value, GRID_KEYS);
      return r ? t.size(formatCount(r.cellW ?? 0), formatCount(r.cellH ?? 0)) : null;
    }
    case 'length': {
      const l = readLength(value);
      return l.mode === 'auto'
        ? t.lengthModes.auto
        : l.mode === 'px'
          ? `${formatCount(l.n)} px`
          : `${l.n}%`;
    }
    case 'color': {
      const c = readColor(value);
      return c ? toHex(c) : null;
    }
    case 'frameRange': {
      const r = numbers(value, ['start', 'end']);
      return r ? `${formatCount(r.start ?? 0)} – ${formatCount(r.end ?? 0)}` : null;
    }
    case 'margin': {
      const m = readMargin(value);
      return m.mode === 'px' ? `${formatCount(m.n)} px` : `${m.n}%`;
    }
    case 'canvasLength': {
      const c = readCanvasLength(value);
      if (c.mode === 'px') return `${formatCount(c.n)} px`;
      if (c.mode === 'percent') return `${c.n}%`;
      if (c.mode === 'multipleOf') return t.canvasMultiple(formatCount(c.n));
      return t.canvasModes[c.mode];
    }
    default:
      return null;
  }
}

/** スタックの行の要約（描画は本数、ほかはふだんの引数の短い表記を 2 つまで）。 */
export function summarizeNode(
  node: EditNodeDto,
  desc: OpDescriptor | null = opSpec(node.op),
): string {
  if (node.log) {
    const n = formatCount(node.log.count);
    return node.op === 'palette.replace' ? t.colorCount(n) : t.strokeCount(n);
  }
  if (!desc) return '';
  const params = (
    node.params && typeof node.params === 'object' && !Array.isArray(node.params) ? node.params : {}
  ) as ParamValues;
  const { basic } = visibleParams(desc, params);
  const parts: string[] = [];
  for (const spec of basic) {
    if (spec.kind.type === 'length' && spec.key === 'height') continue;
    const text = valueSummary(node.op, spec, paramValue(params, spec));
    if (text) parts.push(text);
    if (parts.length >= 2) break;
  }
  return parts.join(' · ');
}

/** ノードの引数（オブジェクトでなければ空）。 */
export function paramsOf(node: EditNodeDto): ParamValues {
  return node.params && typeof node.params === 'object' && !Array.isArray(node.params)
    ? (node.params as ParamValues)
    : {};
}

/** 画像の大きさ・向き・並びを変える操作か（画像の幾何・スプライト。描画・変換・メタデータは変えない）。 */
export function changesGeometry(id: string): boolean {
  const c = opCategory(id);
  return c === 'geometry' || c === 'sprite';
}

/**
 * このノードより後ろに、有効な幾何の操作があるか（あれば、このノードを外した文書の出力はこのノードの入力の座標に
 * ならない：切り抜きの枠はその操作の入力に重ねるので、枠では直せない）。
 */
export function hasLaterGeometry(doc: EditorDoc, id: number): boolean {
  const i = doc.order.indexOf(id);
  if (i < 0) return false;
  return doc.order.slice(i + 1).some((n) => {
    const node = doc.nodes[n];
    return node !== undefined && node.enabled && changesGeometry(node.op);
  });
}

/** このノードより後ろに描画（`paint.actions`）があるか（前の操作を変えても描画の位置は動かない）。 */
export function hasLaterPaint(doc: EditorDoc, id: number): boolean {
  const i = doc.order.indexOf(id);
  if (i < 0) return false;
  return doc.order.slice(i + 1).some((n) => doc.nodes[n]?.op === 'paint.actions');
}
