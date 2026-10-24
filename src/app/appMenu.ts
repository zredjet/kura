import { isTauri } from '@tauri-apps/api/core';
import {
  CheckMenuItem,
  type CheckMenuItemOptions,
  Menu,
  MenuItem,
  type MenuItemOptions,
  type MenuOptions,
  PredefinedMenuItem,
  type PredefinedMenuItemOptions,
  Submenu,
  type SubmenuOptions,
} from '@tauri-apps/api/menu';

import { useEditorStore } from '../features/editor/editorStore';
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
import { useLibraryStore } from '../stores/libraryStore';
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
  /** 編集 › 取り消す・やり直す（M12。入力欄では文字の取り消し、ほかはエディタへ ⌘Z・⇧⌘Z を押したことにする）。 */
  | { kind: 'history'; redo: boolean }
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

/** 編集メニューの「取り消す」「やり直す」（自前の項目。押せるかは `historySyncer` が合わせる）。 */
export interface HistoryItems {
  undo: MenuItemOptions | MenuItem;
  redo: MenuItemOptions | MenuItem;
}

/** ファイルメニューの「保存」「別名で保存…」（画像エディタを開いている間だけ押せる。`installAppMenu` が合わせる）。 */
export interface SaveItems {
  save: MenuItemOptions | MenuItem;
  saveAs: MenuItemOptions | MenuItem;
}

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
 * 編集メニューの「取り消す」（⌘Z）・「やり直す」（⇧⌘Z）。既定の項目（undo:・redo:）は WebView の文字の取り消しにしか
 * 届かず画像エディタへ渡せないので、⌘A（ADR-76）と同じく自前の項目にする（M12。FR-EDIT-06）。
 */
export function historyItemOptions(run: RunMenuAction): {
  undo: MenuItemOptions;
  redo: MenuItemOptions;
} {
  const t = ja.appMenu;
  return {
    undo: {
      text: t.undo,
      accelerator: 'CmdOrCtrl+Z',
      action: () => run({ kind: 'history', redo: false }),
    },
    redo: {
      text: t.redo,
      accelerator: 'CmdOrCtrl+Shift+Z',
      action: () => run({ kind: 'history', redo: true }),
    },
  };
}

/**
 * ファイルメニューの「保存」（⌘S）・「別名で保存…」（⌘⇧S）（M12）。そのキーを押したことにする（エディタのキーの処理が
 * 受ける）。画像エディタを閉じている間は押せない（作ったときは押せない）。
 */
export function saveItemOptions(run: RunMenuAction): {
  save: MenuItemOptions;
  saveAs: MenuItemOptions;
} {
  const t = ja.appMenu;
  return {
    save: { ...keyItem(run, t.save, { key: 's', code: 'KeyS', ...META }), enabled: false },
    saveAs: {
      ...keyItem(run, t.saveAs, { key: 's', code: 'KeyS', meta: true, shift: true }),
      enabled: false,
    },
  };
}

/** 表示メニューの中身（ライブラリ）。 */
export function libraryViewItems(run: RunMenuAction, view: ViewItems): MenuItems {
  const t = ja.appMenu;
  return [
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
  ];
}

/**
 * 表示メニューの中身（画像エディタを開いている間。M12）：⌘0 全体を表示・⌘1 実際のサイズ（100%）・⌘+ 拡大・⌘− 縮小・
 * ⌘I 情報。ライブラリの表示形式（⌘1〜⌘3）・サムネイルの大きさ（⌘+・⌘−）と重なるので、開いている間は差し替える。
 * どれもそのキーを押したことにする（エディタのキーの処理が受ける）。
 */
export function editorViewItemOptions(
  run: RunMenuAction,
): (MenuItemOptions | PredefinedMenuItemOptions)[] {
  const t = ja.appMenu;
  return [
    keyItem(run, t.editorFit, { key: '0', code: 'Digit0', ...META }),
    keyItem(run, t.editorActualSize, { key: '1', code: 'Digit1', ...META }),
    keyItem(run, t.editorZoomIn, { key: '=', code: 'Equal', ...META }),
    keyItem(run, t.editorZoomOut, { key: '-', code: 'Minus', ...META }),
    { item: 'Separator' },
    keyItem(run, t.editorInfo, { key: 'i', code: 'KeyI', ...META }),
    { item: 'Separator' },
    { item: 'Fullscreen', text: t.fullscreen },
  ];
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
 * - 「すべてを選択」は既定の項目（`selectAll:`）をやめて自前の項目にする（ADR-76）。「取り消す」「やり直す」も自前
 *   （M12。`historyItemOptions`）。
 * - Kura の操作は、そのキーを押したことにする（`run` の `key`）。キーの処理と同じ場所で、同じ条件で動く。
 * - 表示メニューは、画像エディタを開いている間だけ中身を差し替える（`viewMenu` を渡せばその Submenu を置く）。
 */
export function appMenuItems(
  run: RunMenuAction,
  view: ViewItems,
  pixelArt: SubmenuOptions | Submenu,
  history: HistoryItems = historyItemOptions(run),
  viewMenu?: Submenu,
  save: SaveItems = saveItemOptions(run),
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
        save.save,
        save.saveAs,
        { item: 'Separator' },
        keyItem(run, t.export, { key: 'e', code: 'KeyE', meta: true, shift: true }),
        { item: 'Separator' },
        { item: 'CloseWindow', text: t.closeWindow },
      ],
    },
    {
      text: t.edit,
      items: [
        history.undo,
        history.redo,
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
    viewMenu ?? { text: t.view, items: libraryViewItems(run, view) },
    {
      text: t.items,
      items: [
        keyItem(run, t.play, { key: 'Enter', code: 'Enter', ...META }),
        keyItem(run, t.quickLook, { key: ' ', code: 'Space' }, 'Space'),
        keyItem(run, t.editItem, { key: 'e', code: 'KeyE', ...META }),
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

/** 文字を編集する所（入力欄・テキストエリア・contenteditable）か。 */
export function isTextTarget(target: Element | null): boolean {
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  );
}

/**
 * 「取り消す」（⌘Z）・「やり直す」（⇧⌘Z）：入力欄にフォーカスがあれば文字の取り消し、なければ ⌘Z・⇧⌘Z を押したことに
 * する（画像エディタのキーの処理が受ける。エディタの外では何も受けない：ADR-117）。
 */
export function historyCommand(
  redo: boolean,
  target: Element | null = document.activeElement,
): void {
  if (isTextTarget(target)) {
    document.execCommand(redo ? 'redo' : 'undo');
    return;
  }
  replayKey({ key: 'z', code: 'KeyZ', meta: true, shift: redo }, target);
}

/** 「取り消す」「やり直す」を押せるか。 */
export interface HistoryState {
  undo: boolean;
  redo: boolean;
}

/**
 * 「取り消す」「やり直す」を押せるか：エディタの外・入力欄の中は押せる（文字の取り消し）。エディタの中は、取り消せる・
 * やり直せる手があるときだけ（Rust の `status.canUndo`・`canRedo`）。
 */
export function historyMenuState(target: Element | null = document.activeElement): HistoryState {
  const s = useEditorStore.getState();
  if (s.phase === 'closed' || isTextTarget(target)) {
    return { undo: true, redo: true };
  }
  return { undo: s.status?.canUndo ?? false, redo: s.status?.canRedo ?? false };
}

/** 「取り消す」「やり直す」を今の状態に合わせる関数を作る（変わっていなければ何もしない）。 */
export function historySyncer(
  items: { undo: Pick<MenuItem, 'setEnabled'>; redo: Pick<MenuItem, 'setEnabled'> },
  read: () => HistoryState = historyMenuState,
): () => void {
  let last: HistoryState | null = null;
  return () => {
    const state = read();
    if (last && last.undo === state.undo && last.redo === state.redo) {
      return;
    }
    last = state;
    void items.undo.setEnabled(state.undo);
    void items.redo.setEnabled(state.redo);
  };
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
  } else if (action.kind === 'history') {
    historyCommand(action.redo);
  } else if (action.kind === 'key') {
    replayKey(action.spec);
  } else if (action.kind === 'pixelArt') {
    applyPixelArtToSelection(action.choice);
  } else if (action.id === 'newPlaylist') {
    void createPlaylistInteractive();
  } else if (action.id === 'collapseUnits') {
    useViewStore.getState().toggleCollapseUnits();
  } else if (action.id === 'settings') {
    // 設定のシートは LibraryScreen の中にある。初回の画面で開いた印を立てると、見えないまま残って次に開いた
    // ライブラリでいきなり出る（M11 の確かめ F1）
    if (useLibraryStore.getState().status === 'open') {
      useSettingsStore.getState().show();
    }
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

/** 出し入れする項目の実体。 */
type MenuObject = Submenu | MenuItem | PredefinedMenuItem | CheckMenuItem;

/** 項目の中身（オプション）を、Submenu に出し入れできる項目の実体にする（実体はそのまま）。 */
async function toMenuObjects(items: MenuItems): Promise<MenuObject[]> {
  const out: MenuObject[] = [];
  for (const item of items) {
    if (
      item instanceof MenuItem ||
      item instanceof CheckMenuItem ||
      item instanceof Submenu ||
      item instanceof PredefinedMenuItem
    ) {
      out.push(item);
    } else if ('item' in item) {
      out.push(await PredefinedMenuItem.new(item));
    } else if (!('items' in item) && !('checked' in item) && !('icon' in item)) {
      // 表示メニューの中身は、実体・区切りなどの既定の項目・ふつうの項目だけ
      out.push(await MenuItem.new(item as MenuItemOptions));
    }
  }
  return out;
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
    const historyOptions = historyItemOptions(run);
    const history = {
      undo: await MenuItem.new(historyOptions.undo),
      redo: await MenuItem.new(historyOptions.redo),
    };
    const saveOptions = saveItemOptions(run);
    const save = {
      save: await MenuItem.new(saveOptions.save),
      saveAs: await MenuItem.new(saveOptions.saveAs),
    };
    const libraryView = await toMenuObjects(libraryViewItems(run, view));
    const editorView = await toMenuObjects(editorViewItemOptions(run));
    const viewMenu = await Submenu.new({ text: ja.appMenu.view, items: libraryView });
    const menu = await Menu.new({
      items: appMenuItems(run, view, pixelArt, history, viewMenu, save),
    });
    await menu.setAsAppMenu();
    const syncHistory = historySyncer(history);
    syncHistory();
    // 画像エディタを開いている間は表示メニューを差し替える（順に。開く・閉じるが続いても最後の状態になる）
    let editorShown = false;
    let swapping = Promise.resolve();
    const swapView = (editor: boolean) => {
      if (editor === editorShown) {
        return;
      }
      editorShown = editor;
      swapping = swapping
        .then(async () => {
          for (const item of await viewMenu.items()) {
            await viewMenu.remove(item);
          }
          await viewMenu.append(editor ? editorView : libraryView);
        })
        .catch((error: unknown) => console.warn('表示のメニューを替えられませんでした', error));
    };
    useEditorStore.subscribe((now, before) => {
      if ((now.phase === 'closed') !== (before.phase === 'closed')) {
        swapView(now.phase !== 'closed');
      }
      if ((now.phase === 'open') !== (before.phase === 'open')) {
        void save.save.setEnabled(now.phase === 'open');
        void save.saveAs.setEnabled(now.phase === 'open');
      }
      if (now.phase !== before.phase || now.status !== before.status) {
        syncHistory();
      }
    });
    // 入力欄に入った・出たら（入力欄の中は文字の取り消しなので押せる）
    document.addEventListener('focusin', syncHistory);
    document.addEventListener('focusout', () => setTimeout(syncHistory, 0));
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
