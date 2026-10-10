import { create } from 'zustand';

import { EMPTY_SELECTION, type Selection, selectOne } from '../features/library/selection';
import { type ListView, unitEntry } from '../ipc/items';

/** サムネイルの大きさの段階ごとの行の高さの目標（px）。 */
export const ROW_HEIGHTS = [110, 140, 180, 230, 300] as const;
const DEFAULT_SIZE = 2;
const STORAGE_KEY = 'kura.view';

interface Saved {
  thumbSize: number;
  inspector: boolean;
  /** set ユニットを 1 枚のタイルにまとめる（「ユニットをまとめて表示」）。 */
  collapseUnits: boolean;
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
      collapseUnits: parsed.collapseUnits === true,
    };
  } catch {
    return { thumbSize: DEFAULT_SIZE, inspector: true, collapseUnits: false };
  }
}

function save(state: Saved) {
  try {
    const { thumbSize, inspector, collapseUnits } = state;
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ thumbSize, inspector, collapseUnits }));
  } catch {
    // 保存できなくても表示はそのまま続ける
  }
}

interface ViewState extends Saved {
  /** いま開いている一覧（サイドバーの項目、またはユニットの中）。 */
  view: ListView;
  /** ユニットを開いているときの戻り先。 */
  parent: ListView | null;
  selection: Selection;
  quickLook: boolean;
  /** 一覧を切り替える（選択は外す）。 */
  setView: (view: ListView) => void;
  /** ユニットの中を開く（戻り先は今の一覧）。 */
  openUnit: (unitId: number) => void;
  /** ユニットの中から戻る。 */
  back: () => void;
  setSelection: (selection: Selection) => void;
  /** 1 件だけ選ぶ（null は選択を外す）。 */
  select: (id: number | null) => void;
  setThumbSize: (size: number) => void;
  toggleInspector: () => void;
  /** インスペクタを出す（T でタグの入力に移るときなど）。 */
  showInspector: () => void;
  toggleCollapseUnits: () => void;
  setQuickLook: (open: boolean) => void;
}

export const useViewStore = create<ViewState>((set, get) => ({
  ...load(),
  view: { kind: 'all' },
  parent: null,
  selection: EMPTY_SELECTION,
  quickLook: false,
  setView: (view) => set({ view, parent: null, selection: EMPTY_SELECTION, quickLook: false }),
  openUnit: (unitId) => {
    const { view, parent } = get();
    set({
      view: { kind: 'unit', id: unitId },
      parent: view.kind === 'unit' ? parent : view,
      selection: EMPTY_SELECTION,
      quickLook: false,
    });
  },
  back: () => {
    const { view, parent } = get();
    if (view.kind !== 'unit') {
      return;
    }
    set({
      view: parent ?? { kind: 'all' },
      parent: null,
      selection: selectOne(unitEntry(view.id)),
      quickLook: false,
    });
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
  setQuickLook: (open) => set({ quickLook: open && get().selection.focus !== null }),
}));

/** いま注目しているアイテム（インスペクタ・クイックルックの対象）。 */
export function selectFocus(s: ViewState): number | null {
  return s.selection.focus;
}
