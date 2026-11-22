/**
 * 音声のアダプタと状態（M13 の段 6）を偽の IPC・偽の WebAudio・偽の取得で試す。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import {
  type EditAudioView,
  type EditStatusDto,
  type EditStepResultDto,
  measureEdit,
  openEdit,
  stepEdit,
  viewEdit,
} from '../../../ipc/edit';
import type { ItemSummaryDto } from '../../../ipc/items';
import { IpcError } from '../../../ipc/result';
import { editInputPcmUrl, editPcmUrl } from '../../../ipc/media';
import { ContextPool } from '../../../lib/assetAudio';
import { useNoticeStore } from '../../../stores/noticeStore';
import { fakeFactory } from '../../../test/fakeAudio';
import { type AudioRuntime, setAudioRuntimeForTest } from '../../asset/audioRuntime';
import { playerController } from '../../player/playerActions';
import { usePlayerStore } from '../../player/playerStore';
import {
  closeEditor,
  editorUndo,
  openEditorAs,
  pushEditorSteps,
  useEditorStore,
} from '../editorStore';
import {
  AUDIO_ADAPTER,
  audioController,
  audioPlayer,
  type AudioViewSummary,
  MEASURE_RETRY_MS,
  overlayOf,
  setAudioDragging,
  setAudioIoForTest,
  setAudioLocal,
  setCursor,
  setSelection,
  toggleAB,
  toggleLoopAudition,
  togglePlay,
  useAudioStore,
} from './audioStore';
import {
  clearLoop,
  selectAudioTool,
  selectionToLoop,
  setInOut,
  setLoop as setLoopFromField,
} from './audioTools';
import { CHUNK, FAIL_RETRY_MS } from './pcmChunks';
import { audioView, type AudioViewOver, fakeFetch, pcmBytes } from './testAudioView';
import { WAVE_IDLE_MS } from './waveController';

vi.mock('../../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../ipc/edit')>()),
  openEdit: vi.fn(),
  stepEdit: vi.fn(),
  viewEdit: vi.fn(),
  closeEdit: vi.fn(async () => null),
  discardEditDraft: vi.fn(async () => true),
  measureEdit: vi.fn(),
  snapEdit: vi.fn(),
}));
vi.mock('../../player/playerActions', () => ({ playerController: vi.fn() }));

const S = 'ab01';
const TOTAL = 96_000;

const ITEM = {
  id: 9,
  hash: 'cd'.repeat(32),
  ext: 'wav',
  kind: 'audio',
  width: null,
  height: null,
  durationMs: 2000,
  title: 'loop.wav',
  pixelArt: false,
} as unknown as ItemSummaryDto;

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

function view(over: AudioViewOver = {}): EditAudioView {
  return audioView({
    frames: TOTAL,
    valid: [{ start: 0, end: TOTAL }],
    changed: { type: 'spans', spans: [] },
    ...over,
    peaks: { frames: TOTAL, changed: { type: 'spans', spans: [] }, ...over.peaks },
  });
}

function result(v: EditAudioView, over: Partial<EditStepResultDto> = {}): EditStepResultDto {
  return {
    status: status({ rev: v.rev, head: v.rev }),
    created: [],
    rejected: null,
    doc: { base: 1, order: [], nodes: [], outputs: null },
    view: { type: 'shown', view: v },
    notices: [],
    ...over,
  };
}

function opened(v: EditAudioView) {
  return {
    session: S,
    itemId: ITEM.id,
    baseVersion: 0,
    source: {
      kind: 'audio',
      width: 0,
      height: 0,
      frames: null,
      format: null,
      audio: {
        rate: 48_000,
        channels: 2,
        frames: TOTAL,
        bits: 16,
        float: false,
        loop: null,
        decoder: 'wav-kura-dec/1',
        lazy: true,
      },
    },
    originalFormat: 'wav',
    pixelArt: false,
    draft: { exists: false, fresh: true, updatedAt: null, opened: false },
    result: { ...result(v), doc: { base: null, order: [], nodes: [], outputs: null } },
  } as unknown as Awaited<ReturnType<typeof openEdit>>;
}

let io: ReturnType<typeof fakeFetch>;
let factory: ReturnType<typeof fakeFactory>;
let runtime: {
  audition: { stop: ReturnType<typeof vi.fn>; playing: boolean };
  sfx: { stop: ReturnType<typeof vi.fn> };
  pool: ContextPool;
};
const pause = vi.fn();

async function flush(ms = 0): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
}

async function open(first: EditAudioView = view(), again: EditAudioView = first): Promise<void> {
  vi.mocked(openEdit).mockResolvedValueOnce(opened(first));
  vi.mocked(viewEdit).mockImplementation(async () => result(again));
  await openEditorAs(AUDIO_ADAPTER, ITEM);
  await flush();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(openEdit).mockReset();
  vi.mocked(stepEdit).mockReset();
  vi.mocked(viewEdit).mockReset();
  vi.mocked(measureEdit).mockReset();
  vi.mocked(measureEdit).mockImplementation(async () => ({ type: 'idle' }));
  pause.mockReset();
  vi.mocked(playerController).mockReturnValue({ pause } as never);
  io = fakeFetch();
  setAudioIoForTest({ fetch: io.fetch, draw: () => undefined });
  factory = fakeFactory();
  runtime = {
    pool: new ContextPool(factory.create, 3),
    audition: { stop: vi.fn(), playing: false },
    sfx: { stop: vi.fn() },
  };
  setAudioRuntimeForTest(runtime as unknown as AudioRuntime);
  usePlayerStore.setState({
    active: false,
    snapshot: { ...usePlayerStore.getState().snapshot, playing: false },
  });
  useNoticeStore.setState({ message: null });
});

afterEach(async () => {
  await closeEditor();
  setAudioIoForTest({});
  setAudioRuntimeForTest(null);
  vi.useRealTimers();
});

describe('音声のアダプタ', () => {
  it('開く：空の spans で頼み、元の形・表示の要約を受け、開いたら見ている区間で頼み直す', async () => {
    await open();
    expect(openEdit).toHaveBeenCalledWith(
      ITEM.id,
      { type: 'audio', visible: null, spans: [], interacting: false, focus: null, work: false },
      null,
    );
    const s = useAudioStore.getState();
    expect(s.source?.rate).toBe(48_000);
    expect(s.view?.frames).toBe(TOTAL);
    expect(audioController()?.total).toBe(TOTAL);
    expect(viewEdit).toHaveBeenCalledWith(
      S,
      expect.objectContaining({ type: 'audio', visible: { start: 0, end: TOTAL }, work: false }),
    );
  });

  it('手の結果の changed に掛かる PCM を捨てる', async () => {
    await open();
    const c = audioController();
    if (!c) throw new Error('面がない');
    for (const i of [0, 1]) {
      c.pcm.put(i, { rev: 1, start: i * CHUNK, frames: 1, planes: [new Float32Array(1)] });
    }
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result(view({ rev: 2, changed: { type: 'spans', spans: [{ start: 0, end: 10 }] } })),
    );
    pushEditorSteps([{ type: 'undo' }]);
    await flush();
    expect(c.pcm.has(0)).toBe(false);
    expect(c.pcm.has(1)).toBe(true);
  });

  it('裏の仕事が残っていれば、手が止まって 150 ms で work: true を頼む（ドラッグの間は頼まない）', async () => {
    const busy = view({ work: true, peaks: { pending: true } });
    await open(busy, busy);
    vi.mocked(viewEdit).mockClear();
    setAudioDragging(true);
    await flush(WAVE_IDLE_MS * 3);
    expect(viewEdit).not.toHaveBeenCalled();
    setAudioDragging(false);
    await flush(WAVE_IDLE_MS);
    expect(viewEdit).toHaveBeenCalledWith(S, expect.objectContaining({ work: true }));
    // 仕事が終われば頼み直しは止まる
    vi.mocked(viewEdit).mockImplementation(async () => result(view({ work: false })));
    await flush(WAVE_IDLE_MS);
    vi.mocked(viewEdit).mockClear();
    await flush(WAVE_IDLE_MS * 3);
    expect(viewEdit).not.toHaveBeenCalled();
  });

  describe('裏の仕事の頼みの連鎖', () => {
    const busy = () => view({ work: true, peaks: { pending: true } });

    /** 応えを後から返す edit_view（届いた順に `answer` で返す）。 */
    function deferredViews() {
      const waiting: ((r: EditStepResultDto) => void)[] = [];
      vi.mocked(viewEdit).mockImplementation(
        () => new Promise<EditStepResultDto>((resolve) => waiting.push(resolve)),
      );
      return {
        answer: async (v: EditAudioView) => {
          waiting.shift()?.(result(v));
          await flush();
        },
      };
    }

    it('仕事が残っている間は、応えが届くたびに次を頼む（手が止まって 150 ms の最初のあとは待たない）', async () => {
      await open(busy(), busy());
      vi.mocked(viewEdit).mockClear();
      let left = 3;
      vi.mocked(viewEdit).mockImplementation(async () => result(left-- > 0 ? busy() : view()));
      await flush(WAVE_IDLE_MS - 1);
      expect(viewEdit).not.toHaveBeenCalled();
      await flush(1);
      await flush(10);
      // 3 回の仕事の応えのたびに続け、仕事のない応えで止まる
      expect(viewEdit).toHaveBeenCalledTimes(4);
      for (const [, vp] of vi.mocked(viewEdit).mock.calls) {
        expect(vp).toMatchObject({ type: 'audio', work: true });
      }
      await flush(WAVE_IDLE_MS * 3);
      expect(viewEdit).toHaveBeenCalledTimes(4);
    });

    it('進まない応え（workStalled）では続けて頼まず、待ってから頼み直す。進めば連鎖に戻る', async () => {
      const stuck = () => view({ work: true, workStalled: true, peaks: { pending: true } });
      await open(busy(), busy());
      vi.mocked(viewEdit).mockClear();
      const replies = [stuck(), stuck(), busy(), view()];
      vi.mocked(viewEdit).mockImplementation(async () => result(replies.shift() ?? view()));
      await flush(WAVE_IDLE_MS);
      await flush(WAVE_IDLE_MS - 1);
      expect(viewEdit).toHaveBeenCalledTimes(1);
      await flush(1);
      expect(viewEdit).toHaveBeenCalledTimes(2);
      await flush(FAIL_RETRY_MS - 1);
      expect(viewEdit).toHaveBeenCalledTimes(2);
      await flush(1);
      await flush(10);
      // 進んだ応えで連鎖に戻り、仕事のない応えで止まる
      expect(viewEdit).toHaveBeenCalledTimes(4);
      await flush(FAIL_RETRY_MS * 3);
      expect(viewEdit).toHaveBeenCalledTimes(4);
    });

    it('同時に 2 本送らない：応えが届くまで次を頼まない', async () => {
      await open(busy(), busy());
      vi.mocked(viewEdit).mockClear();
      const io = deferredViews();
      await flush(WAVE_IDLE_MS);
      expect(viewEdit).toHaveBeenCalledTimes(1);
      await flush(WAVE_IDLE_MS * 5);
      expect(viewEdit).toHaveBeenCalledTimes(1);
      await io.answer(busy());
      await flush(5);
      expect(viewEdit).toHaveBeenCalledTimes(2);
      await flush(WAVE_IDLE_MS * 5);
      expect(viewEdit).toHaveBeenCalledTimes(2);
    });

    it('手が来たらそちらを先に送って連鎖をやめ、手が止まって 150 ms から始め直す', async () => {
      await open(busy(), busy());
      vi.mocked(viewEdit).mockClear();
      const io = deferredViews();
      await flush(WAVE_IDLE_MS);
      expect(viewEdit).toHaveBeenCalledTimes(1);
      // 頼みの応えを待つ間に手が来た（ためて、応えのあとに送る）
      vi.mocked(stepEdit).mockImplementation(async () => result(busy()));
      pushEditorSteps([{ type: 'undo' }]);
      expect(stepEdit).not.toHaveBeenCalled();
      await io.answer(busy());
      await flush(5);
      expect(stepEdit).toHaveBeenCalledTimes(1);
      expect(stepEdit).toHaveBeenCalledWith(
        S,
        [{ type: 'undo' }],
        expect.objectContaining({ work: false }),
      );
      expect(viewEdit).toHaveBeenCalledTimes(1);
      await flush(WAVE_IDLE_MS - 10);
      expect(viewEdit).toHaveBeenCalledTimes(1);
      await flush(10);
      expect(viewEdit).toHaveBeenCalledTimes(2);
      expect(viewEdit).toHaveBeenLastCalledWith(S, expect.objectContaining({ work: true }));
    });

    it('閉じたら止まる', async () => {
      await open(busy(), busy());
      vi.mocked(viewEdit).mockClear();
      await flush(WAVE_IDLE_MS + 5);
      const n = vi.mocked(viewEdit).mock.calls.length;
      expect(n).toBeGreaterThan(1);
      await closeEditor();
      await flush(WAVE_IDLE_MS * 3);
      expect(viewEdit).toHaveBeenCalledTimes(n);
    });
  });

  it('断られた手のあとは、ドラッグの手元の値をやめて文書の値を出す', async () => {
    await open();
    setAudioLocal({ fadeIn: 1234 });
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result(view(), {
        rejected: { index: 0, problem: { code: 'Invalid', message: '写せません', issues: [] } },
      }),
    );
    pushEditorSteps([{ type: 'undo' }]);
    await flush();
    expect(useAudioStore.getState().local).toBeNull();
  });

  it('ラウドネス：仮でない結果で今の出力を計っていなければ 1 回だけ頼み、届いた値を出す（仮の結果では頼まない）', async () => {
    vi.mocked(measureEdit).mockImplementation(async () => ({
      type: 'ready',
      samplesId: '00112233aabbccdd',
      loudness: {
        loudness: -14.2,
        loudnessReason: null,
        loudnessRange: 6.1,
        truePeak: -1,
        samplePeak: -1.2,
      },
    }));
    await open();
    expect(measureEdit).toHaveBeenCalledTimes(1);
    expect(useAudioStore.getState()).toMatchObject({
      loudness: { type: 'ready' },
      loudnessFresh: true,
    });
    // 仮の結果（ドラッグの途中）：出力が変わっても頼まない
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result(view({ rev: 2, samplesId: 'ffff' }), {
        status: status({ rev: 2, provisional: true }),
      }),
    );
    pushEditorSteps([{ type: 'preview', edit: { type: 'setEnabled', node: 1, enabled: false } }]);
    await flush();
    expect(measureEdit).toHaveBeenCalledTimes(1);
    expect(useAudioStore.getState().loudnessFresh).toBe(false);
  });

  const READY = (samplesId: string, loudness = -14.2) =>
    ({
      type: 'ready',
      samplesId,
      loudness: {
        loudness,
        loudnessReason: null,
        loudnessRange: 6.1,
        truePeak: -1,
        samplePeak: -1.2,
      },
    }) as const;
  const ID = '00112233aabbccdd';

  it('ラウドネス：一時的な誤り（Busy）は確かな失敗として覚えず、あとで頼み直す', async () => {
    vi.mocked(measureEdit).mockRejectedValue(
      new IpcError({ code: 'Busy', message: '一括が使っています' }),
    );
    await open();
    const c0 = vi.mocked(measureEdit).mock.calls.length;
    expect(c0).toBeGreaterThanOrEqual(1);
    expect(useAudioStore.getState().loudness.type).not.toBe('failed');
    expect(useAudioStore.getState().loudnessFresh).toBe(false);
    // 同じ出力の仮でない結果が届いたら頼み直す
    vi.mocked(stepEdit).mockResolvedValueOnce(result(view()));
    pushEditorSteps([{ type: 'seal' }]);
    await flush();
    expect(measureEdit).toHaveBeenCalledTimes(c0 + 1);
    expect(useAudioStore.getState().loudness.type).not.toBe('failed');
    // 何も届かなくても、少し待って頼み直す
    vi.mocked(measureEdit).mockImplementation(async () => READY(ID));
    await flush(MEASURE_RETRY_MS);
    expect(vi.mocked(measureEdit).mock.calls.length).toBeGreaterThanOrEqual(c0 + 2);
    expect(useAudioStore.getState()).toMatchObject({
      loudness: { type: 'ready' },
      loudnessFresh: true,
    });
    // 確かな誤りは失敗として出す
    vi.mocked(measureEdit).mockRejectedValueOnce(
      new IpcError({ code: 'TooLarge', message: '大きすぎます' }),
    );
    vi.mocked(stepEdit).mockResolvedValueOnce(result(view({ rev: 2, samplesId: 'eeee' })));
    pushEditorSteps([{ type: 'seal' }]);
    await flush();
    expect(useAudioStore.getState()).toMatchObject({
      loudness: { type: 'failed', samplesId: 'eeee' },
      loudnessFresh: true,
    });
  });

  it('ラウドネス：計っている間（Measuring）は前の値を残して古い印にし、Ready が届いたら替える', async () => {
    vi.mocked(measureEdit).mockImplementation(async () => READY(ID));
    await open();
    expect(useAudioStore.getState().loudness).toEqual(READY(ID));
    // 出力が変わって計り始めた
    vi.mocked(measureEdit).mockImplementation(async () => ({
      type: 'measuring',
      samplesId: 'ffff',
      progress: 0,
    }));
    vi.mocked(stepEdit).mockResolvedValueOnce(result(view({ rev: 2, samplesId: 'ffff' })));
    pushEditorSteps([{ type: 'seal' }]);
    await flush();
    expect(useAudioStore.getState().loudness).toEqual(READY(ID));
    expect(useAudioStore.getState().loudnessFresh).toBe(false);
    // 表示の結果の Measuring でも前の値のまま
    vi.mocked(viewEdit).mockImplementation(async () =>
      result(
        view({
          rev: 2,
          samplesId: 'ffff',
          loudness: { type: 'measuring', samplesId: 'ffff', progress: 0.5 },
        }),
      ),
    );
    await flush(WAVE_IDLE_MS);
    expect(useAudioStore.getState().loudness).toEqual(READY(ID));
    expect(useAudioStore.getState().loudnessFresh).toBe(false);
    // 計り終えた
    vi.mocked(viewEdit).mockImplementation(async () =>
      result(view({ rev: 2, samplesId: 'ffff', loudness: READY('ffff', -20) })),
    );
    await flush(WAVE_IDLE_MS);
    expect(useAudioStore.getState().loudness).toEqual(READY('ffff', -20));
    expect(useAudioStore.getState().loudnessFresh).toBe(true);
  });

  it('ラウドネス：長い音声で計り始めたら（Measuring）、表示の work が偽でも work: true で頼み続けて計りを進める', async () => {
    vi.mocked(measureEdit).mockImplementation(async () => ({
      type: 'measuring',
      samplesId: ID,
      progress: 0,
    }));
    // 範囲の小さい手の応え：peaks.pending も work も偽、loudness は Idle
    await open();
    vi.mocked(viewEdit).mockClear();
    vi.mocked(viewEdit).mockImplementation(async () =>
      result(view({ loudness: { type: 'measuring', samplesId: ID, progress: 0.3 } })),
    );
    await flush(WAVE_IDLE_MS);
    expect(viewEdit).toHaveBeenCalledWith(S, expect.objectContaining({ work: true }));
    const n = vi.mocked(viewEdit).mock.calls.length;
    await flush(WAVE_IDLE_MS);
    expect(vi.mocked(viewEdit).mock.calls.length).toBeGreaterThan(n);
    // 計り終えたら頼み直しは止まる
    vi.mocked(viewEdit).mockImplementation(async () => result(view({ loudness: READY(ID) })));
    await flush(WAVE_IDLE_MS);
    vi.mocked(viewEdit).mockClear();
    await flush(WAVE_IDLE_MS * 3);
    expect(viewEdit).not.toHaveBeenCalled();
    expect(useAudioStore.getState()).toMatchObject({ loudness: READY(ID), loudnessFresh: true });
  });

  it('ラウドネス：計っている途中に仮の手が入り、戻った表示が計っていなければ（Idle）、頼み直す', async () => {
    vi.mocked(measureEdit).mockImplementation(async () => ({
      type: 'measuring',
      samplesId: ID,
      progress: 0,
    }));
    await open();
    expect(measureEdit).toHaveBeenCalledTimes(1);
    // ドラッグの途中（仮の出力）
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result(view({ rev: 2, samplesId: 'ffff' }), {
        status: status({ rev: 2, provisional: true }),
      }),
    );
    pushEditorSteps([{ type: 'preview', edit: { type: 'setEnabled', node: 1, enabled: false } }]);
    await flush();
    // esc で戻った：A の表示は Idle（Rust が計りかけを捨てた）
    vi.mocked(stepEdit).mockResolvedValueOnce(result(view({ rev: 1 })));
    pushEditorSteps([{ type: 'cancelPreview' }]);
    await flush();
    expect(measureEdit).toHaveBeenCalledTimes(2);
  });

  it('重ねるもの：ループ・カーソル・選択・ドラッグの手元のループを出す', async () => {
    await open(view({ loop: { start: 10, end: 20 } }));
    setSelection({ start: 100, end: 200 });
    const o = overlayOf(useAudioStore.getState());
    expect(o.loop).toEqual({ start: 10, end: 20 });
    expect(o.selection).toEqual({ start: 100, end: 200 });
    setAudioLocal({ loop: { start: 30, end: 40 } });
    expect(overlayOf(useAudioStore.getState()).loop).toEqual({ start: 30, end: 40 });
  });
});

describe('フェードの曲線', () => {
  const fade = (
    id: number,
    enabled: boolean,
    inEnd: number,
    more: { inStart?: number | null; outStart?: number | null; outEnd?: number | null } = {},
  ) => ({
    id,
    op: 'audio.fade',
    enabled,
    inRate: 48_000,
    inFrames: TOTAL,
    marks: {
      type: 'fade' as const,
      inStart: 0,
      inEnd,
      outStart: null,
      outEnd: null,
      ...more,
    },
  });
  const doc = (nodes: { id: number; enabled: boolean }[]) => ({
    order: nodes.map((n) => n.id),
    nodes: Object.fromEntries(
      nodes.map((n) => [
        n.id,
        { id: n.id, op: 'audio.fade', v: 1, enabled: n.enabled, params: {}, log: null },
      ]),
    ),
    outputs: [],
  });

  it('外したフェードの曲線は、フェードの道具でそのノードを直しているときだけ描く（ほかは有効なノードの曲線）', async () => {
    await open(view({ nodes: [fade(1, true, 1000), fade(2, false, 2000)] }));
    useEditorStore.setState({
      doc: doc([
        { id: 1, enabled: true },
        { id: 2, enabled: false },
      ]),
      selected: null,
    });
    // 選択の道具：最後の（外した）フェードではなく、有効なフェードの曲線
    expect(overlayOf(useAudioStore.getState()).fade?.inEnd).toBe(1000);
    // 外したフェードだけ：曲線を描かない
    useEditorStore.setState({ doc: doc([{ id: 2, enabled: false }]) });
    useAudioStore.setState({
      view: {
        ...(useAudioStore.getState().view as AudioViewSummary),
        nodes: [fade(2, false, 2000)],
      },
    });
    expect(overlayOf(useAudioStore.getState()).fade).toBeNull();
    // フェードの道具でそのノードを直している：出す（ハンドルも）
    useAudioStore.setState({ tool: 'fade' });
    expect(overlayOf(useAudioStore.getState()).fade).toMatchObject({ inEnd: 2000, handles: true });
  });

  it('曲線の見かけの始まり・終わりは印のまま渡す（下流で切られると出力の外・写せなければ null）。ノードがなければ 0 と出力の長さ', async () => {
    await open(
      view({
        nodes: [
          fade(1, true, 2000, { inStart: -1000, outStart: TOTAL - 500, outEnd: TOTAL + 700 }),
        ],
      }),
    );
    useEditorStore.setState({ doc: doc([{ id: 1, enabled: true }]), selected: null });
    expect(overlayOf(useAudioStore.getState()).fade).toMatchObject({
      inStart: -1000,
      inEnd: 2000,
      outStart: TOTAL - 500,
      outEnd: TOTAL + 700,
    });
    // 写せない：null のまま（曲線を描かない）。三角の位置は今のまま
    useAudioStore.setState({
      view: {
        ...(useAudioStore.getState().view as AudioViewSummary),
        nodes: [fade(1, true, 2000, { inStart: null, outStart: TOTAL - 500, outEnd: null })],
      },
    });
    expect(overlayOf(useAudioStore.getState()).fade).toMatchObject({
      inStart: null,
      inEnd: 2000,
      outStart: TOTAL - 500,
      outEnd: null,
    });
    // フェードのノードがない（フェードの道具で足す）：末尾に足すので 0 と出力の長さ
    useEditorStore.setState({ doc: doc([]) });
    useAudioStore.setState({
      tool: 'fade',
      view: { ...(useAudioStore.getState().view as AudioViewSummary), nodes: [] },
    });
    setAudioLocal({ fadeIn: 3000 });
    expect(overlayOf(useAudioStore.getState()).fade).toMatchObject({
      inStart: 0,
      inEnd: 3000,
      outStart: TOTAL,
      outEnd: TOTAL,
    });
  });
});

describe('編集中の試聴と排他', () => {
  async function deliver(index: number): Promise<void> {
    const v = useAudioStore.getState().view;
    const url = editPcmUrl(S, v?.rev ?? 1, index * CHUNK, Math.min(TOTAL, (index + 1) * CHUNK));
    io.resolve(url, pcmBytes(index, TOTAL, 2, `kura-edit/1:${v?.samplesId}`));
    await flush();
  }

  it('Space：プレーヤーを一時停止・試聴と効果音を止め、出力の PCM を取って鳴らす（文脈に使用中の印）', async () => {
    await open();
    usePlayerStore.setState({
      active: true,
      snapshot: { ...usePlayerStore.getState().snapshot, playing: true },
    });
    togglePlay();
    expect(pause).toHaveBeenCalled();
    expect(runtime.audition.stop).toHaveBeenCalled();
    expect(runtime.sfx.stop).toHaveBeenCalled();
    expect(useAudioStore.getState().play).toEqual({ status: 'playing', mode: 'cursor' });
    expect(runtime.pool.isHeld(48_000)).toBe(true);
    await deliver(0);
    const ctx = factory.contexts[0];
    expect(ctx?.sources.length).toBeGreaterThan(0);
    expect(ctx?.sources[0]?.buffer?.length).toBe(CHUNK);
    expect(ctx?.touched).toEqual([]);
  });

  it('プレーヤーが鳴り始めたら止め、止めた所を新しいカーソルにする', async () => {
    await open();
    togglePlay();
    await deliver(0);
    const ctx = factory.contexts[0];
    if (!ctx) throw new Error('文脈がない');
    ctx.currentTime += 0.5;
    usePlayerStore.setState({
      active: true,
      snapshot: { ...usePlayerStore.getState().snapshot, playing: true },
    });
    expect(audioPlayer()?.playing).toBe(false);
    expect(useAudioStore.getState().play.status).toBe('idle');
    expect(useAudioStore.getState().cursor).toBeGreaterThan(0);
    expect(runtime.pool.isHeld(48_000)).toBe(false);
  });

  it('鳴らしている間にチャンネル数が変わったら（mono を足す・⌘Z で外す）、新しい形で始め直す', async () => {
    await open();
    togglePlay();
    await deliver(0);
    const ctx = factory.contexts[0];
    if (!ctx) throw new Error('文脈がない');
    ctx.currentTime += 0.2;
    // mono を足した結果：rate・frames は同じで channels だけ 1
    const mono = view({ rev: 2, channels: 1, samplesId: 'aaaa', changed: { type: 'all' } });
    vi.mocked(stepEdit).mockResolvedValueOnce(result(mono));
    pushEditorSteps([{ type: 'undo' }]);
    await flush();
    const before = ctx.sources.length;
    io.resolve(editPcmUrl(S, 2, 0, CHUNK), pcmBytes(0, TOTAL, 1, 'kura-edit/1:aaaa'));
    await flush();
    expect(ctx.sources.length).toBeGreaterThan(before);
    expect(ctx.buffers.at(-1)?.numberOfChannels).toBe(1);
    expect(audioPlayer()?.playing).toBe(true);
    // ⌘Z で外した結果：channels 2
    ctx.currentTime += 0.2;
    const stereo = view({ rev: 3, channels: 2, samplesId: 'bbbb', changed: { type: 'all' } });
    vi.mocked(stepEdit).mockResolvedValueOnce(result(stereo));
    editorUndo();
    await flush();
    const n = ctx.sources.length;
    io.resolve(editPcmUrl(S, 3, 0, CHUNK), pcmBytes(0, TOTAL, 2, 'kura-edit/1:bbbb'));
    await flush();
    expect(ctx.sources.length).toBeGreaterThan(n);
    expect(ctx.buffers.at(-1)?.numberOfChannels).toBe(2);
    // 鳴り終えたら止まる（無音のまま「再生中」で固まらない）
    io.resolve(editPcmUrl(S, 3, CHUNK, TOTAL), pcmBytes(1, TOTAL, 2, 'kura-edit/1:bbbb'));
    await flush();
    ctx.currentTime += 3;
    audioPlayer()?.tick();
    expect(audioPlayer()?.playing).toBe(false);
    expect(useAudioStore.getState().play.status).toBe('idle');
  });

  it('形が変わって始め直せない（3 kHz 未満・写せない）ときは止めて知らせ、先読みの区間を外す', async () => {
    await open();
    togglePlay();
    await deliver(0);
    const c = audioController();
    const ctx = factory.contexts[0];
    if (!c || !ctx) throw new Error('面・文脈がない');
    expect(c.viewport().spans.length).toBeGreaterThan(0);
    ctx.currentTime += 0.2;
    // 打っている途中の 2205 Hz の結果
    const low = view({
      rev: 2,
      rate: 2205,
      frames: 4410,
      valid: [{ start: 0, end: 4410 }],
      changed: { type: 'all' },
      sourceMap: [{ outStart: 0, outEnd: 4410, inStart: 0, inRate: 48_000, outRate: 2205 }],
    });
    vi.mocked(stepEdit).mockResolvedValueOnce(result(low));
    pushEditorSteps([{ type: 'undo' }]);
    await flush();
    expect(audioPlayer()?.playing).toBe(false);
    expect(useAudioStore.getState().play.status).toBe('idle');
    expect(useNoticeStore.getState().message).toBe(ja.editor.audio.rateUnsupported('2205'));
    expect(c.viewport().spans).toEqual([]);
    // Space で鳴らし始められる（止まった状態から）
    vi.mocked(stepEdit).mockResolvedValueOnce(result(view({ rev: 3, changed: { type: 'all' } })));
    pushEditorSteps([{ type: 'undo' }]);
    await flush();
    togglePlay();
    expect(audioPlayer()?.playing).toBe(true);
    // 頭を切り詰めて今の位置を写せない：止めて先読みの区間を外す
    await deliver(0);
    ctx.currentTime += 0.5;
    const cut = view({
      rev: 4,
      frames: 48_000,
      valid: [{ start: 0, end: 48_000 }],
      changed: { type: 'all' },
      sourceMap: [
        { outStart: 0, outEnd: 48_000, inStart: 48_000, inRate: 48_000, outRate: 48_000 },
      ],
    });
    vi.mocked(stepEdit).mockResolvedValueOnce(result(cut));
    pushEditorSteps([{ type: 'undo' }]);
    await flush();
    expect(audioPlayer()?.playing).toBe(false);
    expect(useAudioStore.getState().play.status).toBe('idle');
    expect(c.viewport().spans).toEqual([]);
  });

  it('ループの試聴は、欄・I/O・［選択をループにする］・⌘Z で替わったループ点に従い、消したら止まる', async () => {
    await open(view({ loop: { start: 1000, end: 2000 } }));
    toggleLoopAudition();
    const player = audioPlayer();
    expect(player?.loop).toEqual({ start: 1000, end: 2000 });
    expect(useAudioStore.getState().play).toEqual({ status: 'playing', mode: 'loop' });
    const answer = async (
      action: () => void,
      loop: { start: number; end: number } | null,
      rev: number,
    ) => {
      vi.mocked(stepEdit).mockResolvedValueOnce(result(view({ rev, loop })));
      action();
      await flush();
    };
    // 時刻の欄（merge）
    await answer(() => setLoopFromField({ start: 3000, end: 4000 }), { start: 3000, end: 4000 }, 2);
    expect(player?.loop).toEqual({ start: 3000, end: 4000 });
    // ⌘Z
    await answer(editorUndo, { start: 5000, end: 6000 }, 3);
    expect(player?.loop).toEqual({ start: 5000, end: 6000 });
    // ［選択をループにする］
    setSelection({ start: 7000, end: 9000 });
    await answer(selectionToLoop, { start: 7000, end: 9000 }, 4);
    expect(player?.loop).toEqual({ start: 7000, end: 9000 });
    // I/O（ループの道具）
    selectAudioTool('loop');
    await answer(() => setInOut('out'), { start: 7000, end: 8000 }, 5);
    expect(player?.loop).toEqual({ start: 7000, end: 8000 });
    expect(player?.playing).toBe(true);
    // ［ループ点を消す］：試聴を止める
    await answer(clearLoop, null, 6);
    expect(player?.playing).toBe(false);
    expect(useAudioStore.getState().play.status).toBe('idle');
  });

  it('ループの試聴：断られた手のあとは、ドラッグの手元の値をやめて文書のループ点で鳴らす', async () => {
    await open(view({ loop: { start: 1000, end: 2000 } }));
    toggleLoopAudition();
    const player = audioPlayer();
    setAudioLocal({ loop: { start: 1200, end: 1800 } });
    player?.setLoop({ start: 1200, end: 1800 });
    expect(player?.loop).toEqual({ start: 1200, end: 1800 });
    vi.mocked(stepEdit).mockResolvedValueOnce(
      result(view({ loop: { start: 1000, end: 2000 } }), {
        rejected: { index: 0, problem: { code: 'Invalid', message: '写せません', issues: [] } },
      }),
    );
    pushEditorSteps([{ type: 'commitPreview', mode: 'step' }]);
    await flush();
    expect(player?.loop).toEqual({ start: 1000, end: 2000 });
  });

  it('聞き比べ：写せない位置で B を押しても替わらない（表示も鳴る音も元のまま）。止まったら鳴らした音で写す', async () => {
    // 出力は元の [0, 48000) に切り詰めたもの
    const cut = view({
      frames: 48_000,
      valid: [{ start: 0, end: 48_000 }],
      sourceMap: [{ outStart: 0, outEnd: 48_000, inStart: 0, inRate: 48_000, outRate: 48_000 }],
    });
    await open(cut);
    toggleAB();
    expect(useAudioStore.getState().ab).toBe('source');
    setCursor(40_000);
    togglePlay();
    expect(audioPlayer()?.source).toBe('source');
    io.resolve(editInputPcmUrl(S, 0, CHUNK), pcmBytes(0, TOTAL, 2, 'wav-kura-dec/1'));
    io.resolve(editInputPcmUrl(S, CHUNK, TOTAL), pcmBytes(1, TOTAL, 2, 'wav-kura-dec/1'));
    await flush();
    const ctx = factory.contexts[0];
    if (!ctx) throw new Error('文脈がない');
    // 元の 66,000 あたり（切り詰めの外）
    ctx.currentTime += 0.55;
    audioPlayer()?.tick();
    expect(audioPlayer()?.position()).toBeGreaterThan(48_000);
    toggleAB();
    expect(useAudioStore.getState().ab).toBe('source');
    expect(audioPlayer()?.source).toBe('source');
    expect(useNoticeStore.getState().message).toBe(ja.editor.audio.abUnavailable);
    // 元の終わりまで鳴ったら、元の位置は出力へ写せないのでカーソルは動かさない
    ctx.currentTime += 2;
    audioPlayer()?.tick();
    expect(audioPlayer()?.playing).toBe(false);
    expect(useAudioStore.getState().cursor).toBeLessThanOrEqual(48_000);
  });

  it('PCM を取れなかったチャンクは時間をおいて取り直し、続けて 3 回取れなければ止めて知らせる', async () => {
    await open();
    togglePlay();
    await deliver(0);
    const url = editPcmUrl(S, 1, CHUNK, TOTAL);
    for (const wait of [1000, 2000]) {
      io.reject(url, new Error('HTTP 503'));
      await flush(wait - 100);
      expect(io.pending()).not.toContain(url);
      await flush(100);
      expect(io.pending()).toContain(url);
      expect(audioPlayer()?.playing).toBe(true);
    }
    io.reject(url, new Error('HTTP 503'));
    await flush();
    expect(audioPlayer()?.playing).toBe(false);
    expect(useAudioStore.getState().play.status).toBe('idle');
    expect(useNoticeStore.getState().message).toBe(
      ja.editor.audio.playFailed(ja.editor.audio.pcmUnavailable),
    );
    expect(audioController()?.viewport().spans).toEqual([]);
  });

  it('選択があれば選択の始めから終わりまで鳴らす', async () => {
    await open();
    setSelection({ start: 1000, end: 2000 });
    togglePlay();
    expect(useAudioStore.getState().play.mode).toBe('selection');
    await deliver(0);
    expect(factory.contexts[0]?.sources[0]?.buffer?.length).toBe(1000);
    // 途中で止めたら止めた所がカーソル
    togglePlay();
    expect(useAudioStore.getState().play.status).toBe('idle');
    expect(useAudioStore.getState().cursor).toBe(1000);
  });
});
