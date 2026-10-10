import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { useLibraryStore } from '../../stores/libraryStore';
import { ImportProgress } from '../import/ImportProgress';
import { pickAndImport } from '../import/useFileDrop';
import { FilterBar } from './FilterBar';
import { GridArea } from './GridArea';
import { Inspector } from './Inspector';
import styles from './LibraryView.module.css';
import { Toolbar } from './Toolbar';

/** ライブラリ画面（Main.dc.html）。上にツールバーとフィルタバー、下にグリッドとインスペクタ。 */
export function LibraryView() {
  const itemCount = useLibraryStore((s) => s.info?.itemCount ?? 0);
  return (
    <>
      <Toolbar
        title={ja.library.allItems}
        subtitle={ja.library.itemCount(formatCount(itemCount))}
        onImport={() => {
          pickAndImport().catch((error: unknown) =>
            console.warn('ファイルを選べませんでした', error),
          );
        }}
      />
      <FilterBar />
      <div className={styles.content}>
        <div className={styles.gridColumn}>
          <GridArea />
          <ImportProgress />
        </div>
        <Inspector />
      </div>
    </>
  );
}
