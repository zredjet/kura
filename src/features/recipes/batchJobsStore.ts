/**
 * 一括のジョブ（`kind = 'batch'`。M14 段 7）：グリッドの下のカプセルに出す。進捗のイベントを受け、結果（向き・レシピの名前・
 * 件数）を 1 秒に 1 回まで読み直す。当てたアイテムはタイルの詳細を読み直し、別名で作ったアイテムは一覧を読み直す。
 * ライブラリを開いた・替えたときに終わっていない一括を読むので、一時停止した一括はアプリを開き直しても残る。
 */

import { create } from 'zustand';

import { type BatchResultDto, batchResult } from '../../ipc/batch';
import { type JobProgress, listJobs, progressFromJob } from '../../ipc/jobs';
import { invalidateItems } from '../../stores/itemCache';
import { reloadList } from '../../stores/jobsStore';

/** 結果を読み直す間隔の下限（ms）。 */
const RESULT_REFRESH_MS = 1000;

export interface BatchJob {
  id: number;
  state: JobProgress['state'];
  progress: number | null;
  message: string | null;
  /** 最後に読んだ結果（まだ読めていなければ null）。 */
  result: BatchResultDto | null;
}

interface BatchJobsState {
  jobs: Record<number, BatchJob>;
  /** 画面から消した一括。 */
  dismissed: Record<number, true>;
  apply: (event: JobProgress) => void;
  dismiss: (id: number) => void;
  /** 終わっていない一括を読む（ライブラリを開いた・替えたとき）。 */
  load: () => Promise<void>;
  clear: () => void;
}

/** 終わった状態か。 */
export function isFinished(state: JobProgress['state']): boolean {
  return state === 'done' || state === 'failed' || state === 'canceled';
}

/** 一括ごとの、読み直しの予定と、もう読み直したアイテム（タイルの詳細を読み直したもの）。 */
const timers = new Map<number, ReturnType<typeof setTimeout>>();
const lastRead = new Map<number, number>();
const seenSaved = new Map<number, Set<number>>();
let epoch = 0;

/** 結果から、まだ読み直していない当てたアイテムを取り出す（別名は作ったアイテム）。 */
export function freshlySaved(job: number, result: BatchResultDto): number[] {
  const seen = seenSaved.get(job) ?? new Set<number>();
  seenSaved.set(job, seen);
  const out: number[] = [];
  const saved = result.groups
    .filter((g) => g.state === 'saved' || g.state === 'unchanged')
    .flatMap((g) => g.items);
  for (const id of [...saved, ...result.created]) {
    if (!seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  return out;
}

async function readResult(id: number): Promise<void> {
  const mine = epoch;
  lastRead.set(id, Date.now());
  try {
    const result = await batchResult(id);
    if (mine !== epoch) return;
    const fresh = freshlySaved(id, result);
    if (fresh.length > 0) {
      invalidateItems(fresh);
      reloadList();
    }
    useBatchJobsStore.setState((s) => {
      const job = s.jobs[id];
      return job ? { jobs: { ...s.jobs, [id]: { ...job, result } } } : s;
    });
  } catch (error) {
    console.warn('一括の結果を読めませんでした', error);
  }
}

/** 結果を読み直す（状態が変わったらすぐ、進みだけなら 1 秒に 1 回まで）。 */
function scheduleRead(id: number, now: boolean): void {
  const pending = timers.get(id);
  if (now) {
    if (pending) {
      clearTimeout(pending);
      timers.delete(id);
    }
    void readResult(id);
    return;
  }
  if (pending) return;
  const wait = Math.max(0, (lastRead.get(id) ?? 0) + RESULT_REFRESH_MS - Date.now());
  timers.set(
    id,
    setTimeout(() => {
      timers.delete(id);
      void readResult(id);
    }, wait),
  );
}

export const useBatchJobsStore = create<BatchJobsState>((set, get) => ({
  jobs: {},
  dismissed: {},
  apply: (event) => {
    if (event.kind !== 'batch') return;
    const previous = get().jobs[event.id];
    const next: BatchJob = {
      id: event.id,
      state: event.state,
      progress: event.progress ?? previous?.progress ?? null,
      message: event.message ?? previous?.message ?? null,
      result: previous?.result ?? null,
    };
    set((s) => ({ jobs: { ...s.jobs, [event.id]: next } }));
    scheduleRead(event.id, !previous || previous.state !== event.state);
  },
  dismiss: (id) => set((s) => ({ dismissed: { ...s.dismissed, [id]: true } })),
  load: async () => {
    try {
      const jobs = await listJobs({ kinds: ['batch'], states: ['queued', 'running', 'paused'] });
      const apply = get().apply;
      for (const job of jobs) apply(progressFromJob(job));
    } catch (error) {
      console.warn('一括のジョブを読めませんでした', error);
    }
  },
  clear: () => {
    epoch++;
    for (const timer of timers.values()) clearTimeout(timer);
    timers.clear();
    lastRead.clear();
    seenSaved.clear();
    set({ jobs: {}, dismissed: {} });
  },
}));

/** カプセルに出す一括（消していないもの。古い順）。 */
export function shownBatches(state: Pick<BatchJobsState, 'jobs' | 'dismissed'>): BatchJob[] {
  return Object.values(state.jobs)
    .filter((job) => !state.dismissed[job.id])
    .sort((a, b) => a.id - b.id);
}
