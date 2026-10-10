import { create } from 'zustand';

/** サムネイルの大きさの段階ごとの行の高さの目標（px）。 */
export const ROW_HEIGHTS = [110, 140, 180, 230, 300] as const;
const DEFAULT_SIZE = 2;
const STORAGE_KEY = 'kura.view';

interface Saved {
  thumbSize: number;
  inspector: boolean;
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
    };
  } catch {
    return { thumbSize: DEFAULT_SIZE, inspector: true };
  }
}

function save(state: Saved) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // 保存できなくても表示はそのまま続ける
  }
}

interface ViewState extends Saved {
  /** 選んでいるアイテムの ID。 */
  selectedId: number | null;
  quickLook: boolean;
  select: (id: number | null) => void;
  setThumbSize: (size: number) => void;
  toggleInspector: () => void;
  setQuickLook: (open: boolean) => void;
}

export const useViewStore = create<ViewState>((set, get) => ({
  ...load(),
  selectedId: null,
  quickLook: false,
  select: (id) => set({ selectedId: id }),
  setThumbSize: (size) => {
    const thumbSize = Math.min(ROW_HEIGHTS.length - 1, Math.max(0, Math.round(size)));
    set({ thumbSize });
    save({ thumbSize, inspector: get().inspector });
  },
  toggleInspector: () => {
    const inspector = !get().inspector;
    set({ inspector });
    save({ thumbSize: get().thumbSize, inspector });
  },
  setQuickLook: (open) => set({ quickLook: open && get().selectedId !== null }),
}));
