import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { openSaveAs, requestCloseEditor, requestSave } from './editorSave';
import { handleEditorKey, handleEditorKeyUp } from './editorKeys';
import {
  cancelCrop,
  confirmCrop,
  editorActualSize,
  editorFit,
  editorRedo,
  editorUndo,
  editorZoom,
  moveSelected,
  nudgeCropRect,
  removeSelected,
  selectAdjacent,
  selectTool,
  stepEditorFrame,
  strokeCancel,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';
import { DEFAULT_TOOLS, type EditorTool, useToolStore } from './toolStore';

vi.mock('./editorSave', () => ({
  openSaveAs: vi.fn(),
  requestCloseEditor: vi.fn(async () => true),
  requestSave: vi.fn(),
}));

vi.mock('./editorStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./editorStore')>()),
  cancelCrop: vi.fn(),
  confirmCrop: vi.fn(),
  moveSelected: vi.fn(),
  nudgeCropRect: vi.fn(() => true),
  removeSelected: vi.fn(),
  selectAdjacent: vi.fn(),
  selectTool: vi.fn((tool: EditorTool) => useToolStore.getState().setTool(tool)),
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
    useEditorStore.setState({ phase: 'closed', crop: null });
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

  it('esc は描いている途中なら取り消し、そうでなければ閉じる（未保存ならシート：requestCloseEditor）', () => {
    vi.mocked(strokeCancel).mockReturnValueOnce(true);
    press('Escape');
    expect(requestCloseEditor).not.toHaveBeenCalled();
    press('Escape');
    expect(requestCloseEditor).toHaveBeenCalledOnce();
  });

  it('C で切り抜き。切り抜きの間は矢印で枠を動かし（⇧ で 10px）、↵ で切り抜き、esc でやめる', () => {
    press('c');
    expect(selectTool).toHaveBeenCalledWith('crop');
    useEditorStore.setState({
      crop: {
        node: null,
        rect: { x: 0, y: 0, w: 4, h: 4 },
        bounds: { w: 8, h: 8 },
        previewed: false,
        returnTool: 'pen',
      },
    });
    press('ArrowLeft');
    press('ArrowDown', { shiftKey: true });
    expect(vi.mocked(nudgeCropRect).mock.calls).toEqual([
      ['ArrowLeft', false],
      ['ArrowDown', true],
    ]);
    // 切り抜きの間は ← → でコマを替えない・↑↓ で操作を選ばない
    expect(stepEditorFrame).not.toHaveBeenCalled();
    expect(selectAdjacent).not.toHaveBeenCalled();
    press('Enter');
    expect(confirmCrop).toHaveBeenCalledOnce();
    press('Escape');
    expect(cancelCrop).toHaveBeenCalledOnce();
    expect(requestCloseEditor).not.toHaveBeenCalled();
  });

  it('↑↓ で操作を選び、⌫ で消し、⌥⌘↑↓ で並べ替える', () => {
    press('ArrowUp');
    press('ArrowDown');
    expect(vi.mocked(selectAdjacent).mock.calls).toEqual([[-1], [1]]);
    press('Backspace');
    press('Delete');
    expect(removeSelected).toHaveBeenCalledTimes(2);
    press('ArrowUp', { metaKey: true, altKey: true });
    press('ArrowDown', { metaKey: true, altKey: true });
    expect(vi.mocked(moveSelected).mock.calls).toEqual([[-1], [1]]);
  });

  it('⌘S で保存、⌘⇧S で別名で保存', () => {
    expect(press('s', { metaKey: true, code: 'KeyS' } as KeyboardEventInit)).toBe(true);
    expect(requestSave).toHaveBeenCalledOnce();
    press('S', { metaKey: true, shiftKey: true, code: 'KeyS' } as KeyboardEventInit);
    expect(openSaveAs).toHaveBeenCalledOnce();
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
