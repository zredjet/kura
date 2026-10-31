/**
 * 音声のハンドルの手（M13 の段 6。設計 §2.6、Rust は `crates/kura-core/src/edit/editor/audio/handles.rs`）。純粋な関数。
 *
 * - フロントは表示している出力のフレームだけを送る（`{type: 'handle', handle, node, mode}`）。Rust が対象のノードより
 *   下流の時間の写しを逆にたどってノードの座標にし、ノードがあれば引数にし（外していれば有効に戻す）、なければ末尾に足す。
 * - ドラッグの途中は `preview`、離したら `commitPreview`（`step`）、やめたら `cancelPreview`。数の欄は `merge` で送り、
 *   Enter・欄を離れたら `seal`。I／O・ボタンは `step`。
 * - 対象のノード：選んだ操作がその種類ならそれ、なければ最後のその種類のノード、なければ null（末尾に足す）。
 */

import type {
  EditAudioNodeDto,
  EditHandleDto,
  EditHandleModeDto,
  EditStepDto,
} from '../../../ipc/edit';
import type { EditorDoc } from '../editorStore';
import type { AudioTool } from './audioStore';
import type { FrameSpan } from './waveMath';

/** ハンドルの手。 */
export function handleStep(
  handle: EditHandleDto,
  node: number | null,
  mode: EditHandleModeDto,
): EditStepDto {
  return { type: 'handle', handle, node, mode };
}

/** ドラッグを離した（仮の値を 1 手にする）。 */
export const COMMIT_DRAG: EditStepDto = { type: 'commitPreview', mode: 'step' };
/** ドラッグをやめた（仮の値を捨てる）。 */
export const CANCEL_DRAG: EditStepDto = { type: 'cancelPreview' };
/** 数の欄のまとめを閉じる。 */
export const SEAL: EditStepDto = { type: 'seal' };

/** 切り詰めの枠。 */
export function trimHandle(span: FrameSpan): EditHandleDto {
  return { type: 'trim', start: Math.round(span.start), end: Math.round(span.end) };
}

/** フェードインの終わり。 */
export function fadeInHandle(end: number): EditHandleDto {
  return { type: 'fadeIn', end: Math.max(0, Math.round(end)) };
}

/** フェードアウトの始め。 */
export function fadeOutHandle(start: number): EditHandleDto {
  return { type: 'fadeOut', start: Math.max(0, Math.round(start)) };
}

/** ループの区間（null は消す）。 */
export function loopHandle(range: FrameSpan | null): EditHandleDto {
  return {
    type: 'loop',
    range: range ? { start: Math.round(range.start), end: Math.round(range.end) } : null,
  };
}

/** 対象のノード：選んだ操作がその種類ならそれ、なければ文書の最後のその種類のノード（なければ null）。 */
export function targetNode(doc: EditorDoc, selected: number | null, op: string): number | null {
  if (selected !== null && doc.nodes[selected]?.op === op) return selected;
  for (let i = doc.order.length - 1; i >= 0; i--) {
    const id = doc.order[i];
    if (id !== undefined && doc.nodes[id]?.op === op) return id;
  }
  return null;
}

/** 表示の結果のノード（出力につながるもの）。 */
export function viewNode(
  nodes: readonly EditAudioNodeDto[],
  id: number | null,
): EditAudioNodeDto | null {
  return id === null ? null : (nodes.find((n) => n.id === id) ?? null);
}

/** ドラッグの手（途中は preview、離したら commitPreview）。 */
export function dragSteps(
  handle: EditHandleDto,
  node: number | null,
  phase: 'move' | 'end' | 'cancel',
): EditStepDto[] {
  if (phase === 'cancel') return [CANCEL_DRAG];
  if (phase === 'end') return [handleStep(handle, node, 'preview'), COMMIT_DRAG];
  return [handleStep(handle, node, 'preview')];
}

/** I／O で替えるもの（道具ごとの始め・終わり）。 */
export type InOutTarget =
  | { kind: 'selection'; span: FrameSpan }
  | { kind: 'trim'; span: FrameSpan }
  | { kind: 'fadeIn'; end: number }
  | { kind: 'fadeOut'; start: number }
  | { kind: 'loop'; span: FrameSpan };

/**
 * I（始め）・O（終わり）：今の道具の始め・終わりを `pos` にする（純粋な関数）。始めが終わりを越えたら、もう一方を全体の
 * 端に寄せる。替えられなければ null（切り詰めの枠がない）。
 */
export function inOut(
  tool: AudioTool,
  which: 'in' | 'out',
  pos: number,
  now: {
    total: number;
    selection: FrameSpan | null;
    trim: FrameSpan | null;
    loop: FrameSpan | null;
  },
): InOutTarget | null {
  const total = now.total;
  const p = Math.min(total, Math.max(0, Math.round(pos)));
  const span = (base: FrameSpan | null): FrameSpan | null => {
    const cur = base ?? { start: 0, end: total };
    if (which === 'in') {
      const end = cur.end > p ? cur.end : total;
      return end > p ? { start: p, end } : null;
    }
    const start = cur.start < p ? cur.start : 0;
    return p > start ? { start, end: p } : null;
  };
  switch (tool) {
    case 'select': {
      const s = span(now.selection);
      return s ? { kind: 'selection', span: s } : null;
    }
    case 'trim': {
      if (!now.trim) return null;
      const s = span(now.trim);
      return s ? { kind: 'trim', span: s } : null;
    }
    case 'fade':
      return which === 'in' ? { kind: 'fadeIn', end: p } : { kind: 'fadeOut', start: p };
    case 'loop': {
      const s = span(now.loop);
      return s ? { kind: 'loop', span: s } : null;
    }
  }
}
