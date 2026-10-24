import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  closeEdit,
  type EditStatusDto,
  type EditStepResultDto,
  type EditViewDto,
  openEdit,
  sampleEdit,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import { IpcError } from '../../ipc/result';
import { unpackPoints } from '../../lib/paint';
import { useNoticeStore } from '../../stores/noticeStore';
import {
  addOp,
  applyDocDelta,
  cancelCrop,
  closeEditor,
  confirmCrop,
  editorController,
  editorRedo,
  editorUndo,
  fillAt,
  focusNode,
  hasUnsavedChanges,
  moveSelected,
  nudgeCropRect,
  openEditor,
  removeNode,
  sampleAt,
  selectAdjacent,
  selectTool,
  sendParam,
  setCropRect,
  strokeEnd,
  strokeStart,
  useEditorStore,
} from './editorStore';
import { DEFAULT_TOOLS, useToolStore } from './toolStore';

vi.mock('../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/edit')>()),
  openEdit: vi.fn(),
  stepEdit: vi.fn(),
  viewEdit: vi.fn(),
  sampleEdit: vi.fn(),
  closeEdit: vi.fn(async () => null),
  discardEditDraft: vi.fn(async () => true),
}));

function item(over: Partial<ItemSummaryDto> = {}): ItemSummaryDto {
  return {
    id: 7,
    hash: 'ab'.repeat(32),
    ext: 'png',
    kind: 'image',
    width: 64,
    height: 32,
    durationMs: null,
    sizeBytes: 100,
    title: 'hero.png',
    rating: 0,
    sourceSite: null,
    importedAt: 0,
    capturedAt: null,
    thumbState: 'ready',
    proxyState: 'none',
    lastPlayedAt: null,
    thumbRev: 0,
    analysisState: 'done',
    alpha: 'binary',
    pixelArt: true,
    pixelArtSetting: null,
    animated: false,
    frameCount: null,
    hasLoop: false,
    ...over,
  } as ItemSummaryDto;
}

function status(over: Partial<EditStatusDto> = {}): EditStatusDto {
  return {
    rev: 1,
    head: 1,
    provisional: false,
    canUndo: false,
    canRedo: false,
    dirty: false,
    saving: null,
    needsReopen: false,
    ...over,
  };
}

function viewDto(over: Partial<EditViewDto> = {}): EditViewDto {
  return {
    rev: 1,
    level: 0,
    fallback: null,
    fullWidth: 64,
    fullHeight: 32,
    width: 64,
    height: 32,
    frames: [{ index: 0, x: 0, y: 0, w: 64, h: 32, durationMs: null }],
    format: 'rgba8',
    tile: 256,
    changed: { type: 'all' },
    metaChanged: true,
    warnings: [],
    drawnNodes: 0,
    ms: 1,
    ...over,
  };
}

function result(over: Partial<EditStepResultDto> = {}): EditStepResultDto {
  return {
    status: status(),
    created: [],
    rejected: null,
    doc: { base: null, order: [], nodes: [], outputs: null },
    view: { type: 'shown', view: viewDto() },
    notices: [],
    ...over,
  };
}

function opened(session = 'aa01', over: Partial<EditStepResultDto> = {}) {
  return {
    session,
    itemId: 7,
    baseVersion: 0,
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png' },
    pixelArt: true,
    draft: { exists: false, fresh: true, updatedAt: null, opened: false },
    result: result(over),
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  vi.mocked(openEdit).mockReset();
  vi.mocked(stepEdit).mockReset();
  vi.mocked(viewEdit).mockReset();
  vi.mocked(sampleEdit).mockReset();
  vi.mocked(closeEdit).mockClear();
  // 表示だけの要求は、今の状態のまま変わったタイルなし
  vi.mocked(viewEdit).mockImplementation(async () =>
    result({
      status: useEditorStore.getState().status ?? status(),
      doc: { base: 1, order: useEditorStore.getState().doc.order, nodes: [], outputs: null },
      view: { type: 'shown', view: viewDto({ changed: { type: 'tiles', frames: [] } }) },
    }),
  );
  vi.mocked(stepEdit).mockImplementation(async () =>
    result({
      view: { type: 'shown', view: viewDto({ rev: 2, changed: { type: 'tiles', frames: [] } }) },
    }),
  );
  useToolStore.setState({ ...DEFAULT_TOOLS });
  useNoticeStore.setState({ message: null });
});

afterEach(() => {
  closeEditor();
});

describe('開く・閉じる', () => {
  it('画像を開くと、窓から見積もった表示の要求で edit_open を呼び、状態・文書・表示を持つ', async () => {
    vi.mocked(openEdit).mockResolvedValue(
      opened('aa01', {
        status: status({ rev: 3, canUndo: true }),
        doc: {
          base: null,
          order: [2],
          nodes: [
            {
              id: 2,
              op: 'paint.actions',
              v: 1,
              enabled: true,
              params: { actions: [] },
              log: { field: 'actions', count: 4 },
            },
          ],
          outputs: null,
        },
      }),
    );
    const pending = openEditor(item());
    expect(useEditorStore.getState()).toMatchObject({ phase: 'opening', item: { id: 7 } });
    await pending;
    expect(openEdit).toHaveBeenCalledWith(
      7,
      expect.objectContaining({ frame: 0, rect: null }),
      null,
    );
    const s = useEditorStore.getState();
    expect(s).toMatchObject({ phase: 'open', session: 'aa01', pixelArt: true });
    expect(s.status?.canUndo).toBe(true);
    expect(s.doc.order).toEqual([2]);
    expect(s.view).toMatchObject({ level: 0, fullWidth: 64, format: 'rgba8' });
    expect(editorController()?.focus).toBe(2);
  });

  it('画像でなければ開かない', async () => {
    await openEditor(item({ kind: 'audio' }));
    expect(openEdit).not.toHaveBeenCalled();
    expect(useEditorStore.getState().phase).toBe('closed');
  });

  it('開けなければ理由を出す（code つき）', async () => {
    vi.mocked(openEdit).mockRejectedValue(
      new IpcError({ code: 'TooLarge', message: '大きすぎます' }),
    );
    await openEditor(item());
    expect(useEditorStore.getState()).toMatchObject({
      phase: 'error',
      failure: { kind: 'open', code: 'TooLarge', message: '大きすぎます' },
    });
  });

  it('閉じると edit_close を呼び、状態を空にする', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened('aa01'));
    await openEditor(item());
    closeEditor();
    expect(closeEdit).toHaveBeenCalledWith('aa01');
    expect(useEditorStore.getState()).toMatchObject({ phase: 'closed', session: null, item: null });
    expect(editorController()).toBeNull();
  });

  it('別の画像を開くと前のセッションを閉じる。開いている途中に替えたら、遅れて届いたセッションも閉じる', async () => {
    vi.mocked(openEdit).mockResolvedValueOnce(opened('aa01'));
    await openEditor(item());
    let resolveSlow: (v: ReturnType<typeof opened>) => void = () => undefined;
    vi.mocked(openEdit).mockReturnValueOnce(new Promise((r) => (resolveSlow = r)));
    const slow = openEditor(item({ id: 8 }));
    expect(closeEdit).toHaveBeenCalledWith('aa01');
    vi.mocked(openEdit).mockResolvedValueOnce(opened('cc03'));
    await openEditor(item({ id: 9 }));
    resolveSlow(opened('bb02'));
    await slow;
    expect(closeEdit).toHaveBeenCalledWith('bb02');
    expect(useEditorStore.getState().session).toBe('cc03');
  });
});

describe('手', () => {
  beforeEach(async () => {
    vi.mocked(openEdit).mockResolvedValue(opened('aa01'));
    await openEditor(item());
    // 開いたあとの頼み直しを流してから
    await flush();
    vi.mocked(viewEdit).mockClear();
  });

  it('⌘Z・⇧⌘Z は undo・redo の手を送る（出している要求があれば、まとめて次に送る）', async () => {
    editorUndo();
    editorRedo();
    editorUndo();
    await flush();
    const steps = vi.mocked(stepEdit).mock.calls.map((c) => c[1]);
    expect(steps).toEqual([[{ type: 'undo' }], [{ type: 'redo' }, { type: 'undo' }]]);
    expect(stepEdit).toHaveBeenCalledWith(
      'aa01',
      expect.anything(),
      expect.objectContaining({ frame: 0 }),
    );
  });

  it('筆は離したら paint の 1 手で送り、色を最近の色に入れる', async () => {
    expect(strokeStart([1, 1], false)).toBe(true);
    expect(useEditorStore.getState().drawing).toBe(true);
    editorController()?.extendStroke([[4, 1]]);
    strokeEnd();
    expect(useEditorStore.getState().drawing).toBe(false);
    const [, steps] = vi.mocked(stepEdit).mock.calls[0] ?? [];
    const step = steps?.[0];
    expect(step?.type).toBe('paint');
    const action = (step as unknown as { actions: { points: string; tool: string }[] }).actions[0];
    expect(action?.tool).toBe('pen');
    expect(unpackPoints(action?.points ?? '')).toEqual([
      [1, 1],
      [4, 1],
    ]);
    expect(useToolStore.getState().recent).toEqual([DEFAULT_TOOLS.color]);
    await flush();
    expect(useEditorStore.getState().view?.rev).toBe(2);
  });

  it('透明を持てない形式では消しゴムを使わない', async () => {
    useEditorStore.setState({ view: { ...useEditorStore.getState().view!, format: 'rgb8' } });
    useToolStore.setState({ tool: 'eraser' });
    expect(strokeStart([1, 1], false)).toBe(false);
    expect(useNoticeStore.getState().message).toBeTruthy();
    expect(stepEdit).not.toHaveBeenCalled();
  });

  it('塗りつぶしは仮描きせずに送る。画像の外は何もしない', () => {
    fillAt([100, 100]);
    expect(stepEdit).not.toHaveBeenCalled();
    fillAt([2, 3]);
    const [, steps] = vi.mocked(stepEdit).mock.calls[0] ?? [];
    expect(steps?.[0]).toMatchObject({
      type: 'paint',
      actions: [{ type: 'fill', at: { x: 2, y: 3 } }],
    });
  });

  it('スポイトはクリックのときだけ edit_sample を呼び、色にする（パレットの番号も）', async () => {
    vi.mocked(sampleEdit).mockResolvedValue({ rgba: [1, 2, 3, 4], index: 9 });
    await sampleAt([5, 6]);
    expect(sampleEdit).toHaveBeenCalledWith('aa01', 0, 5, 6);
    expect(useToolStore.getState().color).toEqual({ rgba: [1, 2, 3, 4], index: 9 });
    await sampleAt([500, 6]);
    expect(sampleEdit).toHaveBeenCalledOnce();
  });

  it('断られた手は知らせ、描けなかった表示は失敗として出す（手は当たっている）', async () => {
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        status: status({ rev: 2, canUndo: true }),
        rejected: {
          index: 0,
          problem: { code: 'Invalid', message: 'この色は使えません', issues: [] },
        },
        view: {
          type: 'failed',
          problem: { code: 'TooLarge', message: 'メモリが足りません', issues: [] },
        },
      }),
    );
    editorUndo();
    await flush();
    expect(useNoticeStore.getState().message).toBe('この色は使えません');
    expect(useEditorStore.getState().failure).toEqual({
      kind: 'view',
      code: 'TooLarge',
      message: 'メモリが足りません',
    });
    expect(useEditorStore.getState().status?.canUndo).toBe(true);
  });

  it('要求そのものが通らなければ失敗として出す', async () => {
    vi.mocked(stepEdit).mockRejectedValueOnce(
      new IpcError({ code: 'NotFound', message: 'エディタがありません' }),
    );
    editorUndo();
    await flush();
    expect(useEditorStore.getState().failure).toMatchObject({ kind: 'request', code: 'NotFound' });
  });
});

describe('右パネル', () => {
  const crop = (id: number, rect = { x: 0, y: 0, w: 64, h: 32 }, enabled = true) => ({
    id,
    op: 'image.crop',
    v: 1,
    enabled,
    params: { rect },
    log: null,
  });
  const flip = (id: number) => ({
    id,
    op: 'image.flip',
    v: 1,
    enabled: true,
    params: { axis: 'horizontal' },
    log: null,
  });

  async function openWith(nodes: ReturnType<typeof crop | typeof flip>[]) {
    vi.mocked(openEdit).mockResolvedValue(
      opened('aa01', {
        doc: { base: null, order: nodes.map((n) => n.id), nodes, outputs: null },
      }),
    );
    await openEditor(item());
    await flush();
    vi.mocked(viewEdit).mockClear();
    vi.mocked(stepEdit).mockClear();
  }

  const sent = () => vi.mocked(stepEdit).mock.calls.map((c) => c[1]);

  it('↑↓ で選び、消すと次の操作を選び、⌥⌘↑↓ で並べ替える', async () => {
    await openWith([crop(1), flip(2), flip(3)]);
    selectAdjacent(1);
    expect(useEditorStore.getState().selected).toBe(1);
    selectAdjacent(1);
    selectAdjacent(1);
    selectAdjacent(1);
    expect(useEditorStore.getState().selected).toBe(3);
    selectAdjacent(-1);
    expect(useEditorStore.getState().selected).toBe(2);
    moveSelected(-1);
    removeNode(2);
    expect(useEditorStore.getState().selected).toBe(3);
    await flush();
    expect(sent()).toEqual([
      [{ type: 'commit', edit: { type: 'moveNode', node: 2, to: 0 }, mode: 'step' }],
      [{ type: 'commit', edit: { type: 'removeNode', node: 2 }, mode: 'step' }],
    ]);
  });

  it('操作を足すと選んだ操作のあとに足し、足したノードを選ぶ', async () => {
    await openWith([flip(2)]);
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        created: [5],
        doc: { base: 1, order: [2, 5], nodes: [crop(5)], outputs: null },
        view: { type: 'shown', view: viewDto({ rev: 2, changed: { type: 'tiles', frames: [] } }) },
      }),
    );
    addOp('image.crop');
    await flush();
    expect(sent()[0]).toEqual([
      {
        type: 'commit',
        edit: {
          type: 'addNode',
          at: 1,
          op: 'image.crop',
          v: 1,
          params: { rect: { x: 0, y: 0, w: 64, h: 32 } },
        },
        mode: 'step',
      },
    ]);
    expect(useEditorStore.getState().selected).toBe(5);
  });

  it('断られた手の指摘を持ち、次に手を送るまで残す', async () => {
    await openWith([crop(1)]);
    const issue = {
      node: 1,
      param: 'rect',
      code: 'outOfRange' as const,
      severity: 'error' as const,
      message: '入力の外です',
    };
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        rejected: {
          index: 0,
          problem: { code: 'Invalid', message: '入力の外です', issues: [issue] },
        },
        view: { type: 'shown', view: viewDto({ changed: { type: 'tiles', frames: [] } }) },
      }),
    );
    sendParam(1, 'rect', { kind: 'nudge', value: { x: 0, y: 0, w: 99, h: 32 } });
    await flush();
    expect(useEditorStore.getState().issues).toEqual([issue]);
    sendParam(1, 'rect', { kind: 'seal' });
    await flush();
    expect(useEditorStore.getState().issues).toEqual([]);
  });

  it('保存していない変更：保存のジョブが今の頭を保存している途中なら含めない', () => {
    const st = status({ dirty: true, head: 4 });
    const save = {
      job: 9,
      mode: 'overwrite' as const,
      rev: 4,
      state: 'running' as const,
      progress: null,
      slow: false,
      title: null,
    };
    expect(hasUnsavedChanges({ status: st, save: null })).toBe(true);
    expect(hasUnsavedChanges({ status: st, save })).toBe(false);
    expect(hasUnsavedChanges({ status: { ...st, head: 5 }, save })).toBe(true);
    expect(hasUnsavedChanges({ status: st, save: { ...save, state: 'failed' } })).toBe(true);
    expect(hasUnsavedChanges({ status: status({ dirty: false }), save: null })).toBe(false);
  });
});

describe('切り抜き', () => {
  const cropNode = {
    id: 4,
    op: 'image.crop',
    v: 1,
    enabled: true,
    params: { rect: { x: 8, y: 4, w: 16, h: 8 } },
    log: null,
  };

  it('切り抜きがなければ今の出力の全体の枠。↵ で末尾に切り抜きを足し、道具を戻す', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened('aa01'));
    await openEditor(item());
    await flush();
    vi.mocked(stepEdit).mockClear();
    useToolStore.setState({ tool: 'fill' });
    selectTool('crop');
    expect(useToolStore.getState().tool).toBe('crop');
    expect(useEditorStore.getState().crop).toMatchObject({
      node: null,
      rect: { x: 0, y: 0, w: 64, h: 32 },
      bounds: { w: 64, h: 32 },
      previewed: false,
    });
    // 全体のままなら何も足さない
    setCropRect({ x: 0, y: 0, w: 64, h: 32 });
    nudgeCropRect('ArrowRight', false);
    setCropRect({ x: 2, y: 2, w: 30, h: 20 });
    expect(nudgeCropRect('ArrowRight', true)).toBe(true);
    expect(useEditorStore.getState().crop?.rect).toEqual({ x: 12, y: 2, w: 30, h: 20 });
    confirmCrop();
    await flush();
    expect(vi.mocked(stepEdit).mock.calls[0]?.[1]).toEqual([
      {
        type: 'commit',
        edit: {
          type: 'addNode',
          at: 0,
          op: 'image.crop',
          v: 1,
          params: { rect: { x: 12, y: 2, w: 30, h: 20 } },
        },
        mode: 'step',
      },
    ]);
    expect(useEditorStore.getState().crop).toBeNull();
    expect(useToolStore.getState().tool).toBe('fill');
  });

  it('切り抜きがあれば、その操作を仮に無効にして入力を表示し、↵ で引数を替える', async () => {
    vi.mocked(openEdit).mockResolvedValue(
      opened('aa01', {
        doc: { base: null, order: [4], nodes: [cropNode], outputs: null },
        view: { type: 'shown', view: viewDto({ fullWidth: 16, fullHeight: 8 }) },
      }),
    );
    await openEditor(item());
    await flush();
    vi.mocked(stepEdit).mockClear();
    // 仮に無効にした結果（入力の大きさ 64 × 32）
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        status: status({ rev: 5, provisional: true }),
        view: { type: 'shown', view: viewDto({ rev: 5, changed: { type: 'all' } }) },
      }),
    );
    selectTool('crop');
    expect(useEditorStore.getState().crop).toMatchObject({
      node: 4,
      bounds: null,
      previewed: true,
    });
    await flush();
    expect(vi.mocked(stepEdit).mock.calls[0]?.[1]).toEqual([
      { type: 'preview', edit: { type: 'setEnabled', node: 4, enabled: false } },
    ]);
    expect(useEditorStore.getState().crop).toMatchObject({
      bounds: { w: 64, h: 32 },
      rect: { x: 8, y: 4, w: 16, h: 8 },
    });
    setCropRect({ x: 0, y: 0, w: 20, h: 10 });
    confirmCrop();
    await flush();
    expect(vi.mocked(stepEdit).mock.calls[1]?.[1]).toEqual([
      { type: 'cancelPreview' },
      {
        type: 'commit',
        edit: { type: 'setParam', node: 4, key: 'rect', value: { x: 0, y: 0, w: 20, h: 10 } },
        mode: 'step',
      },
    ]);
  });

  it('esc・ほかの道具・ほかの手で切り抜きをやめる（仮の値を捨てる）', async () => {
    vi.mocked(openEdit).mockResolvedValue(
      opened('aa01', { doc: { base: null, order: [4], nodes: [cropNode], outputs: null } }),
    );
    await openEditor(item());
    await flush();
    vi.mocked(stepEdit).mockClear();
    // 手の結果は文書を変えない（差分なし）
    vi.mocked(stepEdit).mockImplementation(async () =>
      result({
        doc: { base: 1, order: [4], nodes: [], outputs: null },
        view: { type: 'shown', view: viewDto({ changed: { type: 'tiles', frames: [] } }) },
      }),
    );
    selectTool('crop');
    await flush();
    cancelCrop();
    await flush();
    expect(vi.mocked(stepEdit).mock.calls.map((c) => c[1])).toEqual([
      [{ type: 'preview', edit: { type: 'setEnabled', node: 4, enabled: false } }],
      [{ type: 'cancelPreview' }],
    ]);
    expect(useToolStore.getState().tool).toBe('pen');
    // ⌘Z は切り抜きをやめてから
    selectTool('crop');
    await flush();
    vi.mocked(stepEdit).mockClear();
    editorUndo();
    await flush();
    await flush();
    expect(vi.mocked(stepEdit).mock.calls.flatMap((c) => c[1])).toEqual([
      { type: 'cancelPreview' },
      { type: 'undo' },
    ]);
    expect(useEditorStore.getState().crop).toBeNull();
  });
});

describe('文書の差分', () => {
  const node = (id: number, op = 'image.flip', count?: number) => ({
    id,
    op,
    v: 1,
    enabled: true,
    params: {},
    log: count === undefined ? null : { field: 'actions', count },
  });

  it('全体なら置き換え、差分なら変わったノードだけ足し、並びにないノードは消す', () => {
    const a = applyDocDelta(
      { order: [], nodes: {}, outputs: [] },
      { base: null, order: [1, 2], nodes: [node(1), node(2, 'paint.actions', 1)], outputs: null },
    );
    expect(a.order).toEqual([1, 2]);
    const b = applyDocDelta(a, {
      base: 3,
      order: [2],
      nodes: [node(2, 'paint.actions', 5)],
      outputs: null,
    });
    expect(b.order).toEqual([2]);
    expect(b.nodes[1]).toBeUndefined();
    expect(b.nodes[2]?.log?.count).toBe(5);
    // 何も変わらなければ同じもの
    expect(applyDocDelta(b, { base: 4, order: [2], nodes: [], outputs: null })).toBe(b);
  });

  it('編集中のノードは、末尾が有効な描画のときだけ', () => {
    expect(
      focusNode({
        order: [1, 2],
        nodes: { 1: node(1), 2: node(2, 'paint.actions', 1) },
        outputs: [],
      }),
    ).toBe(2);
    expect(
      focusNode({
        order: [2, 1],
        nodes: { 1: node(1), 2: node(2, 'paint.actions', 1) },
        outputs: [],
      }),
    ).toBeNull();
    expect(focusNode({ order: [], nodes: {}, outputs: [] })).toBeNull();
  });
});
