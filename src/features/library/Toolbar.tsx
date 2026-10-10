import styles from './Toolbar.module.css';

interface ToolbarProps {
  title: string;
}

/**
 * 画面上部のツールバー（Main.dc.html）。ウィンドウのドラッグ領域を兼ねる。
 * 検索欄・表示形式・並び順などのボタンは M2 以降で入れる。
 */
export function Toolbar({ title }: ToolbarProps) {
  return (
    <header className={styles.toolbar} data-tauri-drag-region>
      <h1 className={styles.title} data-tauri-drag-region>
        {title}
      </h1>
    </header>
  );
}
