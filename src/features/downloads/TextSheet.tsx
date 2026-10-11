import { useId } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ja } from '../../i18n/ja';
import { copyText } from './downloadActions';
import styles from './Downloads.module.css';

const t = ja.downloads;

/** 文字を見せるシート（ジョブのログ・ツールの入れ方・更新の案内）。コピーできる。 */
export function TextSheet({
  title,
  intro,
  text,
  onClose,
}: {
  title: string;
  intro?: string;
  text: string;
  onClose: () => void;
}) {
  const id = useId();
  return (
    <Dialog labelledBy={id} onClose={onClose}>
      <div className={styles.sheet}>
        <h2 id={id} className={styles.sheetTitle}>
          {title}
        </h2>
        {intro && <p className={styles.sheetText}>{intro}</p>}
        {text && <pre className={styles.code}>{text}</pre>}
        <div className={styles.sheetActions}>
          {text && <Button onClick={() => void copyText(text)}>{t.copy}</Button>}
          <Button variant="primary" onClick={onClose}>
            {t.close}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
