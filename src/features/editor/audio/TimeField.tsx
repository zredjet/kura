import { useId, useState } from 'react';

import { ja } from '../../../i18n/ja';
import { formatCount } from '../../../lib/format';
import { isImeKey } from '../../../lib/ime';
import styles from './Audio.module.css';
import { parseFrames, timeText } from './timeText';

const t = ja.editor.audio;

/**
 * 時刻の欄（M13 の段 6。利用者の決定：時間で入れ、下に標本の位置を小さく添える）。打っている間は手元の文字で、Enter・
 * 欄を離れたときに読んで `onValue`（フレーム）し、`onSeal` する。esc で打つ前に戻す。読めない文字は送らずに戻す。
 */
export function TimeField({
  label,
  frames,
  rate,
  min = 0,
  max = Number.POSITIVE_INFINITY,
  disabled,
  hideLabel = false,
  onValue,
  onSeal,
}: {
  label: string;
  frames: number | null;
  rate: number;
  min?: number;
  max?: number;
  disabled?: boolean;
  /** 名前を見せずに読み上げだけにする。 */
  hideLabel?: boolean;
  onValue: (frames: number) => void;
  onSeal?: () => void;
}) {
  const id = useId();
  const samplesId = useId();
  const [text, setText] = useState<string | null>(null);
  const [bad, setBad] = useState(false);
  const shown = text ?? (frames === null ? '' : timeText(frames, rate));
  const finish = () => {
    if (text !== null) {
      const f = parseFrames(text, rate, min, max);
      if (f === null) setBad(text.trim() !== '');
      else {
        setBad(false);
        if (f !== frames) onValue(f);
      }
    }
    setText(null);
    onSeal?.();
  };
  return (
    <span className={styles.timeField}>
      <label htmlFor={id} className={hideLabel ? styles.srOnly : styles.timeLabel}>
        {label}
      </label>
      <span className={styles.timeStack}>
        <input
          id={id}
          className={styles.timeInput}
          type="text"
          inputMode="decimal"
          spellCheck={false}
          autoComplete="off"
          title={bad ? t.badTime : t.timeField(label)}
          aria-invalid={bad || undefined}
          aria-describedby={samplesId}
          disabled={disabled}
          value={shown}
          onChange={(e) => setText(e.target.value)}
          onBlur={finish}
          onKeyDown={(e) => {
            if (isImeKey(e)) return;
            if (e.key === 'Enter') {
              e.preventDefault();
              finish();
            } else if (e.key === 'Escape' && text !== null) {
              e.preventDefault();
              e.stopPropagation();
              setText(null);
              setBad(false);
            }
          }}
        />
        <span id={samplesId} className={styles.samples}>
          {frames === null ? '' : t.samples(formatCount(frames))}
        </span>
      </span>
    </span>
  );
}
