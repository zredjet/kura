import { useEffect, useState } from 'react';

import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { countJobs, useDownloadsStore } from '../../stores/downloadsStore';
import { MiniPlayer } from '../player/MiniPlayer';
import { AddPanel } from './AddPanel';
import styles from './Downloads.module.css';
import { JobList } from './JobList';
import { ToolsPanel } from './ToolsPanel';

const t = ja.downloads;

/** ダウンロードの画面（Downloads.dc.html）。キャプチャの欄は M8 で足す。 */
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
            <ToolsPanel />
          </aside>
        </div>
      </div>
      <MiniPlayer />
    </div>
  );
}
