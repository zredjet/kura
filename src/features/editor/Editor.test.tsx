import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { type MenuAction, type MenuEntry, useMenuStore } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { EDIT_OPS, type IssueCode } from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import { cancelJob, pauseJob, resumeJob } from '../../ipc/jobs';
import { useJobsStore } from '../../stores/jobsStore';
import { Editor } from './Editor';
import { useEditorStore } from './editorStore';
import { useImageStore } from './image/imageStore';
import { DEFAULT_TOOLS, useToolStore } from './image/toolStore';

vi.mock('../library/inspector/ItemInspector', () => ({
  ItemInspector: ({ id }: { id: number }) => <p>インスペクタ {id}</p>,
}));

vi.mock('../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/edit')>()),
  listPendingSaves: vi.fn(() => new Promise(() => undefined)),
}));

vi.mock('../../ipc/jobs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/jobs')>()),
  pauseJob: vi.fn(async () => null),
  resumeJob: vi.fn(async () => null),
  cancelJob: vi.fn(async () => null),
}));

vi.mock('../../ipc/items', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/items')>()),
  getItemDetail: vi.fn(async () => ({ originalName: 'hero.png' })),
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
    kind: 'image',
    item: ITEM,
    session: 'aa01',
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png', audio: null },
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
      outputs: [],
    },
    failure: null,
    panel: 'ops',
  });
  useImageStore.setState({
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
    // 右パネルの操作の並び：名前・要約（描画は本数）・有効のスイッチ
    expect(screen.getByRole('button', { name: t.selectOp('描画') })).toHaveTextContent(
      `描画${t.strokeCount('4')}`,
    );
    expect(screen.getByRole('switch', { name: t.enableOp('描画') })).toBeChecked();
    // 上部の右に主ボタン「保存」（保存していない変更があるので押せる）と ▾
    expect(screen.getByRole('button', { name: t.save })).toBeEnabled();
    expect(screen.getByRole('button', { name: t.saveMore })).toBeInTheDocument();
    // アニメの画像はコマを選べる
    expect(screen.getByText(t.framePosition('1', '2'))).toBeInTheDocument();
  });

  it('筆の大きさの欄を空にしても 1 に戻さず、範囲の外は離したときに収める', () => {
    openState();
    useToolStore.setState({ size: 16 });
    render(<Editor />);
    const size = screen.getByRole('spinbutton', { name: t.brushSize });
    fireEvent.change(size, { target: { value: '' } });
    expect(size).toHaveValue(null);
    expect(useToolStore.getState().size).toBe(16);
    fireEvent.change(size, { target: { value: '8' } });
    expect(size).toHaveValue(8);
    expect(useToolStore.getState().size).toBe(8);
    // 範囲の外は打っている間は送らず、離したら収める
    fireEvent.change(size, { target: { value: '999' } });
    expect(useToolStore.getState().size).toBe(8);
    fireEvent.blur(size);
    expect(useToolStore.getState().size).toBe(255);
    expect(size).toHaveValue(255);
  });

  it('「保存」は保存していない変更がないときは押せない', () => {
    openState();
    const status = useEditorStore.getState().status;
    if (status) useEditorStore.setState({ status: { ...status, dirty: false } });
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.save })).toBeDisabled();
    // 応えの来ていない手があれば押せる（未保存に数える）
    act(() => useEditorStore.setState({ pendingSteps: true }));
    expect(screen.getByRole('button', { name: t.save })).toBeEnabled();
    act(() => useEditorStore.setState({ pendingSteps: false }));
  });

  it('「情報」で右パネルをインスペクタに替える', () => {
    openState();
    render(<Editor />);
    fireEvent.click(screen.getByRole('button', { name: t.info }));
    expect(screen.getByText('インスペクタ 7')).toBeInTheDocument();
  });

  it('透明を持てない形式では消しゴムを押せない', () => {
    openState();
    const view = useImageStore.getState().view;
    if (view) useImageStore.setState({ view: { ...view, format: 'rgb8' } });
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.withKey(t.toolNames.eraser, 'E') })).toBeDisabled();
  });

  it('開けなかったら理由と「もう一度」「閉じる」。閉じるとエディタが消える', () => {
    useEditorStore.setState({
      phase: 'error',
      kind: 'image',
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

describe('保存の一時停止・再開・中止（▾ とボタン）', () => {
  const SAVE = {
    job: 21,
    mode: 'overwrite' as const,
    rev: 2,
    state: 'running' as const,
    progress: 0.42,
    slow: false,
    title: null,
    before: null,
    after: null,
  };

  /** ▾ を押して、開いたメニューの項目。 */
  function openMore(): MenuEntry[] {
    fireEvent.click(screen.getByRole('button', { name: t.saveMore }));
    return useMenuStore.getState().menu?.entries ?? [];
  }
  /** 項目の名前（区切りは「-」）。 */
  const labels = (entries: MenuEntry[]) => entries.map((e) => (e === 'separator' ? '-' : e.label));
  function action(entries: MenuEntry[], label: string): MenuAction {
    const found = entries.find((e) => e !== 'separator' && e.label === label);
    if (!found || found === 'separator' || 'submenu' in found) throw new Error(label);
    return found;
  }

  beforeEach(() => {
    vi.mocked(pauseJob).mockClear();
    vi.mocked(resumeJob).mockClear();
    vi.mocked(cancelJob).mockClear();
    useJobsStore.setState({ saves: {}, endedSaves: {} });
    useToolStore.setState({ ...DEFAULT_TOOLS });
  });

  afterEach(() => {
    useMenuStore.setState({ menu: null });
    useJobsStore.setState({ saves: {}, endedSaves: {} });
    useEditorStore.setState({ phase: 'closed', item: null, save: null, sheet: null });
  });

  it('保存していないときは今までの項目だけで、ボタンは「保存」', () => {
    openState();
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.save })).toBeEnabled();
    expect(labels(openMore())).toEqual([t.saveAs, '-', t.revert, t.versions]);
  });

  it('保存の間：ボタンは「保存中 42%」、▾ は「保存を一時停止」「保存を中止」（別名・元に戻すは押せない）', () => {
    openState();
    useEditorStore.setState({ save: SAVE });
    render(<Editor />);
    const button = screen.getByRole('button', { name: t.savingPercent('42') });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute('aria-busy', 'true');
    const entries = openMore();
    expect(labels(entries)).toEqual([
      t.savePause,
      t.saveCancel,
      '-',
      t.saveAs,
      '-',
      t.revert,
      t.versions,
    ]);
    expect(action(entries, t.saveAs).disabled).toBe(true);
    expect(action(entries, t.revert).disabled).toBe(true);
    expect(action(entries, t.versions).disabled).toBeFalsy();
    action(entries, t.savePause).onSelect();
    expect(pauseJob).toHaveBeenCalledWith(21);
    action(entries, t.saveCancel).onSelect();
    expect(cancelJob).toHaveBeenCalledWith(21);
  });

  it('一時停止の間：ボタンは「一時停止中 42%」、▾ は「保存を再開」「保存を中止」', () => {
    openState();
    useEditorStore.setState({ save: { ...SAVE, state: 'paused' } });
    render(<Editor />);
    const button = screen.getByRole('button', { name: t.savePausedPercent('42') });
    expect(button).toBeDisabled();
    expect(button).not.toHaveAttribute('aria-busy');
    const entries = openMore();
    expect(labels(entries)).toEqual([
      t.saveResume,
      t.saveCancel,
      '-',
      t.saveAs,
      '-',
      t.revert,
      t.versions,
    ]);
    action(entries, t.saveResume).onSelect();
    expect(resumeJob).toHaveBeenCalledWith(21);
    expect(pauseJob).not.toHaveBeenCalled();
    action(entries, t.saveCancel).onSelect();
    expect(cancelJob).toHaveBeenCalledWith(21);
  });

  it('進捗の分からない一時停止は「一時停止中」、再開したあと（頭から）は「保存中…」', () => {
    openState();
    useEditorStore.setState({ save: { ...SAVE, state: 'paused', progress: null } });
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.savePaused })).toBeDisabled();
    act(() => useEditorStore.setState({ save: { ...SAVE, state: 'queued', progress: null } }));
    expect(screen.getByRole('button', { name: t.saving })).toBeDisabled();
  });

  it('前の保存が終わっていない間（waiting）：シートだけで、上部の保存と ▾ は押せず、キーも受けない', () => {
    useJobsStore.getState().trackSave({
      id: 30,
      itemId: ITEM.id,
      itemTitle: 'hero.png',
      mode: 'overwrite',
      newTitle: null,
      state: 'paused',
      progress: 0.42,
      message: null,
    });
    useEditorStore.setState({
      phase: 'waiting',
      kind: 'image',
      item: ITEM,
      session: null,
      status: null,
      sheet: { kind: 'pendingSave' },
    });
    render(<Editor />);
    expect(screen.getByRole('dialog', { name: t.pendingSaveTitle })).toBeInTheDocument();
    expect(screen.queryByRole('status', { name: t.opening })).toBeNull();
    expect(screen.queryByText(t.opening)).toBeNull();
    expect(screen.getByRole('button', { name: t.save, hidden: true })).toBeDisabled();
    expect(screen.getByRole('button', { name: t.saveMore, hidden: true })).toBeDisabled();
    fireEvent.keyDown(window, { key: 's', code: 'KeyS', metaKey: true });
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'pendingSave' });
    expect(useMenuStore.getState().menu).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: t.pendingSaveResume }));
    expect(resumeJob).toHaveBeenCalledWith(30);
    expect(pauseJob).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: t.pendingSaveCancel }));
    expect(cancelJob).toHaveBeenCalledWith(30);
    act(() => useEditorStore.setState({ phase: 'closed', item: null, sheet: null }));
  });

  it('ほかのアイテムの保存はこのエディタに出さない', () => {
    useJobsStore.getState().trackSave({
      id: 30,
      itemId: ITEM.id + 1,
      itemTitle: 'other',
      mode: 'overwrite',
      newTitle: null,
      state: 'paused',
      progress: 0.42,
      message: null,
    });
    openState();
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.save })).toBeEnabled();
  });

  it('開けなかったときは ▾ を押せない', () => {
    useEditorStore.setState({
      phase: 'error',
      kind: 'image',
      item: ITEM,
      failure: { kind: 'open', code: 'Busy', message: '混んでいます' },
    });
    render(<Editor />);
    expect(screen.getByRole('button', { name: t.saveMore })).toBeDisabled();
  });
});

describe('文言', () => {
  it('操作のカタログのすべての操作に名前がある', () => {
    for (const op of EDIT_OPS) {
      expect(t.ops[op.id]?.name, op.id).toBeTruthy();
    }
  });

  it('カタログのすべての引数に文言があり、列挙の値にも名前がある', () => {
    for (const op of EDIT_OPS) {
      for (const p of op.schema.params) {
        expect(t.ops[op.id]?.params[p.key], `${op.id}.${p.key}`).toBeTruthy();
        if (p.kind.type === 'enum') {
          for (const v of p.kind.values) {
            expect(t.ops[op.id]?.values?.[p.key]?.[v], `${op.id}.${p.key}=${v}`).toBeTruthy();
          }
        }
      }
    }
  });

  it('カタログの単位・指摘の種類に文言がある', () => {
    for (const op of EDIT_OPS) {
      for (const p of op.schema.params) {
        expect(p.unit in t.units, `${op.id}.${p.key}: ${p.unit}`).toBe(true);
      }
    }
    // 指摘の種類をすべて並べる（種類が増えたら型の誤りになる）
    const codes: Record<IssueCode, true> = {
      badGraph: true,
      unknownOp: true,
      unknownVersion: true,
      schemaTooNew: true,
      badParams: true,
      outOfRange: true,
      badInput: true,
      tooLarge: true,
      metadataDropped: true,
      colorSpaceAssumed: true,
      loudnessUnmeasurable: true,
      gainLimited: true,
    };
    for (const code of Object.keys(codes)) {
      expect(t.issues[code], code).toBeTruthy();
    }
  });
});
