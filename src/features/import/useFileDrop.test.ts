import { beforeEach, describe, expect, it, vi } from 'vitest';

import { previewImport } from '../../ipc/import';
import { useImportStore } from '../../stores/importStore';
import { beginDragOut, resetDragOutForTest } from '../library/dragOut';
import { importRecipeAction } from '../recipes/recipeActions';
import { handleDragDrop, isRecipeFile } from './useFileDrop';

vi.mock('../../ipc/import', () => ({
  previewImport: vi.fn(() => new Promise(() => {})),
  startImport: vi.fn(),
}));
vi.mock('../recipes/recipeActions', () => ({ importRecipeAction: vi.fn(async () => undefined) }));
vi.mock('../../ipc/dragout', () => ({
  startDragOut: vi.fn(async () => ({
    started: true,
    dir: '/L.kuralib/staging/dragout/1',
    skipped: 0,
  })),
  onDragOutEnded: vi.fn(async () => () => undefined),
}));

beforeEach(() => {
  resetDragOutForTest();
  useImportStore.setState({
    dragging: false,
    phase: 'idle',
    paths: [],
    preview: null,
    error: null,
  });
});

describe('handleDragDrop', () => {
  it('Kura から出したドラッグ（一時置き場のファイル）が戻ってきても取り込まない', async () => {
    await beginDragOut([1]);
    const own = ['/L.kuralib/staging/dragout/1/hero.png'];
    handleDragDrop({ type: 'enter', paths: own });
    expect(useImportStore.getState().dragging).toBe(false);
    handleDragDrop({ type: 'drop', paths: own });
    expect(useImportStore.getState().phase).toBe('idle');
    expect(previewImport).not.toHaveBeenCalled();
    // ほかのファイルは今までどおり
    handleDragDrop({ type: 'enter', paths: ['/a.jpg'] });
    expect(useImportStore.getState().dragging).toBe(true);
  });

  it('ファイルが入ってきたら重ね表示し、離れたら消す', () => {
    handleDragDrop({ type: 'enter', paths: ['/a.jpg'] });
    expect(useImportStore.getState().dragging).toBe(true);
    handleDragDrop({ type: 'leave' });
    expect(useImportStore.getState().dragging).toBe(false);
    handleDragDrop({ type: 'enter', paths: [] });
    expect(useImportStore.getState().dragging).toBe(false);
  });

  it('ドロップで確認を始める。シートを開いている間は受け付けない', () => {
    handleDragDrop({ type: 'enter', paths: ['/src'] });
    handleDragDrop({ type: 'drop', paths: ['/src'] });
    expect(useImportStore.getState()).toMatchObject({
      dragging: false,
      phase: 'checking',
      paths: ['/src'],
    });
    expect(previewImport).toHaveBeenCalledWith(['/src']);

    handleDragDrop({ type: 'enter', paths: ['/other'] });
    expect(useImportStore.getState().dragging).toBe(false);
    handleDragDrop({ type: 'drop', paths: ['/other'] });
    expect(useImportStore.getState().paths).toEqual(['/src']);
  });

  it('レシピのファイル（.kurarecipe）は読み込み、ほかは取り込みのシートへ（M14 段 7）', async () => {
    expect(isRecipeFile('/a/b.KURARECIPE')).toBe(true);
    expect(isRecipeFile('/a/b.png')).toBe(false);
    handleDragDrop({ type: 'drop', paths: ['/r.kurarecipe'] });
    await Promise.resolve();
    expect(importRecipeAction).toHaveBeenCalledWith('/r.kurarecipe');
    expect(useImportStore.getState().phase).toBe('idle');
    handleDragDrop({ type: 'drop', paths: ['/x.kurarecipe', '/a.png'] });
    expect(useImportStore.getState().paths).toEqual(['/a.png']);
  });
});
