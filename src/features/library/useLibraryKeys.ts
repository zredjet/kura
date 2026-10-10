import { useEffect } from 'react';

import { openMenuBelow } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { isUnitEntry, unitIdOf } from '../../ipc/items';
import { useImportStore } from '../../stores/importStore';
import { useLibraryStore } from '../../stores/libraryStore';
import { useSearchStore } from '../../stores/searchStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import {
  dissolveUnits,
  groupEntries,
  moveWithinUnit,
  pickFolderFor,
  removeFromViewedFolder,
  removeFromViewedUnit,
  restoreEntries,
  setRating,
  trashEntries,
} from './actions';
import { TAG_FOCUS_KEY } from './inspector/ItemInspector';
import { itemMenu, viewedSetUnit } from './itemMenu';
import { extendBy, moveFocus, selectAll } from './selection';

/** 文字を入力している場所ではキー操作を奪わない。 */
function isTyping(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))
  );
}

/** シート（ダイアログ）やメニューが開いているか。 */
function isBlocked(): boolean {
  return document.querySelector('[data-sheet], [role="menu"]') !== null;
}

/** インスペクタのタグの入力欄に移る（T）。 */
function focusTagInput(): void {
  useViewStore.getState().showInspector();
  requestAnimationFrame(() => {
    document.querySelector<HTMLInputElement>(`[data-focus-key="${TAG_FOCUS_KEY}"]`)?.focus();
  });
}

/** ライブラリ画面のキー操作（design.md §6）。 */
export function handleLibraryKey(e: KeyboardEvent): boolean {
  if (
    e.defaultPrevented ||
    useImportStore.getState().phase !== 'idle' ||
    isTyping(e.target) ||
    isBlocked()
  ) {
    return false;
  }
  const view = useViewStore.getState();
  const ids = useLibraryStore.getState().list?.ids ?? new Uint32Array();
  const entries = view.selection.ids;
  const focus = view.selection.focus;
  const kind = view.view.kind;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;

  if (e.metaKey) {
    if ((key === 'k' || key === 'f') && !e.shiftKey) {
      useSearchStore.getState().openPalette();
    } else if (key === 'i') {
      view.toggleInspector();
    } else if (key === '+' || key === '=' || key === ';') {
      view.setThumbSize(view.thumbSize + 1);
    } else if (key === '-') {
      view.setThumbSize(view.thumbSize - 1);
    } else if (key === 'a' && !e.shiftKey) {
      view.setSelection(selectAll(ids));
    } else if (key === 'g' && e.shiftKey) {
      void dissolveUnits(entries);
    } else if (key === 'g' && entries.length > 0 && kind !== 'trash') {
      void groupEntries(entries);
    } else if (key === 'Backspace' && entries.length > 0) {
      void (kind === 'trash' ? restoreEntries(entries) : trashEntries(entries));
    } else if (key === 'ArrowUp' && kind === 'unit') {
      view.back();
    } else if ((key === 'ArrowLeft' || key === 'ArrowRight') && e.altKey && kind === 'unit') {
      if (viewedSetUnit() !== null) {
        void moveWithinUnit(entries, key === 'ArrowLeft' ? -1 : 1);
      }
    } else if (key === 'n' && e.shiftKey) {
      // 新しいフォルダ：フォルダを開いていればその中、でなければいちばん上
      const parentId = view.view.kind === 'folder' ? view.view.id : null;
      if (parentId !== null) {
        useSidebarStore.getState().setExpanded(parentId, true);
      }
      useSidebarStore.getState().setEditing({ kind: 'create', parentId });
    } else {
      return false;
    }
    return true;
  }

  if (e.ctrlKey && key === 'Enter') {
    const target = document.querySelector<HTMLElement>(`[data-entry="${focus ?? -1}"]`);
    if (target && entries.length > 0) {
      openMenuBelow(target, ja.itemMenu.label, itemMenu(entries));
      return true;
    }
    return false;
  }
  if (e.ctrlKey || e.altKey) {
    return false;
  }

  if (key === 'ArrowRight' || key === 'ArrowLeft') {
    if (ids.length === 0) {
      return false;
    }
    const delta = key === 'ArrowRight' ? 1 : -1;
    view.setSelection(
      e.shiftKey ? extendBy(view.selection, delta, ids) : moveFocus(view.selection, delta, ids),
    );
    return true;
  }
  if (key === ' ') {
    if (focus === null || isUnitEntry(focus)) {
      return false;
    }
    view.setQuickLook(!view.quickLook);
    return true;
  }
  if (key === 'Enter' && focus !== null && isUnitEntry(focus)) {
    view.openUnit(unitIdOf(focus));
    return true;
  }
  if (key === 'Escape') {
    if (view.quickLook) {
      view.setQuickLook(false);
    } else {
      view.select(null);
    }
    return true;
  }
  if (entries.length === 0 || kind === 'trash') {
    return false;
  }
  if (/^[0-5]$/.test(key)) {
    void setRating(entries, Number(key));
    return true;
  }
  if (key === 't') {
    focusTagInput();
    return true;
  }
  if (key === 'f') {
    void pickFolderFor(entries);
    return true;
  }
  if (key === 'Backspace') {
    if (kind === 'folder') {
      void removeFromViewedFolder(entries);
      return true;
    }
    if (kind === 'unit' && viewedSetUnit() !== null) {
      void removeFromViewedUnit(entries);
      return true;
    }
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
