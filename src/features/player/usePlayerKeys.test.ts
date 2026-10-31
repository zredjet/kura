import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ItemSummaryDto } from '../../ipc/items';
import { openEditor } from '../editor/editorKinds';
import { toggleFullscreen } from './playerActions';
import { usePlayerStore } from './playerStore';
import { handlePlayerKey } from './usePlayerKeys';

const controller = vi.hoisted(() => ({
  pause: vi.fn(),
  item: vi.fn<(id: number) => Pick<ItemSummaryDto, 'id' | 'kind'> | undefined>(),
}));

vi.mock('./playerActions', () => ({
  cycleRepeat: vi.fn(),
  playerController: () => controller,
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
