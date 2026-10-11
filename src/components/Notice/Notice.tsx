import { useNoticeStore } from '../../stores/noticeStore';
import styles from './Notice.module.css';

/** 操作の結果・失敗を知らせる一行（画面の下中央。読み上げもする）。「元に戻す」などのボタンを付けられる。 */
export function Notice() {
  const message = useNoticeStore((s) => s.message);
  const action = useNoticeStore((s) => s.action);
  const serial = useNoticeStore((s) => s.serial);
  const clear = useNoticeStore((s) => s.clear);
  return (
    <div className={styles.region} role="status" aria-live="polite">
      {message && (
        <p key={serial} className={styles.notice}>
          {message}
          {action && (
            <button
              type="button"
              className={styles.action}
              onClick={() => {
                clear();
                action.run();
              }}
            >
              {action.label}
            </button>
          )}
        </p>
      )}
    </div>
  );
}
