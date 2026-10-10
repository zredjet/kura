import styles from './Switch.module.css';

interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  /** `<label>` の中に置かないときの読み上げ名。 */
  'aria-label'?: string;
}

/** オン・オフのスイッチ（design.md §4：34×20px、オンは --accent、つまみ 16px）。 */
export function Switch({ checked, onChange, disabled, 'aria-label': ariaLabel }: SwitchProps) {
  return (
    <span className={styles.track} data-checked={checked}>
      <input
        type="checkbox"
        role="switch"
        className={styles.input}
        checked={checked}
        disabled={disabled}
        aria-label={ariaLabel}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className={styles.thumb} aria-hidden="true" />
    </span>
  );
}
