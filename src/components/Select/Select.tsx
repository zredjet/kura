import type { SelectHTMLAttributes } from 'react';

import { ChevronDownIcon } from '../icons/ChevronDownIcon';
import styles from './Select.module.css';

export interface SelectOption {
  value: string;
  label: string;
}

interface SelectProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, 'onChange'> {
  options: readonly SelectOption[];
  onChange: (value: string) => void;
}

/** 選択肢（副ボタンの見た目の `<select>`。Import.dc.html の「スキップ ▾」「取り込み先」）。 */
export function Select({ options, onChange, className, ...props }: SelectProps) {
  return (
    <span className={[styles.wrap, className].filter(Boolean).join(' ')}>
      <select className={styles.select} onChange={(e) => onChange(e.target.value)} {...props}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <span className={styles.chevron} aria-hidden="true">
        <ChevronDownIcon size={10} strokeWidth={2.6} />
      </span>
    </span>
  );
}
