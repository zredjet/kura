import { ja } from '../../i18n/ja';
import type { ListView } from '../../ipc/items';
import styles from './Grid.module.css';

/** 一覧が空のときの案内（一覧の種類ごと）。 */
export function EmptyView({ view }: { view: ListView }) {
  const t = ja.library.emptyView;
  let title: string = t.other;
  let hint: string | null = null;
  if (view.kind === 'all') {
    title = ja.library.emptyTitle;
    hint = ja.library.emptyHint;
  } else if (view.kind === 'folder') {
    title = t.folder;
    hint = t.folderHint;
  } else if (view.kind === 'trash') {
    title = t.trash;
  } else if (view.kind === 'unit') {
    title = t.unit;
  }
  return (
    <div className={styles.empty}>
      <p className={styles.emptyTitle}>{title}</p>
      {hint && <p className={styles.emptyHint}>{hint}</p>}
    </div>
  );
}
