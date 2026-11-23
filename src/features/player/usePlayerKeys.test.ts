import { beforeEach, describe, expect, it, vi } from 'vitest';

import { markReplayed } from '../../lib/keys';

import type { ItemSummaryDto } from '../../ipc/items';
import { openEditor } from '../editor/editorKinds';
import { toggleFullscreen } from './playerActions';
import { usePlayerStore } from './playerStore';
import { handlePlayerKey } from './usePlayerKeys';

const controller = vi.hoisted(() => ({
  pause: vi.fn(),
  togglePlay: vi.fn(),
  next: vi.fn(),
  item: vi.fn<(id: number) => Pick<ItemSummaryDto, 'id' | 'kind'> | undefined>(),
}));

const target = vi.hoisted(() => ({
  positionMs: vi.fn(() => 10_000),
  endMs: vi.fn<() => number | null>(() => 60_000),
  seek: vi.fn(),
  pause: vi.fn(),
  togglePlay: vi.fn(),
  frameMs: vi.fn<() => number | null>(() => null),
}));

vi.mock('./playerActions', () => ({
  cycleRepeat: vi.fn(),
  playerController: () => controller,
  theaterSeekTarget: () => target,
  toggleFullscreen: vi.fn(),
  toggleShuffle: vi.fn(),
}));
vi.mock('../editor/editorKinds', async () => ({
  canEdit: (await import('../editor/kindTraits')).canEdit,
  openEditor: vi.fn(async () => {}),
}));

const press = (key: string) =>
  handlePlayerKey(new KeyboardEvent('keydown', { key, cancelable: true }));

describe('handlePlayerKey（画像エディタ。M12）', () => {
  it('画像エディタを開いている間はシアターのキーを受けない（エディタが先に受ける）', () => {
    usePlayerStore.setState({ mode: 'theater', fullscreen: false });
    const editor = document.createElement('div');
    editor.setAttribute('data-editor', '');
    document.body.append(editor);
    expect(press('f')).toBe(false);
    expect(press('Escape')).toBe(false);
    editor.remove();
    expect(usePlayerStore.getState().mode).toBe('theater');
  });

  it('⌘E は今の項目がなければ扱わない', () => {
    usePlayerStore.setState({ snapshot: { ...usePlayerStore.getState().snapshot, itemId: null } });
    expect(handlePlayerKey(new KeyboardEvent('keydown', { key: 'e', metaKey: true }))).toBe(false);
  });

  it('⌘E は今の音声もエディタで開き、再生を一時停止する（動画は開かない。M13 の G9）', () => {
    const cmdE = () => handlePlayerKey(new KeyboardEvent('keydown', { key: 'e', metaKey: true }));
    usePlayerStore.setState({ snapshot: { ...usePlayerStore.getState().snapshot, itemId: 7 } });
    controller.item.mockReturnValue({ id: 7, kind: 'audio' });
    expect(cmdE()).toBe(true);
    expect(controller.pause).toHaveBeenCalledOnce();
    expect(openEditor).toHaveBeenCalledWith(expect.objectContaining({ id: 7, kind: 'audio' }));
    vi.mocked(openEditor).mockClear();
    controller.pause.mockClear();
    controller.item.mockReturnValue({ id: 7, kind: 'video' });
    expect(cmdE()).toBe(false);
    expect(controller.pause).not.toHaveBeenCalled();
    expect(openEditor).not.toHaveBeenCalled();
  });
});

describe('handlePlayerKey（フルスクリーン。ADR-128）', () => {
  beforeEach(() => {
    vi.mocked(toggleFullscreen).mockClear();
    usePlayerStore.setState({ mode: 'theater', fullscreen: false });
  });

  it('F でフルスクリーンを切り替える', () => {
    expect(press('f')).toBe(true);
    expect(toggleFullscreen).toHaveBeenCalledOnce();
  });

  it('esc はフルスクリーンならまず抜け、そうでなければライブラリへ戻る', () => {
    usePlayerStore.setState({ fullscreen: true });
    press('Escape');
    expect(toggleFullscreen).toHaveBeenCalledOnce();
    expect(usePlayerStore.getState().mode).toBe('theater');

    usePlayerStore.setState({ fullscreen: false });
    press('Escape');
    expect(usePlayerStore.getState().mode).toBe('mini');
  });
});

describe('handlePlayerKey（シーク。ADR-230）', () => {
  const keyOn = (init: KeyboardEventInit, el?: HTMLElement) => {
    const e = new KeyboardEvent('keydown', { ...init, cancelable: true, bubbles: true });
    if (el) {
      Object.defineProperty(e, 'target', { value: el });
    }
    return e;
  };

  beforeEach(() => {
    Object.values(target).forEach((fn) => fn.mockClear());
    controller.togglePlay.mockClear();
    controller.next.mockClear();
    usePlayerStore.setState({
      mode: 'theater',
      fullscreen: false,
      snapshot: { ...usePlayerStore.getState().snapshot, itemId: 3 },
    });
  });

  it('J・L は 5 秒、⇧, ⇧. は 1 秒、終わりの手前で止める', () => {
    expect(handlePlayerKey(keyOn({ key: 'l', code: 'KeyL' }))).toBe(true);
    expect(target.seek).toHaveBeenLastCalledWith(15_000);
    expect(handlePlayerKey(keyOn({ key: 'j', code: 'KeyJ' }))).toBe(true);
    expect(target.seek).toHaveBeenLastCalledWith(5_000);
    expect(handlePlayerKey(keyOn({ key: '<', code: 'Comma', shiftKey: true }))).toBe(true);
    expect(target.seek).toHaveBeenLastCalledWith(9_000);
    target.positionMs.mockReturnValueOnce(59_500);
    expect(handlePlayerKey(keyOn({ key: '>', code: 'Period', shiftKey: true }))).toBe(true);
    expect(target.seek).toHaveBeenLastCalledWith(59_900);
  });

  it(', . は止めて 1 コマ（分からなければ 0.1 秒）', () => {
    target.frameMs.mockReturnValueOnce(40);
    expect(handlePlayerKey(keyOn({ key: '.', code: 'Period' }))).toBe(true);
    expect(target.pause).toHaveBeenCalled();
    expect(target.seek).toHaveBeenLastCalledWith(10_060);
    expect(handlePlayerKey(keyOn({ key: ',', code: 'Comma' }))).toBe(true);
    expect(target.seek).toHaveBeenLastCalledWith(9_900);
  });

  it('K は再生／一時停止', () => {
    expect(handlePlayerKey(keyOn({ key: 'k', code: 'KeyK' }))).toBe(true);
    expect(target.togglePlay).toHaveBeenCalledOnce();
  });

  it('メニューが送った L はシークにしない', () => {
    const e = keyOn({ key: 'l', code: 'KeyL' });
    markReplayed(e);
    expect(handlePlayerKey(e)).toBe(false);
    expect(target.seek).not.toHaveBeenCalled();
  });

  it('文字を打つ欄の上では受けず、スライダーの上では矢印だけを任せる', () => {
    const text = document.createElement('input');
    expect(handlePlayerKey(keyOn({ key: 'l', code: 'KeyL' }, text))).toBe(false);
    const range = document.createElement('input');
    range.type = 'range';
    expect(handlePlayerKey(keyOn({ key: 'ArrowRight' }, range))).toBe(false);
    expect(controller.next).not.toHaveBeenCalled();
    expect(handlePlayerKey(keyOn({ key: ' ' }, range))).toBe(true);
    expect(controller.togglePlay).toHaveBeenCalledOnce();
    expect(handlePlayerKey(keyOn({ key: 'l', code: 'KeyL' }, range))).toBe(true);
    expect(target.seek).toHaveBeenLastCalledWith(15_000);
  });

  it('今のアイテムがなければシークしない', () => {
    usePlayerStore.setState({
      snapshot: { ...usePlayerStore.getState().snapshot, itemId: null },
    });
    expect(handlePlayerKey(keyOn({ key: 'l', code: 'KeyL' }))).toBe(false);
  });
});

describe('setFullscreen', () => {
  it('入ると右のパネルを閉じ、出ると戻す', () => {
    usePlayerStore.setState({
      fullscreen: false,
      panel: 'queue',
      panelBeforeFullscreen: undefined,
    });
    usePlayerStore.getState().setFullscreen(true);
    expect(usePlayerStore.getState().panel).toBeNull();
    usePlayerStore.getState().setFullscreen(false);
    expect(usePlayerStore.getState().panel).toBe('queue');
  });
});
