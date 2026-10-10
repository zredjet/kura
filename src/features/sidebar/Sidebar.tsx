import glass from '../../components/glass/glass.module.css';
import { ja } from '../../i18n/ja';
import styles from './Sidebar.module.css';

/**
 * サイドバー（Sidebar.dc.html）。先頭の行にはネイティブの信号機が重なる。
 * ライブラリ項目・スマートフォルダ・フォルダの木などの中身は M3 で入れる。
 */
export function Sidebar() {
  return (
    <nav className={`${glass.panel} ${styles.sidebar}`} aria-label={ja.sidebar.label}>
      <div className={styles.titlebar} data-tauri-drag-region />
    </nav>
  );
}
