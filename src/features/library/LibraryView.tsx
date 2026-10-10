import { useEffect } from 'react';

import { ja } from '../../i18n/ja';
import type { FolderNodeDto } from '../../ipc/folders';
import type { ListView } from '../../ipc/items';
import { formatCount } from '../../lib/format';
import { getCachedUnit, useItemCacheVersion } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { ImportProgress } from '../import/ImportProgress';
import { pickAndImport } from '../import/useFileDrop';
import { emptyTrashWithConfirm } from './actions';
import { FilterBar } from './FilterBar';
import { Grid } from './Grid';
import { Inspector } from './inspector/Inspector';
import styles from './LibraryView.module.css';
import { QuickLook } from './QuickLook';
import { Toolbar } from './Toolbar';
import { useLibraryKeys } from './useLibraryKeys';

/** 一覧の見出し（サイドバーの項目名・フォルダ名・ユニット名）。 */
export function viewTitle(view: ListView, folders: readonly FolderNodeDto[]): string {
  switch (view.kind) {
    case 'all':
      return ja.sidebar.all;
    case 'recent':
      return ja.sidebar.recent;
    case 'unsorted':
      return ja.sidebar.unsorted;
    case 'untagged':
      return ja.sidebar.untagged;
    case 'random':
      return ja.sidebar.random;
    case 'trash':
      return ja.sidebar.trash;
    case 'folder':
      return folders.find((f) => f.id === view.id)?.name ?? ja.sidebar.folders;
    case 'unit':
      return getCachedUnit(view.id)?.name ?? ja.library.unitTitle;
  }
}

/** ライブラリ画面（Main.dc.html）。上にツールバーとフィルタバー、下にグリッドとインスペクタ。 */
export function LibraryView() {
  const list = useLibraryStore((s) => s.list);
  const itemCount = useLibraryStore((s) => s.info?.itemCount ?? 0);
  const folders = useSidebarStore((s) => s.folders);
  const trashCount = useSidebarStore((s) => s.counts?.trash ?? 0);
  const view = useViewStore((s) => s.view);
  const parent = useViewStore((s) => s.parent);
  const back = useViewStore((s) => s.back);
  const inspector = useViewStore((s) => s.inspector);
  const toggleInspector = useViewStore((s) => s.toggleInspector);
  const collapseUnits = useViewStore((s) => s.collapseUnits);
  const toggleCollapseUnits = useViewStore((s) => s.toggleCollapseUnits);
  useItemCacheVersion((s) => s.version);
  useLibraryKeys();

  useEffect(() => {
    void useLibraryStore.getState().loadList();
  }, [view, collapseUnits]);

  // 「すべて」はライブラリの件数（まとめて表示のときは一覧の件数＝ユニットを 1 件と数える）
  const count = formatCount(
    view.kind === 'all' && !collapseUnits ? itemCount : (list?.ids.length ?? 0),
  );
  let subtitle = ja.library.subtitle(count);
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
        title={viewTitle(view, folders)}
        subtitle={subtitle}
        parentTitle={view.kind === 'unit' ? viewTitle(parent ?? { kind: 'all' }, folders) : null}
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
      />
      <FilterBar />
      <div className={styles.content}>
        <div className={styles.gridColumn}>
          <Grid />
          <ImportProgress />
        </div>
        {inspector && <Inspector />}
      </div>
      <QuickLook />
    </>
  );
}
