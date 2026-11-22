import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { listPendingSaves } from '../ipc/edit';
import { type JobProgress, onJobProgress } from '../ipc/jobs';
import { refreshAnalyzing, refreshThumbnails } from './itemCache';
import {
  onAnyJob,
  reactToJob,
  SAVE_CAPSULE_DELAY_MS,
  type SaveJob,
  selectCurrentImport,
  nextSaveProgress,
  selectItemSave,
  shownSaves,
  startJobEvents,
  useJobsStore,
} from './jobsStore';

vi.mock('../ipc/jobs', async (original) => ({
  ...(await original<typeof import('../ipc/jobs')>()),
  onJobProgress: vi.fn(async () => () => undefined),
}));

vi.mock('../ipc/edit', async (original) => ({
  ...(await original<typeof import('../ipc/edit')>()),
  listPendingSaves: vi.fn(async () => []),
}));

vi.mock('./itemCache', async (original) => ({
  ...(await original<typeof import('./itemCache')>()),
  refreshAnalyzing: vi.fn(),
  refreshThumbnails: vi.fn(),
}));

function event(over: Partial<JobProgress>): JobProgress {
  return {
    id: 1,
    kind: 'import',
    state: 'running',
    progress: null,
    message: null,
    resultJson: null,
    ...over,
  };
}

beforeEach(() => {
  useJobsStore.setState({ imports: {}, dismissed: {}, saves: {}, endedSaves: {} });
  vi.mocked(listPendingSaves).mockReset();
  vi.mocked(listPendingSaves).mockResolvedValue([]);
});

describe('jobsStore', () => {
  it('取り込みのイベントだけを、前の進捗を引き継いで反映する', () => {
    const { apply } = useJobsStore.getState();
    apply(event({ progress: 0.4, message: '4 / 10 項目' }));
    apply(event({ state: 'paused' }));
    apply(event({ id: 2, kind: 'thumbnail' }));
    const job = useJobsStore.getState().imports[1];
    expect(job).toMatchObject({ state: 'paused', progress: 0.4, message: '4 / 10 項目' });
    expect(useJobsStore.getState().imports[2]).toBeUndefined();
  });

  it('消していないもののうち新しいものを出す', () => {
    const { apply, dismiss } = useJobsStore.getState();
    apply(event({ id: 3, state: 'done' }));
    apply(event({ id: 5, state: 'queued' }));
    expect(selectCurrentImport(useJobsStore.getState())?.id).toBe(5);
    dismiss(5);
    expect(selectCurrentImport(useJobsStore.getState())?.id).toBe(3);
    dismiss(3);
    expect(selectCurrentImport(useJobsStore.getState())).toBeNull();
  });
});

describe('reactToJob', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.mocked(refreshAnalyzing).mockClear();
    vi.mocked(refreshThumbnails).mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('解析のジョブが進んだら、解析待ちのものをまとめて取り直す（透過・ドット絵・印が届く）', () => {
    reactToJob(event({ id: 7, kind: 'analyze', progress: 0.1 }));
    reactToJob(event({ id: 7, kind: 'analyze', progress: 0.2 }));
    reactToJob(event({ id: 7, kind: 'analyze', state: 'done' }));
    expect(refreshAnalyzing).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    expect(refreshAnalyzing).toHaveBeenCalledTimes(1);
    expect(refreshThumbnails).not.toHaveBeenCalled();
  });

  it('サムネイルのジョブが進んだら、まだのもの・作り直し中のものを取り直す', () => {
    reactToJob(event({ id: 8, kind: 'thumbnail' }));
    vi.advanceTimersByTime(500);
    expect(refreshThumbnails).toHaveBeenCalledTimes(1);
    expect(refreshAnalyzing).not.toHaveBeenCalled();
  });
});

describe('onAnyJob', () => {
  it('進捗イベントをほかの画面（画像エディタの保存）へ回し、外したら回さない', async () => {
    const stop = startJobEvents();
    const handler = vi.mocked(onJobProgress).mock.calls[0]?.[0];
    const seen: JobProgress[] = [];
    const off = onAnyJob((e) => seen.push(e));
    handler?.(event({ id: 9, kind: 'edit', state: 'done' }));
    off();
    handler?.(event({ id: 10, kind: 'edit', state: 'done' }));
    expect(seen.map((e) => e.id)).toEqual([9]);
    stop();
  });
});

describe('保存のジョブ（カプセル・開き直したエディタ）', () => {
  /** 積んだばかりの保存を覚える。 */
  function track(over: Partial<Omit<SaveJob, 'shown'>> = {}) {
    useJobsStore.getState().trackSave({
      id: 9,
      itemId: 7,
      itemTitle: 'long-60min',
      mode: 'overwrite',
      newTitle: null,
      state: 'queued',
      progress: null,
      message: null,
      ...over,
    });
  }
  const edit = (over: Partial<JobProgress>) => event({ id: 9, kind: 'edit', ...over });
  const saves = () => useJobsStore.getState().saves;
  const shown = () => shownSaves(saves()).map((j) => j.id);

  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('一瞬で終わる保存は出さず、1 秒動いている保存だけを出す', () => {
    track();
    useJobsStore.getState().apply(edit({ state: 'running', progress: 0.5 }));
    expect(shown()).toEqual([]);
    vi.advanceTimersByTime(SAVE_CAPSULE_DELAY_MS - 1);
    expect(shown()).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(shown()).toEqual([9]);
    expect(saves()[9]).toMatchObject({ state: 'running', progress: 0.5 });
  });

  it('1 秒より前に終わったら出さず、終わったら消す（完了・失敗・中止の状態を持たない）', () => {
    track();
    useJobsStore.getState().apply(edit({ state: 'done', progress: 1 }));
    vi.advanceTimersByTime(SAVE_CAPSULE_DELAY_MS * 2);
    expect(saves()).toEqual({});

    for (const [id, state] of [
      [10, 'failed'],
      [11, 'canceled'],
    ] as const) {
      track({ id });
      vi.advanceTimersByTime(SAVE_CAPSULE_DELAY_MS);
      expect(shown()).toEqual([id]);
      useJobsStore.getState().apply(edit({ id, state }));
      expect(shown()).toEqual([]);
    }
  });

  it('一時停止中はすぐ出す。再開しても消えず、進捗は頭から', () => {
    track();
    useJobsStore
      .getState()
      .apply(edit({ state: 'running', progress: 0.42, message: '描いています' }));
    // 一時停止のイベントは進捗を持たない（DB に進捗を書かないので）：前の値を引き継ぐ
    useJobsStore.getState().apply(edit({ state: 'paused', progress: null, message: null }));
    expect(shown()).toEqual([9]);
    expect(saves()[9]).toMatchObject({ state: 'paused', progress: 0.42 });

    useJobsStore.getState().apply(edit({ state: 'queued', progress: null }));
    expect(shown()).toEqual([9]);
    expect(saves()[9]).toMatchObject({ state: 'queued', progress: null, message: null });
    useJobsStore.getState().apply(edit({ state: 'running', progress: null }));
    expect(saves()[9]?.progress).toBeNull();
    useJobsStore.getState().apply(edit({ state: 'running', progress: 0.05 }));
    expect(saves()[9]?.progress).toBe(0.05);
  });

  it('一時停止のイベントは止めた時点の進捗を保つ（DB の古い値に戻さない）。前の値がなければイベントの値', () => {
    track();
    useJobsStore.getState().apply(edit({ state: 'running', progress: 0.7 }));
    useJobsStore.getState().apply(edit({ state: 'paused', progress: 0.36, message: null }));
    expect(saves()[9]).toMatchObject({ state: 'paused', progress: 0.7 });
    expect(
      nextSaveProgress({ state: 'queued', progress: null }, { state: 'paused', progress: 0.36 }),
    ).toBe(0.36);
  });

  it('取り込み・書き出しの表には入れない。知らないジョブのイベントでは増やさない', () => {
    useJobsStore.getState().apply(edit({ state: 'running' }));
    expect(useJobsStore.getState().imports).toEqual({});
    expect(saves()).toEqual({});
  });

  it('積む応えより先に終わりのイベントが届いても、動いていることにしない', () => {
    useJobsStore.getState().apply(edit({ state: 'done' }));
    track();
    expect(saves()).toEqual({});
  });

  it('アイテムの保存を引ける', () => {
    track();
    track({ id: 10, itemId: 8 });
    const { saves: all } = useJobsStore.getState();
    expect(selectItemSave(all, 7)?.id).toBe(9);
    expect(selectItemSave(all, 8)?.id).toBe(10);
    expect(selectItemSave(all, 1)).toBeNull();
  });

  it('ライブラリを開いたら終わっていない保存を読み直す（前のライブラリのものは捨てる）。別名は新しい名前を持つ', async () => {
    track({ id: 3 });
    vi.mocked(listPendingSaves).mockResolvedValue([
      {
        job: 21,
        itemId: 7,
        itemTitle: 'long-60min',
        mode: { type: 'overwrite' },
        state: 'paused',
        progress: null,
        message: null,
      },
      {
        job: 22,
        itemId: 8,
        itemTitle: 'hero',
        mode: { type: 'saveAs', title: '夕方' },
        state: 'queued',
        progress: null,
        message: null,
      },
      {
        job: 23,
        itemId: 9,
        itemTitle: null,
        mode: { type: 'revert' },
        state: 'running',
        progress: 0.5,
        message: '書いています',
      },
    ]);
    await useJobsStore.getState().reloadSaves();
    expect(saves()[3]).toBeUndefined();
    expect(saves()[21]).toMatchObject({ itemId: 7, mode: 'overwrite', newTitle: null });
    expect(saves()[22]).toMatchObject({ itemId: 8, mode: 'saveAs', newTitle: '夕方' });
    expect(saves()[23]).toMatchObject({ itemTitle: null, mode: 'revert', progress: 0.5 });
    // 一時停止中はすぐ、ほかは 1 秒たってから
    expect(shown()).toEqual([21]);
    vi.advanceTimersByTime(SAVE_CAPSULE_DELAY_MS);
    expect(shown()).toEqual([21, 22, 23]);
  });

  it('読み直しの応えより先に届いたイベントを優先し、ライブラリを替えたあとの応えは捨てる', async () => {
    let answer: (list: Awaited<ReturnType<typeof listPendingSaves>>) => void = () => undefined;
    const row = {
      itemId: 7,
      itemTitle: 'a',
      mode: { type: 'overwrite' as const },
      state: 'running' as const,
      progress: null,
      message: null,
    };
    vi.mocked(listPendingSaves).mockImplementationOnce(
      () => new Promise((resolve) => (answer = resolve)),
    );
    const loading = useJobsStore.getState().reloadSaves();
    // 応えを待つ間に：ジョブ 21 は終わり、ジョブ 22 は積んで一時停止した
    useJobsStore.getState().apply(edit({ id: 21, state: 'done' }));
    track({ id: 22 });
    useJobsStore.getState().apply(edit({ id: 22, state: 'paused' }));
    answer([
      { ...row, job: 21 },
      { ...row, job: 22 },
    ]);
    await loading;
    expect(saves()[21]).toBeUndefined();
    expect(saves()[22]?.state).toBe('paused');

    // ライブラリを替えた（読み直しを頼み直した）あとに前の応えが届いても使わない
    vi.mocked(listPendingSaves).mockImplementationOnce(
      () => new Promise((resolve) => (answer = resolve)),
    );
    const stale = useJobsStore.getState().reloadSaves();
    const staleAnswer = answer;
    await useJobsStore.getState().reloadSaves();
    staleAnswer([{ ...row, job: 30 }]);
    await stale;
    expect(saves()).toEqual({});
  });

  it('読み直し：頼む前から知っていて一覧にないジョブは終わったものとして消す。頼んだあとに積んだものは消さない。動いているものの状態は替えない', async () => {
    track({ id: 1 });
    track({ id: 2, state: 'running', progress: 0.5 });
    let answer: (list: Awaited<ReturnType<typeof listPendingSaves>>) => void = () => undefined;
    vi.mocked(listPendingSaves).mockImplementationOnce(
      () => new Promise((resolve) => (answer = resolve)),
    );
    const reading = useJobsStore.getState().readSaves();
    track({ id: 3 });
    answer([
      {
        job: 2,
        itemId: 7,
        itemTitle: 'a',
        mode: { type: 'overwrite' },
        state: 'queued',
        progress: null,
        message: null,
      },
    ]);
    await reading;
    expect(saves()[1]).toBeUndefined();
    expect(saves()[2]).toMatchObject({ state: 'running', progress: 0.5 });
    expect(saves()[3]).toBeDefined();
    // 消したジョブは一覧の古い応えで戻さない
    track({ id: 1 });
    expect(saves()[1]).toBeUndefined();
  });

  it('読めなくても止めない', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    vi.mocked(listPendingSaves).mockRejectedValueOnce(new Error('読めない'));
    await useJobsStore.getState().reloadSaves();
    expect(saves()).toEqual({});
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });
});
