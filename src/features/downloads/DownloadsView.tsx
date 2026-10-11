import { useEffect, useState } from 'react';

import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { countJobs, useDownloadsStore } from '../../stores/downloadsStore';
import { MiniPlayer } from '../player/MiniPlayer';
import { AddPanel } from './AddPanel';
import { CapturePanel, refreshCaptureRules } from './CapturePanel';
import styles from './Downloads.module.css';
import { JobList } from './JobList';
import { ToolsPanel } from './ToolsPanel';

const t = ja.downloads;

/** ダウンロードとキャプチャの画面（Downloads.dc.html）。 */
export function DownloadsView() {
  const jobs = useDownloadsStore((s) => s.jobs);
  // 「今日の完了」の区切りが変わるので、ときどき時刻を取り直す
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    void useDownloadsStore.getState().load();
    const timer = setInterval(() => setNow(new Date()), 60_000);
    return () => clearInterval(timer);
  }, []);
  const counts = countJobs(jobs, now);
  // キャプチャが終わったら、ルールの最終実行を読み直す
  const captureKey = jobs
    .filter((j) => j.kind === 'capture')
    .map((j) => `${j.id}:${j.state}`)
    .join(',');
  useEffect(() => {
    refreshCaptureRules();
  }, [captureKey]);

  return (
    <div className={styles.view}>
      <div className={styles.scroll}>
        <div className={styles.layout}>
          <div className={styles.main}>
            <header className={styles.head}>
              <h1 className={styles.title}>{t.title}</h1>
              <span className={styles.summary}>
                {t.summary(
                  formatCount(counts.active),
                  formatCount(counts.queued),
                  formatCount(counts.doneToday),
                )}
              </span>
            </header>
            <AddPanel />
            <JobList failedCount={counts.failed} />
          </div>
          <aside className={styles.side}>
            <CapturePanel />
            <ToolsPanel />
          </aside>
        </div>
      </div>
      <MiniPlayer />
    </div>
  );
}
