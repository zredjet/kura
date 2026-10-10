import { beforeEach, describe, expect, it, vi } from 'vitest';

import { previewImport } from '../../ipc/import';
import { useImportStore } from '../../stores/importStore';
import { handleDragDrop } from './useFileDrop';

vi.mock('../../ipc/import', () => ({
  previewImport: vi.fn(() => new Promise(() => {})),
  startImport: vi.fn(),
}));

beforeEach(() => {
  useImportStore.setState({
    dragging: false,
    phase: 'idle',
    paths: [],
    preview: null,
    error: null,
  });
});

describe('handleDragDrop', () => {
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
});
