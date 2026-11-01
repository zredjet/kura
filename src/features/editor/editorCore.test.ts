/**
 * エディタの核を偽のアダプタで試す（M13 の段 6a。画像の部分を通さずに、核の口だけで開く・手・結果・閉じるが回ることの
 * 裏付け。音声のアダプタもこの口で差し込む）。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  cancelOpenEdit,
  closeEdit,
  type EditStatusDto,
  type EditStepResultDto,
  type EditViewDto,
  type EditViewportDto,
  openEdit,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import { IpcError } from '../../ipc/result';
import { useNoticeStore } from '../../stores/noticeStore';
import {
  addOp,
  closeEditor,
  type EditorAdapter,
  type EditorAdapterContext,
  type EditorAdapterFactory,
  editorRedo,
  editorUndo,
  openEditorAs,
  refreshEditorView,
  reopenEditorWith,
  setNodeEnabled,
  useEditorStore,
} from './editorStore';

vi.mock('../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/edit')>()),
  openEdit: vi.fn(),
  stepEdit: vi.fn(),
  viewEdit: vi.fn(),
  closeEdit: vi.fn(async () => null),
  cancelOpenEdit: vi.fn(async () => null),
  discardEditDraft: vi.fn(async () => true),
}));

/** 種類によらない核の試し：画像でないアイテム（音声）でも、アダプタの口だけで回る。 */
const AUDIO = {
  id: 9,
  hash: 'cd'.repeat(32),
  ext: 'wav',
  kind: 'audio',
  width: null,
  height: null,
  durationMs: 3000,
  title: 'loop.wav',
  pixelArt: false,
} as unknown as ItemSummaryDto;

function viewport(scale: number): EditViewportDto {
  return { type: 'image', frame: 0, rect: null, scale, interacting: false, focus: null };
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

function viewDto(rev = 1): EditViewDto {
  return {
    type: 'image',
    rev,
    level: 0,
    fallback: null,
    fullWidth: 1,
    fullHeight: 1,
    width: 1,
    height: 1,
    frames: [],
    format: null,
    tile: 256,
    changed: { type: 'all' },
    metaChanged: false,
    warnings: [],
    drawnNodes: 0,
    ms: 1,
  };
}

function result(over: Partial<EditStepResultDto> = {}): EditStepResultDto {
  return {
    status: status(),
    created: [],
    rejected: null,
    doc: { base: 1, order: useEditorStore.getState().doc.order, nodes: [], outputs: null },
    view: { type: 'shown', view: viewDto() },
    notices: [],
    ...over,
  };
}

function node(id: number) {
  return { id, op: 'audio.gain', v: 1, enabled: true, params: { db: 0 }, log: null };
}

function opened(session: string, over: Partial<EditStepResultDto> = {}) {
  return {
    session,
    itemId: AUDIO.id,
    baseVersion: 0,
    source: { kind: 'audio', width: 0, height: 0, frames: null, format: 'wav' },
    originalFormat: 'wav',
    pixelArt: false,
    draft: { exists: false, fresh: true, updatedAt: null, opened: false },
    result: result({
      doc: { base: null, order: [3], nodes: [node(3)], outputs: null },
      ...over,
    }),
  } as unknown as Awaited<ReturnType<typeof openEdit>>;
}

/** 偽のアダプタ（呼ばれた口を順に書き残す）。 */
interface FakeAdapter extends EditorAdapter<string> {
  log: string[];
  ctx: EditorAdapterContext<string>;
}

function fakeKind(over: Partial<EditorAdapter<string>> = {}) {
  const made: FakeAdapter[] = [];
  const factory: EditorAdapterFactory<string> = {
    kind: 'image',
    create(_item, ctx) {
      const log: string[] = [];
      const adapter: FakeAdapter = {
        log,
        ctx,
        begin: () => void log.push('begin'),
        initialViewport: () => {
          log.push('initialViewport');
          return viewport(0.5);
        },
        opened: (o) => void log.push(`opened:${o.session}`),
        viewport: () => viewport(1),
        sent: (req) =>
          void log.push(`sent:${req.seq}:${req.steps.map((s) => s.tag ?? '-').join(',')}`),
        onRejected: (seq, index) => void log.push(`rejected:${seq}:${index}`),
        applyView: (view, info) =>
          void log.push(
            `view:${info.seq}:${view.rev}:${info.provisional ? 'provisional' : 'fixed'}`,
          ),
        onFailed: (seq) => void log.push(`failed:${seq}`),
        settle: (doc) => void log.push(`settle:${doc.order.join(',')}`),
        beforeSteps: () => void log.push('beforeSteps'),
        dispose: (closing) => void log.push(`dispose:${closing ? 'close' : 'replace'}`),
        ...over,
      };
      made.push(adapter);
      return adapter;
    },
  };
  return { factory, made };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

/** 開いて、開いたあとの頼み直しを流し、書き残しを空にする。 */
async function openFake(over: Partial<EditorAdapter<string>> = {}): Promise<FakeAdapter> {
  const { factory, made } = fakeKind(over);
  vi.mocked(openEdit).mockResolvedValueOnce(opened('aa01'));
  await openEditorAs(factory, AUDIO);
  await flush();
  const adapter = made[0];
  if (!adapter) throw new Error('アダプタを作っていない');
  adapter.log.length = 0;
  vi.mocked(viewEdit).mockClear();
  vi.mocked(stepEdit).mockClear();
  return adapter;
}

beforeEach(() => {
  vi.mocked(openEdit).mockReset();
  vi.mocked(stepEdit).mockReset();
  vi.mocked(viewEdit).mockReset();
  vi.mocked(closeEdit).mockClear();
  vi.mocked(cancelOpenEdit).mockReset();
  vi.mocked(cancelOpenEdit).mockImplementation(async () => null);
  vi.mocked(viewEdit).mockImplementation(async () => result());
  vi.mocked(stepEdit).mockImplementation(async () =>
    result({ view: { type: 'shown', view: viewDto(2) } }),
  );
  useNoticeStore.setState({ message: null });
});

afterEach(() => {
  closeEditor();
});

describe('核と種類ごとのアダプタ', () => {
  it('開く：アダプタの見積もりで edit_open を呼び、begin → initialViewport → opened → 結果の順。開いたらアダプタの今の表示の要求で頼み直す', async () => {
    const { factory, made } = fakeKind();
    vi.mocked(openEdit).mockResolvedValueOnce(opened('aa01'));
    const pending = openEditorAs(factory, AUDIO);
    expect(useEditorStore.getState()).toMatchObject({
      phase: 'opening',
      kind: 'image',
      item: { id: 9 },
    });
    await pending;
    await flush();
    expect(openEdit).toHaveBeenCalledWith(9, viewport(0.5), null);
    expect(made[0]?.log).toEqual([
      'begin',
      'initialViewport',
      'opened:aa01',
      'view:0:1:fixed',
      'settle:3',
      // 見積もりと違うので頼み直す
      'sent:1:',
      'view:1:1:fixed',
      'settle:3',
    ]);
    expect(useEditorStore.getState()).toMatchObject({
      phase: 'open',
      session: 'aa01',
      source: { kind: 'audio' },
      doc: { order: [3] },
    });
    expect(viewEdit).toHaveBeenCalledWith('aa01', viewport(1));
  });

  // M13 段 6：音声の裏の仕事（波形の段の作り直し）の頼み直しは、同じビューポートでも送る
  it('アダプタの requestView は同じビューポートなら送らず、force なら送る', async () => {
    const adapter = await openFake();
    adapter.ctx.requestView(viewport(1));
    await flush();
    expect(viewEdit).not.toHaveBeenCalled();
    adapter.ctx.requestView(viewport(1), true);
    await flush();
    expect(viewEdit).toHaveBeenCalledTimes(1);
    expect(viewEdit).toHaveBeenCalledWith('aa01', viewport(1));
  });

  it('手の結果：送るときに印つきで sent、断られたら onRejected → applyView → settle（acked・指摘・知らせは核）', async () => {
    const adapter = await openFake();
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        status: status({ rev: 2, provisional: true }),
        rejected: { index: 1, problem: { code: 'Invalid', message: '範囲の外です', issues: [] } },
        view: { type: 'shown', view: viewDto(2) },
      }),
    );
    expect(
      adapter.ctx.push([
        { step: { type: 'undo' }, tag: 'a' },
        { step: { type: 'redo' }, tag: 'b' },
      ]),
    ).toBe(true);
    await flush();
    expect(adapter.log).toEqual([
      'beforeSteps',
      'sent:2:a,b',
      'rejected:2:1',
      'view:2:2:provisional',
      'settle:3',
    ]);
    expect(stepEdit).toHaveBeenCalledWith(
      'aa01',
      [{ type: 'undo' }, { type: 'redo' }],
      viewport(1),
    );
    expect(useNoticeStore.getState().message).toBe('範囲の外です');
    expect(useEditorStore.getState().acked).toBe(1);
  });

  it('表示できなければ onFailed → settle で、失敗は核が持つ。要求そのものが通らなければ onFailed だけ', async () => {
    const adapter = await openFake();
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        view: {
          type: 'failed',
          problem: { code: 'TooLarge', message: '大きすぎます', issues: [] },
        },
      }),
    );
    editorUndo();
    await flush();
    expect(adapter.log).toEqual(['beforeSteps', 'sent:2:-', 'failed:2', 'settle:3']);
    expect(useEditorStore.getState().failure).toEqual({
      kind: 'view',
      code: 'TooLarge',
      message: '大きすぎます',
    });
    adapter.log.length = 0;
    vi.mocked(stepEdit).mockRejectedValueOnce(new IpcError({ code: 'NotFound', message: 'ない' }));
    editorRedo();
    await flush();
    expect(adapter.log).toEqual(['beforeSteps', 'sent:3:-', 'failed:3']);
    expect(useEditorStore.getState().failure).toMatchObject({ kind: 'request', code: 'NotFound' });
  });

  it('核の手（右パネル）も先に beforeSteps を呼ぶ。send は呼ばず、selectCreated なら足したノードを選ぶ', async () => {
    const adapter = await openFake();
    setNodeEnabled(3, false);
    await flush();
    expect(adapter.log[0]).toBe('beforeSteps');
    adapter.log.length = 0;
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result({
        created: [5],
        doc: { base: 1, order: [3, 5], nodes: [node(5)], outputs: null },
        view: { type: 'shown', view: viewDto(3) },
      }),
    );
    adapter.ctx.send(
      [{ step: { type: 'preview', edit: { type: 'setEnabled', node: 3, enabled: false } } }],
      { selectCreated: true },
    );
    await flush();
    expect(adapter.log).not.toContain('beforeSteps');
    expect(useEditorStore.getState().selected).toBe(5);
  });

  it('busy の間は取り消す・やり直すを送らない', async () => {
    let busy = true;
    await openFake({ busy: () => busy });
    editorUndo();
    editorRedo();
    await flush();
    expect(stepEdit).not.toHaveBeenCalled();
    busy = false;
    editorUndo();
    await flush();
    expect(stepEdit).toHaveBeenCalledOnce();
  });

  it('足す操作の引数はアダプタの newParams（末尾か）。なければ大きさなしの既定値', async () => {
    const calls: [string, boolean][] = [];
    await openFake({
      newParams: (desc, atEnd) => {
        calls.push([desc.id, atEnd]);
        return { db: -6 };
      },
    });
    addOp('audio.gain');
    await flush();
    expect(calls).toEqual([['audio.gain', true]]);
    expect(vi.mocked(stepEdit).mock.calls[0]?.[1]).toEqual([
      {
        type: 'commit',
        edit: { type: 'addNode', at: 1, op: 'audio.gain', v: 1, params: { db: -6 } },
        mode: 'step',
      },
    ]);
    closeEditor();
    await openFake();
    addOp('audio.gain');
    await flush();
    expect(vi.mocked(stepEdit).mock.calls[0]?.[1]).toEqual([
      {
        type: 'commit',
        edit: { type: 'addNode', at: 1, op: 'audio.gain', v: 1, params: { db: 0 } },
        mode: 'step',
      },
    ]);
  });

  it('取って代わられたら、アダプタの今の表示の要求で頼み直す（続けて 2 回まで）', async () => {
    await openFake();
    vi.mocked(viewEdit).mockImplementation(async () => result({ view: { type: 'superseded' } }));
    refreshEditorView();
    await flush();
    await flush();
    await flush();
    await flush();
    expect(vi.mocked(viewEdit).mock.calls).toEqual([
      ['aa01', viewport(1)],
      ['aa01', viewport(1)],
      ['aa01', viewport(1)],
    ]);
  });

  it('閉じたら dispose(閉じる)・edit_close。開き直したら前のアダプタは dispose(開き直し) で、前の口は何もしない', async () => {
    const first = await openFake();
    const { factory, made } = fakeKind();
    vi.mocked(openEdit).mockResolvedValueOnce(opened('bb02'));
    await openEditorAs(factory, { ...AUDIO, id: 10 });
    expect(first.log).toEqual(['dispose:replace']);
    expect(closeEdit).toHaveBeenCalledWith('aa01');
    expect(first.ctx.current()).toBe(false);
    expect(first.ctx.session()).toBeNull();
    expect(first.ctx.push([{ step: { type: 'undo' } }])).toBe(false);
    const second = made[0];
    expect(second?.ctx.current()).toBe(true);
    expect(second?.ctx.session()).toBe('bb02');
    closeEditor();
    expect(second?.log.at(-1)).toBe('dispose:close');
    expect(closeEdit).toHaveBeenCalledWith('bb02');
    expect(second?.ctx.current()).toBe(false);
    expect(useEditorStore.getState()).toMatchObject({ phase: 'closed', kind: null, item: null });
  });

  // F15・docs の検証の直し：開いている途中（セッションがまだない）に閉じたら、Rust の開く途中を止める
  it('開いている途中に閉じると edit_cancel_open でそのアイテムの開く途中を止め、止められて返った Conflict は失敗にしない', async () => {
    const { factory, made } = fakeKind();
    let rejectOpen: (e: unknown) => void = () => undefined;
    vi.mocked(openEdit).mockReturnValueOnce(new Promise((_, reject) => (rejectOpen = reject)));
    const pending = openEditorAs(factory, AUDIO);
    expect(useEditorStore.getState().phase).toBe('opening');
    const closing = closeEditor();
    expect(cancelOpenEdit).toHaveBeenCalledTimes(1);
    expect(cancelOpenEdit).toHaveBeenCalledWith(AUDIO.id);
    expect(closeEdit).not.toHaveBeenCalled();
    expect(made[0]?.log.at(-1)).toBe('dispose:close');
    await closing;
    rejectOpen(new IpcError({ code: 'Conflict', message: '中止しました' }));
    await pending;
    expect(useEditorStore.getState()).toMatchObject({ phase: 'closed', failure: null, item: null });
    expect(closeEdit).not.toHaveBeenCalled();
  });

  it('止めが間に合わずに開き終えたら、届いたセッションを edit_close で閉じる', async () => {
    const { factory } = fakeKind();
    let resolveOpen: (v: ReturnType<typeof opened>) => void = () => undefined;
    vi.mocked(openEdit).mockReturnValueOnce(new Promise((r) => (resolveOpen = r)));
    const pending = openEditorAs(factory, AUDIO);
    await closeEditor();
    expect(cancelOpenEdit).toHaveBeenCalledWith(AUDIO.id);
    resolveOpen(opened('dd04'));
    await pending;
    expect(closeEdit).toHaveBeenCalledWith('dd04');
    expect(useEditorStore.getState()).toMatchObject({ phase: 'closed', session: null });
  });

  it('開けなかった（失敗のカード）あとに閉じるときは edit_cancel_open を送らない', async () => {
    const { factory } = fakeKind();
    vi.mocked(openEdit).mockRejectedValueOnce(
      new IpcError({ code: 'Busy', message: '混んでいます' }),
    );
    await openEditorAs(factory, AUDIO);
    expect(useEditorStore.getState().phase).toBe('error');
    await closeEditor();
    expect(cancelOpenEdit).not.toHaveBeenCalled();
    expect(closeEdit).not.toHaveBeenCalled();
  });

  it('開いている途中にほかを開くときは edit_cancel_open を送らない（次の edit_open が前のエディタを止める）', async () => {
    const { factory } = fakeKind();
    vi.mocked(openEdit).mockReturnValueOnce(new Promise(() => undefined));
    void openEditorAs(factory, AUDIO);
    vi.mocked(openEdit).mockResolvedValueOnce(opened('ee05'));
    await openEditorAs(factory, { ...AUDIO, id: 10 });
    expect(cancelOpenEdit).not.toHaveBeenCalled();
    expect(useEditorStore.getState()).toMatchObject({ phase: 'open', session: 'ee05' });
  });

  it('開く途中を止めた直後に開き直すと、止めの応えを待ってから edit_open を送る（遅れた止めで新しい開く途中を止めない）', async () => {
    const { factory } = fakeKind();
    vi.mocked(openEdit).mockReturnValueOnce(new Promise(() => undefined));
    void openEditorAs(factory, AUDIO);
    let finishCancel: () => void = () => undefined;
    vi.mocked(cancelOpenEdit).mockReturnValueOnce(
      new Promise((r) => (finishCancel = () => r(null))),
    );
    void closeEditor();
    vi.mocked(openEdit).mockResolvedValueOnce(opened('ff06'));
    const reopening = openEditorAs(factory, AUDIO);
    await flush();
    expect(openEdit).toHaveBeenCalledTimes(1);
    expect(useEditorStore.getState().phase).toBe('opening');
    finishCancel();
    await reopening;
    expect(openEdit).toHaveBeenCalledTimes(2);
    expect(useEditorStore.getState()).toMatchObject({ phase: 'open', session: 'ff06' });
  });

  it('開き直す（下書きから・版に戻したあと）は、最後に開いた種類の口で開く', async () => {
    const { factory, made } = fakeKind();
    vi.mocked(openEdit).mockResolvedValueOnce(opened('aa01'));
    await openEditorAs(factory, AUDIO);
    closeEditor();
    vi.mocked(openEdit).mockResolvedValueOnce(opened('cc03'));
    await reopenEditorWith(AUDIO, { fromDraft: true });
    expect(made).toHaveLength(2);
    expect(openEdit).toHaveBeenLastCalledWith(9, viewport(0.5), true);
    expect(useEditorStore.getState().session).toBe('cc03');
  });
});
