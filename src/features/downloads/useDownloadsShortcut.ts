import { useEffect } from 'react';

import { useViewStore } from '../../stores/viewStore';

/** シート・メニュー・シアターが開いているか（そのあいだは画面を替えない）。 */
function isBlocked(): boolean {
  return document.querySelector('[data-sheet], [role="menu"], [data-theater]') !== null;
}

/** ⌘⇧D でダウンロードの画面を開く（design.md §6）。 */
export function useDownloadsShortcut(): void {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (
        e.metaKey &&
        e.shiftKey &&
        !e.altKey &&
        !e.ctrlKey &&
        e.key.toLowerCase() === 'd' &&
        !isBlocked()
      ) {
        e.preventDefault();
        useViewStore.getState().openDownloads();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);
}
