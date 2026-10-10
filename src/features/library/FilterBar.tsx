import { ThumbLargeIcon } from '../../components/icons/ThumbLargeIcon';
import { ThumbSmallIcon } from '../../components/icons/ThumbSmallIcon';
import { ja } from '../../i18n/ja';
import { ROW_HEIGHTS, useViewStore } from '../../stores/viewStore';
import styles from './FilterBar.module.css';

/**
 * 絞り込みのチップを並べる帯（Main.dc.html）。チップ（種類・評価・タグなど）は M4 で入れる。
 * 右端にサムネイルの大きさのスライダー。
 */
export function FilterBar() {
  const thumbSize = useViewStore((s) => s.thumbSize);
  const setThumbSize = useViewStore((s) => s.setThumbSize);
  return (
    <div className={styles.filterBar}>
      <label className={styles.size}>
        <ThumbSmallIcon size={12} strokeWidth={2.2} />
        <input
          type="range"
          className={styles.slider}
          min={1}
          max={ROW_HEIGHTS.length}
          value={thumbSize + 1}
          aria-label={ja.library.thumbSize}
          onChange={(e) => setThumbSize(Number(e.target.value) - 1)}
        />
        <ThumbLargeIcon size={16} strokeWidth={1.8} />
      </label>
    </div>
  );
}
