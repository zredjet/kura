import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { type JobProgress, onJobProgress } from '../ipc/jobs';
import { refreshAnalyzing, refreshThumbnails } from './itemCache';
import {
  onAnyJob,
  reactToJob,
  selectCurrentImport,
  startJobEvents,
  useJobsStore,
} from './jobsStore';

vi.mock('../ipc/jobs', async (original) => ({
  ...(await original<typeof import('../ipc/jobs')>()),
  onJobProgress: vi.fn(async () => () => undefined),
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
  useJobsStore.setState({ imports: {}, dismissed: {} });
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
