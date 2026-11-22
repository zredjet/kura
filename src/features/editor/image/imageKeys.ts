/**
 * 画像エディタのキー（design.md §6 の「画像エディタ」の列。種類によらないキーは `../editorKeys.ts`）。
 *
 * メニューが送ったキー（離したキーが来ない）では押している間だけの道具にしない。
 *
 * | キー | 動き |
 * |---|---|
 * | B・E・G・I・H・C | ペン・消しゴム・塗りつぶし・スポイト・手のひら・切り抜き |
 * | 切り抜きの間：矢印（⇧ で 10px）・↵・esc | 枠を動かす・切り抜く・やめる |
 * | Space（押している間） | 手のひら |
 * | ⌥（押している間） | スポイト（ペン・消しゴム・塗りつぶしのとき） |
 * | [ ]（⇧ で 10 ずつ。⇧ を押すと `key` は { } になる） | 筆の大きさ |
 * | ← → | 前・次のコマ（アニメの画像） |
 * | ⌘0・⌘1・⌘+・⌘− | 全体を表示・100%・拡大・縮小（表示メニューの項目もこのキー） |
 * | esc | 切り抜きの途中ならやめる、描いている途中なら取り消し（そうでなければ核が閉じる） |
 */

import { isReplayed } from '../../../lib/keys';
import type { EditorKindKeys } from '../editorKeys';
import { useEditorStore } from '../editorStore';
import {
  cancelCrop,
  confirmCrop,
  editorActualSize,
  editorFit,
  editorZoom,
  nudgeCropRect,
  selectTool,
  setTemporaryTool,
  stepEditorFrame,
  strokeCancel,
  useImageStore,
} from './imageStore';
import { type EditorTool, useToolStore } from './toolStore';
import { formatCanBeTransparent } from './viewMath';

const TOOL_KEYS: Record<string, EditorTool> = {
  b: 'pen',
  e: 'eraser',
  g: 'fill',
  i: 'eyedropper',
  h: 'hand',
  c: 'crop',
};

/** ⌥ で一時的にスポイトにする道具。 */
const PAINTING: readonly EditorTool[] = ['pen', 'eraser', 'fill'];

/**
 * 筆の大きさのキー（[ は −1、] は +1、ほかは 0）。⇧ を押すと `key` は { } になる（US・JIS とも）。`key` が括弧でない配列
 * では、キーの位置（`code`）で見る。
 */
function bracketOf(e: KeyboardEvent, key: string): number {
  if (key === '[' || key === '{') return -1;
  if (key === ']' || key === '}') return 1;
  if (e.code === 'BracketLeft') return -1;
  if (e.code === 'BracketRight') return 1;
  return 0;
}

/** ⌘ の組み合わせ（表示）。 */
function meta(e: KeyboardEvent, key: string): boolean {
  if (key === '0') {
    editorFit();
    return true;
  }
  if (key === '1' && !e.shiftKey) {
    editorActualSize();
    return true;
  }
  if (key === '=' || key === '+' || key === ';') {
    editorZoom(1);
    return true;
  }
  if (key === '-') {
    editorZoom(-1);
    return true;
  }
  return false;
}

/** ⌘・⌃ のないキー。 */
function down(e: KeyboardEvent, key: string): boolean | null {
  const s = useImageStore.getState();
  // 切り抜きの枠：矢印で動かす・↵ で切り抜く・esc でやめる
  if (s.crop) {
    if (key === 'Enter') {
      confirmCrop();
      return true;
    }
    if (key === 'Escape') {
      cancelCrop();
      return true;
    }
    if (!e.altKey && nudgeCropRect(key, e.shiftKey)) return true;
  }
  // 押している間だけの道具（メニューが送ったキーでは替えない：離したキーが来ず、戻らなくなる）
  if (key === 'Alt') {
    if (isReplayed(e)) return false;
    if (PAINTING.includes(useToolStore.getState().tool)) setTemporaryTool('eyedropper');
    return true;
  }
  if (e.altKey) return false;

  if (key === ' ') {
    if (isReplayed(e)) return false;
    if (!e.repeat) setTemporaryTool('hand');
    return true;
  }
  const tool = TOOL_KEYS[key];
  if (tool && !e.shiftKey) {
    if (tool === 'eraser' && s.view && !formatCanBeTransparent(s.view.format)) return true;
    if (useEditorStore.getState().phase === 'open') selectTool(tool);
    else useToolStore.getState().setTool(tool);
    return true;
  }
  const bracket = bracketOf(e, key);
  if (bracket !== 0) {
    useToolStore.getState().stepSize(bracket * (e.shiftKey ? 10 : 1));
    return true;
  }
  switch (key) {
    case 'ArrowLeft':
      stepEditorFrame(-1);
      return true;
    case 'ArrowRight':
      stepEditorFrame(1);
      return true;
    default:
      return null;
  }
}

/** キーを離したとき（押している間だけの道具を戻す）。 */
function up(e: KeyboardEvent): boolean {
  const temporary = useImageStore.getState().temporary;
  if (e.key === ' ' && temporary === 'hand') {
    setTemporaryTool(null);
    return true;
  }
  if (e.key === 'Alt' && temporary === 'eyedropper') {
    setTemporaryTool(null);
    return true;
  }
  return false;
}

/** 画像エディタのキー。 */
export const IMAGE_KEYS: EditorKindKeys = {
  meta,
  down,
  escape: () => strokeCancel(),
  up,
  reset: () => setTemporaryTool(null),
};
