import { create } from 'zustand';

import { EMPTY_SELECTION, type Selection, selectOne } from '../features/library/selection';
import { DEFAULT_SORT, type ItemSort, type ListView, type SortKey, unitEntry } from '../ipc/items';

/** サムネイルの大きさの段階ごとの行の高さの目標（px）。 */
export const ROW_HEIGHTS = [110, 140, 180, 230, 300] as const;
const DEFAULT_SIZE = 2;
const STORAGE_KEY = 'kura.view';

/** 一覧の表示形式（⌘1 ジャスティファイ・⌘2 グリッド・⌘3 リスト。design.md §6）。 */
export type LayoutMode = 'justify' | 'grid' | 'list';
export const LAYOUT_MODES: readonly LayoutMode[] = ['justify', 'grid', 'list'];

interface Saved {
  thumbSize: number;
  inspector: boolean;
  /** 表示形式。 */
  layout: LayoutMode;
  /** サイドバーを出しているか（⌘⌥S）。 */
  sidebar: boolean;
  /** set ユニットを 1 枚のタイルにまとめる（「ユニットをまとめて表示」）。 */
  collapseUnits: boolean;
  /** 並び順。 */
  sort: ItemSort;
}

const SORT_KEYS: readonly SortKey[] = [
  'imported',
  'captured',
  'name',
  'rating',
  'duration',
  'size',
];

function loadSort(value: unknown): ItemSort {
  const sort = value as Partial<ItemSort> | undefined;
  if (sort && SORT_KEYS.includes(sort.key as SortKey) && typeof sort.desc === 'boolean') {
    return { key: sort.key as SortKey, desc: sort.desc };
  }
  return DEFAULT_SORT;
}

/** 表示の設定だけを localStorage に置く（ライブラリのデータは置かない）。 */
function load(): Saved {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const parsed = raw ? (JSON.parse(raw) as Partial<Saved>) : {};
    const size = Number(parsed.thumbSize);
    return {
      thumbSize:
        Number.isInteger(size) && size >= 0 && size < ROW_HEIGHTS.length ? size : DEFAULT_SIZE,
      inspector: parsed.inspector !== false,
      layout: LAYOUT_MODES.includes(parsed.layout as LayoutMode)
        ? (parsed.layout as LayoutMode)
        : 'justify',
      sidebar: parsed.sidebar !== false,
      collapseUnits: parsed.collapseUnits === true,
      sort: loadSort(parsed.sort),
    };
  } catch {
    return {
      thumbSize: DEFAULT_SIZE,
      inspector: true,
      layout: 'justify',
      sidebar: true,
      collapseUnits: false,
      sort: DEFAULT_SORT,
    };
  }
}

function save(state: Saved) {
  try {
    const { thumbSize, inspector, layout, sidebar, collapseUnits, sort } = state;
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ thumbSize, inspector, layout, sidebar, collapseUnits, sort }),
    );
  } catch {
    // 保存できなくても表示はそのまま続ける
  }
}

interface ViewState extends Saved {
  /** いま開いている一覧（サイドバーの項目、またはユニットの中）。 */
  view: ListView;
  /** プレイリストの画面を開いていればその ID（一覧の代わりに出す）。 */
  playlist: number | null;
  /** ダウンロードの画面を開いているか（一覧の代わりに出す）。 */
  downloads: boolean;
  /** タグの一覧の画面を開いているか（一覧の代わりに出す）。 */
  tags: boolean;
  /** ユニットを開いているときの戻り先。 */
  parent: ListView | null;
  /** 検索の条件（チップの列。docs/search-query.md）。一覧を切り替えると外す（ADR-53）。 */
  filter: string[];
  /** ユニットを開く前の条件（戻ったら戻す）。 */
  parentFilter: string[];
  selection: Selection;
  quickLook: boolean;
  /** 一覧を切り替える（選択は外す。プレイリストの画面は閉じる）。 */
  setView: (view: ListView) => void;
  /** プレイリストの画面を開く。 */
  openPlaylist: (id: number) => void;
  /** ダウンロードの画面を開く。 */
  openDownloads: () => void;
  /** タグの一覧の画面を開く。 */
  openTags: () => void;
  /** ユニットの中を開く（戻り先は今の一覧）。 */
  openUnit: (unitId: number) => void;
  /** ユニットの中から戻る。 */
  back: () => void;
  setFilter: (filter: string[]) => void;
  setSort: (sort: ItemSort) => void;
  setSelection: (selection: Selection) => void;
  /** 1 件だけ選ぶ（null は選択を外す）。 */
  select: (id: number | null) => void;
  setThumbSize: (size: number) => void;
  toggleInspector: () => void;
  /** インスペクタを出す（T でタグの入力に移るときなど）。 */
  showInspector: () => void;
  toggleCollapseUnits: () => void;
  setLayout: (layout: LayoutMode) => void;
  toggleSidebar: () => void;
  setQuickLook: (open: boolean) => void;
}

export const useViewStore = create<ViewState>((set, get) => ({
  ...load(),
  view: { kind: 'all' },
  playlist: null,
  downloads: false,
  tags: false,
  parent: null,
  filter: [],
  parentFilter: [],
  selection: EMPTY_SELECTION,
  quickLook: false,
  setView: (view) =>
    set({
      view,
      playlist: null,
      downloads: false,
      tags: false,
      parent: null,
      filter: [],
      parentFilter: [],
      selection: EMPTY_SELECTION,
      quickLook: false,
    }),
  openPlaylist: (id) =>
    set({
      playlist: id,
      downloads: false,
      tags: false,
      selection: EMPTY_SELECTION,
      quickLook: false,
    }),
  openDownloads: () =>
    set({
      downloads: true,
      playlist: null,
      tags: false,
      selection: EMPTY_SELECTION,
      quickLook: false,
    }),
  openTags: () =>
    set({
      tags: true,
      downloads: false,
      playlist: null,
      selection: EMPTY_SELECTION,
      quickLook: false,
    }),
  openUnit: (unitId) => {
    const { view, parent, filter, parentFilter } = get();
    const fromUnit = view.kind === 'unit';
    set({
      view: { kind: 'unit', id: unitId },
      playlist: null,
      downloads: false,
      tags: false,
      parent: fromUnit ? parent : view,
      filter: [],
      parentFilter: fromUnit ? parentFilter : filter,
      selection: EMPTY_SELECTION,
      quickLook: false,
    });
  },
  back: () => {
    const { view, parent, parentFilter } = get();
    if (view.kind !== 'unit') {
      return;
    }
    set({
      view: parent ?? { kind: 'all' },
      parent: null,
      filter: parentFilter,
      parentFilter: [],
      selection: selectOne(unitEntry(view.id)),
      quickLook: false,
    });
  },
  setFilter: (filter) => set({ filter, selection: EMPTY_SELECTION, quickLook: false }),
  setSort: (sort) => {
    set({ sort });
    save({ ...get(), sort });
  },
  setSelection: (selection) =>
    set({ selection, quickLook: get().quickLook && selection.focus !== null }),
  select: (id) => get().setSelection(id === null ? EMPTY_SELECTION : selectOne(id)),
  setThumbSize: (size) => {
    const thumbSize = Math.min(ROW_HEIGHTS.length - 1, Math.max(0, Math.round(size)));
    set({ thumbSize });
    save({ ...get(), thumbSize });
  },
  toggleInspector: () => {
    const inspector = !get().inspector;
    set({ inspector });
    save({ ...get(), inspector });
  },
  showInspector: () => {
    if (!get().inspector) {
      get().toggleInspector();
    }
  },
  toggleCollapseUnits: () => {
    const collapseUnits = !get().collapseUnits;
    set({ collapseUnits });
    save({ ...get(), collapseUnits });
  },
  setLayout: (layout) => {
    set({ layout });
    save({ ...get(), layout });
  },
  toggleSidebar: () => {
    const sidebar = !get().sidebar;
    set({ sidebar });
    save({ ...get(), sidebar });
  },
  setQuickLook: (open) => set({ quickLook: open && get().selection.focus !== null }),
}));

/** いま注目しているアイテム（インスペクタ・クイックルックの対象）。 */
export function selectFocus(s: ViewState): number | null {
  return s.selection.focus;
}
