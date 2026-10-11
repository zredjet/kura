import { create } from 'zustand';

import { type DownloadJobDto, listDownloads } from '../ipc/downloads';
import type { JobProgress } from '../ipc/jobs';

/** 一覧のタブ（Downloads.dc.html）。 */
export type DownloadTab = 'all' | 'active' | 'done' | 'failed';

/** 動いている・待っている状態。 */
const ACTIVE = new Set(['running', 'queued', 'paused']);

/** ジョブの結果（kura-core の DownloadResult。camelCase）。 */
interface DownloadResultJson {
  title?: string | null;
  files?: number;
  added?: number;
  duplicates?: number;
  skipped?: number;
  tags?: number;
  itemIds?: number[];
  unitIds?: number[];
}

/** 進捗イベントを一覧の 1 件に重ねる（来なかった値は前のまま）。 */
export function mergeEvent(job: DownloadJobDto, event: JobProgress): DownloadJobDto {
  let result: DownloadResultJson = {};
  if (event.resultJson) {
    try {
      result = JSON.parse(event.resultJson) as DownloadResultJson;
    } catch {
      result = {};
    }
  }
  return {
    ...job,
    state: event.state,
    progress: event.progress ?? job.progress,
    message: event.message ?? job.message,
    title: result.title ?? job.title,
    files: result.files ?? job.files,
    added: result.added ?? job.added,
    duplicates: result.duplicates ?? job.duplicates,
    skipped: result.skipped ?? job.skipped,
    tags: result.tags ?? job.tags,
    itemIds: result.itemIds ?? job.itemIds,
    unitIds: result.unitIds ?? job.unitIds,
  };
}

/** タブに当てはまるものだけ。 */
export function filterJobs(jobs: readonly DownloadJobDto[], tab: DownloadTab): DownloadJobDto[] {
  switch (tab) {
    case 'all':
      return [...jobs];
    case 'active':
      return jobs.filter((j) => ACTIVE.has(j.state));
    case 'done':
      return jobs.filter((j) => j.state === 'done');
    case 'failed':
      return jobs.filter((j) => j.state === 'failed' || j.state === 'canceled');
  }
}

/** 見出しの数（進行中・待機・今日の完了・失敗）。今日は端末の時刻で 0 時から。 */
export function countJobs(
  jobs: readonly DownloadJobDto[],
  now: Date,
): { active: number; queued: number; doneToday: number; failed: number } {
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  let active = 0;
  let queued = 0;
  let doneToday = 0;
  let failed = 0;
  for (const job of jobs) {
    if (job.state === 'running') {
      active += 1;
    } else if (job.state === 'queued' || job.state === 'paused') {
      queued += 1;
    } else if (job.state === 'done' && (job.finishedAt ?? 0) >= midnight) {
      doneToday += 1;
    } else if (job.state === 'failed') {
      failed += 1;
    }
  }
  return { active, queued, doneToday, failed };
}

interface DownloadsState {
  /** 新しい順。 */
  jobs: DownloadJobDto[];
  loaded: boolean;
  load: () => Promise<void>;
  /** 進捗イベントを反映する（知らないジョブ・状態の変わり目は読み直す）。 */
  apply: (event: JobProgress) => void;
}

let reloadTimer: ReturnType<typeof setTimeout> | null = null;

/** 少し待ってから読み直す（続けて来たイベントは 1 回にまとめる）。 */
function scheduleReload(): void {
  if (reloadTimer !== null) {
    return;
  }
  reloadTimer = setTimeout(() => {
    reloadTimer = null;
    void useDownloadsStore.getState().load();
  }, 250);
}

export const useDownloadsStore = create<DownloadsState>((set, get) => ({
  jobs: [],
  loaded: false,
  load: async () => {
    try {
      set({ jobs: await listDownloads(), loaded: true });
    } catch (error) {
      console.warn('ダウンロードの一覧を読めませんでした', error);
    }
  },
  apply: (event) => {
    // ダウンロードとキャプチャの一覧（docs/design.md §3.1）
    if (event.kind !== 'download' && event.kind !== 'capture') {
      return;
    }
    const jobs = get().jobs;
    const index = jobs.findIndex((j) => j.id === event.id);
    const current = jobs[index];
    if (!current) {
      scheduleReload();
      return;
    }
    const next = [...jobs];
    next[index] = mergeEvent(current, event);
    set({ jobs: next });
    // 終わったとき（エラーの内容・終わった時刻）と待ち行列の並びが変わったときは読み直す
    if (current.state !== event.state && event.state !== 'running') {
      scheduleReload();
    }
  },
}));

/** サイドバーに出す数（取得中と待機中）。 */
export function selectActiveCount(state: DownloadsState): number {
  return state.jobs.filter((j) => ACTIVE.has(j.state)).length;
}
