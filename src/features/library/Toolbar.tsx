import { PlusIcon } from '../../components/icons/PlusIcon';
import { ja } from '../../i18n/ja';
import styles from './Toolbar.module.css';

interface ToolbarProps {
  title: string;
  subtitle: string;
  onImport: () => void;
}

/**
 * 画面上部のツールバー（Main.dc.html）。ウィンドウのドラッグ領域を兼ねる。
 * 検索欄・表示形式・並び順などのボタンは M2 以降で入れる。
 */
export function Toolbar({ title, subtitle, onImport }: ToolbarProps) {
  return (
    <header className={styles.toolbar} data-tauri-drag-region>
      <div className={styles.heading} data-tauri-drag-region>
        <h1 className={styles.title} data-tauri-drag-region>
          {title}
        </h1>
        <span className={styles.subtitle} data-tauri-drag-region>
          {subtitle}
        </span>
      </div>
      <button
        type="button"
        className={styles.importButton}
        aria-label={ja.library.importFiles}
        onClick={onImport}
      >
        <PlusIcon size={18} strokeWidth={2.4} />
      </button>
    </header>
  );
}
