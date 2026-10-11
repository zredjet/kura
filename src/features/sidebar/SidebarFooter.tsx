import { useEffect, useState } from 'react';

import { GearIcon } from '../../components/icons/GearIcon';
import { ja } from '../../i18n/ja';
import { libraryUsage, type LibraryUsageDto } from '../../ipc/library';
import { formatBytes } from '../../lib/format';
import { useSettingsStore } from '../../stores/settingsStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import styles from './Sidebar.module.css';

/**
 * サイドバーの下（Sidebar.dc.html）：ライブラリ容量と、ディスクの使っている割合のバー、設定の歯車。
 * 件数が変わったら（取り込み・ゴミ箱を空にしたあと）読み直す。
 */
export function SidebarFooter() {
  const counts = useSidebarStore((s) => s.counts);
  const [usage, setUsage] = useState<LibraryUsageDto | null>(null);
  useEffect(() => {
    libraryUsage().then(setUsage, () => undefined);
  }, [counts]);

  const total = usage?.volumeTotalBytes ?? null;
  const free = usage?.volumeAvailableBytes ?? null;
  const used = total !== null && free !== null && total > 0 ? (total - free) / total : null;
  const disk =
    total !== null && free !== null ? ja.usage.disk(formatBytes(free), formatBytes(total)) : '';
  return (
    <div className={styles.footer}>
      <div className={styles.usage} title={disk}>
        <div className={styles.usageText}>
          <span>{ja.usage.label}</span>
          <span className={styles.usageValue}>{usage ? formatBytes(usage.libraryBytes) : ''}</span>
        </div>
        {used !== null && (
          <div
            className={styles.usageTrack}
            role="meter"
            aria-label={disk}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(used * 100)}
          >
            <div className={styles.usageFill} style={{ width: `${Math.round(used * 100)}%` }} />
          </div>
        )}
      </div>
      <button
        type="button"
        className={styles.footerButton}
        aria-label={ja.settings.open}
        title={`${ja.settings.open}（⌘,）`}
        onClick={() => useSettingsStore.getState().show()}
      >
        <GearIcon size={16} strokeWidth={1.8} />
      </button>
    </div>
  );
}
