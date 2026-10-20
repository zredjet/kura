import { isTauri } from '@tauri-apps/api/core';
import {
  CheckMenuItem,
  type CheckMenuItemOptions,
  Menu,
  MenuItem,
  type MenuItemOptions,
  type MenuOptions,
  Submenu,
  type SubmenuOptions,
} from '@tauri-apps/api/menu';

import {
  applyPixelArtToSelection,
  PIXEL_ART_CHOICES,
  PIXEL_ART_LABELS,
  pixelArtMenuState,
} from '../features/library/pixelArtMenu';
import { selectAllEntries } from '../features/library/useLibraryKeys';
import { createPlaylistInteractive } from '../features/playlists/playlistActions';
import { ja } from '../i18n/ja';
import type { AssetChoiceDto } from '../ipc/asset';
import { useItemCacheVersion } from '../stores/itemCache';
import { useSettingsStore } from '../stores/settingsStore';
import { type LayoutMode, useViewStore } from '../stores/viewStore';

/** Tauri が macOS の「ウインドウ」「ヘルプ」メニューとして扱う ID（`WINDOW_SUBMENU_ID`・`HELP_SUBMENU_ID`）。 */
export const WINDOW_MENU_ID = '__tauri_window_menu__';
export const HELP_MENU_ID = '__tauri_help_menu__';

type MenuItems = NonNullable<MenuOptions['items']>;

/** メニューの項目が押したことにするキー（design.md §6）。 */
export interface KeySpec {
  key: string;
  /** `KeyboardEvent.code`（⌥ を押すと `key` が変わるキーはこちらで見る）。 */
  code: string;
  meta?: boolean;
  shift?: boolean;
  alt?: boolean;
}

/** メニューから呼ぶ、キーではない操作。 */
export type MenuCommandId = 'newPlaylist' | 'collapseUnits' | 'settings';

/** メニューの項目を押したときにすること。 */
export type MenuAction =
  | { kind: 'selectAll' }
  | { kind: 'key'; spec: KeySpec }
  | { kind: 'command'; id: MenuCommandId }
  /** 項目 › ドット絵として表示 ▸：一覧で選んでいる画像に当てる（キーがないので、キーを押したことにはしない）。 */
  | { kind: 'pixelArt'; choice: AssetChoiceDto };

export type RunMenuAction = (action: MenuAction) => void;

/** 表示メニューの、状態を持つ項目（チェックの付く表示形式・まとめて表示、文字の変わるサイドバー）。 */
export interface ViewItems {
  layouts: Record<LayoutMode, CheckMenuItemOptions | CheckMenuItem>;
  collapseUnits: CheckMenuItemOptions | CheckMenuItem;
  sidebar: MenuItemOptions | MenuItem;
}

/** 「ドット絵として表示 ▸」の状態：使えるか（一覧で画像を選んでいるか）と、チェックを付ける値（そろっていなければ null）。 */
export interface PixelArtState {
  enabled: boolean;
  current: AssetChoiceDto | null;
}

/** 「ドット絵として表示 ▸」の中身の型（選択肢は自動・はい・いいえのチェック項目）。 */
export type PixelArtOptions = Omit<SubmenuOptions, 'items'> & {
  id: string;
  items: CheckMenuItemOptions[];
};

/** ⌘ などの付いたキーを Tauri のアクセラレータの書き方にする。修飾キーがなければ undefined（文字の入力を奪わない）。 */
export function acceleratorOf(spec: KeySpec): string | undefined {
  if (!spec.meta) {
    return undefined;
  }
  const names: Record<string, string> = { Enter: 'Enter', Backspace: 'Backspace', ' ': 'Space' };
  const key = names[spec.key] ?? spec.key.toUpperCase();
  return ['CmdOrCtrl', spec.shift ? 'Shift' : '', spec.alt ? 'Alt' : '', key]
    .filter(Boolean)
    .join('+');
}

/** キーで動く項目。修飾キーのないキー（T・F・P・1〜5・Space）は表記だけ添えて、アクセラレータにしない。 */
function keyItem(run: RunMenuAction, text: string, spec: KeySpec, hint?: string): MenuItemOptions {
  const accelerator = acceleratorOf(spec);
  return {
    text: accelerator || !hint ? text : `${text}（${hint}）`,
    accelerator,
    action: () => run({ kind: 'key', spec }),
  };
}

const META = { meta: true } as const;

/** 表示メニューの、状態を持つ項目の中身（ID は読み込み直しても前のメニューの片付けと重ならないよう毎回変える）。 */
export function viewItemOptions(
  run: RunMenuAction,
  state: { layout: LayoutMode; collapseUnits: boolean; sidebar: boolean },
  idPrefix: string,
): {
  layouts: Record<LayoutMode, CheckMenuItemOptions>;
  collapseUnits: CheckMenuItemOptions;
  sidebar: MenuItemOptions;
} {
  const t = ja.appMenu;
  const layout = (mode: LayoutMode, digit: string): CheckMenuItemOptions => {
    const spec = { key: digit, code: `Digit${digit}`, ...META };
    return {
      id: `${idPrefix}-layout-${mode}`,
      text: ja.layouts[mode],
      checked: state.layout === mode,
      accelerator: acceleratorOf(spec),
      action: () => run({ kind: 'key', spec }),
    };
  };
  return {
    layouts: {
      justify: layout('justify', '1'),
      grid: layout('grid', '2'),
      list: layout('list', '3'),
    },
    collapseUnits: {
      id: `${idPrefix}-collapse-units`,
      text: ja.library.collapseUnits,
      checked: state.collapseUnits,
      action: () => run({ kind: 'command', id: 'collapseUnits' }),
    },
    sidebar: {
      id: `${idPrefix}-sidebar`,
      ...keyItem(run, state.sidebar ? t.hideSidebar : t.showSidebar, {
        key: 's',
        code: 'KeyS',
        meta: true,
        alt: true,
      }),
    },
  };
}

/**
 * 項目メニューの「ドット絵として表示 ▸ 自動・はい・いいえ」の中身（design.md §3.1・§6。M11）。右クリックの同じ項目と同じく、
 * 一覧で選んでいる画像に当てる。画像を選んでいなければ ▸ ごと無効。ID は読み込み直しても前のメニューの片付けと
 * 重ならないよう毎回変える。
 */
export function pixelArtItemOptions(
  run: RunMenuAction,
  state: PixelArtState,
  idPrefix: string,
): PixelArtOptions {
  return {
    id: `${idPrefix}-pixel-art`,
    text: ja.mediaView.pixelArt,
    enabled: state.enabled,
    items: PIXEL_ART_CHOICES.map((choice) => ({
      id: `${idPrefix}-pixel-art-${choice}`,
      text: PIXEL_ART_LABELS[choice],
      checked: state.current === choice,
      action: () => run({ kind: 'pixelArt', choice }),
    })),
  };
}

/**
 * メニューバーの中身。Tauri の既定のメニューと同じ項目を日本語で並べ、Kura の操作を足す（design.md §6）。
 * - 「すべてを選択」は既定の項目（`selectAll:`）をやめて自前の項目にする（ADR-76）。
 * - Kura の操作は、そのキーを押したことにする（`run` の `key`）。キーの処理と同じ場所で、同じ条件で動く。
 */
export function appMenuItems(
  run: RunMenuAction,
  view: ViewItems,
  pixelArt: SubmenuOptions | Submenu,
): MenuItems {
  const t = ja.appMenu;
  const app = ja.app.name;
  return [
    {
      text: app,
      items: [
        { item: { About: null }, text: t.about(app) },
        { item: 'Separator' },
        {
          text: t.settings,
          accelerator: 'CmdOrCtrl+,',
          action: () => run({ kind: 'command', id: 'settings' }),
        },
        { item: 'Separator' },
        { item: 'Services', text: t.services },
        { item: 'Separator' },
        { item: 'Hide', text: t.hide(app) },
        { item: 'HideOthers', text: t.hideOthers },
        { item: 'Separator' },
        { item: 'Quit', text: t.quit(app) },
      ],
    },
    {
      text: t.file,
      items: [
        keyItem(run, t.import, { key: 'o', code: 'KeyO', ...META }),
        keyItem(run, t.newFolder, { key: 'n', code: 'KeyN', meta: true, shift: true }),
        { text: t.newPlaylist, action: () => run({ kind: 'command', id: 'newPlaylist' }) },
        { item: 'Separator' },
        keyItem(run, t.export, { key: 'e', code: 'KeyE', meta: true, shift: true }),
        { item: 'Separator' },
        { item: 'CloseWindow', text: t.closeWindow },
      ],
    },
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
        {
          text: t.selectAll,
          accelerator: 'CmdOrCtrl+A',
          action: () => run({ kind: 'selectAll' }),
        },
        { item: 'Separator' },
        keyItem(run, t.addTag, { key: 't', code: 'KeyT' }, 'T'),
        keyItem(run, t.putInFolder, { key: 'f', code: 'KeyF' }, 'F'),
        keyItem(run, t.addToPlaylist, { key: 'p', code: 'KeyP' }, 'P'),
      ],
    },
    {
      text: t.view,
      items: [
        view.layouts.justify,
        view.layouts.grid,
        view.layouts.list,
        { item: 'Separator' },
        keyItem(run, t.bigger, { key: '=', code: 'Equal', ...META }),
        keyItem(run, t.smaller, { key: '-', code: 'Minus', ...META }),
        { item: 'Separator' },
        view.collapseUnits,
        { item: 'Separator' },
        view.sidebar,
        keyItem(run, t.inspector, { key: 'i', code: 'KeyI', ...META }),
        { item: 'Separator' },
        { item: 'Fullscreen', text: t.fullscreen },
      ],
    },
    {
      text: t.items,
      items: [
        keyItem(run, t.play, { key: 'Enter', code: 'Enter', ...META }),
        keyItem(run, t.quickLook, { key: ' ', code: 'Space' }, 'Space'),
        keyItem(run, ja.asset.menuAudition, { key: 'l', code: 'KeyL' }, 'L'),
        { item: 'Separator' },
        keyItem(run, t.group, { key: 'g', code: 'KeyG', ...META }),
        keyItem(run, t.ungroup, { key: 'g', code: 'KeyG', meta: true, shift: true }),
        { item: 'Separator' },
        {
          text: t.rating,
          items: [0, 1, 2, 3, 4, 5].map((n) =>
            keyItem(
              run,
              n === 0 ? t.clearRating : t.stars(n),
              { key: String(n), code: `Digit${n}` },
              String(n),
            ),
          ),
        },
        pixelArt,
        { item: 'Separator' },
        keyItem(run, t.trash, { key: 'Backspace', code: 'Backspace', ...META }),
      ],
    },
    {
      text: t.go,
      items: [
        keyItem(run, t.search, { key: 'k', code: 'KeyK', ...META }),
        keyItem(run, t.downloads, { key: 'd', code: 'KeyD', meta: true, shift: true }),
      ],
    },
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

/**
 * キーを押したことにする：フォーカスのある要素（なければ body）にキーのイベントを送る。
 * ライブラリ・シアター・パレットなどのキーの処理が、ふだんと同じ条件で受け取る。
 */
export function replayKey(spec: KeySpec, target: Element | null = document.activeElement): void {
  const el = target instanceof HTMLElement ? target : document.body;
  el.dispatchEvent(
    new KeyboardEvent('keydown', {
      key: spec.key,
      code: spec.code,
      metaKey: spec.meta ?? false,
      shiftKey: spec.shift ?? false,
      altKey: spec.alt ?? false,
      bubbles: true,
      cancelable: true,
    }),
  );
}

/** メニューの項目を押したときの処理。 */
export function runMenuAction(action: MenuAction): void {
  if (action.kind === 'selectAll') {
    selectAllCommand();
  } else if (action.kind === 'key') {
    replayKey(action.spec);
  } else if (action.kind === 'pixelArt') {
    applyPixelArtToSelection(action.choice);
  } else if (action.id === 'newPlaylist') {
    void createPlaylistInteractive();
  } else if (action.id === 'collapseUnits') {
    useViewStore.getState().toggleCollapseUnits();
  } else if (action.id === 'settings') {
    useSettingsStore.getState().show();
  }
}

/** 選択・キャッシュが続けて変わっても、「ドット絵として表示 ▸」を合わせ直すのはこの間隔に 1 回（ms）。 */
const PIXEL_ART_SYNC_MS = 100;

/** 「ドット絵として表示 ▸」のメニューの部品（▸ の行と、選択肢の 3 つのチェック項目）。 */
export interface PixelArtMenuItems {
  submenu: Pick<Submenu, 'setEnabled'>;
  choices: readonly Pick<CheckMenuItem, 'setChecked'>[];
}

/**
 * 「ドット絵として表示 ▸」を今の選択に合わせる関数を作る（`shown` は作ったときの状態）。選んでいる画像がなければ
 * ▸ ごと無効、設定がそろっていればその値にチェック。変わっていなければ何もしない。チェック項目は押すと自分で
 * チェックを反転するので、押したあとは変わっていなくても合わせ直す（`force`）。
 */
export function pixelArtSyncer(
  items: PixelArtMenuItems,
  shown: PixelArtState,
  read: () => PixelArtState = pixelArtMenuState,
): (force: boolean) => void {
  let last = shown;
  return (force) => {
    const state = read();
    if (!force && last.enabled === state.enabled && last.current === state.current) {
      return;
    }
    last = state;
    void items.submenu.setEnabled(state.enabled);
    items.choices.forEach((item, i) => {
      void item.setChecked(state.current === PIXEL_ART_CHOICES[i]);
    });
  };
}

/** メニューバーを差し替える。Tauri の外（ブラウザでの確認など）では何もしない。 */
export async function installAppMenu(): Promise<void> {
  if (!isTauri()) {
    return;
  }
  try {
    let view: {
      layouts: Record<LayoutMode, CheckMenuItem>;
      collapseUnits: CheckMenuItem;
      sidebar: MenuItem;
    } | null = null;
    let syncPixelArt: ((force: boolean) => void) | null = null;
    let pixelArtTimer: ReturnType<typeof setTimeout> | null = null;
    const schedulePixelArt = () => {
      pixelArtTimer ??= setTimeout(() => {
        pixelArtTimer = null;
        syncPixelArt?.(false);
      }, PIXEL_ART_SYNC_MS);
    };
    // チェックの付く項目は押すと自分でチェックを反転するので、押したあとと設定が変わったときに設定に合わせ直す
    const sync = () => {
      if (!view) {
        return;
      }
      const now = useViewStore.getState();
      for (const mode of ['justify', 'grid', 'list'] as const) {
        void view.layouts[mode].setChecked(now.layout === mode);
      }
      void view.collapseUnits.setChecked(now.collapseUnits);
      void view.sidebar.setText(now.sidebar ? ja.appMenu.hideSidebar : ja.appMenu.showSidebar);
    };
    const run: RunMenuAction = (action) => {
      runMenuAction(action);
      sync();
      if (action.kind === 'pixelArt') {
        syncPixelArt?.(true);
      }
    };
    const idPrefix = `view-${Date.now()}`;
    const options = viewItemOptions(run, useViewStore.getState(), idPrefix);
    view = {
      layouts: {
        justify: await CheckMenuItem.new(options.layouts.justify),
        grid: await CheckMenuItem.new(options.layouts.grid),
        list: await CheckMenuItem.new(options.layouts.list),
      },
      collapseUnits: await CheckMenuItem.new(options.collapseUnits),
      sidebar: await MenuItem.new(options.sidebar),
    };
    const pixelState = pixelArtMenuState();
    const pixelOptions = pixelArtItemOptions(run, pixelState, idPrefix);
    const choices = await Promise.all(pixelOptions.items.map((item) => CheckMenuItem.new(item)));
    const pixelArt = await Submenu.new({ ...pixelOptions, items: choices });
    syncPixelArt = pixelArtSyncer({ submenu: pixelArt, choices }, pixelState);
    const menu = await Menu.new({ items: appMenuItems(run, view, pixelArt) });
    await menu.setAsAppMenu();
    useViewStore.subscribe((now, before) => {
      if (
        now.layout !== before.layout ||
        now.collapseUnits !== before.collapseUnits ||
        now.sidebar !== before.sidebar
      ) {
        sync();
      }
      if (now.selection !== before.selection || now.view !== before.view) {
        schedulePixelArt();
      }
    });
    // 選んでいるものの概要が届いた・設定を変えて取り直したとき
    useItemCacheVersion.subscribe(schedulePixelArt);
  } catch (error) {
    console.warn('メニューを設定できませんでした', error);
  }
}
