import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { EDIT_OPS } from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import { Editor } from './Editor';
import { useEditorStore } from './editorStore';
import { DEFAULT_TOOLS, useToolStore } from './toolStore';

vi.mock('../library/inspector/ItemInspector', () => ({
  ItemInspector: ({ id }: { id: number }) => <p>インスペクタ {id}</p>,
}));

const t = ja.editor;

const ITEM = {
  id: 7,
  hash: 'ab'.repeat(32),
  ext: 'png',
  kind: 'image',
  width: 64,
  height: 32,
  title: 'hero.png',
  thumbState: 'ready',
  thumbRev: 0,
  alpha: 'binary',
  pixelArt: true,
} as ItemSummaryDto;

function openState() {
  useEditorStore.setState({
    phase: 'open',
    item: ITEM,
    session: 'aa01',
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png' },
    status: {
      rev: 2,
      head: 2,
      provisional: false,
      canUndo: true,
      canRedo: false,
      dirty: true,
      saving: null,
      needsReopen: false,
    },
    doc: {
      order: [3],
      nodes: {
        3: {
          id: 3,
          op: 'paint.actions',
          v: 1,
          enabled: true,
          params: { actions: [] },
          log: { field: 'actions', count: 4 },
        },
      },
    },
    view: {
      rev: 2,
      level: 0,
      fullWidth: 64,
      fullHeight: 32,
      frames: [
        { index: 0, x: 0, y: 0, w: 64, h: 32, durationMs: 100 },
        { index: 1, x: 0, y: 0, w: 64, h: 32, durationMs: 100 },
      ],
      format: 'rgba8',
      fallback: null,
    },
    frame: 0,
    failure: null,
    panel: 'ops',
    zoom: 2,
  });
}

describe('画像エディタの画面', () => {
  beforeEach(() => {
    useToolStore.setState({ ...DEFAULT_TOOLS });
  });

  afterEach(() => {
    useEditorStore.setState({ phase: 'closed', item: null, failure: null, panel: 'ops' });
  });

  it('閉じているときは何も出さない', () => {
    const { container } = render(<Editor />);
    expect(container).toBeEmptyDOMElement();
  });

  it('上部に名前・「64 × 64 · PNG · 未保存」・道具、下に道具の設定、右下に倍率、右に操作の並び', () => {
    openState();
    const { container } = render(<Editor />);
    expect(container.querySelector('[data-editor]')).not.toBeNull();
    expect(screen.getByText('hero.png')).toBeInTheDocument();
    expect(screen.getByText(`${t.size('64', '32')} · PNG · ${t.unsaved}`)).toBeInTheDocument();
    const pen = screen.getByRole('button', { name: t.withKey(t.toolNames.pen, 'B') });
    expect(pen).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(screen.getByRole('button', { name: t.withKey(t.toolNames.fill, 'G') }));
    expect(useToolStore.getState().tool).toBe('fill');
    expect(screen.getByLabelText(t.tolerance)).toBeInTheDocument();
    expect(screen.getByText('200%')).toBeInTheDocument();
    expect(
      screen.getByText(t.opCount(t.ops['paint.actions']?.name ?? '', '4')),
    ).toBeInTheDocument();
    // アニメの画像はコマを選べる
    expect(screen.getByText(t.framePosition('1', '2'))).toBeInTheDocument();
  });

  it('「情報」で右パネルをインスペクタに替える', () => {
    openState();
    render(<Editor />);
    fireEvent.click(screen.getByRole('button', { name: t.info }));
    expect(screen.getByText('インスペクタ 7')).toBeInTheDocument();
  });

  it('透明を持てない形式では消しゴムを押せない', () => {
    openState();
    const view = useEditorStore.getState().view;
    if (view) useEditorStore.setState({ view: { ...view, format: 'rgb8' } });
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.withKey(t.toolNames.eraser, 'E') })).toBeDisabled();
  });

  it('開けなかったら理由と「もう一度」「閉じる」。閉じるとエディタが消える', () => {
    useEditorStore.setState({
      phase: 'error',
      item: ITEM,
      failure: { kind: 'open', code: 'Busy', message: '混んでいます' },
    });
    render(<Editor />);
    expect(screen.getByRole('alert')).toHaveTextContent(t.openFailed);
    expect(screen.getByText('混んでいます')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.retry })).toBeInTheDocument();
    fireEvent.click(screen.getAllByRole('button', { name: t.close })[1] as HTMLElement);
    expect(useEditorStore.getState().phase).toBe('closed');
  });

  it('描けなかった表示は「取り消す」を出す', () => {
    openState();
    useEditorStore.setState({
      failure: { kind: 'view', code: 'TooLarge', message: '大きすぎます' },
    });
    render(<Editor />);
    expect(screen.getByRole('alert')).toHaveTextContent(t.viewFailed);
    expect(screen.getByRole('button', { name: t.undo })).toBeInTheDocument();
  });
});

describe('文言', () => {
  it('操作のカタログのすべての操作に名前がある', () => {
    for (const op of EDIT_OPS) {
      expect(t.ops[op.id]?.name, op.id).toBeTruthy();
    }
  });
});
