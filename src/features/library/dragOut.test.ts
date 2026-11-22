import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { type DragOutEnded, onDragOutEnded, startDragOut } from '../../ipc/dragout';
import { useNoticeStore } from '../../stores/noticeStore';
import { useDragStore, watchDrag } from './dragState';
import {
  beginDragOut,
  endedMessage,
  isOwnDrop,
  outsideWindow,
  resetDragOutForTest,
  useDragOutEvents,
} from './dragOut';

const tauri = vi.hoisted(() => ({ on: true }));
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => tauri.on }));
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
  tauri.on = true;
  vi.mocked(startDragOut).mockClear();
  vi.mocked(onDragOutEnded).mockClear();
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

  it('名前の正規化（NFC・NFD）が違っても同じパスとみなす', async () => {
    const nfc = '/Users/me/ゲーム素材/Kura.kuralib/staging/dragout/1'.normalize('NFC');
    vi.mocked(startDragOut).mockResolvedValueOnce({ started: true, dir: nfc, skipped: 0 });
    await beginDragOut([1]);
    const nfd = `${nfc}/ヒーロー.png`.normalize('NFD');
    expect(nfd).not.toBe(`${nfc}/ヒーロー.png`);
    expect(isOwnDrop([nfd])).toBe(true);
  });

  it('飛ばしたアイテムは始めたときだけ知らせ、始められなかった理由を知らせる', async () => {
    vi.mocked(startDragOut).mockResolvedValueOnce({
      started: true,
      dir: '/L.kuralib/staging/dragout/2',
      skipped: 3,
    });
    await expect(beginDragOut([1])).resolves.toBe(true);
    expect(useNoticeStore.getState().message).toContain('3');
    useNoticeStore.setState({ message: null, action: null });
    // ボタンが離れていて始められなかったときは何も知らせない
    vi.mocked(startDragOut).mockResolvedValueOnce({
      started: false,
      dir: '/L.kuralib/staging/dragout/3',
      skipped: 3,
    });
    await expect(beginDragOut([1])).resolves.toBe(false);
    expect(useNoticeStore.getState().message).toBeNull();
    vi.mocked(startDragOut).mockRejectedValueOnce(new Error('多すぎます'));
    await expect(beginDragOut([1])).resolves.toBe(false);
    expect(useNoticeStore.getState().message).toContain('多すぎます');
  });
});

describe('watchDrag', () => {
  const move = (x: number, y: number) =>
    window.dispatchEvent(new MouseEvent('pointermove', { clientX: x, clientY: y }));

  const up = () => window.dispatchEvent(new MouseEvent('pointerup', { clientX: 30, clientY: 30 }));
  const click = () => {
    const event = new MouseEvent('click', { bubbles: true, cancelable: true });
    document.body.dispatchEvent(event);
    return event.defaultPrevented;
  };

  it('タイルのドラッグが窓の外へ出たら OS のドラッグに切り替え、アプリの中のドラッグを終える', () => {
    watchDrag({ button: 0, ctrlKey: false, clientX: 10, clientY: 10 }, () => ({
      payload: { kind: 'entries', ids: [4, 5] },
      label: '2 項目',
    }));
    move(30, 30);
    expect(useDragStore.getState().payload).toEqual({ kind: 'entries', ids: [4, 5] });
    expect(document.documentElement.dataset.dragging).toBe('true');
    move(-3, 30);
    expect(startDragOut).toHaveBeenCalledWith([4, 5]);
    expect(useDragStore.getState().payload).toBeNull();
    expect(document.documentElement.dataset.dragging).toBeUndefined();
    // もう見張らない（Esc も取らない）
    move(-10, 30);
    expect(startDragOut).toHaveBeenCalledTimes(1);
    const esc = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true });
    window.dispatchEvent(esc);
    expect(esc.defaultPrevented).toBe(false);
  });

  it('OS のドラッグの終わりに届く離しに続くクリックは捨て、そのあとのクリックは捨てない', () => {
    vi.useFakeTimers();
    try {
      watchDrag({ button: 0, ctrlKey: false, clientX: 10, clientY: 10 }, () => ({
        payload: { kind: 'entries', ids: [4] },
        label: '1 項目',
      }));
      move(30, 30);
      move(-3, 30);
      // OS のドラッグが終わって WebView に離しが届き、押したタイルの上ならクリックが続く
      up();
      expect(click()).toBe(true);
      vi.runAllTimers();
      expect(click()).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('離しが届かずに次の押しが来たら、そのクリックは捨てない', () => {
    watchDrag({ button: 0, ctrlKey: false, clientX: 10, clientY: 10 }, () => ({
      payload: { kind: 'entries', ids: [4] },
      label: '1 項目',
    }));
    move(30, 30);
    move(-3, 30);
    window.dispatchEvent(new MouseEvent('pointerdown', { clientX: 30, clientY: 30 }));
    up();
    expect(click()).toBe(false);
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
  it('渡せなかった項目と、渡したフォルダの中で作れなかったファイルを知らせ、外すと受けるのをやめる', async () => {
    let handler: ((event: DragOutEnded) => void) | null = null;
    const unlisten = vi.fn();
    vi.mocked(onDragOutEnded).mockImplementationOnce(async (h) => {
      handler = h;
      return unlisten;
    });
    const hook = renderHook(() => useDragOutEvents());
    await act(async () => {});
    expect(handler).not.toBeNull();
    act(() => handler?.({ dir: '/d', delivered: 2, missed: 0, incomplete: 0 }));
    expect(useNoticeStore.getState().message).toBeNull();
    act(() => handler?.({ dir: '/d', delivered: 0, missed: 1, incomplete: 0 }));
    expect(useNoticeStore.getState().message).toBe(endedMessage(1, 0));
    act(() => handler?.({ dir: '/d', delivered: 1, missed: 0, incomplete: 2 }));
    expect(useNoticeStore.getState().message).toBe(endedMessage(0, 2));
    hook.unmount();
    expect(unlisten).toHaveBeenCalled();
  });

  it('Tauri の外では受けない', () => {
    tauri.on = false;
    renderHook(() => useDragOutEvents());
    expect(onDragOutEnded).not.toHaveBeenCalled();
  });
});

describe('endedMessage', () => {
  it('渡せなかった項目とフォルダの中のファイルを言い分ける', () => {
    expect(endedMessage(0, 0)).toBeNull();
    expect(endedMessage(2, 0)).toContain('2 項目');
    expect(endedMessage(0, 3)).toContain('3 ファイル');
    const both = endedMessage(2, 3);
    expect(both).toContain('2 項目');
    expect(both).toContain('3 ファイル');
  });
});
