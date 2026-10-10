import { create } from 'zustand';

import { listJobs, type JobProgress, onJobProgress, progressFromJob } from '../ipc/jobs';
import { refreshPending } from './itemCache';
import { useLibraryStore } from './libraryStore';

/** 取り込み中に一覧を読み直す間隔・サムネイルのでき具合を反映する間隔（ms）。 */
const LIST_RELOAD_MS = 1000;
const THUMB_REFRESH_MS = 500;

/** 呼ばれてから `ms` のあいだに何度呼ばれても 1 回にまとめる。 */
function coalesce(ms: number, run: () => void): () => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return () => {
    if (timer === null) {
      timer = setTimeout(() => {
        timer = null;
        run();
      }, ms);
    }
  };
}

const reloadList = coalesce(LIST_RELOAD_MS, () => {
  void useLibraryStore.getState().loadList();
  void useLibraryStore.getState().refresh();
});
const refreshThumbs = coalesce(THUMB_REFRESH_MS, refreshPending);

/** ジョブの進みに合わせて画面のデータを更新する（取り込み → 一覧、サムネイル → タイル）。 */
export function reactToJob(event: JobProgress): void {
  if (event.kind === 'import' && (event.state === 'running' || event.state === 'done')) {
    reloadList();
  } else if (event.kind === 'thumbnail') {
    refreshThumbs();
  }
}

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
  onJobProgress((event) => {
    useJobsStore.getState().apply(event);
    reactToJob(event);
  })
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
