/**
 * 右パネルの操作を Rust へ送る手に振り分ける（純粋な関数。docs/editing.md §12.12「まとめ方に時刻を使わない」）。
 *
 * | 操作 | 手 |
 * |---|---|
 * | スライダー・ハンドルのドラッグの途中 | `preview`（履歴に入らない。前の仮の値は捨てる） |
 * | ドラッグを離した | `commitPreview{step}` |
 * | ドラッグを esc でやめた | `cancelPreview` |
 * | キーボードの矢印・数の入力・色を選ぶ部品のつまみと 16 進 | `commit{merge}`（同じ引数を続けて変える手を 1 手にまとめる） |
 * | Enter・欄を離れた・色を選ぶ部品を閉じた | `seal`（まとめを閉じる） |
 * | スイッチ・セグメント・選択・「指定する」 | `commit{step}` |
 */

import type { EditDto, EditStepDto, FormatChoice, JsonValue, OutputSpec } from '../../ipc/edit';

/** 引数の欄での操作。 */
export type ParamGesture =
  | { kind: 'drag'; value: JsonValue }
  | { kind: 'release' }
  | { kind: 'cancel' }
  | { kind: 'nudge'; value: JsonValue }
  | { kind: 'set'; value: JsonValue }
  | { kind: 'seal' };

/** 引数 1 つを替える手。 */
export function setParamEdit(node: number, key: string, value: JsonValue): EditDto {
  return { type: 'setParam', node, key, value };
}

/** 引数の欄の操作を手にする。 */
export function paramSteps(node: number, key: string, g: ParamGesture): EditStepDto[] {
  switch (g.kind) {
    case 'drag':
      return [{ type: 'preview', edit: setParamEdit(node, key, g.value) }];
    case 'release':
      return [{ type: 'commitPreview', mode: 'step' }];
    case 'cancel':
      return [{ type: 'cancelPreview' }];
    case 'nudge':
      return [{ type: 'commit', edit: setParamEdit(node, key, g.value), mode: 'merge' }];
    case 'set':
      return [{ type: 'commit', edit: setParamEdit(node, key, g.value), mode: 'step' }];
    case 'seal':
      return [{ type: 'seal' }];
  }
}

/** 1 手で確定する。 */
export function commitStep(edit: EditDto): EditStepDto {
  return { type: 'commit', edit, mode: 'step' };
}

/** 有効・無効を替える。 */
export function enableSteps(node: number, enabled: boolean): EditStepDto[] {
  return [commitStep({ type: 'setEnabled', node, enabled })];
}

/** ノードを消す。 */
export function removeSteps(node: number): EditStepDto[] {
  return [commitStep({ type: 'removeNode', node })];
}

/** ノードを `to` 番目へ動かす（範囲の外なら手はない）。 */
export function moveSteps(order: readonly number[], node: number, delta: number): EditStepDto[] {
  const from = order.indexOf(node);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= order.length || to === from) return [];
  return [commitStep({ type: 'moveNode', node, to })];
}

/** 操作を `at` 番目に足す。 */
export function addSteps(
  at: number,
  op: string,
  v: number,
  params: Record<string, JsonValue>,
): EditStepDto[] {
  return [commitStep({ type: 'addNode', at, op, v, params })];
}

/** 出力の形式を替える（つなぐ先・量子化・展開は今のまま。出力がなければ手はない）。 */
export function outputSteps(outputs: readonly OutputSpec[], format: FormatChoice): EditStepDto[] {
  const out = outputs[0];
  if (!out) return [];
  return [
    commitStep({
      type: 'setOutput',
      output: out.id,
      format,
      quantize: out.quantize ?? null,
      expand: out.expand,
    }),
  ];
}

/** 同じ形式の指定か。 */
export function sameChoice(a: FormatChoice, b: FormatChoice): boolean {
  if (a.type !== b.type) return false;
  return a.type === 'keep' || (b.type === 'to' && a.format === b.format);
}
