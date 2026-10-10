import { useEffect } from 'react';

import { ImportIcon } from '../../components/icons/ImportIcon';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { ProgressBar } from '../../components/ProgressBar/ProgressBar';
import { ja } from '../../i18n/ja';
import { cancelJob, pauseJob, resumeJob } from '../../ipc/jobs';
import { selectCurrentImport, useJobsStore } from '../../stores/jobsStore';
import { useLibraryStore } from '../../stores/libraryStore';
import styles from './ImportProgress.module.css';
import { type ProgressAction, progressView } from './progressView';

/** 完了・中止の表示を消すまでの時間。 */
const AUTO_HIDE_MS = 6000;

const t = ja.progress;

/** 取り込みの進捗（グリッドの下中央に浮かぶカプセル）。 */
export function ImportProgress({ raised = false }: { raised?: boolean }) {
  const job = useJobsStore(selectCurrentImport);
  const dismiss = useJobsStore((s) => s.dismiss);
  const refresh = useLibraryStore((s) => s.refresh);
  const id = job?.id;
  const state = job?.state;

  // 終わったら件数を読み直し、完了・中止は少ししてから消す
  useEffect(() => {
    if (id === undefined || (state !== 'done' && state !== 'failed' && state !== 'canceled')) {
      return;
    }
    void refresh();
    if (state === 'failed') {
      return;
    }
    const timer = setTimeout(() => dismiss(id), AUTO_HIDE_MS);
    return () => clearTimeout(timer);
  }, [id, state, refresh, dismiss]);

  if (!job) {
    return null;
  }
  const view = progressView(job);
  const run = (action: ProgressAction) => {
    const call =
      action === 'pause'
        ? pauseJob(job.id)
        : action === 'resume'
          ? resumeJob(job.id)
          : action === 'cancel'
            ? cancelJob(job.id)
            : null;
    if (call) {
      call.catch((error: unknown) => console.warn('ジョブを操作できませんでした', error));
    } else {
      dismiss(job.id);
    }
  };

  return (
    <div
      className={styles.capsule}
      data-raised={raised || undefined}
      aria-label={t.label}
      role="group"
    >
      <span className={styles.icon}>
        <ImportIcon size={18} />
      </span>
      <div className={styles.text}>
        <span className={styles.title} role="status">
          {view.title}
        </span>
        {view.detail && <span className={styles.detail}>{view.detail}</span>}
        {view.progress !== undefined && (
          <div className={styles.bar}>
            <ProgressBar value={view.progress} label={t.label} />
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
            onClick={() => run(action)}
          >
            {action === 'pause' && <PauseIcon size={12} />}
            {action === 'resume' && <PlayIcon size={12} />}
            {(action === 'cancel' || action === 'close') && (
              <CloseIcon size={12} strokeWidth={2.6} />
            )}
          </button>
        ))}
      </div>
    </div>
  );
}
