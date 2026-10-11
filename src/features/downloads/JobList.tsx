import { useEffect, useState } from 'react';

import { CloseIcon } from '../../components/icons/CloseIcon';
import { DownloadIcon } from '../../components/icons/DownloadIcon';
import { ArrowUpIcon } from '../../components/icons/ArrowUpIcon';
import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { ProgressBar } from '../../components/ProgressBar/ProgressBar';
import { ja } from '../../i18n/ja';
import type { DownloadJobDto } from '../../ipc/downloads';
import { cancelJob, jobLog, pauseJob, prioritizeJob, resumeJob, retryJob } from '../../ipc/jobs';
import { thumbUrl } from '../../ipc/media';
import { formatCount } from '../../lib/format';
import { type DownloadTab, filterJobs, useDownloadsStore } from '../../stores/downloadsStore';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { fail, showInLibrary } from './downloadActions';
import styles from './Downloads.module.css';
import { TextSheet } from './TextSheet';

const t = ja.downloads;

const TABS: { value: DownloadTab; label: string }[] = [
  { value: 'all', label: t.tabAll },
  { value: 'active', label: t.tabActive },
  { value: 'done', label: t.tabDone },
  { value: 'failed', label: t.tabFailed },
];

/** 表紙：取り込んだあとはライブラリのサムネイル、それまではダウンロードの印。 */
function Thumb({ job }: { job: DownloadJobDto }) {
  useItemCacheVersion((s) => s.version);
  const [first] = job.itemIds;
  useEffect(() => {
    if (first !== undefined) {
      requestItems([first]);
    }
  }, [first]);
  const item = first !== undefined ? getCached(first) : undefined;
  return (
    <span className={styles.thumb} aria-hidden="true">
      {item?.thumbState === 'ready' ? (
        <img src={thumbUrl(item.hash, 's')} alt="" draggable={false} />
      ) : (
        <DownloadIcon size={18} strokeWidth={1.8} />
      )}
    </span>
  );
}

/** 1 件の行（Downloads.dc.html）：表紙・題名・サイトとツールと状態・進捗・説明・操作。 */
function JobRow({ job, onLog }: { job: DownloadJobDto; onLog: (job: DownloadJobDto) => void }) {
  const state = job.state;
  const percent =
    state === 'running' && job.progress !== null
      ? t.percent(String(Math.round(job.progress * 100)))
      : '';
  const act = (run: (id: number) => Promise<unknown>) => () => {
    run(job.id).then(() => useDownloadsStore.getState().load(), fail);
  };
  return (
    <div className={styles.row}>
      <Thumb job={job} />
      <div className={styles.body}>
        <span className={styles.jobTitle} title={job.url}>
          {job.title ?? job.url}
        </span>
        <span className={styles.meta}>
          <span>{job.siteLabel}</span>
          <code className={styles.tool}>{job.toolLabel}</code>
          {job.kind === 'capture' && job.ruleName && (
            <span>{t.capture.detected(job.ruleName)}</span>
          )}
          <span className={styles.state} data-state={state}>
            {t.states[state]}
            {percent && ` ${percent}`}
          </span>
        </span>
        {(state === 'running' || state === 'paused') && (
          <ProgressBar
            value={state === 'running' && job.progress === 0 ? null : job.progress}
            label={t.title}
          />
        )}
        {job.message && (
          <span className={styles.detail} data-state={state}>
            {job.message}
          </span>
        )}
      </div>
      <div className={styles.actions}>
        {state === 'running' && (
          <>
            <button
              type="button"
              className={styles.roundButton}
              aria-label={t.pause}
              onClick={act(pauseJob)}
            >
              <PauseIcon size={12} />
            </button>
            <button
              type="button"
              className={styles.roundButton}
              aria-label={t.cancel}
              onClick={act(cancelJob)}
            >
              <CloseIcon size={12} strokeWidth={2.6} />
            </button>
          </>
        )}
        {state === 'paused' && (
          <>
            <button
              type="button"
              className={styles.roundButton}
              aria-label={t.resume}
              onClick={act(resumeJob)}
            >
              <PlayIcon size={12} />
            </button>
            <button
              type="button"
              className={styles.roundButton}
              aria-label={t.cancel}
              onClick={act(cancelJob)}
            >
              <CloseIcon size={12} strokeWidth={2.6} />
            </button>
          </>
        )}
        {state === 'queued' && (
          <>
            <button
              type="button"
              className={styles.roundButton}
              aria-label={t.prioritize}
              title={t.prioritize}
              onClick={act(prioritizeJob)}
            >
              <ArrowUpIcon size={14} strokeWidth={2.2} />
            </button>
            <button
              type="button"
              className={styles.roundButton}
              aria-label={t.cancel}
              onClick={act(cancelJob)}
            >
              <CloseIcon size={12} strokeWidth={2.6} />
            </button>
          </>
        )}
        {state === 'done' && job.itemIds.length > 0 && (
          <button type="button" className={styles.pillButton} onClick={() => showInLibrary(job)}>
            {t.showInLibrary}
          </button>
        )}
        {(state === 'failed' || state === 'canceled') && (
          <>
            <button type="button" className={styles.pillButton} onClick={() => onLog(job)}>
              {t.log}
            </button>
            <button
              type="button"
              className={`${styles.pillButton} ${styles.pillPrimary}`}
              onClick={act(retryJob)}
            >
              {t.retry}
            </button>
          </>
        )}
      </div>
    </div>
  );
}

/** タブと一覧。 */
export function JobList({ failedCount }: { failedCount: number }) {
  const jobs = useDownloadsStore((s) => s.jobs);
  const loaded = useDownloadsStore((s) => s.loaded);
  const [tab, setTab] = useState<DownloadTab>('all');
  const [log, setLog] = useState<{ title: string; text: string } | null>(null);
  const shown = filterJobs(jobs, tab);

  const openLog = (job: DownloadJobDto) => {
    jobLog(job.id).then(
      (text) =>
        setLog({ title: `${t.logTitle} — ${job.title ?? job.url}`, text: text || t.logEmpty }),
      fail,
    );
  };

  return (
    <>
      <div role="tablist" aria-label={t.tabs} className={styles.tabs}>
        {TABS.map((item) => (
          <button
            key={item.value}
            type="button"
            role="tab"
            aria-selected={tab === item.value}
            className={styles.tab}
            onClick={() => setTab(item.value)}
          >
            {item.label}
            {item.value === 'failed' && failedCount > 0 && ` ${formatCount(failedCount)}`}
          </button>
        ))}
      </div>
      <div
        className={styles.list}
        role="tabpanel"
        aria-label={TABS.find((x) => x.value === tab)?.label}
      >
        {shown.map((job) => (
          <JobRow key={job.id} job={job} onLog={openLog} />
        ))}
        {loaded && shown.length === 0 && (
          <p className={styles.empty}>{jobs.length === 0 ? t.empty : t.emptyTab}</p>
        )}
      </div>
      {log && <TextSheet title={log.title} text={log.text} onClose={() => setLog(null)} />}
    </>
  );
}
