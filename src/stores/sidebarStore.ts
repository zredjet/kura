import { create } from 'zustand';

import { type FolderNodeDto, foldersTree } from '../ipc/folders';
import { libraryCounts, type LibraryCountsDto } from '../ipc/items';
import { type SmartFolderDto, smartFolders } from '../ipc/search';

const STORAGE_KEY = 'kura.sidebar';

/** 開いているフォルダ（見た目の設定なので localStorage に置く）。 */
function loadExpanded(): Record<number, true> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    const ids = raw ? (JSON.parse(raw) as { expanded?: unknown }).expanded : [];
    const out: Record<number, true> = {};
    if (Array.isArray(ids)) {
      ids.filter((id): id is number => Number.isInteger(id)).forEach((id) => (out[id] = true));
    }
    return out;
  } catch {
    return {};
  }
}

function saveExpanded(expanded: Record<number, true>) {
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({ expanded: Object.keys(expanded).map(Number) }),
    );
  } catch {
    // 保存できなくても表示はそのまま続ける
  }
}

/** サイドバーで編集中のもの（フォルダの改名、新しいフォルダの名前入力）。 */
export type SidebarEditing =
  | { kind: 'rename'; id: number }
  | { kind: 'create'; parentId: number | null }
  | { kind: 'renameSmart'; id: number }
  | null;

interface SidebarState {
  counts: LibraryCountsDto | null;
  folders: FolderNodeDto[];
  smartFolders: SmartFolderDto[];
  expanded: Record<number, true>;
  editing: SidebarEditing;
  /** 件数とフォルダを読み直す。 */
  load: () => Promise<void>;
  setExpanded: (id: number, open: boolean) => void;
  setEditing: (editing: SidebarEditing) => void;
  clear: () => void;
}

let loading: Promise<void> | null = null;
let again = false;

export const useSidebarStore = create<SidebarState>((set, get) => ({
  counts: null,
  folders: [],
  smartFolders: [],
  expanded: loadExpanded(),
  editing: null,
  load: () => {
    // 読み込み中に呼ばれたら、終わってからもう一度だけ読む
    if (loading) {
      again = true;
      return loading;
    }
    loading = (async () => {
      try {
        const [counts, folders, smart] = await Promise.all([
          libraryCounts(),
          foldersTree(true),
          smartFolders(true),
        ]);
        set({ counts, folders, smartFolders: smart });
      } catch (error) {
        console.warn('サイドバーの内容を読めませんでした', error);
      } finally {
        loading = null;
        if (again) {
          again = false;
          void get().load();
        }
      }
    })();
    return loading;
  },
  setExpanded: (id, open) => {
    const expanded = { ...get().expanded };
    if (open) {
      expanded[id] = true;
    } else {
      delete expanded[id];
    }
    set({ expanded });
    saveExpanded(expanded);
  },
  setEditing: (editing) => set({ editing }),
  clear: () => set({ counts: null, folders: [], smartFolders: [], editing: null }),
}));
