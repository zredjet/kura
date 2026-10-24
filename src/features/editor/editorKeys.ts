/**
 * 画像エディタのキー（design.md §6 の「画像エディタ」の列。段 5a の道具の分）。
 *
 * シアターと同じく capture で受け、処理したら伝えない（ライブラリ・シアターのキーより先に）。入力欄・日本語入力の変換中・
 * シート・メニューの上では受けない。
 *
 * | キー | 動き |
 * |---|---|
 * | B・E・G・I・H | ペン・消しゴム・塗りつぶし・スポイト・手のひら |
 * | Space（押している間） | 手のひら |
 * | ⌥（押している間） | スポイト（ペン・消しゴム・塗りつぶしのとき） |
 * | [ ]（⇧ で 10 ずつ） | 筆の大きさ |
 * | ← → | 前・次のコマ（アニメの画像） |
 * | ⌘Z・⇧⌘Z | 取り消す・やり直す（編集メニューの項目もこのキーを押したことにする） |
 * | ⌘0・⌘1・⌘+・⌘− | 全体を表示・100%・拡大・縮小（表示メニューの項目もこのキー） |
 * | ⌘I | 右パネルの「情報」 |
 * | esc | 描いている途中なら取り消し、そうでなければ閉じる |
 */

import { useEffect } from 'react';

import { isImeKey } from '../../lib/ime';
import {
  closeEditor,
  editorActualSize,
  editorFit,
  editorRedo,
  editorUndo,
  editorZoom,
  setTemporaryTool,
  stepEditorFrame,
  strokeCancel,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';
import { type EditorTool, useToolStore } from './toolStore';
import { formatCanBeTransparent } from './viewMath';

/** 文字を入力している場所か。 */
function isTyping(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))
  );
}

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
};

/** ⌥ で一時的にスポイトにする道具。 */
const PAINTING: readonly EditorTool[] = ['pen', 'eraser', 'fill'];

/** キーを押したとき（処理したら true）。 */
export function handleEditorKey(e: KeyboardEvent): boolean {
  const s = useEditorStore.getState();
  if (e.defaultPrevented || s.phase === 'closed' || isBlocked() || isImeKey(e)) return false;
  if (isTyping(e.target)) return false;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;

  if (e.metaKey) {
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
  if (key === 'Alt') {
    if (PAINTING.includes(useToolStore.getState().tool)) setTemporaryTool('eyedropper');
    return true;
  }
  if (e.altKey) return false;

  if (key === ' ') {
    if (!e.repeat) setTemporaryTool('hand');
    return true;
  }
  const tool = TOOL_KEYS[key];
  if (tool && !e.shiftKey) {
    if (tool === 'eraser' && s.view && !formatCanBeTransparent(s.view.format)) return true;
    useToolStore.getState().setTool(tool);
    return true;
  }
  switch (key) {
    case '[':
      useToolStore.getState().stepSize(e.shiftKey ? -10 : -1);
      return true;
    case ']':
      useToolStore.getState().stepSize(e.shiftKey ? 10 : 1);
      return true;
    case 'ArrowLeft':
      stepEditorFrame(-1);
      return true;
    case 'ArrowRight':
      stepEditorFrame(1);
      return true;
    case 'Escape':
      if (!strokeCancel()) closeEditor();
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
