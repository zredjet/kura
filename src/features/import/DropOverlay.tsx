import { ImportIcon } from '../../components/icons/ImportIcon';
import { ja } from '../../i18n/ja';
import { useImportStore } from '../../stores/importStore';
import styles from './DropOverlay.module.css';

/** ファイルをウィンドウの上でドラッグしている間の重ね表示。 */
export function DropOverlay() {
  const dragging = useImportStore((s) => s.dragging);
  if (!dragging) {
    return null;
  }
  return (
    <div className={styles.overlay} data-testid="drop-overlay">
      <div className={styles.frame}>
        <span className={styles.icon}>
          <ImportIcon size={44} strokeWidth={1.6} />
        </span>
        <p className={styles.message}>{ja.drop.message}</p>
      </div>
    </div>
  );
}
