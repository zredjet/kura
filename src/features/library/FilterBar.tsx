import styles from './FilterBar.module.css';

/**
 * 絞り込みのチップを並べる帯（Main.dc.html）。中身（種類・評価・タグなど）は M2〜M4 で入れる。
 * グリッドとインスペクタの上端をモックに揃えるため、高さだけ先に確保する。
 */
export function FilterBar() {
  return <div className={styles.filterBar} />;
}
