import { useEffect } from 'react';

import { controlOwnsKey, isTextEntry } from '../../lib/keys';
import { applySeekAction, seekKeyAction } from '../../lib/seekKeys';
import { setRating } from '../library/actions';
import { IMAGE_STEP_MS } from './PlayBar';
import { canEdit, openEditor } from '../editor/editorKinds';
import {
  cycleRepeat,
  playerController,
  theaterSeekTarget,
  toggleFullscreen,
  toggleShuffle,
} from './playerActions';
import { usePlayerStore } from './playerStore';

/** シアターの上にダイアログ・メニュー・画像エディタが開いているか。 */
function isBlocked(): boolean {
  return document.querySelector('[data-sheet], [role="menu"], [data-editor]') !== null;
}

/** シアターで今出している項目（画像・音声）をエディタで開く（再生は一時停止する）。開けない種類なら false。 */
export function editCurrentItem(): boolean {
  const controller = playerController();
  const itemId = usePlayerStore.getState().snapshot.itemId;
  const item = itemId !== null ? controller.item(itemId) : undefined;
  if (!item || !canEdit(item)) {
    return false;
  }
  controller.pause();
  void openEditor(item);
  return true;
}

/** シアターのキー操作（docs/playlists.md §6.3、design.md §6）。処理したら true。 */
export function handlePlayerKey(e: KeyboardEvent): boolean {
  if (e.defaultPrevented || isBlocked()) {
    return false;
  }
  // 文字を打つ欄の中ではキーを受けない（esc もシアターを閉じず、入力欄から抜けるだけ）。スライダー・選択などの上では、
  // その部品が使うキー（スライダーは矢印・Home・End）だけを任せ、Space・esc・F などは受ける（ADR-230）
  if (isTextEntry(e.target) || controlOwnsKey(e.target, e.key)) {
    return false;
  }
  const controller = playerController();
  const store = usePlayerStore.getState();
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;

  if (e.metaKey) {
    if (key === 'i' && !e.shiftKey) {
      store.togglePanel('info');
      return true;
    }
    if (key === 'e' && !e.shiftKey && !e.altKey) {
      // エディタ（M12・M13）：今の画像・音声を開く（スライドショー・再生は止める）
      return editCurrentItem();
    }
    return false;
  }
  if (e.ctrlKey || e.altKey) {
    return false;
  }
  switch (key) {
    case ' ':
      controller.togglePlay();
      return true;
    case 'ArrowRight':
      // ⇧→ はユニット単位なら次のユニットの先頭（ファイル単位では次のスロット）
      if (e.shiftKey) {
        controller.nextUnit();
      } else {
        controller.next();
      }
      return true;
    case 'ArrowLeft':
      if (e.shiftKey) {
        controller.prevUnit();
      } else {
        controller.prev();
      }
      return true;
    case 's':
      void toggleShuffle();
      return true;
    case '[':
      controller.stepImage(-IMAGE_STEP_MS);
      return true;
    case ']':
      controller.stepImage(IMAGE_STEP_MS);
      return true;
    case 't':
      store.requestTagFocus();
      return true;
    case 'r':
      cycleRepeat();
      return true;
    case 'f':
      toggleFullscreen();
      return true;
    case 'Escape':
      // フルスクリーンならまずフルスクリーンを抜け、もう一度でライブラリへ（ADR-128）
      if (store.fullscreen) {
        toggleFullscreen();
      } else {
        store.setMode('mini');
      }
      return true;
    default:
      break;
  }
  if (/^[0-5]$/.test(key)) {
    const itemId = store.snapshot.itemId;
    if (itemId !== null) {
      void setRating([itemId], Number(key));
      return true;
    }
  }
  // シーク（, . で 1 コマ、⇧, ⇧. で 1 秒、J・L で 5 秒、K で再生／一時停止。ADR-230）
  const action = seekKeyAction(e);
  if (action !== null && store.snapshot.itemId !== null) {
    applySeekAction(action, theaterSeekTarget());
    return true;
  }
  return false;
}

/** シアターを出しているあいだ、キー操作を受け付ける（ライブラリのキー操作より先に）。 */
export function usePlayerKeys(enabled: boolean): void {
  useEffect(() => {
    if (!enabled) {
      return;
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (handlePlayerKey(e)) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [enabled]);
}
