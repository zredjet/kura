import { Button } from '../../components/Button/Button';
import { ja } from '../../i18n/ja';
import type { ListView } from '../../ipc/items';
import { formatCount } from '../../lib/format';
import { useViewStore } from '../../stores/viewStore';
import { useUnanalyzedCount } from '../search/useUnanalyzed';
import styles from './Grid.module.css';

/**
 * 空のときの見出しと案内（純粋関数。検索の条件が掛かっていれば、それを先に言う）。
 * `unanalyzed` は解析がまだで一致が決まっていない件数（解析で決まるフィールドを使う条件のとき。design.md §3.1）。
 */
export function emptyMessage(
  view: ListView,
  filtered: boolean,
  unanalyzed = 0,
): { title: string; hint: string | null } {
  const t = ja.library.emptyView;
  if (filtered) {
    return {
      title: t.filtered,
      hint: unanalyzed > 0 ? t.filteredUnanalyzed(formatCount(unanalyzed)) : t.filteredHint,
    };
  }
  switch (view.kind) {
    case 'all':
      return { title: ja.library.emptyTitle, hint: ja.library.emptyHint };
    case 'recent':
      return { title: t.recent, hint: t.recentHint };
    case 'unsorted':
      return { title: t.unsorted, hint: null };
    case 'untagged':
      return { title: t.untagged, hint: null };
    case 'folder':
      return { title: t.folder, hint: t.folderHint };
    case 'trash':
      return { title: t.trash, hint: null };
    case 'unit':
      return { title: t.unit, hint: null };
    case 'smart':
      return { title: t.smart, hint: t.smartHint };
    default:
      return { title: t.other, hint: null };
  }
}

/** 一覧が空のときの案内（一覧の種類ごと。検索の条件が掛かっていれば、外すボタン）。 */
export function EmptyView({ view }: { view: ListView }) {
  const filter = useViewStore((s) => s.filter);
  const setFilter = useViewStore((s) => s.setFilter);
  const unanalyzed = useUnanalyzedCount(view, filter);
  const { title, hint } = emptyMessage(view, filter.length > 0, unanalyzed);
  return (
    <div className={styles.empty}>
      <p className={styles.emptyTitle}>{title}</p>
      {hint && <p className={styles.emptyHint}>{hint}</p>}
      {filter.length > 0 && (
        <Button size="sm" onClick={() => setFilter([])}>
          {ja.search.clear}
        </Button>
      )}
    </div>
  );
}
