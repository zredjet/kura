/**
 * エディタのキー（design.md §6 の「画像エディタ」の列。段 5a の道具と、段 5b の切り抜き・右パネル・保存。M13 の段 6a で
 * 種類によらないキー（ここ）と種類ごとのキー（`EditorKindKeys`。画像は `image/imageKeys.ts`）に分けた）。
 *
 * シアターと同じく capture で受け、処理したら伝えない（ライブラリ・シアターのキーより先に）。文字を打つ入力欄・日本語入力の
 * 変換中・シート・メニューの上では受けない。選択・スライダー・スイッチ（文字は打たないが矢印・Space を自分で使う部品）に
 * フォーカスがあるときは ⌘ の組み合わせ（⌘Z・⌘S・⌘0 など）だけを受ける。
 *
 * 種類によらないキー：
 *
 * | キー | 動き |
 * |---|---|
 * | ↑ ↓ | 右パネルの操作を選ぶ |
 * | ⌫ | 選んだ操作を消す |
 * | ⌥⌘↑ ⌥⌘↓ | 選んだ操作を前・後ろへ |
 * | ⌘S・⌘⇧S | 保存（上書き）・別名で保存…（ファイルメニューの項目もこのキー） |
 * | ⌘Z・⇧⌘Z | 取り消す・やり直す（編集メニューの項目もこのキーを押したことにする） |
 * | ⌘I | 右パネルの「情報」 |
 * | esc | 種類のキーが使わなければ閉じる（未保存ならシート） |
 *
 * 種類のキーを見る順：⌘ の組み合わせは ⌘S・⌥⌘↑↓・⌘Z のあと（⌥・⌃ を除く）、⌘ のないキーは ↑↓・⌫・esc より先。
 */

import { useEffect } from 'react';

import { isImeKey } from '../../lib/ime';
import { isKeyControl, isTextEntry } from '../../lib/keys';
import { editorKindKeys, everyEditorKindKeys } from './editorKinds';
import { openSaveAs, requestCloseEditor, requestSave } from './editorSave';
import {
  editorRedo,
  editorUndo,
  moveSelected,
  removeSelected,
  selectAdjacent,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';

/** 種類ごとのキー（核のキーの間に差し込む）。`key` は 1 文字なら小文字にしたもの。 */
export interface EditorKindKeys {
  /** ⌘ の組み合わせ（⌥・⌃ を含むものは来ない）。処理したら true。 */
  meta?(e: KeyboardEvent, key: string): boolean;
  /** ⌘・⌃ のないキー。処理したら true、受けない（核も見ない）なら false、核へ回すなら null。 */
  down?(e: KeyboardEvent, key: string): boolean | null;
  /** esc で閉じる前（描いている途中の筆を取り消したら true）。 */
  escape?(): boolean;
  /** キーを離したとき（処理したら true）。 */
  up?(e: KeyboardEvent): boolean;
  /** 押している間だけの道具を戻す（窓を離れた・キーを受けるのをやめた）。 */
  reset?(): void;
}

/** エディタの上にシート・メニュー・色を選ぶ部品が開いているか。 */
function isBlocked(): boolean {
  return document.querySelector('[data-sheet], [role="menu"]') !== null;
}

/** キーを押したとき（処理したら true）。 */
export function handleEditorKey(e: KeyboardEvent): boolean {
  const s = useEditorStore.getState();
  if (e.defaultPrevented || s.phase === 'closed' || isBlocked() || isImeKey(e)) return false;
  if (isTextEntry(e.target)) return false;
  // 選択・スライダー・スイッチの上では、矢印・Space などは部品に任せ、⌘ の組み合わせだけを受ける
  if (isKeyControl(e.target) && !e.metaKey) return false;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const kind = editorKindKeys(s.kind);

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
    if (kind?.meta?.(e, key)) return true;
    if (key === 'i' && !e.shiftKey) {
      toggleEditorInfo();
      return true;
    }
    return false;
  }
  if (e.ctrlKey) return false;
  const handled = kind?.down?.(e, key) ?? null;
  if (handled !== null) return handled;
  if (e.altKey) return false;
  switch (key) {
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
      if (!kind?.escape?.()) void requestCloseEditor();
      return true;
    default:
      return false;
  }
}

/** キーを離したとき（押している間だけの道具を戻す）。 */
export function handleEditorKeyUp(e: KeyboardEvent): boolean {
  return editorKindKeys(useEditorStore.getState().kind)?.up?.(e) ?? false;
}

/** 押している間だけの道具をすべての種類で戻す。 */
function resetKindKeys(): void {
  for (const keys of everyEditorKindKeys()) keys.reset?.();
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
    const onBlur = () => resetKindKeys();
    window.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('keyup', onKeyUp, true);
      window.removeEventListener('blur', onBlur);
      resetKindKeys();
    };
  }, [enabled]);
}
