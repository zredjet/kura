import type {
  CheckMenuItemOptions,
  MenuItemOptions,
  PredefinedMenuItemOptions,
  Submenu,
  SubmenuOptions,
} from '@tauri-apps/api/menu';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { applyPixelArtToSelection } from '../features/library/pixelArtMenu';
import { EMPTY_SELECTION } from '../features/library/selection';
import { ja } from '../i18n/ja';
import { useImportStore } from '../stores/importStore';
import { useLibraryStore } from '../stores/libraryStore';
import { useSettingsStore } from '../stores/settingsStore';
import { useViewStore } from '../stores/viewStore';
import {
  HELP_MENU_ID,
  type MenuAction,
  WINDOW_MENU_ID,
  acceleratorOf,
  appMenuItems,
  editorViewItemOptions,
  historyCommand,
  historyMenuState,
  historySyncer,
  libraryViewItems,
  pixelArtItemOptions,
  type PixelArtState,
  pixelArtSyncer,
  replayKey,
  runMenuAction,
  selectAllCommand,
  viewItemOptions,
} from './appMenu';
import { useEditorStore } from '../features/editor/editorStore';

vi.mock('../features/library/pixelArtMenu', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../features/library/pixelArtMenu')>()),
  applyPixelArtToSelection: vi.fn(),
}));

type Item = MenuItemOptions | PredefinedMenuItemOptions | SubmenuOptions;

const STATE = { layout: 'justify', collapseUnits: false, sidebar: true } as const;
const PIXEL_ART = { enabled: true, current: null } as const;

function items(run: (action: MenuAction) => void = () => {}): Item[] {
  return appMenuItems(
    run,
    viewItemOptions(run, STATE, 'test'),
    pixelArtItemOptions(run, PIXEL_ART, 'test'),
  ) as Item[];
}

function submenu(text: string, run?: (action: MenuAction) => void): SubmenuOptions {
  const found = items(run).find((m) => 'text' in m && m.text === text);
  if (!found || !('items' in found)) {
    throw new Error(`メニュー「${text}」がない`);
  }
  return found as SubmenuOptions;
}

function entry(menu: SubmenuOptions, text: string): MenuItemOptions {
  const found = (menu.items as Item[]).find((i) => 'text' in i && i.text === text);
  if (!found) {
    throw new Error(`項目「${text}」がない`);
  }
  return found as MenuItemOptions;
}

describe('appMenuItems', () => {
  it('編集の「すべてを選択」は ⌘A の自前の項目で、押すと selectAll を呼ぶ', () => {
    const run = vi.fn();
    const selectAll = entry(submenu(ja.appMenu.edit, run), ja.appMenu.selectAll);

    expect(selectAll.accelerator).toBe('CmdOrCtrl+A');
    expect('item' in selectAll).toBe(false);
    selectAll.action?.('');
    expect(run).toHaveBeenCalledWith({ kind: 'selectAll' });
  });

  it('既定の「すべてを選択」（selectAll:）はどこにも置かない', () => {
    const all = items().flatMap((m) => ('items' in m ? ((m.items ?? []) as Item[]) : [m]));
    expect(all.some((i) => 'item' in i && i.item === 'SelectAll')).toBe(false);
    // 文字の編集に要る項目は既定のまま（取り消す・やり直すは M12 から自前）
    for (const kind of ['Cut', 'Copy', 'Paste'] as const) {
      expect(all.some((i) => 'item' in i && i.item === kind)).toBe(true);
    }
    for (const kind of ['Undo', 'Redo'] as const) {
      expect(all.some((i) => 'item' in i && i.item === kind)).toBe(false);
    }
  });

  it('編集の「取り消す」「やり直す」は ⌘Z・⇧⌘Z の自前の項目で、押すと history を呼ぶ（M12）', () => {
    const run = vi.fn();
    const edit = submenu(ja.appMenu.edit, run);
    const undo = entry(edit, ja.appMenu.undo);
    const redo = entry(edit, ja.appMenu.redo);
    expect(undo.accelerator).toBe('CmdOrCtrl+Z');
    expect(redo.accelerator).toBe('CmdOrCtrl+Shift+Z');
    undo.action?.('');
    redo.action?.('');
    expect(run).toHaveBeenNthCalledWith(1, { kind: 'history', redo: false });
    expect(run).toHaveBeenNthCalledWith(2, { kind: 'history', redo: true });
    // 並びは既定のメニューと同じく編集の先頭
    expect((edit.items as Item[]).slice(0, 2)).toEqual([undo, redo]);
  });

  it('項目の「編集…」は ⌘E で、押すと ⌘E を押したことにする（M12）', () => {
    const run = vi.fn();
    const edit = entry(submenu(ja.appMenu.items, run), ja.appMenu.editItem);
    expect(edit.accelerator).toBe('CmdOrCtrl+E');
    edit.action?.('');
    expect(run).toHaveBeenCalledWith({ kind: 'key', spec: { key: 'e', code: 'KeyE', meta: true } });
  });

  it('表示メニューは、エディタの間は ⌘0・⌘1・⌘+・⌘− と情報（M12）', () => {
    const run = vi.fn();
    const items = editorViewItemOptions(run) as Item[];
    const keyed = items.filter((i): i is MenuItemOptions => 'accelerator' in i);
    expect(keyed.map((i) => [i.text, i.accelerator])).toEqual([
      [ja.appMenu.editorFit, 'CmdOrCtrl+0'],
      [ja.appMenu.editorActualSize, 'CmdOrCtrl+1'],
      [ja.appMenu.editorZoomIn, 'CmdOrCtrl+='],
      [ja.appMenu.editorZoomOut, 'CmdOrCtrl+-'],
      [ja.appMenu.editorInfo, 'CmdOrCtrl+I'],
    ]);
    keyed[1]?.action?.('');
    expect(run).toHaveBeenCalledWith({
      kind: 'key',
      spec: { key: '1', code: 'Digit1', meta: true },
    });
    expect(items.some((i) => 'item' in i && i.item === 'Fullscreen')).toBe(true);
  });

  it('表示メニューは、Submenu を渡せばそれを置き、渡さなければライブラリの中身', () => {
    const run = vi.fn();
    const view = viewItemOptions(run, STATE, 'test');
    const library = libraryViewItems(run, view) as Item[];
    const texts = (list: Item[]) =>
      list.map((i) => ('text' in i ? i.text : 'item' in i ? i.item : ''));
    expect(texts(submenu(ja.appMenu.view).items as Item[])).toEqual(texts(library));
    expect(library[0]).toBe(view.layouts.justify);
    const swapped = { text: 'swapped' } as unknown as Submenu;
    const menus = appMenuItems(
      run,
      view,
      pixelArtItemOptions(run, PIXEL_ART, 'test'),
      undefined,
      swapped,
    );
    expect(menus).toContain(swapped);
  });

  it('ウインドウ・ヘルプのメニューは Tauri がそれと分かる ID を持つ', () => {
    expect(submenu(ja.appMenu.window).id).toBe(WINDOW_MENU_ID);
    expect(submenu(ja.appMenu.help).id).toBe(HELP_MENU_ID);
  });

  it('⌘ の付く操作はアクセラレータにし、押すとそのキーを押したことにする', () => {
    const run = vi.fn();
    const trash = entry(submenu(ja.appMenu.items, run), ja.appMenu.trash);
    expect(trash.accelerator).toBe('CmdOrCtrl+Backspace');
    trash.action?.('');
    expect(run).toHaveBeenCalledWith({
      kind: 'key',
      spec: { key: 'Backspace', code: 'Backspace', meta: true },
    });
  });

  it('1 文字のキーは表記だけ添えて、文字の入力を奪わない', () => {
    const tag = entry(submenu(ja.appMenu.edit), `${ja.appMenu.addTag}（T）`);
    expect(tag.accelerator).toBeUndefined();
  });

  it('項目の「ループを試聴（L）」は L を押したことにする（M11）', () => {
    const run = vi.fn();
    const audition = entry(submenu(ja.appMenu.items, run), `${ja.asset.menuAudition}（L）`);
    expect(audition.accelerator).toBeUndefined();
    audition.action?.('');
    expect(run).toHaveBeenCalledWith({ kind: 'key', spec: { key: 'l', code: 'KeyL' } });
  });

  it('項目の「ドット絵として表示 ▸」は評価の次にあり、自動・はい・いいえのチェック項目で、押すとその選択肢を当てる（M11）', () => {
    const run = vi.fn();
    const menu = submenu(ja.appMenu.items, run).items as Item[];
    const at = menu.findIndex((i) => 'text' in i && i.text === ja.mediaView.pixelArt);
    const before = menu[at - 1];
    expect(before && 'text' in before ? before.text : null).toBe(ja.appMenu.rating);
    const pixel = menu[at] as SubmenuOptions;
    const choices = pixel.items as CheckMenuItemOptions[];
    expect(choices.map((c) => c.text)).toEqual([
      ja.mediaView.pixelArtAuto,
      ja.mediaView.pixelArtYes,
      ja.mediaView.pixelArtNo,
    ]);
    expect(choices.every((c) => c.accelerator === undefined)).toBe(true);
    choices[1]?.action?.('');
    expect(run).toHaveBeenCalledWith({ kind: 'pixelArt', choice: 'yes' });
  });

  it('「ドット絵として表示 ▸」は画像を選んでいなければ無効、設定がそろっていればその値にチェック', () => {
    const off = pixelArtItemOptions(() => {}, { enabled: false, current: null }, 'test');
    expect(off.enabled).toBe(false);
    expect(off.items.map((c) => c.checked)).toEqual([false, false, false]);
    const on = pixelArtItemOptions(() => {}, { enabled: true, current: 'no' }, 'test');
    expect(on.enabled).toBe(true);
    expect(on.items.map((c) => c.checked)).toEqual([false, false, true]);
    // ID は読み込み直しても前のメニューの片付けと重ならないよう、呼ぶ側の接頭辞を付ける
    expect(on.id).toBe('test-pixel-art');
    expect(new Set(on.items.map((c) => c.id)).size).toBe(3);
  });

  it('「設定…」（⌘,）はライブラリを開いているときだけ設定のシートを開く（初回の画面ではシートがない）', () => {
    const run = vi.fn();
    const settings = entry(submenu(ja.app.name, run), ja.appMenu.settings);
    expect(settings.accelerator).toBe('CmdOrCtrl+,');
    settings.action?.('');
    expect(run).toHaveBeenCalledWith({ kind: 'command', id: 'settings' });
    // M11 の確かめ F1：初回の画面で押すと、見えないまま開いた印が残り、次に開いたライブラリでいきなり出ていた
    useSettingsStore.setState({ open: false });
    useLibraryStore.setState({ status: 'none' });
    runMenuAction({ kind: 'command', id: 'settings' });
    expect(useSettingsStore.getState().open).toBe(false);
    useLibraryStore.setState({ status: 'open' });
    runMenuAction({ kind: 'command', id: 'settings' });
    expect(useSettingsStore.getState().open).toBe(true);
    useSettingsStore.setState({ open: false });
  });

  it('「ドット絵として表示 ▸」の選択肢は、一覧で選んでいる画像に当てる', () => {
    runMenuAction({ kind: 'pixelArt', choice: 'auto' });
    expect(applyPixelArtToSelection).toHaveBeenCalledWith('auto');
  });

  it('「ドット絵として表示 ▸」は選択が変わったら合わせ直し、押したあとは自分で反転したチェックを戻す', async () => {
    const setEnabled = vi.fn<(enabled: boolean) => Promise<void>>(async () => {});
    const checks = [0, 1, 2].map(() => vi.fn<(checked: boolean) => Promise<void>>(async () => {}));
    let state: PixelArtState = { enabled: false, current: null };
    const sync = pixelArtSyncer(
      { submenu: { setEnabled }, choices: checks.map((setChecked) => ({ setChecked })) },
      state,
      () => state,
    );
    sync(false);
    expect(setEnabled).not.toHaveBeenCalled();

    state = { enabled: true, current: 'yes' };
    sync(false);
    expect(setEnabled).toHaveBeenLastCalledWith(true);
    expect(checks.map((c) => c.mock.lastCall?.[0])).toEqual([false, true, false]);

    // 押したあと（設定はまだ届いていない）：変わっていなくても今の値に戻す
    checks.forEach((c) => c.mockClear());
    sync(false);
    expect(checks[1]).not.toHaveBeenCalled();
    sync(true);
    expect(checks.map((c) => c.mock.lastCall?.[0])).toEqual([false, true, false]);

    state = { enabled: false, current: null };
    sync(false);
    expect(setEnabled).toHaveBeenLastCalledWith(false);
    expect(checks.map((c) => c.mock.lastCall?.[0])).toEqual([false, false, false]);
  });

  it('表示形式は ⌘1〜⌘3 のチェック項目で、いまの形式にチェックが付く', () => {
    const view = viewItemOptions(() => {}, { ...STATE, layout: 'grid' }, 'test');
    expect(view.layouts.justify).toMatchObject({ accelerator: 'CmdOrCtrl+1', checked: false });
    expect(view.layouts.grid).toMatchObject({ accelerator: 'CmdOrCtrl+2', checked: true });
    expect(view.layouts.list).toMatchObject({ accelerator: 'CmdOrCtrl+3', checked: false });
    expect(view.sidebar).toMatchObject({
      text: ja.appMenu.hideSidebar,
      accelerator: 'CmdOrCtrl+Alt+S',
    });
  });
});

describe('acceleratorOf', () => {
  it('修飾キーの順に並べ、修飾キーがなければ付けない', () => {
    expect(acceleratorOf({ key: 'g', code: 'KeyG', meta: true, shift: true })).toBe(
      'CmdOrCtrl+Shift+G',
    );
    expect(acceleratorOf({ key: 'Enter', code: 'Enter', meta: true })).toBe('CmdOrCtrl+Enter');
    expect(acceleratorOf({ key: '=', code: 'Equal', meta: true })).toBe('CmdOrCtrl+=');
    expect(acceleratorOf({ key: 't', code: 'KeyT' })).toBeUndefined();
  });
});

describe('replayKey', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('フォーカスのある要素からキーのイベントを送り、window まで届く', () => {
    const button = document.createElement('button');
    document.body.append(button);
    button.focus();
    const seen: KeyboardEvent[] = [];
    const listener = (e: KeyboardEvent) => seen.push(e);
    window.addEventListener('keydown', listener);

    replayKey({ key: 's', code: 'KeyS', meta: true, alt: true });

    window.removeEventListener('keydown', listener);
    expect(seen).toHaveLength(1);
    expect(seen[0]).toMatchObject({ key: 's', code: 'KeyS', metaKey: true, altKey: true });
    expect(seen[0]?.target).toBe(button);
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

describe('取り消す・やり直す（M12。エディタの中だけエディタへ）', () => {
  afterEach(() => {
    document.body.innerHTML = '';
    useEditorStore.setState({ phase: 'closed', status: null });
  });

  it('入力欄の中は文字の取り消し（execCommand）で、キーは送らない', () => {
    const input = document.createElement('input');
    document.body.append(input);
    input.focus();
    const exec = vi.fn(() => true);
    Object.defineProperty(document, 'execCommand', { value: exec, configurable: true });
    const seen: KeyboardEvent[] = [];
    const listener = (e: KeyboardEvent) => seen.push(e);
    window.addEventListener('keydown', listener);

    historyCommand(false);
    historyCommand(true);

    window.removeEventListener('keydown', listener);
    expect(exec.mock.calls).toEqual([['undo'], ['redo']]);
    expect(seen).toEqual([]);
  });

  it('入力欄の外は ⌘Z・⇧⌘Z を押したことにする（エディタのキーの処理が受ける）', () => {
    const button = document.createElement('button');
    document.body.append(button);
    button.focus();
    const seen: KeyboardEvent[] = [];
    const listener = (e: KeyboardEvent) => seen.push(e);
    window.addEventListener('keydown', listener, true);

    runMenuAction({ kind: 'history', redo: false });
    runMenuAction({ kind: 'history', redo: true });

    window.removeEventListener('keydown', listener, true);
    expect(seen.map((e) => [e.key, e.code, e.metaKey, e.shiftKey])).toEqual([
      ['z', 'KeyZ', true, false],
      ['z', 'KeyZ', true, true],
    ]);
  });

  it('押せるか：エディタの外と入力欄の中はいつも、エディタの中は取り消せる・やり直せるときだけ', () => {
    expect(historyMenuState(null)).toEqual({ undo: true, redo: true });
    useEditorStore.setState({
      phase: 'open',
      status: {
        rev: 3,
        head: 3,
        provisional: false,
        canUndo: true,
        canRedo: false,
        dirty: true,
        saving: null,
        needsReopen: false,
      },
    });
    expect(historyMenuState(null)).toEqual({ undo: true, redo: false });
    const input = document.createElement('input');
    document.body.append(input);
    expect(historyMenuState(input)).toEqual({ undo: true, redo: true });
    useEditorStore.setState({ status: null });
    expect(historyMenuState(document.body)).toEqual({ undo: false, redo: false });
  });

  it('合わせる関数は、変わったときだけ項目を替える', () => {
    const undo = { setEnabled: vi.fn(async () => {}) };
    const redo = { setEnabled: vi.fn(async () => {}) };
    let state = { undo: true, redo: true };
    const sync = historySyncer({ undo, redo }, () => state);
    sync();
    sync();
    expect(undo.setEnabled).toHaveBeenCalledTimes(1);
    state = { undo: false, redo: true };
    sync();
    expect(undo.setEnabled).toHaveBeenLastCalledWith(false);
    expect(redo.setEnabled).toHaveBeenLastCalledWith(true);
  });
});
