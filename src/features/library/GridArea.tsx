import { ja } from '../../i18n/ja';
import styles from './GridArea.module.css';

/** アイテムのグリッドを置く領域。仮想スクロールのグリッドは M2 で入れる。 */
export function GridArea() {
  return <section className={styles.grid} aria-label={ja.library.gridLabel} />;
}
