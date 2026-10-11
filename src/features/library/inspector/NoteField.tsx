import { useId, useRef, useState } from 'react';

import { ja } from '../../../i18n/ja';
import styles from './Inspector.module.css';
import { isImeKey } from '../../../lib/ime';

interface NoteFieldProps {
  initial: string;
  placeholder: string;
  /** 書き換えたときだけ、欄を離れたときに呼ぶ（空ならメモを消す）。 */
  onSave: (note: string) => void;
}

/** メモの欄（Main.dc.html）。欄を離れるか ⌘Return で保存、esc で元に戻す。 */
export function NoteField({ initial, placeholder, onSave }: NoteFieldProps) {
  const id = useId();
  const [value, setValue] = useState(initial);
  const cancelled = useRef(false);
  return (
    <section className={styles.section}>
      <label htmlFor={id} className={styles.label}>
        {ja.inspector.note}
      </label>
      <textarea
        id={id}
        rows={2}
        className={styles.note}
        value={value}
        placeholder={placeholder}
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => {
          if (cancelled.current) {
            cancelled.current = false;
          } else if (value !== initial) {
            onSave(value);
          }
        }}
        onKeyDown={(e) => {
          e.stopPropagation();
          // 変換中の Esc は変換の取り消しで、メモの取り消しではない
          if (isImeKey(e)) {
            return;
          }
          if (e.key === 'Enter' && e.metaKey) {
            e.preventDefault();
            e.currentTarget.blur();
          } else if (e.key === 'Escape') {
            e.preventDefault();
            cancelled.current = true;
            setValue(initial);
            e.currentTarget.blur();
          }
        }}
      />
    </section>
  );
}
