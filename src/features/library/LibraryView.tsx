import { ja } from '../../i18n/ja';
import { FilterBar } from './FilterBar';
import { GridArea } from './GridArea';
import { Inspector } from './Inspector';
import styles from './LibraryView.module.css';
import { Toolbar } from './Toolbar';

/** ライブラリ画面（Main.dc.html）。上にツールバーとフィルタバー、下にグリッドとインスペクタ。 */
export function LibraryView() {
  return (
    <>
      <Toolbar title={ja.library.allItems} />
      <FilterBar />
      <div className={styles.content}>
        <GridArea />
        <Inspector />
      </div>
    </>
  );
}
