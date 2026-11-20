/**
 * 一括のカプセルに出す内容（M14 段 7。利用者の決定 4：終わったら「N 件に当てました」と［結果］［閉じる］を出し、飛ばした・
 * 失敗があれば閉じるまで残す）。
 */

import { ja } from '../../i18n/ja';
import type { BatchResultDto } from '../../ipc/batch';
import { formatCount } from '../../lib/format';
import type { ProgressView } from '../import/progressView';
import type { BatchJob } from './batchJobsStore';

const t = ja.batch;

/** 状態ごとの件数。 */
export interface BatchCounts {
  /** 当てた（版を足した・変わらない・同じ中身があった）。 */
  applied: number;
  skipped: number;
  failed: number;
  canceled: number;
  pending: number;
  total: number;
}

/** 結果の群から件数を数える。 */
export function batchCounts(result: BatchResultDto | null): BatchCounts {
  const out: BatchCounts = { applied: 0, skipped: 0, failed: 0, canceled: 0, pending: 0, total: 0 };
  if (!result) return out;
  out.total = result.total;
  for (const g of result.groups) {
    const n = g.items.length;
    if (g.state === 'saved' || g.state === 'unchanged' || g.state === 'existing') out.applied += n;
    else if (g.state === 'skipped') out.skipped += n;
    else if (g.state === 'failed') out.failed += n;
    else if (g.state === 'canceled') out.canceled += n;
    else out.pending += n;
  }
  return out;
}

/** 飛ばした・失敗の一言（なければ null）。 */
function problems(c: BatchCounts): string | null {
  const parts: string[] = [];
  if (c.skipped > 0) parts.push(t.skipped(formatCount(c.skipped)));
  if (c.failed > 0) parts.push(t.failures(formatCount(c.failed)));
  return parts.length > 0 ? parts.join(' · ') : null;
}

/** 残しておく（自動で消さない）終わり方か：失敗した・飛ばした・失敗した行がある。 */
export function keepsCapsule(job: BatchJob): boolean {
  if (job.state === 'failed') return true;
  const c = batchCounts(job.result);
  return job.state === 'done' && (c.skipped > 0 || c.failed > 0);
}

/** 一括のジョブのカプセルの内容。 */
export function batchProgressView(job: BatchJob): ProgressView {
  const result = job.result;
  const action = result?.action.type ?? 'overwrite';
  const name = result?.recipe ?? t.unnamed;
  const c = batchCounts(result);
  const done = c.total - c.pending;
  const count = c.total > 0 ? t.progressCount(formatCount(done), formatCount(c.total)) : null;
  const progress = job.progress ?? (c.total > 0 ? done / c.total : null);
  const titles = {
    queued: action === 'undo' ? t.undoQueued : action === 'merge' ? t.mergeQueued : t.queued(name),
    running:
      action === 'undo' ? t.undoRunning : action === 'merge' ? t.mergeRunning : t.running(name),
    paused: action === 'undo' ? t.undoPaused : action === 'merge' ? t.mergePaused : t.paused(name),
  };
  switch (job.state) {
    case 'queued':
      return {
        title: titles.queued,
        detail: null,
        progress: null,
        actions: ['pause', 'cancel'],
        finished: false,
      };
    case 'running':
      return {
        title: titles.running,
        detail: [count, problems(c)].filter(Boolean).join(' · ') || null,
        progress,
        actions: ['pause', 'cancel'],
        finished: false,
      };
    case 'paused':
      return {
        title: titles.paused,
        detail: t.resumeNote,
        progress,
        actions: ['resume', 'cancel'],
        finished: false,
      };
    case 'done': {
      let title: string;
      if (action === 'undo') title = t.undoDone(formatCount(c.applied));
      else if (action === 'merge')
        title = (result?.groups ?? []).some((g) => g.state === 'existing')
          ? t.mergeExisting
          : t.mergeDone;
      else title = c.applied > 0 ? t.done(formatCount(c.applied)) : t.doneNone;
      return {
        title,
        detail: problems(c),
        progress: undefined,
        actions: ['result', 'close'],
        finished: true,
      };
    }
    case 'failed':
      return {
        title:
          action === 'undo' ? t.undoFailed : action === 'merge' ? t.mergeFailed : t.failed(name),
        detail: job.message,
        progress: undefined,
        actions: ['result', 'close'],
        finished: true,
      };
    case 'canceled':
      return {
        title:
          action === 'undo'
            ? t.undoCanceled
            : action === 'merge'
              ? t.mergeCanceled
              : t.canceled(name),
        detail: c.applied > 0 ? t.canceledDetail(formatCount(c.applied)) : null,
        progress: undefined,
        actions: ['result', 'close'],
        finished: true,
      };
  }
}
