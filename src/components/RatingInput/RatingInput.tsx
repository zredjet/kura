import { StarIcon } from '../icons/StarIcon';
import styles from './RatingInput.module.css';

interface RatingInputProps {
  /** 評価（0 は未評価。null は「複数の値」）。 */
  value: number | null;
  onChange: (rating: number) => void;
  label: string;
  starLabel: (n: number) => string;
}

/** 5 つの星（Main.dc.html のインスペクタ）。いまの評価と同じ星を押すと評価を消す。 */
export function RatingInput({ value, onChange, label, starLabel }: RatingInputProps) {
  return (
    <div role="group" aria-label={label} className={styles.stars} data-mixed={value === null}>
      {[1, 2, 3, 4, 5].map((n) => {
        const on = value !== null && n <= value;
        return (
          <button
            key={n}
            type="button"
            className={styles.star}
            data-on={on}
            aria-label={starLabel(n)}
            aria-pressed={value === n}
            onClick={() => onChange(value === n ? 0 : n)}
          >
            <StarIcon size={16} strokeWidth={1.7} filled={on} />
          </button>
        );
      })}
    </div>
  );
}
