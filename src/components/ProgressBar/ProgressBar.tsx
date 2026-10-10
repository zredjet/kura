import styles from './ProgressBar.module.css';

interface ProgressBarProps {
  /** 0.0〜1.0。null ならわからない（動き続ける表示）。 */
  value: number | null;
  label: string;
}

/** 進捗バー（design.md §4：高さ 4px、溝 --fill-track、値 --accent）。 */
export function ProgressBar({ value, label }: ProgressBarProps) {
  const percent = value === null ? undefined : Math.round(Math.min(1, Math.max(0, value)) * 100);
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={percent}
      className={styles.track}
    >
      <div
        className={percent === undefined ? styles.indeterminate : styles.fill}
        style={percent === undefined ? undefined : { width: `${percent}%` }}
      />
    </div>
  );
}
