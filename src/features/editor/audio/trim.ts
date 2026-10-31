/**
 * 切り詰めの道具の流れの純粋な部分（M13 の段 6。設計 §2.6。画像の切り抜き `image/imageStore.ts` と同じ形）。
 *
 * - 対象：選んだ切り詰めか、最後の切り詰めか、なければ新しく足す枠。
 * - 有効なノードなら、そのノードを仮に無効にして（`preview setEnabled false`）入力を表示し、`nodes[].marks.trim`
 *   （このノードを外して入力を出しているときの位置）に枠を出す。外は暗くする。印が null（後ろの操作で写せない）なら、
 *   枠は出さずに引数パネルで直すよう知らせる。
 * - ↵ で `[cancelPreview, handle{trim, node, step}]`（仮の値を捨ててから 1 手）、esc・ほかの道具・ほかの手でやめる。
 *   新しく足す枠が全体のままなら何も足さない。
 */

import type { EditAudioNodeDto, EditStepDto } from '../../../ipc/edit';
import type { EditorDoc } from '../editorStore';
import type { AudioTrim } from './audioStore';
import { handleStep, targetNode, trimHandle, viewNode } from './handles';

/** 切り詰めを始めるときの決め方。 */
export type TrimStart =
  /** 枠を出す（`preview` なら、そのノードを仮に無効にする手を先に送る）。 */
  | { kind: 'frame'; trim: AudioTrim; preview: EditStepDto | null }
  /** 後ろの操作で時間が写せない：このノードを選んで引数パネルで直す。 */
  | { kind: 'params'; node: number };

/** 切り詰めを始める（純粋な関数）。`frames` は今の出力の長さ。 */
export function beginTrim(
  doc: EditorDoc,
  selected: number | null,
  nodes: readonly EditAudioNodeDto[],
  frames: number,
): TrimStart {
  const node = targetNode(doc, selected, 'audio.trim');
  if (node === null) {
    return {
      kind: 'frame',
      trim: { node: null, start: 0, end: frames, bounds: frames, previewed: false },
      preview: null,
    };
  }
  const marks = viewNode(nodes, node)?.marks;
  if (!marks || marks.type !== 'trim') return { kind: 'params', node };
  const enabled = doc.nodes[node]?.enabled ?? false;
  return {
    kind: 'frame',
    trim: {
      node,
      start: marks.start,
      end: marks.end,
      // 有効なノードは仮に無効にした結果（入力）が届いてから長さが決まる
      bounds: enabled ? null : frames,
      previewed: enabled,
    },
    preview: enabled
      ? { type: 'preview', edit: { type: 'setEnabled', node, enabled: false } }
      : null,
  };
}

/** 枠を収める（`[0, bounds]` の中で、長さ 1 以上の整数）。 */
export function clampTrim(
  start: number,
  end: number,
  bounds: number,
): { start: number; end: number } {
  const b = Math.max(1, Math.round(bounds));
  const s = Math.min(b - 1, Math.max(0, Math.round(start)));
  const e = Math.min(b, Math.max(s + 1, Math.round(end)));
  return { start: s, end: e };
}

/**
 * ↵ で送る手（仮の値を捨てる `cancelPreview` は呼び手が先に付ける）。新しく足す枠が全体のままなら空。`create` は足すか。
 */
export function confirmTrimSteps(trim: AudioTrim): { steps: EditStepDto[]; create: boolean } {
  if (trim.bounds === null) return { steps: [], create: false };
  if (trim.node === null && trim.start === 0 && trim.end === trim.bounds) {
    return { steps: [], create: false };
  }
  return {
    steps: [handleStep(trimHandle({ start: trim.start, end: trim.end }), trim.node, 'step')],
    create: trim.node === null,
  };
}
