import { ja } from '../../i18n/ja';
import { parseImportResult } from '../../ipc/import';
import type { JobProgress } from '../../ipc/jobs';
import { formatCount } from '../../lib/format';

const t = ja.progress;

export type ProgressAction = 'pause' | 'resume' | 'cancel' | 'close';

/** 進捗カプセルに出す内容。 */
export interface ProgressView {
  title: string;
  detail: string | null;
  /** 進捗バー。undefined なら出さない、null なら動き続ける表示。 */
  progress: number | null | undefined;
  actions: ProgressAction[];
  /** 終わった（完了・失敗・中止）か。 */
  finished: boolean;
}

/** 取り込みジョブの状態から、カプセルに出す内容を決める。 */
export function progressView(job: JobProgress): ProgressView {
  const result = parseImportResult(job.resultJson);
  switch (job.state) {
    case 'queued':
      return {
        title: t.queued,
        detail: null,
        progress: null,
        actions: ['cancel'],
        finished: false,
      };
    case 'running':
      return {
        title: t.running,
        detail: job.message,
        progress: job.progress,
        actions: ['pause', 'cancel'],
        finished: false,
      };
    case 'paused':
      return {
        title: t.paused,
        detail: job.message,
        progress: job.progress ?? 0,
        actions: ['resume', 'cancel'],
        finished: false,
      };
    case 'done': {
      const parts: string[] = [];
      if (result && result.duplicates > 0) {
        parts.push(t.duplicates(formatCount(result.duplicates)));
      }
      if (result && result.failed > 0) {
        parts.push(t.failures(formatCount(result.failed)));
      }
      return {
        title: t.done(formatCount(result?.added ?? 0)),
        detail: parts.length > 0 ? parts.join('・') : null,
        progress: undefined,
        actions: ['close'],
        finished: true,
      };
    }
    case 'failed':
      return {
        title: t.failed,
        detail: job.message,
        progress: undefined,
        actions: ['close'],
        finished: true,
      };
    case 'canceled':
      return {
        title: t.canceled,
        detail: result && result.added > 0 ? t.canceledDetail(formatCount(result.added)) : null,
        progress: undefined,
        actions: ['close'],
        finished: true,
      };
  }
}
