import glass from '../../components/glass/glass.module.css';
import { ja } from '../../i18n/ja';
import styles from './Inspector.module.css';

/** 選択中のアイテムの詳細を出すインスペクタ（Main.dc.html）。中身は M2 で入れる。 */
export function Inspector() {
  return (
    <aside
      className={`${glass.panel} ${styles.inspector}`}
      aria-label={ja.library.inspectorLabel}
    />
  );
}
