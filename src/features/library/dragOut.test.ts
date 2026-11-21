import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { type DragOutEnded, onDragOutEnded, startDragOut } from '../../ipc/dragout';
import { useNoticeStore } from '../../stores/noticeStore';
import { useDragStore, watchDrag } from './dragState';
import {
  beginDragOut,
  isOwnDrop,
  outsideWindow,
  resetDragOutForTest,
  useDragOutEvents,
} from './dragOut';

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => true }));
vi.mock('../../ipc/dragout', () => ({
  startDragOut: vi.fn(async () => ({
    started: true,
    dir: '/L.kuralib/staging/dragout/1',
    skipped: 0,
  })),
  onDragOutEnded: vi.fn(async () => () => undefined),
}));

beforeEach(() => {
  // jsdom には elementFromPoint がない（落とし先は見ない）
  Object.defineProperty(document, 'elementFromPoint', { value: () => null, configurable: true });
  resetDragOutForTest();
  vi.mocked(startDragOut).mockClear();
  useNoticeStore.setState({ message: null, action: null });
  useDragStore.setState({ payload: null, target: null });
});

describe('outsideWindow', () => {
  it('表示範囲の外（端の外側）だけを外とみなす', () => {
    expect(outsideWindow(0, 0, 100, 50)).toBe(false);
    expect(outsideWindow(99, 49, 100, 50)).toBe(false);
    expect(outsideWindow(-1, 10, 100, 50)).toBe(true);
    expect(outsideWindow(10, -1, 100, 50)).toBe(true);
    expect(outsideWindow(100, 10, 100, 50)).toBe(true);
    expect(outsideWindow(10, 50, 100, 50)).toBe(true);
  });
});

describe('beginDragOut', () => {
  it('始めた回の一時置き場を覚え、そこのファイルだけを Kura 自身へのドロップとみなす', async () => {
    expect(isOwnDrop(['/L.kuralib/staging/dragout/1/a.png'])).toBe(false);
    await expect(beginDragOut([1, 2])).resolves.toBe(true);
    expect(startDragOut).toHaveBeenCalledWith([1, 2]);
    expect(isOwnDrop(['/L.kuralib/staging/dragout/1/a.png'])).toBe(true);
    expect(isOwnDrop(['/L.kuralib/staging/dragout/1'])).toBe(true);
    expect(isOwnDrop(['/L.kuralib/staging/dragout/1/a.png', '/Users/me/Desktop/b.png'])).toBe(
      false,
    );
    expect(isOwnDrop(['/L.kuralib/staging/dragout/10/a.png'])).toBe(false);
    expect(isOwnDrop([])).toBe(false);
  });

  it('飛ばしたアイテムと始められなかった理由を知らせる', async () => {
    vi.mocked(startDragOut).mockResolvedValueOnce({
      started: false,
      dir: '/L.kuralib/staging/dragout/2',
      skipped: 3,
    });
    await expect(beginDragOut([1])).resolves.toBe(false);
    expect(useNoticeStore.getState().message).toContain('3');
    vi.mocked(startDragOut).mockRejectedValueOnce(new Error('多すぎます'));
    await expect(beginDragOut([1])).resolves.toBe(false);
    expect(useNoticeStore.getState().message).toContain('多すぎます');
  });
});

describe('watchDrag', () => {
  const move = (x: number, y: number) =>
    window.dispatchEvent(new MouseEvent('pointermove', { clientX: x, clientY: y }));

  it('タイルのドラッグが窓の外へ出たら OS のドラッグに切り替え、アプリの中のドラッグを終える', () => {
    watchDrag({ button: 0, ctrlKey: false, clientX: 10, clientY: 10 }, () => ({
      payload: { kind: 'entries', ids: [4, 5] },
      label: '2 項目',
    }));
    move(30, 30);
    expect(useDragStore.getState().payload).toEqual({ kind: 'entries', ids: [4, 5] });
    move(-3, 30);
    expect(startDragOut).toHaveBeenCalledWith([4, 5]);
    expect(useDragStore.getState().payload).toBeNull();
    // もう見張らない
    move(-10, 30);
    expect(startDragOut).toHaveBeenCalledTimes(1);
  });

  it('フォルダの行のドラッグは窓の外へ出ても切り替えない', () => {
    watchDrag({ button: 0, ctrlKey: false, clientX: 10, clientY: 10 }, () => ({
      payload: { kind: 'folder', id: 1 },
      label: 'フォルダ',
    }));
    move(30, 30);
    move(-3, 30);
    expect(startDragOut).not.toHaveBeenCalled();
    window.dispatchEvent(new MouseEvent('pointerup', { clientX: -3, clientY: 30 }));
    expect(useDragStore.getState().payload).toBeNull();
  });
});

describe('useDragOutEvents', () => {
  it('落とす先が求めたのに渡せなかった項目があれば知らせる', async () => {
    let handler: ((event: DragOutEnded) => void) | null = null;
    vi.mocked(onDragOutEnded).mockImplementationOnce(async (h) => {
      handler = h;
      return () => undefined;
    });
    renderHook(() => useDragOutEvents());
    await act(async () => {});
    expect(handler).not.toBeNull();
    act(() => handler?.({ dir: '/d', delivered: 2, missed: 0 }));
    expect(useNoticeStore.getState().message).toBeNull();
    act(() => handler?.({ dir: '/d', delivered: 0, missed: 1 }));
    expect(useNoticeStore.getState().message).toContain('1');
  });
});
