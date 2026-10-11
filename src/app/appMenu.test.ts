import type {
  MenuItemOptions,
  PredefinedMenuItemOptions,
  SubmenuOptions,
} from '@tauri-apps/api/menu';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { EMPTY_SELECTION } from '../features/library/selection';
import { ja } from '../i18n/ja';
import { useImportStore } from '../stores/importStore';
import { useLibraryStore } from '../stores/libraryStore';
import { useViewStore } from '../stores/viewStore';
import { HELP_MENU_ID, WINDOW_MENU_ID, appMenuItems, selectAllCommand } from './appMenu';

type Item = MenuItemOptions | PredefinedMenuItemOptions | SubmenuOptions;

function submenu(text: string): SubmenuOptions {
  const found = (appMenuItems(() => {}) as Item[]).find((m) => 'text' in m && m.text === text);
  if (!found || !('items' in found)) {
    throw new Error(`メニュー「${text}」がない`);
  }
  return found as SubmenuOptions;
}

describe('appMenuItems', () => {
  it('編集の「すべてを選択」は ⌘A の自前の項目で、押すと渡した処理を呼ぶ', () => {
    const onSelectAll = vi.fn();
    const edit = (appMenuItems(onSelectAll) as Item[]).find(
      (m) => 'text' in m && m.text === ja.appMenu.edit,
    ) as SubmenuOptions;
    const selectAll = (edit.items as Item[]).find(
      (i) => 'text' in i && i.text === ja.appMenu.selectAll,
    ) as MenuItemOptions;

    expect(selectAll.accelerator).toBe('CmdOrCtrl+A');
    expect('item' in selectAll).toBe(false);
    selectAll.action?.('');
    expect(onSelectAll).toHaveBeenCalledOnce();
  });

  it('既定の「すべてを選択」（selectAll:）はどこにも置かない', () => {
    const all = (appMenuItems(() => {}) as Item[]).flatMap((m) =>
      'items' in m ? ((m.items ?? []) as Item[]) : [m],
    );
    expect(all.some((i) => 'item' in i && i.item === 'SelectAll')).toBe(false);
    // 文字の編集に要る項目は既定のまま
    for (const kind of ['Undo', 'Redo', 'Cut', 'Copy', 'Paste'] as const) {
      expect(all.some((i) => 'item' in i && i.item === kind)).toBe(true);
    }
  });

  it('ウインドウ・ヘルプのメニューは Tauri がそれと分かる ID を持つ', () => {
    expect(submenu(ja.appMenu.window).id).toBe(WINDOW_MENU_ID);
    expect(submenu(ja.appMenu.help).id).toBe(HELP_MENU_ID);
  });
});

describe('selectAllCommand', () => {
  const ids = Uint32Array.from([10, 20, 30]);

  beforeEach(() => {
    useLibraryStore.setState({
      status: 'open',
      list: { ids, aspects: new Uint16Array(3), groups: [] },
    });
    useImportStore.setState({ phase: 'idle' });
    useViewStore.setState({ selection: EMPTY_SELECTION, playlist: null });
  });

  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('入力欄にフォーカスがあれば、その文字をすべて選び、一覧の選択は変えない', () => {
    const input = document.createElement('input');
    input.value = '旅行 鎌倉';
    document.body.append(input);
    input.focus();
    input.setSelectionRange(2, 2);

    selectAllCommand();

    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(input.value.length);
    expect(useViewStore.getState().selection).toEqual(EMPTY_SELECTION);
  });

  it('複数行の入力欄でも文字をすべて選ぶ', () => {
    const area = document.createElement('textarea');
    area.value = '一行目\n二行目';
    document.body.append(area);
    area.focus();

    selectAllCommand();

    expect(area.selectionStart).toBe(0);
    expect(area.selectionEnd).toBe(area.value.length);
  });

  it('入力欄の外なら一覧のすべてを選ぶ', () => {
    selectAllCommand();

    expect(useViewStore.getState().selection).toEqual({ ids: [10, 20, 30], anchor: 10, focus: 30 });
  });

  it('タイル（ボタン）にフォーカスがあっても一覧のすべてを選ぶ', () => {
    const tile = document.createElement('button');
    document.body.append(tile);
    tile.focus();

    selectAllCommand();

    expect(useViewStore.getState().selection.ids).toEqual([10, 20, 30]);
  });

  it('ダイアログ・メニュー・シアターが開いているあいだは何もしない', () => {
    for (const attr of ['data-sheet', 'data-theater']) {
      const el = document.createElement('div');
      el.setAttribute(attr, '');
      document.body.append(el);

      selectAllCommand();

      expect(useViewStore.getState().selection).toEqual(EMPTY_SELECTION);
      el.remove();
    }
    const menu = document.createElement('div');
    menu.setAttribute('role', 'menu');
    document.body.append(menu);
    selectAllCommand();
    expect(useViewStore.getState().selection).toEqual(EMPTY_SELECTION);
  });

  it('取り込みのシートを出しているあいだと、ライブラリを開いていないときは何もしない', () => {
    useImportStore.setState({ phase: 'sheet' });
    selectAllCommand();
    expect(useViewStore.getState().selection).toEqual(EMPTY_SELECTION);

    useImportStore.setState({ phase: 'idle' });
    useLibraryStore.setState({ status: 'none' });
    selectAllCommand();
    expect(useViewStore.getState().selection).toEqual(EMPTY_SELECTION);
  });

  it('プレイリストを開いているあいだは、見えていない一覧を選ばない', () => {
    useViewStore.setState({ playlist: 7 });

    selectAllCommand();

    expect(useViewStore.getState().selection).toEqual(EMPTY_SELECTION);
  });
});
