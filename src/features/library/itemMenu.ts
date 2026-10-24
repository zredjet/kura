import type { MenuEntry } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { isUnitEntry, unitIdOf } from '../../ipc/items';
import { getCached, getCachedUnit } from '../../stores/itemCache';
import { useViewStore } from '../../stores/viewStore';
import { canEdit, openEditor } from '../editor/editorStore';
import { playSelection } from '../player/playSelection';
import { pickPlaylistFor } from '../playlists/playlistActions';
import { exportEntries } from './exportActions';
import { pixelArtMenu } from './pixelArtMenu';
import {
  dissolveUnits,
  groupEntries,
  makeCover,
  moveWithinUnit,
  pickFolderFor,
  pickUnitFor,
  removeFromViewedFolder,
  removeFromViewedUnit,
  restoreEntries,
  trashEntries,
} from './actions';

/** 開いているユニットが set（並べ替え・メンバーの増減ができる）か。 */
export function viewedSetUnit(): number | null {
  const view = useViewStore.getState().view;
  if (view.kind !== 'unit') {
    return null;
  }
  return getCachedUnit(view.id)?.kind === 'folder' ? null : view.id;
}

/**
 * 項目のメニュー（右クリック・⌃Return・インスペクタの「その他の操作」で同じもの）。
 * 右に出すキーは、メニューを開かずに同じことをするキー。
 */
export function itemMenu(entries: readonly number[]): MenuEntry[] {
  const state = useViewStore.getState();
  const kind = state.view.kind;
  const items = entries.filter((id) => !isUnitEntry(id));
  const units = entries.filter(isUnitEntry);
  const only = entries.length === 1 ? entries[0] : undefined;

  if (kind === 'trash') {
    return [
      ...(only !== undefined && items.length === 1
        ? [
            {
              label: ja.itemMenu.quickLook,
              shortcut: 'Space',
              onSelect: () => state.setQuickLook(true),
            },
            'separator' as const,
          ]
        : []),
      { label: ja.itemMenu.restore, shortcut: '⌘⌫', onSelect: () => void restoreEntries(entries) },
    ];
  }

  const setUnit = viewedSetUnit();
  const out: MenuEntry[] = [
    { label: ja.itemMenu.play, shortcut: '⌘↩', onSelect: () => playSelection(entries) },
  ];
  if (only !== undefined && items.length === 1) {
    out.push({
      label: ja.itemMenu.quickLook,
      shortcut: 'Space',
      onSelect: () => state.setQuickLook(true),
    });
    // 画像エディタ（M12）：画像 1 件のときだけ
    const item = getCached(only);
    if (item && canEdit(item)) {
      out.push({ label: ja.itemMenu.edit, shortcut: '⌘E', onSelect: () => void openEditor(item) });
    }
  }
  if (only !== undefined && units.length === 1) {
    out.push({
      label: ja.itemMenu.openUnit,
      shortcut: '↩',
      onSelect: () => state.openUnit(unitIdOf(only)),
    });
  }
  out.push('separator');
  out.push({
    label: ja.itemMenu.addToFolder,
    shortcut: 'F',
    onSelect: () => void pickFolderFor(entries),
  });
  out.push({
    label: ja.itemMenu.addToPlaylist,
    shortcut: 'P',
    onSelect: () => void pickPlaylistFor(entries),
  });
  if (kind === 'folder') {
    out.push({
      label: ja.itemMenu.removeFromFolder,
      shortcut: '⌫',
      onSelect: () => void removeFromViewedFolder(entries),
    });
  }
  out.push('separator');
  out.push({
    label: ja.itemMenu.group,
    shortcut: '⌘G',
    onSelect: () => void groupEntries(entries),
  });
  out.push({ label: ja.itemMenu.addToUnit, onSelect: () => void pickUnitFor(entries) });
  if (setUnit !== null && items.length > 0) {
    out.push({
      label: ja.itemMenu.removeFromUnit,
      shortcut: '⌫',
      onSelect: () => void removeFromViewedUnit(entries),
    });
    if (only !== undefined && items.length === 1) {
      out.push({ label: ja.itemMenu.makeCover, onSelect: () => void makeCover(only) });
    }
    out.push({
      label: ja.itemMenu.moveEarlier,
      shortcut: '⌥⌘←',
      onSelect: () => void moveWithinUnit(entries, -1),
    });
    out.push({
      label: ja.itemMenu.moveLater,
      shortcut: '⌥⌘→',
      onSelect: () => void moveWithinUnit(entries, 1),
    });
  }
  if (units.length > 0) {
    out.push({
      label: ja.itemMenu.dissolve,
      shortcut: '⌘⇧G',
      onSelect: () => void dissolveUnits(entries),
    });
  }
  // 素材の表示（M11）：画像を選んでいるときだけ
  const pixelArt = pixelArtMenu(entries);
  if (pixelArt) {
    out.push('separator');
    out.push(pixelArt);
  }
  out.push('separator');
  out.push({
    label: ja.itemMenu.export,
    shortcut: '⌘⇧E',
    onSelect: () => void exportEntries(entries),
  });
  out.push({
    label: ja.itemMenu.trash,
    shortcut: '⌘⌫',
    danger: true,
    onSelect: () => void trashEntries(entries),
  });
  return out;
}
