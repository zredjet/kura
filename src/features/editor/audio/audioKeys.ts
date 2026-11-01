/**
 * 音声エディタのキー（design.md §6 の「音声エディタ」の列。M13 の段 6。種類によらないキーは `../editorKeys.ts`）。
 *
 * | キー | 動き |
 * |---|---|
 * | Space | 再生・止める（選択があれば選択だけ） |
 * | L | ループを試聴（もう一度で止める） |
 * | B | 聞き比べの元・編集後 |
 * | I・O | 今の道具の始め・終わりを再生位置（鳴っていなければカーソル）に |
 * | ← → | カーソルを 1 秒（⇧で 10 秒） |
 * | Home・End | カーソルを先頭・末尾へ |
 * | S・T・F・R | 選択・切り詰め・フェード・ループ |
 * | ↵ | 切り詰めを確定 |
 * | esc | フェード・ループのドラッグ中ならやめる → 切り詰めをやめる → 選択を外す →（核が）閉じる |
 * | ⌘A | すべてを選択（編集メニューの項目も `selectAll` を通る） |
 * | ⌘0・⌘+・⌘− | 全体を表示・拡大・縮小（表示メニューの項目もこのキー） |
 *
 * ↑↓・⌫・⌘Z・⌘S・⌘I は核のまま。メニューが押したことにした文字のキー・Space（ライブラリ向けの T・F・L・Space の
 * 項目）は受けない。
 */

import { isReplayed } from '../../../lib/keys';
import type { EditorKindKeys } from '../editorKeys';
import {
  type AudioTool,
  audioFit,
  audioZoom,
  cancelTrim,
  clearSelection,
  cursorToEdge,
  nudgeCursor,
  selectAllAudio,
  toggleAB,
  toggleLoopAudition,
  togglePlay,
  useAudioStore,
} from './audioStore';
import { cancelAudioDrag, confirmTrim, selectAudioTool, setInOut } from './audioTools';

const TOOL_KEYS: Record<string, AudioTool> = { s: 'select', t: 'trim', f: 'fade', r: 'loop' };

/** ⌘ の組み合わせ（表示・すべてを選択）。 */
function meta(e: KeyboardEvent, key: string): boolean {
  if (key === '0') {
    audioFit();
    return true;
  }
  if (key === '=' || key === '+' || key === ';') {
    audioZoom(1);
    return true;
  }
  if (key === '-') {
    audioZoom(-1);
    return true;
  }
  if ((key === 'a' || e.code === 'KeyA') && !e.shiftKey) {
    return selectAllAudio();
  }
  return false;
}

/** ⌘・⌃ のないキー。 */
function down(e: KeyboardEvent, key: string): boolean | null {
  // メニューが押したことにした文字のキー・Space（「タグを追加（T）」「クイックルック（Space）」など、ライブラリ向けの
  // 項目）は道具・再生に回さない（画像のエディタと同じく何も起きない。ライブラリのキーは [data-editor] で止まる）
  if (key.length === 1 && isReplayed(e)) return false;
  const s = useAudioStore.getState();
  if (key === 'Enter' && s.trim) {
    confirmTrim();
    return true;
  }
  if (e.altKey) return null;
  if (key === ' ') {
    if (!e.repeat) togglePlay();
    return true;
  }
  const tool = TOOL_KEYS[key];
  if (tool && !e.shiftKey) {
    selectAudioTool(tool);
    return true;
  }
  switch (key) {
    case 'l':
      toggleLoopAudition();
      return true;
    case 'b':
      toggleAB();
      return true;
    case 'i':
      setInOut('in');
      return true;
    case 'o':
      setInOut('out');
      return true;
    case 'ArrowLeft':
      nudgeCursor(e.shiftKey ? -10 : -1);
      return true;
    case 'ArrowRight':
      nudgeCursor(e.shiftKey ? 10 : 1);
      return true;
    case 'Home':
      cursorToEdge(false);
      return true;
    case 'End':
      cursorToEdge(true);
      return true;
    default:
      return null;
  }
}

/**
 * esc：フェード・ループのドラッグ中ならやめる（仮の値を捨てる）→ 切り詰めをやめる → 選択を外す（どれもなければ核が
 * 閉じる）。
 */
function escape(): boolean {
  if (cancelAudioDrag()) return true;
  if (useAudioStore.getState().trim) {
    cancelTrim();
    return true;
  }
  return clearSelection();
}

/** 音声エディタのキー。 */
export const AUDIO_KEYS: EditorKindKeys = {
  meta,
  down,
  escape,
  selectAll: selectAllAudio,
};
