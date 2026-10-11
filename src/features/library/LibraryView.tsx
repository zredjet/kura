import { useEffect } from 'react';

import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { CheckIcon } from '../../components/icons/CheckIcon';
import { ja } from '../../i18n/ja';
import type { ItemSort, SortKey } from '../../ipc/items';
import { formatCount } from '../../lib/format';
import { getCachedUnit, useItemCacheVersion } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { useSearchStore } from '../../stores/searchStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { ImportProgress } from '../import/ImportProgress';
import { MiniPlayer, useMiniPlayerVisible } from '../player/MiniPlayer';
import { pickAndImport } from '../import/useFileDrop';
import { joinClauses } from '../search/clauses';
import { emptyTrashWithConfirm } from './actions';
import { FilterBar } from './FilterBar';
import { Grid } from './Grid';
import { Inspector } from './inspector/Inspector';
import styles from './LibraryView.module.css';
import { QuickLook } from './QuickLook';
import { Toolbar } from './Toolbar';
import { useLibraryKeys } from './useLibraryKeys';
import { viewTitle } from './viewTitle';

/** 並び順の言い方（「追加日の新しい順」など）。 */
export function sortLabel(sort: ItemSort): string {
  return ja.sort.orders[sort.key][sort.desc ? 0 : 1];
}

const SORT_KEYS: readonly SortKey[] = [
  'imported',
  'captured',
  'name',
  'rating',
  'duration',
  'size',
];

/** 並び順のメニュー（基準と、大きい・新しい順か小さい・古い順か）。 */
function sortMenu(anchor: HTMLElement): void {
  const { sort, setSort } = useViewStore.getState();
  const check = (on: boolean) => (on ? <CheckIcon size={12} strokeWidth={2.4} /> : null);
  const entries: MenuEntry[] = SORT_KEYS.map((key) => ({
    label: ja.sort.keys[key],
    icon: check(sort.key === key),
    // 名前は小さい順（A→Z）、それ以外は大きい・新しい順から
    onSelect: () => setSort({ key, desc: key === sort.key ? sort.desc : key !== 'name' }),
  }));
  entries.push('separator');
  for (const desc of [true, false]) {
    entries.push({
      label: ja.sort.orders[sort.key][desc ? 0 : 1],
      icon: check(sort.desc === desc),
      onSelect: () => setSort({ ...sort, desc }),
    });
  }
  openMenuBelow(anchor, ja.sort.label, entries);
}

/** ライブラリ画面（Main.dc.html）。上にツールバーとフィルタバー、下にグリッドとインスペクタ。 */
export function LibraryView() {
  const list = useLibraryStore((s) => s.list);
  const miniPlayer = useMiniPlayerVisible();
  const itemCount = useLibraryStore((s) => s.info?.itemCount ?? 0);
  const folders = useSidebarStore((s) => s.folders);
  const smartFolders = useSidebarStore((s) => s.smartFolders);
  const filter = useViewStore((s) => s.filter);
  const setFilter = useViewStore((s) => s.setFilter);
  const sort = useViewStore((s) => s.sort);
  const openPalette = useSearchStore((s) => s.openPalette);
  const trashCount = useSidebarStore((s) => s.counts?.trash ?? 0);
  const view = useViewStore((s) => s.view);
  const parent = useViewStore((s) => s.parent);
  const back = useViewStore((s) => s.back);
  const inspector = useViewStore((s) => s.inspector);
  const toggleInspector = useViewStore((s) => s.toggleInspector);
  const collapseUnits = useViewStore((s) => s.collapseUnits);
  const toggleCollapseUnits = useViewStore((s) => s.toggleCollapseUnits);
  const layout = useViewStore((s) => s.layout);
  const setLayout = useViewStore((s) => s.setLayout);
  useItemCacheVersion((s) => s.version);
  useLibraryKeys();

  useEffect(() => {
    void useLibraryStore.getState().loadList();
  }, [view, collapseUnits, filter, sort]);

  // 「すべて」はライブラリの件数（まとめて表示・検索のときは一覧の件数＝ユニットを 1 件と数える）
  const count = formatCount(
    view.kind === 'all' && !collapseUnits && filter.length === 0
      ? itemCount
      : (list?.ids.length ?? 0),
  );
  const sortable = view.kind !== 'random' && view.kind !== 'trash' && view.kind !== 'unit';
  let subtitle = ja.library.subtitleSorted(count, sortLabel(sort));
  if (view.kind === 'random') {
    subtitle = ja.library.subtitleRandom(count);
  } else if (view.kind === 'trash') {
    subtitle = ja.library.subtitleTrash(count);
  } else if (view.kind === 'unit') {
    const kind = getCachedUnit(view.id)?.kind;
    subtitle = ja.library.subtitleUnit(
      count,
      kind === 'folder' ? ja.unitInspector.folder : ja.unitInspector.set,
    );
  }

  return (
    <>
      <Toolbar
        title={viewTitle(view, folders, smartFolders)}
        subtitle={subtitle}
        parentTitle={
          view.kind === 'unit' ? viewTitle(parent ?? { kind: 'all' }, folders, smartFolders) : null
        }
        onBack={back}
        onImport={() => {
          pickAndImport().catch((error: unknown) =>
            console.warn('ファイルを選べませんでした', error),
          );
        }}
        inspectorVisible={inspector}
        onToggleInspector={toggleInspector}
        collapseUnits={collapseUnits}
        canCollapse={view.kind !== 'trash' && view.kind !== 'unit'}
        onToggleCollapse={toggleCollapseUnits}
        onEmptyTrash={
          view.kind === 'trash' && trashCount > 0 ? () => void emptyTrashWithConfirm() : null
        }
        query={joinClauses(filter)}
        onOpenSearch={() => openPalette()}
        onClearSearch={() => setFilter([])}
        sortLabel={sortable ? ja.sort.keys[sort.key] : null}
        onSortMenu={sortMenu}
        layout={layout}
        onLayout={setLayout}
      />
      <FilterBar />
      <div className={styles.content}>
        <div className={styles.gridColumn}>
          <Grid />
          <MiniPlayer />
          <ImportProgress raised={miniPlayer} />
        </div>
        {inspector && <Inspector />}
      </div>
      <QuickLook />
    </>
  );
}
