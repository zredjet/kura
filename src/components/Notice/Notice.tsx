import { useNoticeStore } from '../../stores/noticeStore';
import styles from './Notice.module.css';

/** 操作の結果・失敗を知らせる一行（画面の下中央。読み上げもする）。 */
export function Notice() {
  const message = useNoticeStore((s) => s.message);
  const serial = useNoticeStore((s) => s.serial);
  return (
    <div className={styles.region} role="status" aria-live="polite">
      {message && (
        <p key={serial} className={styles.notice}>
          {message}
        </p>
      )}
    </div>
  );
}
