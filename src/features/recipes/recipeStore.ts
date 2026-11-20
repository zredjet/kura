import { create } from 'zustand';

import { ja } from '../../i18n/ja';
import { listRecipes, type RecipeSummaryDto, reorderRecipes } from '../../ipc/recipes';
import { notify } from '../../stores/noticeStore';
import { movedOrder } from '../playlists/playlistActions';

/** レシピの一覧（サイドバーの節。M14 段 7）。 */
interface RecipeState {
  list: RecipeSummaryDto[];
  /** 名前を変えている行。 */
  renaming: number | null;
  /** 一覧を読み直す（重なった呼び出しはまとめる）。 */
  load: () => Promise<void>;
  setRenaming: (id: number | null) => void;
  clear: () => void;
}

let loading: Promise<void> | null = null;
let again = false;

export const useRecipeStore = create<RecipeState>((set) => ({
  list: [],
  renaming: null,
  load: () => {
    if (loading) {
      again = true;
      return loading;
    }
    loading = (async () => {
      try {
        do {
          again = false;
          const list = await listRecipes();
          set({ list });
        } while (again);
      } catch {
        // 読めなければ前の一覧のまま（ライブラリを閉じた直後など）
      } finally {
        loading = null;
      }
    })();
    return loading;
  },
  setRenaming: (renaming) => set({ renaming }),
  clear: () => set({ list: [], renaming: null }),
}));

/** 並びを変える：`id` を `target` の前か後ろへ（先に並べ替えて見せ、失敗したら読み直す。ドラッグ・⌥⌘↑↓）。 */
export async function moveRecipe(
  id: number,
  target: number,
  side: 'before' | 'after',
): Promise<void> {
  const list = useRecipeStore.getState().list;
  const ids = list.map((r) => r.id);
  const next = movedOrder(ids, id, target, side);
  if (next.every((x, i) => x === ids[i])) return;
  const byId = new Map(list.map((r) => [r.id, r]));
  useRecipeStore.setState({
    list: next.map((x) => byId.get(x)).filter((r): r is RecipeSummaryDto => r !== undefined),
  });
  try {
    await reorderRecipes(next);
  } catch (error) {
    notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
  }
  await useRecipeStore.getState().load();
}
