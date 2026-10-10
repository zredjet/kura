import { useEffect } from 'react';

import { useImportStore } from '../../stores/importStore';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';

/** 文字を入力している場所ではキー操作を奪わない。 */
function isTyping(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))
  );
}

/** ライブラリ画面のキー操作（design.md §6 のうち M2 の分）。 */
export function handleLibraryKey(e: KeyboardEvent): boolean {
  if (useImportStore.getState().phase !== 'idle' || isTyping(e.target)) {
    return false;
  }
  const view = useViewStore.getState();
  const ids = useLibraryStore.getState().list?.ids;
  if (e.metaKey) {
    if (e.key === 'i' || e.key === 'I') {
      view.toggleInspector();
      return true;
    }
    if (e.key === '+' || e.key === '=' || e.key === ';') {
      view.setThumbSize(view.thumbSize + 1);
      return true;
    }
    if (e.key === '-') {
      view.setThumbSize(view.thumbSize - 1);
      return true;
    }
    return false;
  }
  if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
    if (!ids || ids.length === 0) {
      return false;
    }
    const current = view.selectedId === null ? -1 : ids.indexOf(view.selectedId);
    const delta = e.key === 'ArrowRight' ? 1 : -1;
    const next = current < 0 ? 0 : Math.min(ids.length - 1, Math.max(0, current + delta));
    view.select(ids[next] ?? null);
    return true;
  }
  if (e.key === ' ') {
    view.setQuickLook(!view.quickLook);
    return view.selectedId !== null;
  }
  if (e.key === 'Escape') {
    if (view.quickLook) {
      view.setQuickLook(false);
    } else {
      view.select(null);
    }
    return true;
  }
  return false;
}

/** キー操作を受け付ける。 */
export function useLibraryKeys(): void {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (handleLibraryKey(e)) {
        e.preventDefault();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);
}
