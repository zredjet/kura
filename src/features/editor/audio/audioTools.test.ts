/**
 * 音声エディタの道具とハンドルの手（M13 の段 6）を偽の IPC で試す。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import {
  type EditAudioView,
  type EditNodeDto,
  type EditStepDto,
  type EditStepResultDto,
  openEdit,
  snapEdit,
  stepEdit,
  viewEdit,
} from '../../../ipc/edit';
import type { ItemSummaryDto } from '../../../ipc/items';
import { ContextPool } from '../../../lib/assetAudio';
import { useNoticeStore } from '../../../stores/noticeStore';
import { fakeFactory } from '../../../test/fakeAudio';
import { type AudioRuntime, setAudioRuntimeForTest } from '../../asset/audioRuntime';
import { closeEditor, openEditorAs, useEditorStore } from '../editorStore';
import {
  AUDIO_ADAPTER,
  audioController,
  audioPlayer,
  setAudioIoForTest,
  setCursor,
  setSelection,
  toggleLoopAudition,
  useAudioStore,
} from './audioStore';
import {
  AUDIO_POINTER,
  confirmTrim,
  selectAudioTool,
  selectionToLoop,
  selectionToTrim,
  setInOut,
  setTrimFrame,
  startTrim,
} from './audioTools';
import { audioView, type AudioViewOver, fakeFetch } from './testAudioView';

vi.mock('../../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../ipc/edit')>()),
  openEdit: vi.fn(),
  stepEdit: vi.fn(),
  viewEdit: vi.fn(),
  closeEdit: vi.fn(async () => null),
  discardEditDraft: vi.fn(async () => true),
  measureEdit: vi.fn(async () => ({ type: 'idle' })),
  snapEdit: vi.fn(),
}));
vi.mock('../../player/playerActions', () => ({ playerController: vi.fn() }));

const t = ja.editor.audio;
const S = 'ab01';
const TOTAL = 10_000;

const ITEM = {
  id: 9,
  hash: 'cd'.repeat(32),
  ext: 'wav',
  kind: 'audio',
  title: 'loop.wav',
  pixelArt: false,
} as unknown as ItemSummaryDto;

function view(over: AudioViewOver = {}): EditAudioView {
  return audioView({
    frames: TOTAL,
    valid: [{ start: 0, end: TOTAL }],
    changed: { type: 'spans', spans: [] },
    ...over,
    peaks: { frames: TOTAL, changed: { type: 'spans', spans: [] }, ...over.peaks },
  });
}

function result(
  v: EditAudioView,
  over: Partial<EditStepResultDto> = {},
  nodes: EditNodeDto[] = [],
): EditStepResultDto {
  return {
    status: {
      rev: v.rev,
      head: v.rev,
      provisional: false,
      canUndo: false,
      canRedo: false,
      dirty: false,
      saving: null,
      needsReopen: false,
    },
    created: [],
    rejected: null,
    doc: { base: null, order: nodes.map((n) => n.id), nodes, outputs: null },
    view: { type: 'shown', view: v },
    notices: [],
    ...over,
  };
}

function node(id: number, op: string, enabled = true): EditNodeDto {
  return { id, op, v: 1, enabled, params: {}, log: null };
}

async function flush(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
}

async function open(v: EditAudioView = view(), nodes: EditNodeDto[] = []): Promise<void> {
  vi.mocked(openEdit).mockResolvedValueOnce({
    session: S,
    itemId: ITEM.id,
    baseVersion: 0,
    source: { kind: 'audio', width: 0, height: 0, frames: null, format: null, audio: null },
    pixelArt: false,
    draft: { exists: false, fresh: true, updatedAt: null, opened: false },
    result: result(v, {}, nodes),
  } as unknown as Awaited<ReturnType<typeof openEdit>>);
  vi.mocked(viewEdit).mockImplementation(async () =>
    result(v, { doc: { base: 1, order: nodes.map((n) => n.id), nodes: [], outputs: null } }),
  );
  await openEditorAs(AUDIO_ADAPTER, ITEM);
  await flush();
  const c = audioController();
  c?.resize(1000, 300, 1);
  c?.setInsets({ top: 0, right: 0, bottom: 0, left: 0 });
  vi.mocked(stepEdit).mockClear();
}

/** 送った手（呼んだ順につないだもの）。 */
function sentSteps(): EditStepDto[] {
  return vi.mocked(stepEdit).mock.calls.flatMap((c) => c[1]);
}

/** フレームの x（面は幅 1000・余白なし）。 */
const xOf = (f: number) => (f / TOTAL) * 1000;
/** 段の中の y（目盛り 20・全体図 24 の下）。 */
const LANE_Y = 120;

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(stepEdit).mockReset();
  vi.mocked(snapEdit).mockReset();
  vi.mocked(stepEdit).mockImplementation(async () => result(view()));
  setAudioIoForTest({ fetch: fakeFetch().fetch, draw: () => undefined });
  setAudioRuntimeForTest({
    pool: new ContextPool(fakeFactory().create, 3),
    audition: { stop: vi.fn(), playing: false },
    sfx: { stop: vi.fn() },
  } as unknown as AudioRuntime);
  useNoticeStore.setState({ message: null });
});

afterEach(async () => {
  await closeEditor();
  setAudioIoForTest({});
  setAudioRuntimeForTest(null);
  vi.useRealTimers();
});

describe('切り詰めの道具', () => {
  it('切り詰めがなければ全体の枠を出し、↵ で末尾に足す（全体のままなら何も足さない）', async () => {
    await open();
    selectAudioTool('trim');
    expect(useAudioStore.getState()).toMatchObject({
      tool: 'trim',
      trim: { node: null, start: 0, end: TOTAL, bounds: TOTAL },
    });
    setTrimFrame(1000, 9000);
    confirmTrim();
    await flush();
    expect(sentSteps()).toEqual([
      {
        type: 'handle',
        handle: { type: 'trim', start: 1000, end: 9000 },
        node: null,
        mode: 'step',
      },
    ]);
    expect(useAudioStore.getState()).toMatchObject({ tool: 'select', trim: null });
  });

  it('有効な切り詰めは仮に無効にして入力を出し、↵ で [cancelPreview, handle{trim, node, step}]', async () => {
    const trimNode = {
      id: 5,
      op: 'audio.trim',
      enabled: true,
      inRate: 48_000,
      inFrames: 20_000,
      marks: { type: 'trim' as const, start: 2000, end: 12_000 },
    };
    await open(view({ nodes: [trimNode] }), [node(5, 'audio.trim')]);
    vi.mocked(stepEdit).mockImplementationOnce(async () => ({
      ...result(view({ frames: 20_000, rev: 2, nodes: [trimNode] })),
      status: { ...result(view()).status, rev: 2, provisional: true },
    }));
    startTrim();
    await flush();
    expect(sentSteps()).toEqual([
      { type: 'preview', edit: { type: 'setEnabled', node: 5, enabled: false } },
    ]);
    expect(useAudioStore.getState().trim).toMatchObject({
      node: 5,
      bounds: 20_000,
      previewed: true,
    });
    setTrimFrame(3000, 12_000);
    vi.mocked(stepEdit).mockClear();
    confirmTrim();
    await flush();
    expect(sentSteps()).toEqual([
      { type: 'cancelPreview' },
      { type: 'handle', handle: { type: 'trim', start: 3000, end: 12_000 }, node: 5, mode: 'step' },
    ]);
  });

  it('仮に外した表示でも写せない（印が null）なら、枠を出さずに知らせ、その操作を選んで仮の値を捨てる', async () => {
    const trimNode = {
      id: 5,
      op: 'audio.trim',
      enabled: true,
      inRate: 48_000,
      inFrames: 20_000,
      marks: null,
    };
    await open(view({ nodes: [trimNode] }), [node(5, 'audio.trim')]);
    // 仮の値を捨てた結果（文書はそのまま）
    vi.mocked(stepEdit).mockImplementation(async () =>
      result(view({ nodes: [trimNode] }), {}, [node(5, 'audio.trim')]),
    );
    vi.mocked(stepEdit).mockImplementationOnce(async () => ({
      ...result(view({ frames: 20_000, rev: 2, nodes: [trimNode] }), {}, [node(5, 'audio.trim')]),
      status: { ...result(view()).status, rev: 2, provisional: true },
    }));
    startTrim();
    await flush();
    expect(useAudioStore.getState().trim).toBeNull();
    expect(useAudioStore.getState().tool).toBe('select');
    expect(useEditorStore.getState().selected).toBe(5);
    expect(useNoticeStore.getState().message).toBe(t.trimUseParams);
    expect(sentSteps()).toEqual([
      { type: 'preview', edit: { type: 'setEnabled', node: 5, enabled: false } },
      { type: 'cancelPreview' },
    ]);
  });

  it('外す前の印では写せなくても（後ろに end: null の切り詰め）、外した表示の印で枠を出して直せる', async () => {
    // [trim A {1000, 2000}, trim B {100, null}]：A が有効なとき、B の写しでは A の印が範囲の外
    const a = { id: 1, op: 'audio.trim', enabled: true, inRate: 48_000, inFrames: 20_000 };
    const b = { id: 2, op: 'audio.trim', enabled: true, inRate: 48_000, inFrames: 1000 };
    await open(
      view({
        frames: 900,
        nodes: [
          { ...a, marks: null },
          { ...b, marks: { type: 'trim' as const, start: 0, end: 900 } },
        ],
      }),
      [node(1, 'audio.trim'), node(2, 'audio.trim')],
    );
    useEditorStore.setState({ selected: 1 });
    // A を仮に外した表示：B は [100, 20000) で、A の印は 900..1900
    vi.mocked(stepEdit).mockImplementationOnce(async () => ({
      ...result(
        view({
          frames: 19_900,
          rev: 2,
          nodes: [
            { ...a, enabled: false, marks: { type: 'trim' as const, start: 900, end: 1900 } },
            { ...b, inFrames: 20_000, marks: { type: 'trim' as const, start: 0, end: 19_900 } },
          ],
        }),
      ),
      status: { ...result(view()).status, rev: 2, provisional: true },
    }));
    startTrim();
    await flush();
    expect(useNoticeStore.getState().message).toBeNull();
    expect(useAudioStore.getState().trim).toEqual({
      node: 1,
      start: 900,
      end: 1900,
      bounds: 19_900,
      previewed: true,
    });
    setTrimFrame(900, 5000);
    vi.mocked(stepEdit).mockClear();
    confirmTrim();
    await flush();
    expect(sentSteps()).toEqual([
      { type: 'cancelPreview' },
      { type: 'handle', handle: { type: 'trim', start: 900, end: 5000 }, node: 1, mode: 'step' },
    ]);
  });

  it('切り詰めの枠を出している間は、ループの旗をつかめない（表示の座標が切り詰めを外したものなので）', async () => {
    await open(view({ loop: { start: 3000, end: 5000 } }));
    selectAudioTool('trim');
    expect(useAudioStore.getState().trim).not.toBeNull();
    expect(AUDIO_POINTER.down({ shiftKey: false }, xOf(5000), 5)).toBe(false);
    AUDIO_POINTER.move({ shiftKey: false }, xOf(6000), 5);
    AUDIO_POINTER.up({ shiftKey: false }, xOf(6000), 5, false);
    await flush();
    expect(sentSteps()).toEqual([]);
    expect(useAudioStore.getState().local).toBeNull();
    expect(AUDIO_POINTER.cursor(xOf(5000), 5)).toBeUndefined();
  });

  it('ほかの道具を選んだらやめる（仮の値を捨てる）', async () => {
    const trimNode = {
      id: 5,
      op: 'audio.trim',
      enabled: true,
      inRate: 48_000,
      inFrames: 20_000,
      marks: { type: 'trim' as const, start: 0, end: 100 },
    };
    await open(view({ nodes: [trimNode] }), [node(5, 'audio.trim')]);
    startTrim();
    await flush();
    vi.mocked(stepEdit).mockClear();
    selectAudioTool('fade');
    await flush();
    expect(sentSteps()).toEqual([{ type: 'cancelPreview' }]);
    expect(useAudioStore.getState()).toMatchObject({ tool: 'fade', trim: null });
  });
});

describe('フェード・ループのドラッグ', () => {
  it('フェードインの三角をドラッグすると preview を送り、離したら commitPreview', async () => {
    await open();
    selectAudioTool('fade');
    expect(AUDIO_POINTER.down({ shiftKey: false }, xOf(0) + 2, 48)).toBe(true);
    AUDIO_POINTER.move({ shiftKey: false }, xOf(2000), 48);
    expect(useAudioStore.getState().local?.fadeIn).toBe(2000);
    await flush();
    AUDIO_POINTER.up({ shiftKey: false }, xOf(2000), 48, false);
    await flush();
    expect(sentSteps()).toEqual([
      { type: 'handle', handle: { type: 'fadeIn', end: 2000 }, node: null, mode: 'preview' },
      { type: 'commitPreview', mode: 'step' },
    ]);
  });

  it('ループの旗を離したら、ゼロ交差に寄せた値を preview → commitPreview で送る', async () => {
    vi.mocked(snapEdit).mockResolvedValue({ frame: 1490, found: true });
    await open(view({ loop: { start: 1000, end: 4000 } }));
    selectAudioTool('loop');
    expect(AUDIO_POINTER.down({ shiftKey: false }, xOf(1000), 5)).toBe(true);
    AUDIO_POINTER.move({ shiftKey: false }, xOf(1500), 5);
    expect(useAudioStore.getState().local?.loop).toEqual({ start: 1500, end: 4000 });
    await flush();
    vi.mocked(stepEdit).mockClear();
    AUDIO_POINTER.up({ shiftKey: false }, xOf(1500), 5, false);
    await flush();
    // 48 kHz の 5 ms
    expect(snapEdit).toHaveBeenCalledWith(S, 1500, 240);
    expect(sentSteps()).toEqual([
      {
        type: 'handle',
        handle: { type: 'loop', range: { start: 1490, end: 4000 } },
        node: null,
        mode: 'preview',
      },
      { type: 'commitPreview', mode: 'step' },
    ]);
  });

  it('ループを試聴しながら旗のドラッグを esc でやめたら、試聴も文書のループ点に戻す', async () => {
    const loop = { start: 1000, end: 4000 };
    await open(view({ loop }));
    // 仮の値は仮の結果、やめたら文書の値
    vi.mocked(stepEdit).mockImplementation(async (_s, steps) => {
      const preview = steps.find((q) => q.type === 'handle');
      return preview?.type === 'handle' && preview.handle.type === 'loop'
        ? {
            ...result(view({ rev: 2, loop: preview.handle.range })),
            status: { ...result(view()).status, rev: 2, provisional: true },
          }
        : result(view({ loop }));
    });
    selectAudioTool('loop');
    toggleLoopAudition();
    const player = audioPlayer();
    expect(player?.loop).toEqual(loop);
    AUDIO_POINTER.down({ shiftKey: false }, xOf(4000), 5);
    AUDIO_POINTER.move({ shiftKey: false }, xOf(6000), 5);
    await flush();
    expect(player?.loop).toEqual({ start: 1000, end: 6000 });
    AUDIO_POINTER.up({ shiftKey: false }, xOf(6000), 5, true);
    await flush();
    expect(player?.loop).toEqual({ start: 1000, end: 4000 });
    expect(useAudioStore.getState().local).toBeNull();
  });

  it('吸着がオフなら寄せない・動かさずに離したら何も送らない', async () => {
    await open(view({ loop: { start: 1000, end: 4000 } }));
    useAudioStore.setState({ snapZero: false });
    selectAudioTool('loop');
    AUDIO_POINTER.down({ shiftKey: false }, xOf(4000), 5);
    AUDIO_POINTER.up({ shiftKey: false }, xOf(4000), 5, false);
    await flush();
    expect(sentSteps()).toEqual([]);
    AUDIO_POINTER.down({ shiftKey: false }, xOf(4000), 5);
    AUDIO_POINTER.move({ shiftKey: false }, xOf(6000), 5);
    await flush();
    vi.mocked(stepEdit).mockClear();
    AUDIO_POINTER.up({ shiftKey: false }, xOf(6000), 5, false);
    await flush();
    expect(snapEdit).not.toHaveBeenCalled();
    expect(sentSteps()[0]).toEqual({
      type: 'handle',
      handle: { type: 'loop', range: { start: 1000, end: 6000 } },
      node: null,
      mode: 'preview',
    });
  });
});

describe('選択', () => {
  it('ドラッグで選び、クリックはカーソル（選択を外す）、⇧クリックで広げる', async () => {
    await open();
    expect(AUDIO_POINTER.down({ shiftKey: false }, xOf(1000), LANE_Y)).toBe(true);
    AUDIO_POINTER.move({ shiftKey: false }, xOf(3000), LANE_Y);
    AUDIO_POINTER.up({ shiftKey: false }, xOf(3000), LANE_Y, false);
    expect(useAudioStore.getState().selection).toEqual({ start: 1000, end: 3000 });
    AUDIO_POINTER.down({ shiftKey: true }, xOf(5000), LANE_Y);
    AUDIO_POINTER.up({ shiftKey: true }, xOf(5000), LANE_Y, false);
    expect(useAudioStore.getState().selection).toEqual({ start: 1000, end: 5000 });
    AUDIO_POINTER.down({ shiftKey: false }, xOf(7000), LANE_Y);
    AUDIO_POINTER.up({ shiftKey: false }, xOf(7000), LANE_Y, false);
    expect(useAudioStore.getState()).toMatchObject({ selection: null, cursor: 7000 });
  });

  it('選択の範囲に切り詰める（末尾に足す）・選択をループにする', async () => {
    await open();
    setSelection({ start: 100, end: 200 });
    selectionToTrim();
    await flush();
    expect(sentSteps()).toEqual([
      { type: 'handle', handle: { type: 'trim', start: 100, end: 200 }, node: null, mode: 'step' },
    ]);
    vi.mocked(stepEdit).mockClear();
    setSelection({ start: 300, end: 400 });
    selectionToLoop();
    await flush();
    expect(sentSteps()).toEqual([
      {
        type: 'handle',
        handle: { type: 'loop', range: { start: 300, end: 400 } },
        node: null,
        mode: 'step',
      },
    ]);
  });

  it('I／O：今の道具の始め・終わりをカーソルにする', async () => {
    await open();
    setCursor(500);
    setInOut('in');
    expect(useAudioStore.getState().selection).toEqual({ start: 500, end: TOTAL });
    selectAudioTool('fade');
    setInOut('out');
    await flush();
    expect(sentSteps()).toEqual([
      { type: 'handle', handle: { type: 'fadeOut', start: 500 }, node: null, mode: 'step' },
    ]);
  });
});
