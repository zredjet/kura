/**
 * 画像エディタのキー（design.md §6 の「画像エディタ」の列。段 5a の道具と、段 5b の切り抜き・右パネル・保存）。
 *
 * シアターと同じく capture で受け、処理したら伝えない（ライブラリ・シアターのキーより先に）。文字を打つ入力欄・日本語入力の
 * 変換中・シート・メニューの上では受けない。選択・スライダー・スイッチ（文字は打たないが矢印・Space を自分で使う部品）に
 * フォーカスがあるときは ⌘ の組み合わせ（⌘Z・⌘S・⌘0 など）だけを受ける。メニューが送ったキー（離したキーが来ない）では
 * 押している間だけの道具にしない。
 *
 * | キー | 動き |
 * |---|---|
 * | B・E・G・I・H・C | ペン・消しゴム・塗りつぶし・スポイト・手のひら・切り抜き |
 * | 切り抜きの間：矢印（⇧ で 10px）・↵・esc | 枠を動かす・切り抜く・やめる |
 * | ↑ ↓ | 右パネルの操作を選ぶ |
 * | ⌫ | 選んだ操作を消す |
 * | ⌥⌘↑ ⌥⌘↓ | 選んだ操作を前・後ろへ |
 * | ⌘S・⌘⇧S | 保存（上書き）・別名で保存…（ファイルメニューの項目もこのキー） |
 * | Space（押している間） | 手のひら |
 * | ⌥（押している間） | スポイト（ペン・消しゴム・塗りつぶしのとき） |
 * | [ ]（⇧ で 10 ずつ。⇧ を押すと `key` は { } になる） | 筆の大きさ |
 * | ← → | 前・次のコマ（アニメの画像） |
 * | ⌘Z・⇧⌘Z | 取り消す・やり直す（編集メニューの項目もこのキーを押したことにする） |
 * | ⌘0・⌘1・⌘+・⌘− | 全体を表示・100%・拡大・縮小（表示メニューの項目もこのキー） |
 * | ⌘I | 右パネルの「情報」 |
 * | esc | 切り抜きの途中ならやめる、描いている途中なら取り消し、そうでなければ閉じる（未保存ならシート） |
 */

import { useEffect } from 'react';

import { isImeKey } from '../../lib/ime';
import { isKeyControl, isReplayed, isTextEntry } from '../../lib/keys';
import { openSaveAs, requestCloseEditor, requestSave } from './editorSave';
import {
  cancelCrop,
  confirmCrop,
  editorActualSize,
  editorFit,
  editorRedo,
  editorUndo,
  editorZoom,
  moveSelected,
  nudgeCropRect,
  removeSelected,
  selectAdjacent,
  selectTool,
  setTemporaryTool,
  stepEditorFrame,
  strokeCancel,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';
import { type EditorTool, useToolStore } from './toolStore';
import { formatCanBeTransparent } from './viewMath';

/** エディタの上にシート・メニュー・色を選ぶ部品が開いているか。 */
function isBlocked(): boolean {
  return document.querySelector('[data-sheet], [role="menu"]') !== null;
}

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

/** キーを押したとき（処理したら true）。 */
export function handleEditorKey(e: KeyboardEvent): boolean {
  const s = useEditorStore.getState();
  if (e.defaultPrevented || s.phase === 'closed' || isBlocked() || isImeKey(e)) return false;
  if (isTextEntry(e.target)) return false;
  // 選択・スライダー・スイッチの上では、矢印・Space などは部品に任せ、⌘ の組み合わせだけを受ける
  if (isKeyControl(e.target) && !e.metaKey) return false;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;

  if (e.metaKey) {
    if ((e.code === 'KeyS' || key === 's') && !e.altKey && !e.ctrlKey) {
      if (e.shiftKey) openSaveAs();
      else requestSave();
      return true;
    }
    if (e.altKey && !e.ctrlKey && !e.shiftKey && (key === 'ArrowUp' || key === 'ArrowDown')) {
      moveSelected(key === 'ArrowUp' ? -1 : 1);
      return true;
    }
    if (e.code === 'KeyZ' || key === 'z') {
      if (e.shiftKey) editorRedo();
      else editorUndo();
      return true;
    }
    if (e.altKey || e.ctrlKey) return false;
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
    if (key === 'i' && !e.shiftKey) {
      toggleEditorInfo();
      return true;
    }
    return false;
  }
  if (e.ctrlKey) return false;
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
    if (s.phase === 'open') selectTool(tool);
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
    case 'ArrowUp':
      selectAdjacent(-1);
      return true;
    case 'ArrowDown':
      selectAdjacent(1);
      return true;
    case 'Backspace':
    case 'Delete':
      removeSelected();
      return true;
    case 'Escape':
      if (!strokeCancel()) void requestCloseEditor();
      return true;
    default:
      return false;
  }
}

/** キーを離したとき（押している間だけの道具を戻す）。 */
export function handleEditorKeyUp(e: KeyboardEvent): boolean {
  const temporary = useEditorStore.getState().temporary;
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

/** エディタを開いている間、キーを受け付ける（シアター・ライブラリのキーより先に）。 */
export function useEditorKeys(enabled: boolean): void {
  useEffect(() => {
    if (!enabled) {
      return;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (handleEditorKey(e)) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (handleEditorKeyUp(e)) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    // 窓を離れたら押している間だけの道具を戻す（離したキーが届かない）
    const onBlur = () => setTemporaryTool(null);
    window.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('keyup', onKeyUp, true);
      window.removeEventListener('blur', onBlur);
      setTemporaryTool(null);
    };
  }, [enabled]);
}
