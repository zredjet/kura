import { type ReactNode, useEffect, useMemo } from 'react';

import { ExportIcon } from '../../components/icons/ExportIcon';
import { FolderIcon } from '../../components/icons/FolderIcon';
import { ImportIcon } from '../../components/icons/ImportIcon';
import { ListIcon } from '../../components/icons/ListIcon';
import { RecipeIcon } from '../../components/icons/RecipeIcon';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { PauseIcon } from '../../components/icons/PauseIcon';
import { PenIcon } from '../../components/icons/PenIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { ProgressBar } from '../../components/ProgressBar/ProgressBar';
import { ja } from '../../i18n/ja';
import { parseExportResult } from '../../ipc/export';
import { cancelJob, pauseJob, resumeJob } from '../../ipc/jobs';
import type { JobProgress } from '../../ipc/jobs';
import {
  type SaveJob,
  selectCurrentImport,
  shownSaves,
  useJobsStore,
} from '../../stores/jobsStore';
import { revealExport } from '../library/exportActions';
import {
  type BatchJob,
  isFinished,
  shownBatches,
  useBatchJobsStore,
} from '../recipes/batchJobsStore';
import { batchProgressView, keepsCapsule } from '../recipes/batchView';
import { openResult } from '../recipes/resultStore';
import { useLibraryStore } from '../../stores/libraryStore';
import styles from './ImportProgress.module.css';
import {
  type ProgressAction,
  progressView,
  type ProgressView,
  saveProgressView,
} from './progressView';

/** 完了・中止の表示を消すまでの時間（書き出しは「Finder で表示」を押せるよう長め）。 */
const AUTO_HIDE_MS = 6000;
const AUTO_HIDE_EXPORT_MS = 15000;

const t = ja.progress;

/** 一時停止・再開・中止をジョブに送る（ほかの操作なら偽）。 */
function controlJob(id: number, action: ProgressAction): boolean {
  const call =
    action === 'pause'
      ? pauseJob(id)
      : action === 'resume'
        ? resumeJob(id)
        : action === 'cancel'
          ? cancelJob(id)
          : null;
  call?.catch((error: unknown) => console.warn('ジョブを操作できませんでした', error));
  return call !== null;
}

/**
 * ジョブの進捗（グリッドの下中央に浮かぶカプセル。上から保存・取り込みと書き出しの順に積む）。保存のジョブはエディタを閉じたあとも
 * ここで止められる（一瞬で終わるものは出さない。`shownSaves`）。
 */
export function ImportProgress({ raised = false }: { raised?: boolean }) {
  const job = useJobsStore(selectCurrentImport);
  const saves = useJobsStore((s) => s.saves);
  const shown = useMemo(() => shownSaves(saves), [saves]);
  const batchJobs = useBatchJobsStore((s) => s.jobs);
  const batchDismissed = useBatchJobsStore((s) => s.dismissed);
  const batches = useMemo(
    () => shownBatches({ jobs: batchJobs, dismissed: batchDismissed }),
    [batchJobs, batchDismissed],
  );
  const dismiss = useJobsStore((s) => s.dismiss);
  const refresh = useLibraryStore((s) => s.refresh);
  const id = job?.id;
  const state = job?.state;
  const exporting = job?.kind === 'export';

  // 終わったら件数を読み直し（取り込み）、完了・中止は少ししてから消す
  useEffect(() => {
    if (id === undefined || (state !== 'done' && state !== 'failed' && state !== 'canceled')) {
      return;
    }
    if (!exporting) {
      void refresh();
    }
    if (state === 'failed') {
      return;
    }
    const timer = setTimeout(() => dismiss(id), exporting ? AUTO_HIDE_EXPORT_MS : AUTO_HIDE_MS);
    return () => clearTimeout(timer);
  }, [id, state, exporting, refresh, dismiss]);

  if (!job && shown.length === 0 && batches.length === 0) {
    return null;
  }
  return (
    <div className={styles.stack} data-raised={raised || undefined}>
      {shown.map((save) => (
        <SaveCapsule key={save.id} job={save} />
      ))}
      {batches.map((batch) => (
        <BatchCapsule key={batch.id} job={batch} />
      ))}
      {job && <ImportCapsule job={job} />}
    </div>
  );
}

/**
 * 一括のジョブのカプセル（M14 段 7）。終わったら［結果を見る］［閉じる］：飛ばした・失敗があれば閉じるまで残し、なければ少し
 * してから消す（利用者の決定 4）。
 */
function BatchCapsule({ job }: { job: BatchJob }) {
  const dismiss = useBatchJobsStore((s) => s.dismiss);
  const finished = isFinished(job.state);
  const keep = keepsCapsule(job);
  const loaded = job.result !== null;
  useEffect(() => {
    if (!finished || keep || !loaded) return;
    const timer = setTimeout(() => dismiss(job.id), AUTO_HIDE_MS);
    return () => clearTimeout(timer);
  }, [finished, keep, loaded, job.id, dismiss]);
  const run = (action: ProgressAction) => {
    if (action === 'result') {
      openResult(job.id);
      return;
    }
    if (!controlJob(job.id, action)) {
      dismiss(job.id);
    }
  };
  return (
    <Capsule
      label={ja.batch.capsuleLabel}
      icon={<RecipeIcon size={18} />}
      view={batchProgressView(job)}
      onAction={run}
    />
  );
}

/** 保存のジョブのカプセル。 */
function SaveCapsule({ job }: { job: SaveJob }) {
  return (
    <Capsule
      label={ja.savingJob.label}
      icon={<PenIcon size={18} />}
      view={saveProgressView(job)}
      onAction={(action) => controlJob(job.id, action)}
    />
  );
}

/** 取り込み・書き出しのジョブのカプセル。 */
function ImportCapsule({ job }: { job: JobProgress }) {
  const dismiss = useJobsStore((s) => s.dismiss);
  const exporting = job.kind === 'export';
  const run = (action: ProgressAction) => {
    if (action === 'reveal') {
      const result = parseExportResult(job.resultJson);
      if (result) {
        revealExport(result);
      }
      return;
    }
    if (!controlJob(job.id, action)) {
      dismiss(job.id);
    }
  };
  return (
    <Capsule
      label={exporting ? ja.exporting.label : t.label}
      icon={exporting ? <ExportIcon size={18} /> : <ImportIcon size={18} />}
      view={progressView(job)}
      onAction={run}
    />
  );
}

/** カプセル 1 つ（印・題・一言・進捗・操作）。 */
function Capsule({
  label,
  icon,
  view,
  onAction,
}: {
  label: string;
  icon: ReactNode;
  view: ProgressView;
  onAction: (action: ProgressAction) => void;
}) {
  return (
    <div className={styles.capsule} aria-label={label} role="group">
      <span className={styles.icon}>{icon}</span>
      <div className={styles.text}>
        <span className={styles.title} role="status">
          {view.title}
        </span>
        {view.detail && <span className={styles.detail}>{view.detail}</span>}
        {view.progress !== undefined && (
          <div className={styles.bar}>
            <ProgressBar value={view.progress} label={label} />
          </div>
        )}
      </div>
      <div className={styles.actions}>
        {view.actions.map((action) => (
          <button
            key={action}
            type="button"
            className={styles.action}
            aria-label={t[action]}
            onClick={() => onAction(action)}
          >
            {action === 'pause' && <PauseIcon size={12} />}
            {action === 'resume' && <PlayIcon size={12} />}
            {(action === 'cancel' || action === 'close') && (
              <CloseIcon size={12} strokeWidth={2.6} />
            )}
            {action === 'reveal' && <FolderIcon size={13} strokeWidth={2} />}
            {action === 'result' && <ListIcon size={13} strokeWidth={2} />}
          </button>
        ))}
      </div>
    </div>
  );
}
