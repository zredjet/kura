import { useDragStore } from './dragState';
import styles from './DragGhost.module.css';

/** ドラッグ中にポインタの横に出す印（「3 項目」・フォルダ名）。 */
export function DragGhost() {
  const payload = useDragStore((s) => s.payload);
  const x = useDragStore((s) => s.x);
  const y = useDragStore((s) => s.y);
  const label = useDragStore((s) => s.label);
  if (!payload) {
    return null;
  }
  return (
    <div
      className={styles.ghost}
      style={{ transform: `translate(${x + 14}px, ${y + 10}px)` }}
      aria-hidden="true"
    >
      {label}
    </div>
  );
}
