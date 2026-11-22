import type { ReactNode } from 'react';
import { create } from 'zustand';

/** メニューの 1 項目。 */
export interface MenuAction {
  label: string;
  /** 右に出すキーの表記（「⌘G」など）。 */
  shortcut?: string;
  /** 取り消せない操作（文字を警告の色にする）。 */
  danger?: boolean;
  disabled?: boolean;
  /** 左に出す小さな印（フォルダの色など）。 */
  icon?: ReactNode;
  /** 選択肢のチェック（「自動・はい・いいえ」など）。真なら左にチェックの印。省けば普通の項目。 */
  checked?: boolean;
  onSelect: () => void;
}

/** 右に ▸ を出し、選ぶ（→・Return・クリック・ポインタを載せる）と横に開く項目（「ドット絵として表示 ▸」）。 */
export interface MenuSubmenu {
  label: string;
  icon?: ReactNode;
  disabled?: boolean;
  submenu: MenuAction[];
}

/** メニューの 1 行（区切り線は 'separator'）。 */
export type MenuEntry = MenuAction | MenuSubmenu | 'separator';

/** 横に開く項目か。 */
export function isSubmenu(entry: MenuEntry): entry is MenuSubmenu {
  return entry !== 'separator' && 'submenu' in entry;
}

interface OpenMenu {
  x: number;
  y: number;
  label: string;
  entries: MenuEntry[];
  /** 閉じたときにフォーカスを戻す要素。 */
  returnFocus: HTMLElement | null;
}

interface MenuState {
  menu: OpenMenu | null;
  close: () => void;
}

export const useMenuStore = create<MenuState>((set) => ({
  menu: null,
  close: () => set({ menu: null }),
}));

/** 画面上の位置にメニューを開く（右クリック）。 */
export function openMenuAt(x: number, y: number, label: string, entries: MenuEntry[]): void {
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  useMenuStore.setState({ menu: { x, y, label, entries, returnFocus: active } });
}

/** 要素の下にメニューを開く（「その他の操作」ボタン、⌃Return）。 */
export function openMenuBelow(element: HTMLElement, label: string, entries: MenuEntry[]): void {
  const rect = element.getBoundingClientRect();
  useMenuStore.setState({
    menu: { x: rect.left, y: rect.bottom + 4, label, entries, returnFocus: element },
  });
}
