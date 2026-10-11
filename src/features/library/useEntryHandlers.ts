import { useMemo } from 'react';

import { openMenuAt } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { isUnitEntry, unitIdOf } from '../../ipc/items';
import { formatCount } from '../../lib/format';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { watchDrag } from './dragState';
import { itemMenu } from './itemMenu';
import { extendTo, selectOne, toggle } from './selection';
import type { TileHandlers } from './Tile';

/** タイル・リストの行のマウス・ドラッグの受け口（グリッドとリストで共通）。 */
export function useEntryHandlers(): TileHandlers {
  return useMemo<TileHandlers>(() => {
    const store = useViewStore.getState;
    const order = () => useLibraryStore.getState().list?.ids ?? new Uint32Array();
    return {
      onClick: (id, e) => {
        // WebKit ではボタンを押してもフォーカスが移らないので、キー操作がこの行に届くよう移す
        (e.currentTarget as HTMLElement).focus({ preventScroll: true });
        const { selection: current, setSelection } = store();
        if (e.metaKey) {
          setSelection(toggle(current, id));
        } else if (e.shiftKey) {
          setSelection(extendTo(current, id, order()));
        } else {
          setSelection(selectOne(id));
        }
      },
      onOpen: (id) => {
        const s = store();
        if (isUnitEntry(id)) {
          s.openUnit(unitIdOf(id));
        } else {
          s.setSelection(selectOne(id));
          s.setQuickLook(true);
        }
      },
      onContextMenu: (id, e) => {
        e.preventDefault();
        const s = store();
        const entries = s.selection.ids.includes(id) ? s.selection.ids : [id];
        if (!s.selection.ids.includes(id)) {
          s.setSelection(selectOne(id));
        }
        openMenuAt(e.clientX, e.clientY, ja.itemMenu.label, itemMenu(entries));
      },
      onPointerDown: (id, e) => {
        if (e.metaKey || e.shiftKey) {
          return;
        }
        watchDrag(e, () => {
          const s = store();
          const entries = s.selection.ids.includes(id) ? [...s.selection.ids] : [id];
          if (!s.selection.ids.includes(id)) {
            s.setSelection(selectOne(id));
          }
          return {
            payload: { kind: 'entries', ids: entries },
            label: ja.library.itemCount(formatCount(entries.length)),
          };
        });
      },
    };
  }, []);
}
