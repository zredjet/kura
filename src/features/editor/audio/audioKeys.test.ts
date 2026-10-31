import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { editorSelectAll, handleEditorKey } from '../editorKeys';
import { useEditorStore } from '../editorStore';
import * as store from './audioStore';
import * as tools from './audioTools';

vi.mock('./audioStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./audioStore')>()),
  togglePlay: vi.fn(),
  toggleLoopAudition: vi.fn(),
  toggleAB: vi.fn(),
  nudgeCursor: vi.fn(),
  cursorToEdge: vi.fn(),
  audioFit: vi.fn(),
  audioZoom: vi.fn(),
  cancelTrim: vi.fn(),
  selectAllAudio: vi.fn(() => true),
}));
vi.mock('./audioTools', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./audioTools')>()),
  selectAudioTool: vi.fn(),
  setInOut: vi.fn(),
  confirmTrim: vi.fn(),
}));
vi.mock('../editorSave', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../editorSave')>()),
  requestCloseEditor: vi.fn(async () => undefined),
  requestSave: vi.fn(),
  openSaveAs: vi.fn(),
}));

function press(key: string, init: KeyboardEventInit = {}): boolean {
  const e = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
  document.body.dispatchEvent(e);
  return handleEditorKey(e);
}

beforeEach(() => {
  vi.clearAllMocks();
  useEditorStore.setState({ phase: 'open', kind: 'audio' });
  store.useAudioStore.setState({ trim: null, selection: null });
});

afterEach(() => {
  useEditorStore.setState({ phase: 'closed', kind: null });
});

describe('音声エディタのキー', () => {
  it('Space で再生・止める、L でループを試聴、B で聞き比べ（押し続けの繰り返しは数えない）', () => {
    expect(press(' ')).toBe(true);
    expect(store.togglePlay).toHaveBeenCalledTimes(1);
    expect(press(' ', { repeat: true })).toBe(true);
    expect(store.togglePlay).toHaveBeenCalledTimes(1);
    expect(press('l')).toBe(true);
    expect(store.toggleLoopAudition).toHaveBeenCalled();
    expect(press('b')).toBe(true);
    expect(store.toggleAB).toHaveBeenCalled();
  });

  it('S T F R で道具、I・O で始め・終わり', () => {
    press('s');
    press('t');
    press('F', { shiftKey: false });
    press('r');
    expect(vi.mocked(tools.selectAudioTool).mock.calls.map((c) => c[0])).toEqual([
      'select',
      'trim',
      'fade',
      'loop',
    ]);
    press('i');
    press('o');
    expect(vi.mocked(tools.setInOut).mock.calls.map((c) => c[0])).toEqual(['in', 'out']);
  });

  it('← → でカーソルを 1 秒（⇧で 10 秒）、Home・End で先頭・末尾', () => {
    press('ArrowLeft');
    press('ArrowRight', { shiftKey: true });
    expect(vi.mocked(store.nudgeCursor).mock.calls.map((c) => c[0])).toEqual([-1, 10]);
    press('Home');
    press('End');
    expect(vi.mocked(store.cursorToEdge).mock.calls.map((c) => c[0])).toEqual([false, true]);
  });

  it('↵ は切り詰めの間だけ確定、esc は切り詰めをやめる → 選択を外す → 閉じる', async () => {
    const { requestCloseEditor } = await import('../editorSave');
    expect(press('Enter')).toBe(false);
    store.useAudioStore.setState({
      trim: { node: null, start: 0, end: 10, bounds: 10, previewed: false },
    });
    expect(press('Enter')).toBe(true);
    expect(tools.confirmTrim).toHaveBeenCalled();
    press('Escape');
    expect(store.cancelTrim).toHaveBeenCalled();
    expect(requestCloseEditor).not.toHaveBeenCalled();
    store.useAudioStore.setState({ trim: null, selection: { start: 0, end: 5 } });
    press('Escape');
    expect(store.useAudioStore.getState().selection).toBeNull();
    expect(requestCloseEditor).not.toHaveBeenCalled();
    press('Escape');
    expect(requestCloseEditor).toHaveBeenCalled();
  });

  it('⌘0・⌘+・⌘− で全体・拡大・縮小、⌘A ですべてを選択（メニューの項目も種類へ回す）', () => {
    press('0', { metaKey: true });
    press('=', { metaKey: true });
    press('-', { metaKey: true });
    expect(store.audioFit).toHaveBeenCalled();
    expect(vi.mocked(store.audioZoom).mock.calls.map((c) => c[0])).toEqual([1, -1]);
    expect(press('a', { metaKey: true })).toBe(true);
    expect(store.selectAllAudio).toHaveBeenCalledTimes(1);
    expect(editorSelectAll()).toBe(true);
    expect(store.selectAllAudio).toHaveBeenCalledTimes(2);
    // 画像のエディタ・閉じているときは種類が受けない
    useEditorStore.setState({ kind: 'image' });
    expect(editorSelectAll()).toBe(false);
    useEditorStore.setState({ phase: 'closed', kind: null });
    expect(editorSelectAll()).toBe(false);
  });

  it('文字を打つ欄の上では受けない', () => {
    const input = document.createElement('input');
    document.body.append(input);
    const e = new KeyboardEvent('keydown', { key: ' ', bubbles: true });
    input.dispatchEvent(e);
    expect(handleEditorKey(e)).toBe(false);
    input.remove();
  });
});
