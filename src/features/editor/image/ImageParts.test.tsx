import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import type { EditNodeDto } from '../../../ipc/edit';
import type { ItemSummaryDto } from '../../../ipc/items';
import { sendParam, useEditorStore } from '../editorStore';
import { ImageOps } from './ImageParts';
import { useImageStore } from './imageStore';

vi.mock('../editorStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../editorStore')>()),
  sendParam: vi.fn(),
}));

vi.mock('../../../ipc/items', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../ipc/items')>()),
  getItemDetail: vi.fn(async () => ({ originalName: 'hero.png' })),
}));

const t = ja.editor;

afterEach(() => {
  useEditorStore.setState({ phase: 'closed', kind: null, item: null, selected: null });
  useImageStore.setState({ view: null });
});

describe('画像の右パネル（ImageOps）', () => {
  it('引数の欄の既定値は今の出力の大きさ（点を「指定する」にすると真ん中）', () => {
    const pivot = {
      id: 4,
      op: 'meta.set_pivot',
      v: 1,
      enabled: true,
      params: { pivot: null },
      log: null,
    } as EditNodeDto;
    useEditorStore.setState({
      phase: 'open',
      kind: 'image',
      item: { id: 7, title: 'hero', ext: 'png', kind: 'image' } as ItemSummaryDto,
      doc: { order: [4], nodes: { 4: pivot }, outputs: [] },
      selected: 4,
      issues: [],
      warnings: [],
    });
    useImageStore.setState({
      view: {
        rev: 1,
        level: 0,
        fullWidth: 64,
        fullHeight: 32,
        frames: [],
        format: 'rgba8',
        fallback: null,
      },
    });
    render(<ImageOps />);
    fireEvent.click(screen.getByRole('switch', { name: t.specifyParam('ピボット') }));
    expect(sendParam).toHaveBeenLastCalledWith(4, 'pivot', {
      kind: 'set',
      value: { x: 32, y: 16 },
    });
    // 出力の行は画像の形式の表から組む
    expect(screen.getByRole('combobox', { name: t.output })).toHaveValue('keep');
  });
});
