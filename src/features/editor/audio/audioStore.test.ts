/**
 * 音声のアダプタと状態（M13 の段 6）を偽の IPC・偽の WebAudio・偽の取得で試す。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

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
import { editPcmUrl } from '../../../ipc/media';
import { ContextPool } from '../../../lib/assetAudio';
import { fakeFactory } from '../../../test/fakeAudio';
import { type AudioRuntime, setAudioRuntimeForTest } from '../../asset/audioRuntime';
import { playerController } from '../../player/playerActions';
import { usePlayerStore } from '../../player/playerStore';
import { closeEditor, editorUndo, openEditorAs, pushEditorSteps } from '../editorStore';
import {
  AUDIO_ADAPTER,
  audioController,
  audioPlayer,
  overlayOf,
  setAudioDragging,
  setAudioIoForTest,
  setAudioLocal,
  setSelection,
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
import { CHUNK } from './pcmChunks';
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
