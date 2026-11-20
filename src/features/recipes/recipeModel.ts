/**
 * レシピの画面と保存のシートの純粋な関数（M14 段 7。docs/editing.md §4.3、ADR-199）。
 *
 * - 操作の行：名前・引数の短い表記（エディタのスタックと同じ `summarizeNode`）・アイテムごとに決まる値の印。
 * - 結び付けの欄：結び付けられる引数（`ParamSpec.binds`）ごとに「見本の値のまま／素材の設定から／秒で」を選ぶ。区間（ループ点）は
 *   全体を素材の試聴のループに結ぶか、始め・終わりを別々に秒に結ぶ。
 */

import { ja } from '../../i18n/ja';
import type { RangePart, SettingKey, Source as BindSource, TimeAnchor } from '../../ipc/bindings';
import { EDIT_CATALOG, type EditNodeDto, type JsonValue, type OpDescriptor } from '../../ipc/edit';
import type { ItemKind } from '../../ipc/kinds';
import type { RecipeBinding } from '../../ipc/recipes';
import { opName, opSpec, paramLabel, summarizeNode } from '../editor/opsCatalog';

const t = ja.recipes;

/** レシピが扱う種類（音声の操作があれば音声、ほかは画像）。 */
export type RecipeMedia = Extract<ItemKind, 'image'> | Extract<ItemKind, 'audio'>;

/** 文書のノードの、画面に要る分。 */
export interface RecipeNodeLike {
  id: number;
  op: string;
  v: number;
  params?: JsonValue;
  enabled?: boolean;
}

/** 操作の列から、画像のレシピか音声のレシピか。 */
export function recipeMedia(ops: readonly string[]): RecipeMedia {
  return ops.some((op) => op.startsWith('audio.')) ? 'audio' : 'image';
}

/** 形式の ID の表示名（PNG など）。 */
export function formatLabel(format: string): string {
  return (ja.editor.formats as Record<string, string>)[format] ?? format.toUpperCase();
}

/** ミリ秒を秒の表記に（小数 3 桁まで、末尾の 0 は落とす）。 */
export function secondsText(ms: number): string {
  return String(Math.round(ms) / 1000);
}

/** 結び付けの出どころの言い方。 */
export function sourceLabel(from: BindSource): string {
  if (from.type === 'time') {
    const s = secondsText(from.ms);
    return from.anchor === 'end' ? t.from.timeEnd(s) : t.from.timeStart(s);
  }
  return t.from[from.key];
}

/** 区間の片方の言い方。 */
function partLabel(part: RangePart | null | undefined): string {
  if (part === 'start') return ` · ${ja.saveRecipe.rangeStart}`;
  if (part === 'end') return ` · ${ja.saveRecipe.rangeEnd}`;
  return '';
}

/** レシピの画面の操作の行。 */
export interface RecipeRow {
  id: number;
  op: string;
  name: string;
  summary: string;
  enabled: boolean;
  /** アイテムごとに決まる値（「格子：素材の格子」）。 */
  bound: string[];
}

/** ノードをエディタのスタックと同じ形にする（要約を同じ関数で作る）。 */
function asEditNode(node: RecipeNodeLike): EditNodeDto {
  return {
    id: node.id,
    op: node.op,
    v: node.v,
    enabled: node.enabled ?? true,
    params: node.params ?? {},
    log: null,
  };
}

/** レシピの画面の操作の行（文書の並び）。 */
export function recipeRows(
  nodes: readonly RecipeNodeLike[],
  bindings: readonly RecipeBinding[],
): RecipeRow[] {
  return nodes.map((node) => ({
    id: node.id,
    op: node.op,
    name: opName(node.op),
    summary: summarizeNode(asEditNode(node), opSpec(node.op, node.v)),
    enabled: node.enabled ?? true,
    bound: bindings
      .filter((b) => b.node === node.id)
      .map((b) => `${paramLabel(node.op, b.param)}${partLabel(b.part)}：${sourceLabel(b.from)}`),
  }));
}

/** 出力の形式を決めていれば、その形式の ID（「元と同じ」なら null）。 */
export function outputFormat(
  outputs: readonly { format?: { type: string; format?: string } }[],
): string | null {
  const f = outputs[0]?.format;
  return f?.type === 'to' && f.format ? f.format : null;
}

/** 名前の既定（操作の名前を 3 つまで「→」でつなぐ）。 */
export function defaultRecipeName(ops: readonly string[]): string {
  const names = ops.slice(0, 3).map(opName);
  return ops.length > 3 ? `${names.join(' → ')} …` : names.join(' → ');
}

/** 初めから含めない操作（この画像の座標・色の番号のままの描画とパレットの置き換え）。 */
export const EXCLUDED_BY_DEFAULT: readonly string[] = ['paint.actions', 'palette.replace'];

/** 含める操作の既定（無効にした操作・描画・パレットの置き換えを外す）。 */
export function defaultKeep(nodes: readonly RecipeNodeLike[]): number[] {
  return nodes
    .filter((n) => (n.enabled ?? true) && !EXCLUDED_BY_DEFAULT.includes(n.op))
    .map((n) => n.id);
}

/** 結び付けの欄 1 つ（引数 1 つ。区間は全体か片方）。 */
export interface BindSlot {
  key: string;
  node: number;
  op: string;
  param: string;
  part: RangePart | null;
  /** 「格子 · 裏返す」の形の見出し。 */
  label: string;
  /** 結べる素材の設定。 */
  settings: SettingKey[];
  /** 秒に結べる。 */
  time: boolean;
  /** 区間の全体の欄（片方の欄は、全体を素材の設定に結んでいれば出さない）。 */
  whole: string | null;
}

/** 欄の選び方。 */
export type BindChoice =
  | { type: 'keep' }
  | { type: 'setting'; key: SettingKey }
  | { type: 'time'; anchor: TimeAnchor; seconds: string };

const SETTING_KINDS: readonly SettingKey[] = ['grid', 'pivot', 'loop', 'frameMs'];

function slotKey(node: number, param: string, part: RangePart | null): string {
  return `${node}:${param}:${part ?? ''}`;
}

/** 含める操作の、結び付けられる引数の欄（文書の並び。今の版でない操作には置けない）。 */
export function bindSlots(
  nodes: readonly RecipeNodeLike[],
  keep: readonly number[],
  catalog: readonly OpDescriptor[] = EDIT_CATALOG,
): BindSlot[] {
  const out: BindSlot[] = [];
  for (const node of nodes) {
    if (!keep.includes(node.id) || !(node.enabled ?? true)) continue;
    const desc = opSpec(node.op, node.v, catalog);
    if (!desc) continue;
    for (const spec of desc.schema.params) {
      if (spec.binds.length === 0) continue;
      const settings = SETTING_KINDS.filter((k) => spec.binds.includes(k));
      const time = spec.binds.includes('time');
      const base = `${paramLabel(node.op, spec.key)} · ${opName(node.op)}`;
      if (spec.kind.type === 'frameRange') {
        const whole = slotKey(node.id, spec.key, null);
        if (settings.length > 0) {
          out.push({
            key: whole,
            node: node.id,
            op: node.op,
            param: spec.key,
            part: null,
            label: base,
            settings,
            time: false,
            whole: null,
          });
        }
        if (time) {
          for (const part of ['start', 'end'] as const) {
            out.push({
              key: slotKey(node.id, spec.key, part),
              node: node.id,
              op: node.op,
              param: spec.key,
              part,
              label: `${base}${partLabel(part)}`,
              settings: [],
              time: true,
              whole: settings.length > 0 ? whole : null,
            });
          }
        }
      } else {
        out.push({
          key: slotKey(node.id, spec.key, null),
          node: node.id,
          op: node.op,
          param: spec.key,
          part: null,
          label: base,
          settings,
          time,
          whole: null,
        });
      }
    }
  }
  return out;
}

/** 今の結び付けから欄の選び方を作る（結び付けのない欄は見本の値のまま）。 */
export function choicesOf(
  slots: readonly BindSlot[],
  bindings: readonly RecipeBinding[],
): Record<string, BindChoice> {
  const out: Record<string, BindChoice> = {};
  for (const slot of slots) {
    const b = bindings.find(
      (x) => x.node === slot.node && x.param === slot.param && (x.part ?? null) === slot.part,
    );
    if (!b) {
      out[slot.key] = { type: 'keep' };
    } else if (b.from.type === 'setting') {
      out[slot.key] = { type: 'setting', key: b.from.key };
    } else {
      out[slot.key] = { type: 'time', anchor: b.from.anchor, seconds: secondsText(b.from.ms) };
    }
  }
  return out;
}

/** 欄の選び方を今使えるか（区間の片方は、全体を素材の設定に結んでいれば使わない）。 */
export function slotActive(slot: BindSlot, choices: Record<string, BindChoice>): boolean {
  return !(slot.whole && choices[slot.whole]?.type === 'setting');
}

/** 秒の欄を読む（0 以上の数。ミリ秒に丸める）。読めなければ null。 */
export function parseSeconds(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === '') return null;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || n < 0) return null;
  return Math.round(n * 1000);
}

/** 欄の選び方から結び付けを作る。秒が読めない欄があれば、その欄の見出しを `invalid` に返す。 */
export function bindingsFrom(
  slots: readonly BindSlot[],
  choices: Record<string, BindChoice>,
): { bindings: RecipeBinding[]; invalid: string | null } {
  const bindings: RecipeBinding[] = [];
  for (const slot of slots) {
    if (!slotActive(slot, choices)) continue;
    const c = choices[slot.key] ?? { type: 'keep' };
    if (c.type === 'keep') continue;
    if (c.type === 'setting') {
      if (!slot.settings.includes(c.key)) continue;
      bindings.push({
        node: slot.node,
        param: slot.param,
        part: slot.part,
        from: { type: 'setting', key: c.key },
      });
      continue;
    }
    if (!slot.time) continue;
    const ms = parseSeconds(c.seconds);
    if (ms === null) {
      return { bindings: [], invalid: slot.label };
    }
    bindings.push({
      node: slot.node,
      param: slot.param,
      part: slot.part,
      from: { type: 'time', ms, anchor: c.anchor },
    });
  }
  return { bindings, invalid: null };
}

/**
 * 秒の欄の既定（見本の値をフレームから秒に直す。`rate` が分からなければ 0）。区間の片方は `start`・`end` の値。
 */
export function defaultSeconds(
  node: RecipeNodeLike | undefined,
  slot: BindSlot,
  rate: number | null,
): string {
  if (!node || !rate) return '0';
  const params = (node.params ?? {}) as Record<string, JsonValue>;
  const value = params[slot.param];
  let frames: number | null = null;
  if (typeof value === 'number') {
    frames = value;
  } else if (slot.part && value && typeof value === 'object' && !Array.isArray(value)) {
    const v = (value as Record<string, JsonValue>)[slot.part];
    frames = typeof v === 'number' ? v : null;
  }
  return frames === null ? '0' : secondsText((frames / rate) * 1000);
}
