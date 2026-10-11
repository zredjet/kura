import styles from './Segmented.module.css';

interface SegmentedProps<T extends string> {
  /** 読み上げる名前（グループの名前）。 */
  label: string;
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
  disabled?: boolean;
}

/** セグメント（Main.dc.html の種類の切り替えと同じ形。押しているものは --fill-pressed＋白）。 */
export function Segmented<T extends string>({
  label,
  value,
  options,
  onChange,
  disabled,
}: SegmentedProps<T>) {
  return (
    <div role="group" aria-label={label} className={styles.segment}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          className={styles.button}
          aria-pressed={option.value === value}
          disabled={disabled}
          onClick={(e) => {
            e.currentTarget.focus();
            if (option.value !== value) {
              onChange(option.value);
            }
          }}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
