import { Button } from '../../components/Button/Button';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { GridIcon } from '../../components/icons/GridIcon';
import { InspectorIcon } from '../../components/icons/InspectorIcon';
import { JustifyIcon } from '../../components/icons/JustifyIcon';
import { ListIcon } from '../../components/icons/ListIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { SearchIcon } from '../../components/icons/SearchIcon';
import { SortIcon } from '../../components/icons/SortIcon';
import { UnitIcon } from '../../components/icons/UnitIcon';
import { ja } from '../../i18n/ja';
import { LAYOUT_MODES, type LayoutMode } from '../../stores/viewStore';
import styles from './Toolbar.module.css';

const LAYOUT_ICONS = { justify: JustifyIcon, grid: GridIcon, list: ListIcon } as const;

interface ToolbarProps {
  title: string;
  subtitle: string;
  /** ユニットの中を開いているときの戻り先（パンくずの「すべて › 鎌倉」の左側）。 */
  parentTitle: string | null;
  onBack: () => void;
  onImport: () => void;
  inspectorVisible: boolean;
  onToggleInspector: () => void;
  collapseUnits: boolean;
  /** まとめて表示を切り替えられる一覧か（ゴミ箱とユニットの中では切り替えない）。 */
  canCollapse: boolean;
  onToggleCollapse: () => void;
  /** ゴミ箱を開いているときの「ゴミ箱を空にする」（null なら出さない）。 */
  onEmptyTrash: (() => void) | null;
  /** 掛かっている検索の条件（空なら欄に案内を出す）。 */
  query: string;
  onOpenSearch: () => void;
  onClearSearch: () => void;
  /** 並び順のボタンの文字（null なら並び順を切り替えない一覧）。 */
  sortLabel: string | null;
  onSortMenu: (anchor: HTMLElement) => void;
  /** 表示形式（⌘1〜⌘3）。 */
  layout: LayoutMode;
  onLayout: (layout: LayoutMode) => void;
}

/**
 * 画面上部のツールバー（Main.dc.html）。ウィンドウのドラッグ領域を兼ねる。
 */
export function Toolbar({
  title,
  subtitle,
  parentTitle,
  onBack,
  onImport,
  inspectorVisible,
  onToggleInspector,
  collapseUnits,
  canCollapse,
  onToggleCollapse,
  onEmptyTrash,
  query,
  onOpenSearch,
  onClearSearch,
  sortLabel,
  onSortMenu,
  layout,
  onLayout,
}: ToolbarProps) {
  return (
    <header className={styles.toolbar} data-tauri-drag-region>
      <div className={styles.heading} data-tauri-drag-region>
        <h1 className={styles.title} data-tauri-drag-region>
          {parentTitle !== null && (
            <>
              <button
                type="button"
                className={styles.crumb}
                aria-label={ja.library.back(parentTitle)}
                onClick={onBack}
              >
                {parentTitle}
              </button>
              <span className={styles.crumbSeparator} aria-hidden="true">
                ›
              </span>
            </>
          )}
          {title}
        </h1>
        <span className={styles.subtitle} data-tauri-drag-region>
          {subtitle}
        </span>
      </div>
      <div className={styles.search} data-active={query !== '' || undefined}>
        <button
          type="button"
          className={styles.searchButton}
          aria-label={query ? `${ja.search.fieldLabel}：${query}` : ja.search.fieldLabel}
          aria-haspopup="dialog"
          onClick={onOpenSearch}
        >
          <SearchIcon size={16} strokeWidth={1.9} />
          <span className={query ? styles.searchQuery : styles.searchPlaceholder}>
            {query || ja.search.placeholder}
          </span>
          {!query && <kbd className={styles.kbd}>⌘K</kbd>}
        </button>
        {query && (
          <button
            type="button"
            className={styles.searchClear}
            aria-label={ja.search.clear}
            onClick={onClearSearch}
          >
            <CloseIcon size={10} strokeWidth={3} />
          </button>
        )}
      </div>
      <div className={styles.segment} role="group" aria-label={ja.layouts.label}>
        {LAYOUT_MODES.map((mode, i) => {
          const ModeIcon = LAYOUT_ICONS[mode];
          return (
            <button
              key={mode}
              type="button"
              className={styles.segmentButton}
              aria-label={ja.layouts[mode]}
              aria-pressed={layout === mode}
              title={`${ja.layouts[mode]}（⌘${i + 1}）`}
              onClick={() => onLayout(mode)}
            >
              <ModeIcon size={16} strokeWidth={1.8} />
            </button>
          );
        })}
      </div>
      {sortLabel !== null && (
        <button
          type="button"
          className={styles.pillButton}
          aria-label={`${ja.sort.label}：${sortLabel}`}
          aria-haspopup="menu"
          onClick={(e) => onSortMenu(e.currentTarget)}
        >
          <SortIcon size={15} strokeWidth={1.8} />
          {sortLabel}
        </button>
      )}
      {onEmptyTrash && (
        <Button size="sm" onClick={onEmptyTrash}>
          {ja.library.emptyTrash}
        </Button>
      )}
      {canCollapse && (
        <button
          type="button"
          className={styles.iconButton}
          aria-label={ja.library.collapseUnits}
          aria-pressed={collapseUnits}
          title={ja.library.collapseUnits}
          onClick={onToggleCollapse}
        >
          <UnitIcon size={18} strokeWidth={1.8} />
        </button>
      )}
      <button
        type="button"
        className={styles.importButton}
        aria-label={ja.library.importFiles}
        onClick={onImport}
      >
        <PlusIcon size={18} strokeWidth={2.4} />
      </button>
      <button
        type="button"
        className={styles.iconButton}
        aria-label={ja.library.toggleInspector}
        aria-pressed={inspectorVisible}
        onClick={onToggleInspector}
      >
        <InspectorIcon size={18} strokeWidth={1.8} />
      </button>
    </header>
  );
}
