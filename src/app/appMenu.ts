import { isTauri } from '@tauri-apps/api/core';
import { Menu, type MenuOptions } from '@tauri-apps/api/menu';

import { selectAllEntries } from '../features/library/useLibraryKeys';
import { ja } from '../i18n/ja';

/** Tauri が macOS の「ウインドウ」「ヘルプ」メニューとして扱う ID（`WINDOW_SUBMENU_ID`・`HELP_SUBMENU_ID`）。 */
export const WINDOW_MENU_ID = '__tauri_window_menu__';
export const HELP_MENU_ID = '__tauri_help_menu__';

type MenuItems = NonNullable<MenuOptions['items']>;

/**
 * メニューバーの中身。Tauri の既定のメニュー（`Menu::default`）と同じ項目を日本語で並べ、
 * 「すべてを選択」だけを自前の項目にする。既定の項目（`selectAll:`）は ⌘A をページより先に取り、
 * ライブラリの「すべて選ぶ」が効かないため（ADR-76）。
 */
export function appMenuItems(onSelectAll: () => void): MenuItems {
  const t = ja.appMenu;
  const app = ja.app.name;
  return [
    {
      text: app,
      items: [
        { item: { About: null }, text: t.about(app) },
        { item: 'Separator' },
        { item: 'Services', text: t.services },
        { item: 'Separator' },
        { item: 'Hide', text: t.hide(app) },
        { item: 'HideOthers', text: t.hideOthers },
        { item: 'Separator' },
        { item: 'Quit', text: t.quit(app) },
      ],
    },
    { text: t.file, items: [{ item: 'CloseWindow', text: t.closeWindow }] },
    {
      text: t.edit,
      items: [
        { item: 'Undo', text: t.undo },
        { item: 'Redo', text: t.redo },
        { item: 'Separator' },
        { item: 'Cut', text: t.cut },
        { item: 'Copy', text: t.copy },
        { item: 'Paste', text: t.paste },
        // ID は付けない（読み込み直したときに前のメニューの片付けで受け口が消されないように）
        { text: t.selectAll, accelerator: 'CmdOrCtrl+A', action: onSelectAll },
      ],
    },
    { text: t.view, items: [{ item: 'Fullscreen', text: t.fullscreen }] },
    {
      id: WINDOW_MENU_ID,
      text: t.window,
      items: [
        { item: 'Minimize', text: t.minimize },
        { item: 'Maximize', text: t.zoom },
        { item: 'Separator' },
        { item: 'CloseWindow', text: t.closeWindow },
      ],
    },
    { id: HELP_MENU_ID, text: t.help, items: [] },
  ];
}

/** 入力欄の文字をすべて選ぶ。入力欄でなければ false。 */
export function selectAllText(target: Element | null): boolean {
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
    target.select();
    return true;
  }
  if (target instanceof HTMLElement && target.isContentEditable) {
    window.getSelection()?.selectAllChildren(target);
    return true;
  }
  return false;
}

/** 「すべてを選択」（⌘A）：入力欄にフォーカスがあればその文字を、なければ一覧のすべてを選ぶ（design.md §6）。 */
export function selectAllCommand(target: Element | null = document.activeElement): void {
  if (!selectAllText(target)) {
    selectAllEntries(target);
  }
}

/** メニューバーを差し替える。Tauri の外（ブラウザでの確認など）では何もしない。 */
export async function installAppMenu(): Promise<void> {
  if (!isTauri()) {
    return;
  }
  try {
    const menu = await Menu.new({ items: appMenuItems(() => selectAllCommand()) });
    await menu.setAsAppMenu();
  } catch (error) {
    console.warn('メニューを設定できませんでした', error);
  }
}
