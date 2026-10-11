import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { useImportStore } from '../../stores/importStore';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { setVerticalNavigator } from './navigation';
import { EMPTY_SELECTION, selectOne } from './selection';
import { handleLibraryKey } from './useLibraryKeys';

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
