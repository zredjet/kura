import { ja } from '../../i18n/ja';
import { useUpgradeStore } from '../../stores/upgradeStore';
import styles from './LibraryUpgrade.module.css';

/**
 * ライブラリの更新（design.md §3.1）：開くときに DB を新しい版にするのが 300ms を超えたら、窓の中央に
 * 「ライブラリを更新しています」と進捗の溝を出す（割合は分からないので溝だけ）。ガラスにはしない（窓の地の上）。
 * 起動の途中（地の色だけの画面）でも、ほかのライブラリに切り替えるときでも出す。
 */
export function LibraryUpgrade() {
  const visible = useUpgradeStore((s) => s.visible);
  if (!visible) {
    return null;
  }
  return (
    <div className={styles.layer} data-tauri-drag-region>
      <div className={styles.box} role="status">
        <p className={styles.text}>{ja.libraryOpen.upgrading}</p>
        <div className={styles.track} aria-hidden="true" />
      </div>
    </div>
  );
}
