import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ItemSummaryDto } from '../../ipc/items';
import { getCached } from '../../stores/itemCache';
import { useImportStore } from '../../stores/importStore';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { setVerticalNavigator } from './navigation';
import { EMPTY_SELECTION, selectOne } from './selection';
import { handleLibraryKey } from './useLibraryKeys';
import { toggleAuditionKey } from '../asset/auditionStore';
import { openEditor } from '../editor/editorStore';

vi.mock('../asset/auditionStore', () => ({ toggleAuditionKey: vi.fn(() => true) }));
vi.mock('../editor/editorStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../editor/editorStore')>()),
  openEditor: vi.fn(async () => undefined),
}));
vi.mock('../../stores/itemCache', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../stores/itemCache')>()),
  getCached: vi.fn(),
}));

function press(init: KeyboardEventInit): boolean {
  return handleLibraryKey(new KeyboardEvent('keydown', { cancelable: true, ...init }));
}

describe('handleLibraryKey（M9 で足したキー）', () => {
  beforeEach(() => {
    useLibraryStore.setState({
      status: 'open',
      list: { ids: Uint32Array.from([10, 20, 30, 40]), aspects: new Uint16Array(4), groups: [] },
    });
    useImportStore.setState({ phase: 'idle' });
    useViewStore.setState({
      selection: EMPTY_SELECTION,
      playlist: null,
      layout: 'justify',
      sidebar: true,
    });
  });

  afterEach(() => setVerticalNavigator(null));

  it('⌘1〜⌘3 で表示形式を変える', () => {
    expect(press({ key: '3', code: 'Digit3', metaKey: true })).toBe(true);
    expect(useViewStore.getState().layout).toBe('list');
    press({ key: '2', code: 'Digit2', metaKey: true });
    expect(useViewStore.getState().layout).toBe('grid');
    press({ key: '1', code: 'Digit1', metaKey: true });
    expect(useViewStore.getState().layout).toBe('justify');
  });

  it('⌘⌥S でサイドバーを切り替える（⌥ で文字が変わってもキーの位置で見る）', () => {
    expect(press({ key: 'ß', code: 'KeyS', metaKey: true, altKey: true })).toBe(true);
    expect(useViewStore.getState().sidebar).toBe(false);
    press({ key: 'ß', code: 'KeyS', metaKey: true, altKey: true });
    expect(useViewStore.getState().sidebar).toBe(true);
  });

  it('↑↓ は描いている側が決めた行へ移り、⇧ で範囲を広げる', () => {
    // 2 列のグリッドのつもり
    setVerticalNavigator((index, dir) => {
      const next = index + dir * 2;
      return next >= 0 && next < 4 ? next : null;
    });
    // 何も選んでいなければ先頭
    press({ key: 'ArrowDown' });
    expect(useViewStore.getState().selection.focus).toBe(10);
    press({ key: 'ArrowDown' });
    expect(useViewStore.getState().selection.ids).toEqual([30]);
    // いちばん下ではそのまま
    press({ key: 'ArrowDown' });
    expect(useViewStore.getState().selection.ids).toEqual([30]);
    useViewStore.setState({ selection: selectOne(40) });
    press({ key: 'ArrowUp', shiftKey: true });
    expect(useViewStore.getState().selection.ids).toEqual([40, 30, 20]);
  });
});

// design.md §6：L で選んでいる音声のループを試聴（もう一度で止める）
describe('handleLibraryKey（L：ループを試聴。M11）', () => {
  const toggle = vi.mocked(toggleAuditionKey);
  const cached = vi.mocked(getCached);

  beforeEach(() => {
    useLibraryStore.setState({
      status: 'open',
      list: { ids: Uint32Array.from([10, 20]), aspects: new Uint16Array(2), groups: [] },
    });
    useImportStore.setState({ phase: 'idle' });
    useViewStore.setState({ selection: selectOne(20), playlist: null, view: { kind: 'all' } });
    toggle.mockReset();
    toggle.mockReturnValue(true);
    cached.mockReset();
    cached.mockImplementation(
      (id) => ({ id, kind: id === 20 ? 'audio' : 'image' }) as ItemSummaryDto,
    );
  });

  it('選んでいるものとその種類を試聴の口に渡す（扱ったら true）', () => {
    expect(press({ key: 'l', code: 'KeyL' })).toBe(true);
    expect(toggle).toHaveBeenCalledWith(20, 'audio');
    // ⇧ を押していても大文字でも同じ
    useViewStore.setState({ selection: selectOne(10) });
    expect(press({ key: 'L', code: 'KeyL', shiftKey: true })).toBe(true);
    expect(toggle).toHaveBeenLastCalledWith(10, 'image');
  });

  it('何も選んでいなくても渡す（試聴していれば止める）。口が扱わなければ false', () => {
    useViewStore.setState({ selection: EMPTY_SELECTION });
    toggle.mockReturnValue(false);
    expect(press({ key: 'l', code: 'KeyL' })).toBe(false);
    expect(toggle).toHaveBeenCalledWith(null, undefined);
  });

  it('⌘L・⌃L・⌥L と、文字を入力している所では受けない', () => {
    expect(press({ key: 'l', code: 'KeyL', metaKey: true })).toBe(false);
    expect(press({ key: 'l', code: 'KeyL', ctrlKey: true })).toBe(false);
    expect(press({ key: '¬', code: 'KeyL', altKey: true })).toBe(false);
    const input = document.createElement('input');
    document.body.append(input);
    expect(
      handleLibraryKey(
        Object.defineProperty(new KeyboardEvent('keydown', { key: 'l' }), 'target', {
          value: input,
        }),
      ),
    ).toBe(false);
    input.remove();
    expect(toggle).not.toHaveBeenCalled();
  });
});

// design.md §3.1：⌘E で画像エディタ（M12。画像 1 件のときだけ）
describe('handleLibraryKey（⌘E：編集…。M12）', () => {
  const cached = vi.mocked(getCached);
  const open = vi.mocked(openEditor);

  beforeEach(() => {
    useLibraryStore.setState({
      status: 'open',
      list: { ids: Uint32Array.from([10, 20]), aspects: new Uint16Array(2), groups: [] },
    });
    useImportStore.setState({ phase: 'idle' });
    useViewStore.setState({ selection: selectOne(10), playlist: null, view: { kind: 'all' } });
    open.mockClear();
    cached.mockReset();
    cached.mockImplementation(
      (id) => ({ id, kind: id === 20 ? 'audio' : 'image' }) as ItemSummaryDto,
    );
  });

  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('画像 1 件を選んでいれば開く', () => {
    expect(press({ key: 'e', code: 'KeyE', metaKey: true })).toBe(true);
    expect(open).toHaveBeenCalledWith(expect.objectContaining({ id: 10 }));
  });

  it('画像でない・複数・ゴミ箱の中では開かない', () => {
    useViewStore.setState({ selection: selectOne(20) });
    expect(press({ key: 'e', code: 'KeyE', metaKey: true })).toBe(false);
    useViewStore.setState({ selection: { ids: [10, 20], anchor: 10, focus: 20 } });
    expect(press({ key: 'e', code: 'KeyE', metaKey: true })).toBe(false);
    useViewStore.setState({ selection: selectOne(10), view: { kind: 'trash' } });
    expect(press({ key: 'e', code: 'KeyE', metaKey: true })).toBe(false);
    expect(open).not.toHaveBeenCalled();
  });

  it('画像エディタを開いている間は、ライブラリのキーを受けない', () => {
    const editor = document.createElement('div');
    editor.setAttribute('data-editor', '');
    document.body.append(editor);
    expect(press({ key: '1', code: 'Digit1', metaKey: true })).toBe(false);
    expect(press({ key: 'e', code: 'KeyE', metaKey: true })).toBe(false);
  });
});
