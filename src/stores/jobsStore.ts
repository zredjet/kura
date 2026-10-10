import { create } from 'zustand';

import { listJobs, type JobProgress, onJobProgress, progressFromJob } from '../ipc/jobs';

interface JobsState {
  /** 取り込みジョブの最新の状態（ID ごと）。 */
  imports: Record<number, JobProgress>;
  /** 画面から消したジョブ。 */
  dismissed: Record<number, true>;
  /** 進捗イベントを反映する。 */
  apply: (event: JobProgress) => void;
  /** 表示から消す。 */
  dismiss: (id: number) => void;
  /** 終わっていない取り込みジョブを読み込む（起動時に再開したもの）。 */
  load: () => Promise<void>;
}

export const useJobsStore = create<JobsState>((set) => ({
  imports: {},
  dismissed: {},
  apply: (event) => {
    if (event.kind !== 'import') {
      return;
    }
    set((state) => {
      const previous = state.imports[event.id];
      const next: JobProgress = {
        ...event,
        // 状態の変わり目のイベントは進捗を持たないことがあるので、前の値を引き継ぐ
        progress: event.progress ?? previous?.progress ?? null,
        message: event.message ?? previous?.message ?? null,
        resultJson: event.resultJson ?? previous?.resultJson ?? null,
      };
      return { imports: { ...state.imports, [event.id]: next } };
    });
  },
  dismiss: (id) => set((state) => ({ dismissed: { ...state.dismissed, [id]: true } })),
  load: async () => {
    try {
      const jobs = await listJobs({ kinds: ['import'], states: ['queued', 'running', 'paused'] });
      const apply = useJobsStore.getState().apply;
      jobs.forEach((job) => apply(progressFromJob(job)));
    } catch (error) {
      console.warn('ジョブを読めませんでした', error);
    }
  },
}));

/** 画面に出す取り込みジョブ（消していないもののうち新しいもの）。 */
export function selectCurrentImport(state: JobsState): JobProgress | null {
  const candidates = Object.values(state.imports)
    .filter((job) => !state.dismissed[job.id])
    .sort((a, b) => b.id - a.id);
  return candidates[0] ?? null;
}

/** 進捗イベントの受け取りを始める。戻り値で止める。 */
export function startJobEvents(): () => void {
  let stop: (() => void) | null = null;
  let stopped = false;
  onJobProgress((event) => useJobsStore.getState().apply(event))
    .then((unlisten) => {
      if (stopped) {
        unlisten();
      } else {
        stop = unlisten;
      }
    })
    .catch((error: unknown) => console.warn('進捗イベントを受け取れませんでした', error));
  return () => {
    stopped = true;
    stop?.();
  };
}
