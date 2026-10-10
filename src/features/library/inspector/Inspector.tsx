import glass from '../../../components/glass/glass.module.css';
import { ja } from '../../../i18n/ja';
import { isUnitEntry, unitIdOf } from '../../../ipc/items';
import { useViewStore } from '../../../stores/viewStore';
import styles from './Inspector.module.css';
import { ItemInspector } from './ItemInspector';
import { MultiInspector } from './MultiInspector';
import { UnitInspector } from './UnitInspector';

/**
 * インスペクタ（Main.dc.html）。選んでいるものに合わせて中身を変える：
 * 1 件のアイテム・ユニットのタイル・複数選択。ユニットの中で何も選んでいなければそのユニット。
 */
export function Inspector() {
  const selection = useViewStore((s) => s.selection);
  const view = useViewStore((s) => s.view);
  const entries = selection.ids;
  const only = entries.length === 1 ? entries[0] : undefined;

  let content;
  if (entries.length > 1) {
    content = <MultiInspector entries={entries} />;
  } else if (only !== undefined && isUnitEntry(only)) {
    content = <UnitInspector key={only} unitId={unitIdOf(only)} opened={false} />;
  } else if (only !== undefined) {
    content = <ItemInspector key={only} id={only} />;
  } else if (view.kind === 'unit') {
    content = <UnitInspector key={`open-${view.id}`} unitId={view.id} opened />;
  } else {
    content = <p className={styles.empty}>{ja.inspector.empty}</p>;
  }
  return (
    <aside className={`${glass.panel} ${styles.inspector}`} aria-label={ja.library.inspectorLabel}>
      {content}
    </aside>
  );
}
