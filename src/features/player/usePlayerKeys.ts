import { useEffect } from 'react';

import { setRating } from '../library/actions';
import { IMAGE_STEP_MS } from './PlayBar';
import { playerController } from './playerActions';
import { usePlayerStore } from './playerStore';

/** 文字を入力している場所ではキー操作を奪わない。 */
function isTyping(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))
  );
}

/** シアターの上にダイアログやメニューが開いているか。 */
function isBlocked(): boolean {
  return document.querySelector('[data-sheet], [role="menu"]') !== null;
}

/** シアターのキー操作（docs/playlists.md §6.3、design.md §6）。処理したら true。 */
export function handlePlayerKey(e: KeyboardEvent): boolean {
  if (e.defaultPrevented || isBlocked()) {
    return false;
  }
  const typing = isTyping(e.target);
  // 入力欄の中でも esc はシアターを閉じず、入力欄から抜けるだけにする
  if (typing) {
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
      // ファイル単位のキューでは ⇧→ も次のスロット（ユニット単位は M6）
      controller.next();
      return true;
    case 'ArrowLeft':
      controller.prev();
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
      controller.setRepeat(!store.snapshot.repeat);
      return true;
    case 'Escape':
      store.setMode('mini');
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
