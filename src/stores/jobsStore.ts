import { create } from 'zustand';

import { type EditPendingSaveDto, listPendingSaves } from '../ipc/edit';
import { listJobs, type JobProgress, onJobProgress, progressFromJob } from '../ipc/jobs';
import { useDownloadsStore } from './downloadsStore';
import { refreshAnalyzing, refreshThumbnails } from './itemCache';
import { useLibraryStore } from './libraryStore';
import { useSidebarStore } from './sidebarStore';

/** 取り込み中に一覧を読み直す間隔・サムネイルのでき具合・解析の結果を反映する間隔（ms）。 */
const LIST_RELOAD_MS = 1000;
const THUMB_REFRESH_MS = 500;
const ANALYSIS_REFRESH_MS = 1000;
/**
 * 保存のジョブをカプセルに出すまでの時間（ms）。画像の保存のように一瞬で終わるものでカプセルがちらつかないように（一時停止中は
 * すぐ出す）。
 */
export const SAVE_CAPSULE_DELAY_MS = 1000;
/** カプセルに並べる保存のジョブの数（古い順。動くのは 1 本ずつで、終わると次が出る）。 */
const MAX_SAVE_CAPSULES = 3;

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

/** 一覧・件数を読み直す（取り込み・取得・撮影でアイテムが増えたとき。続けて呼ばれても 1 回）。 */
export const reloadList = coalesce(LIST_RELOAD_MS, () => {
  void useLibraryStore.getState().loadList();
  void useLibraryStore.getState().refresh();
  void useSidebarStore.getState().load();
});
const refreshThumbs = coalesce(THUMB_REFRESH_MS, refreshThumbnails);
const refreshAnalysis = coalesce(ANALYSIS_REFRESH_MS, refreshAnalyzing);

/** ジョブの進みに合わせて画面のデータを更新する（取り込み → 一覧、サムネイル・解析 → タイル）。 */
export function reactToJob(event: JobProgress): void {
  if (event.kind === 'import' && (event.state === 'running' || event.state === 'done')) {
    reloadList();
  } else if ((event.kind === 'download' || event.kind === 'capture') && event.state === 'done') {
    // 取得・撮影したものがライブラリに入った
    reloadList();
  } else if (event.kind === 'thumbnail') {
    refreshThumbs();
  } else if (event.kind === 'analyze') {
    // 透過・ドット絵・アニメ・ループの印（市松模様・最近傍・タイルの印）が届く
    refreshAnalysis();
  }
}

/**
 * 終わっていない保存のジョブ（`kind = 'edit'`。グリッドの下のカプセルと、開き直したエディタの保存のボタンが読む。ADR-189）。
 * 終わったら表から消す（カプセルに完了の状態は出さない。知らせはエディタの保存の流れ）。
 */
export interface SaveJob {
  id: number;
  itemId: number;
  /** 保存しているアイテムの表示名（行がなければ null）。 */
  itemTitle: string | null;
  mode: 'overwrite' | 'saveAs' | 'revert';
  /** 別名で保存の名前。 */
  newTitle: string | null;
  state: JobProgress['state'];
  progress: number | null;
  message: string | null;
  /** カプセルに出す（`SAVE_CAPSULE_DELAY_MS` 動いた・一時停止した）。 */
  shown: boolean;
}

/**
 * 保存のジョブのイベントのあとの進捗。一時停止のイベントは進捗を持たない（ジョブの進捗は画面へ送るだけで DB に書かない）ので前の
 * 値を引き継ぎ、再開（一時停止から待ちへ）は初めから書き直すので進捗も頭から。
 */
export function nextSaveProgress(
  previous: { state: JobProgress['state']; progress: number | null },
  event: Pick<JobProgress, 'state' | 'progress'>,
): number | null {
  if (previous.state === 'paused' && event.state === 'queued') return null;
  return event.progress ?? previous.progress;
}

/** カプセルに出す保存のジョブ（古い順）。 */
export function shownSaves(saves: Record<number, SaveJob>): SaveJob[] {
  return Object.values(saves)
    .filter((job) => job.shown)
    .sort((a, b) => a.id - b.id)
    .slice(0, MAX_SAVE_CAPSULES);
}

/** アイテムの終わっていない保存のジョブ（同じアイテムは同時に 1 つ。`except` は今のエディタの保存）。 */
export function selectItemSave(
  saves: Record<number, SaveJob>,
  itemId: number,
  except?: number,
): SaveJob | null {
  return Object.values(saves).find((job) => job.itemId === itemId && job.id !== except) ?? null;
}

/** Rust の一覧の行を、表の形にする。 */
function saveFromDto(row: EditPendingSaveDto): Omit<SaveJob, 'shown'> {
  return {
    id: row.job,
    itemId: row.itemId,
    itemTitle: row.itemTitle,
    mode: row.mode.type,
    newTitle: row.mode.type === 'saveAs' ? row.mode.title : null,
    state: row.state,
    progress: row.progress,
    message: row.message,
  };
}

/** 読み直しの世代（ライブラリを替えたあとに届いた前のライブラリの一覧を捨てる）。 */
let savesEpoch = 0;

interface JobsState {
  /** 取り込み・書き出しのジョブの最新の状態（ID ごと）。 */
  imports: Record<number, JobProgress>;
  /** 画面から消したジョブ。 */
  dismissed: Record<number, true>;
  /** 終わっていない保存のジョブ（ID ごと）。 */
  saves: Record<number, SaveJob>;
  /** 終わったと分かった保存のジョブ（積む応え・一覧の応えより先に終わりが届いても、動いていることにしない）。 */
  endedSaves: Record<number, true>;
  /** 進捗イベントを反映する。 */
  apply: (event: JobProgress) => void;
  /** 表示から消す。 */
  dismiss: (id: number) => void;
  /** 終わっていない取り込み・書き出しのジョブを読み込む（起動時に再開したもの）。 */
  load: () => Promise<void>;
  /** 保存のジョブを表に載せる（積んだ・一覧で知った。知っている・終わったジョブなら何もしない）。 */
  trackSave: (job: Omit<SaveJob, 'shown'>) => void;
  /**
   * 終わっていない保存のジョブを Rust に聞いて表に合わせる（エディタを開いたとき・Rust が Busy で断ったとき）。頼む前から知って
   * いて一覧にないジョブは終わった（イベントを取りこぼした）ものとして消す。知っているジョブの状態はイベントのほうが新しいので
   * 替えない。
   */
  readSaves: () => Promise<void>;
  /** 表を捨てて読み直す（ライブラリを開いた・替えた。一時停止した保存はアプリを開き直しても残っている）。 */
  reloadSaves: () => Promise<void>;
}

export const useJobsStore = create<JobsState>((set, get) => ({
  imports: {},
  dismissed: {},
  saves: {},
  endedSaves: {},
  apply: (event) => {
    if (event.kind === 'edit') {
      set((state) => {
        const previous = state.saves[event.id];
        if (event.state === 'done' || event.state === 'failed' || event.state === 'canceled') {
          const saves = { ...state.saves };
          delete saves[event.id];
          return { saves, endedSaves: { ...state.endedSaves, [event.id]: true } };
        }
        if (!previous) return state;
        const restarted = previous.state === 'paused' && event.state === 'queued';
        const next: SaveJob = {
          ...previous,
          state: event.state,
          progress: nextSaveProgress(previous, event),
          message: restarted ? null : (event.message ?? previous.message),
          shown: previous.shown || event.state === 'paused',
        };
        return { saves: { ...state.saves, [event.id]: next } };
      });
      return;
    }
    // 進捗のカプセルに出すのは取り込みと書き出し（と、上の保存）
    if (event.kind !== 'import' && event.kind !== 'export') {
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
      const jobs = await listJobs({
        kinds: ['import', 'export'],
        states: ['queued', 'running', 'paused'],
      });
      const apply = useJobsStore.getState().apply;
      jobs.forEach((job) => apply(progressFromJob(job)));
    } catch (error) {
      console.warn('ジョブを読めませんでした', error);
    }
  },
  trackSave: (job) => {
    const { saves, endedSaves } = get();
    if (saves[job.id] || endedSaves[job.id]) return;
    const shown = job.state === 'paused';
    set({ saves: { ...saves, [job.id]: { ...job, shown } } });
    if (shown) return;
    setTimeout(() => {
      set((state) => {
        const now = state.saves[job.id];
        return now && !now.shown
          ? { saves: { ...state.saves, [job.id]: { ...now, shown: true } } }
          : state;
      });
    }, SAVE_CAPSULE_DELAY_MS);
  },
  readSaves: async () => {
    const epoch = savesEpoch;
    const known = Object.values(get().saves).map((job) => job.id);
    try {
      const list = await listPendingSaves();
      if (epoch !== savesEpoch) return;
      const alive = new Set(list.map((row) => row.job));
      const gone = known.filter((id) => !alive.has(id));
      if (gone.length > 0) {
        set((state) => {
          const saves = { ...state.saves };
          const endedSaves = { ...state.endedSaves };
          for (const id of gone) {
            delete saves[id];
            endedSaves[id] = true;
          }
          return { saves, endedSaves };
        });
      }
      for (const row of list) get().trackSave(saveFromDto(row));
    } catch (error) {
      console.warn('保存のジョブを読めませんでした', error);
    }
  },
  reloadSaves: () => {
    savesEpoch++;
    set({ saves: {}, endedSaves: {} });
    return get().readSaves();
  },
}));

/** 画面に出す取り込み・書き出しのジョブ（消していないもののうち新しいもの）。 */
export function selectCurrentImport(state: JobsState): JobProgress | null {
  const candidates = Object.values(state.imports)
    .filter((job) => !state.dismissed[job.id])
    .sort((a, b) => b.id - a.id);
  return candidates[0] ?? null;
}

/** 進捗イベントをほかの画面へ回す先（画像エディタの保存のジョブ。jobsStore から画面を読み込まないための口）。 */
const jobListeners = new Set<(event: JobProgress) => void>();

/** 進捗イベントを受ける（戻り値で外す）。 */
export function onAnyJob(listener: (event: JobProgress) => void): () => void {
  jobListeners.add(listener);
  return () => {
    jobListeners.delete(listener);
  };
}

/** 進捗イベントの受け取りを始める。戻り値で止める。 */
export function startJobEvents(): () => void {
  let stop: (() => void) | null = null;
  let stopped = false;
  onJobProgress((event) => {
    useJobsStore.getState().apply(event);
    useDownloadsStore.getState().apply(event);
    reactToJob(event);
    for (const listener of jobListeners) listener(event);
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
