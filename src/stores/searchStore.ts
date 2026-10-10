import { create } from 'zustand';

/** ⌘K パレットの開き方。 */
interface OpenOptions {
  /** 最初に並べておく条件（チップ）。 */
  initial?: string[];
  /** 条件を書き換えるスマートフォルダ（↵・⌘↵ で上書き保存する）。 */
  editing?: { id: number; name: string } | null;
  /** 新しいスマートフォルダを作る（↵・⌘↵ で名前を聞いて保存する）。 */
  createSmart?: boolean;
}

interface SearchState {
  open: boolean;
  initial: string[];
  editing: { id: number; name: string } | null;
  createSmart: boolean;
  openPalette: (options?: OpenOptions) => void;
  close: () => void;
}

/** ⌘K パレットの状態（検索の条件そのものは viewStore の `filter`）。 */
export const useSearchStore = create<SearchState>((set) => ({
  open: false,
  initial: [],
  editing: null,
  createSmart: false,
  openPalette: (options = {}) =>
    set({
      open: true,
      initial: options.initial ?? [],
      editing: options.editing ?? null,
      createSmart: options.createSmart ?? false,
    }),
  close: () => set({ open: false, editing: null, createSmart: false }),
}));
