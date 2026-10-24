import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { handleEditorKey, handleEditorKeyUp } from './editorKeys';
import {
  closeEditor,
  editorActualSize,
  editorFit,
  editorRedo,
  editorUndo,
  editorZoom,
  stepEditorFrame,
  strokeCancel,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';
import { DEFAULT_TOOLS, useToolStore } from './toolStore';

vi.mock('./editorStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./editorStore')>()),
  closeEditor: vi.fn(),
  editorActualSize: vi.fn(),
  editorFit: vi.fn(),
  editorRedo: vi.fn(),
  editorUndo: vi.fn(),
  editorZoom: vi.fn(),
  stepEditorFrame: vi.fn(),
  strokeCancel: vi.fn(() => false),
  toggleEditorInfo: vi.fn(),
}));

function press(key: string, init: KeyboardEventInit = {}, target?: EventTarget): boolean {
  const e = new KeyboardEvent('keydown', { key, cancelable: true, ...init });
  if (target) Object.defineProperty(e, 'target', { value: target });
  return handleEditorKey(e);
}

describe('エディタのキー', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useEditorStore.setState({ phase: 'open', temporary: null, view: null });
    useToolStore.setState({ ...DEFAULT_TOOLS });
  });

  afterEach(() => {
    document.body.innerHTML = '';
    useEditorStore.setState({ phase: 'closed' });
  });

  it('B・E・G・I・H で道具を替える', () => {
    for (const [key, tool] of [
      ['g', 'fill'],
      ['i', 'eyedropper'],
      ['h', 'hand'],
      ['e', 'eraser'],
      ['b', 'pen'],
    ] as const) {
      expect(press(key)).toBe(true);
      expect(useToolStore.getState().tool).toBe(tool);
    }
  });

  it('透明を持てない形式では E で消しゴムにしない', () => {
    useEditorStore.setState({
      view: {
        rev: 1,
        level: 0,
        fullWidth: 1,
        fullHeight: 1,
        frames: [],
        format: 'rgb8',
        fallback: null,
      },
    });
    expect(press('e')).toBe(true);
    expect(useToolStore.getState().tool).toBe('pen');
  });

  it('[ ] で筆の大きさ（⇧ で 10 ずつ。1〜255）', () => {
    press(']');
    press(']', { shiftKey: true });
    expect(useToolStore.getState().size).toBe(12);
    press('[', { shiftKey: true });
    press('[', { shiftKey: true });
    expect(useToolStore.getState().size).toBe(1);
  });

  it('⌘Z・⇧⌘Z・⌘0・⌘1・⌘+・⌘−・⌘I', () => {
    press('z', { metaKey: true, code: 'KeyZ' } as KeyboardEventInit);
    press('Z', { metaKey: true, shiftKey: true, code: 'KeyZ' } as KeyboardEventInit);
    press('0', { metaKey: true });
    press('1', { metaKey: true });
    press('=', { metaKey: true });
    press('-', { metaKey: true });
    press('i', { metaKey: true });
    expect(editorUndo).toHaveBeenCalledOnce();
    expect(editorRedo).toHaveBeenCalledOnce();
    expect(editorFit).toHaveBeenCalledOnce();
    expect(editorActualSize).toHaveBeenCalledOnce();
    expect(vi.mocked(editorZoom).mock.calls).toEqual([[1], [-1]]);
    expect(toggleEditorInfo).toHaveBeenCalledOnce();
    // ほかの ⌘ のキーは奪わない
    expect(press('w', { metaKey: true })).toBe(false);
  });

  it('esc は描いている途中なら取り消し、そうでなければ閉じる', () => {
    vi.mocked(strokeCancel).mockReturnValueOnce(true);
    press('Escape');
    expect(closeEditor).not.toHaveBeenCalled();
    press('Escape');
    expect(closeEditor).toHaveBeenCalledOnce();
  });

  it('← → でコマを替える', () => {
    press('ArrowLeft');
    press('ArrowRight');
    expect(vi.mocked(stepEditorFrame).mock.calls).toEqual([[-1], [1]]);
  });

  it('Space・⌥ は押している間だけ手のひら・スポイト（離したら戻す）', () => {
    press(' ');
    expect(useEditorStore.getState().temporary).toBe('hand');
    handleEditorKeyUp(new KeyboardEvent('keyup', { key: ' ' }));
    expect(useEditorStore.getState().temporary).toBeNull();
    press('Alt', { altKey: true });
    expect(useEditorStore.getState().temporary).toBe('eyedropper');
    handleEditorKeyUp(new KeyboardEvent('keyup', { key: 'Alt' }));
    expect(useEditorStore.getState().temporary).toBeNull();
    // 手のひらのときは ⌥ でスポイトにしない
    useToolStore.setState({ tool: 'hand' });
    press('Alt', { altKey: true });
    expect(useEditorStore.getState().temporary).toBeNull();
  });

  it('入力欄・日本語入力の変換中・シートやメニューの上・閉じているときは受けない', () => {
    const input = document.createElement('input');
    expect(press('b', {}, input)).toBe(false);
    expect(press('Escape', { isComposing: true })).toBe(false);
    const sheet = document.createElement('div');
    sheet.setAttribute('data-sheet', '');
    document.body.append(sheet);
    expect(press('Escape')).toBe(false);
    sheet.remove();
    useEditorStore.setState({ phase: 'closed' });
    expect(press('b')).toBe(false);
  });
});
