import { useEffect } from 'react';

import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { ImportProgress } from '../import/ImportProgress';
import { pickAndImport } from '../import/useFileDrop';
import { FilterBar } from './FilterBar';
import { Grid } from './Grid';
import { Inspector } from './Inspector';
import styles from './LibraryView.module.css';
import { QuickLook } from './QuickLook';
import { Toolbar } from './Toolbar';
import { useLibraryKeys } from './useLibraryKeys';

/** ライブラリ画面（Main.dc.html）。上にツールバーとフィルタバー、下にグリッドとインスペクタ。 */
export function LibraryView() {
  const itemCount = useLibraryStore((s) => s.info?.itemCount ?? 0);
  const inspector = useViewStore((s) => s.inspector);
  const toggleInspector = useViewStore((s) => s.toggleInspector);
  useLibraryKeys();

  useEffect(() => {
    void useLibraryStore.getState().loadList();
  }, []);

  return (
    <>
      <Toolbar
        title={ja.library.allItems}
        subtitle={ja.library.subtitle(formatCount(itemCount))}
        onImport={() => {
          pickAndImport().catch((error: unknown) =>
            console.warn('ファイルを選べませんでした', error),
          );
        }}
        inspectorVisible={inspector}
        onToggleInspector={toggleInspector}
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
