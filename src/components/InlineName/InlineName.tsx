import { useEffect, useRef, useState } from 'react';

import styles from './InlineName.module.css';

interface InlineNameProps {
  initial: string;
  label: string;
  /** 確定したときに呼ぶ（空や変わっていないときは `onCancel`）。 */
  onCommit: (value: string) => void;
  onCancel: () => void;
  className?: string;
}

/** その場で名前を入力する欄（Return で確定、esc で取りやめ、フォーカスが外れたら確定）。 */
export function InlineName({ initial, label, onCommit, onCancel, className }: InlineNameProps) {
  const [value, setValue] = useState(initial);
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);

  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  const finish = (commit: boolean) => {
    if (done.current) {
      return;
    }
    done.current = true;
    const trimmed = value.trim();
    if (commit && trimmed && trimmed !== initial) {
      onCommit(trimmed);
    } else {
      onCancel();
    }
  };

  return (
    <input
      ref={ref}
      className={[styles.input, className].filter(Boolean).join(' ')}
      value={value}
      aria-label={label}
      onChange={(e) => setValue(e.target.value)}
      onBlur={() => finish(true)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.nativeEvent.isComposing) {
          return;
        }
        if (e.key === 'Enter') {
          e.preventDefault();
          finish(true);
        } else if (e.key === 'Escape') {
          e.preventDefault();
          finish(false);
        }
      }}
    />
  );
}
