import { create } from 'zustand';

import { dragOutTarget, type DragOutTargetDto } from '../../ipc/dragout';
import { listProjects, type ProjectDto } from '../../ipc/projects';

/** 登録したプロジェクトと持ち出し先（サイドバーの節・一覧の見出し・ドラッグの印。M15 段 6）。 */
interface ProjectState {
  list: ProjectDto[];
  /** 持ち出し先（選んだもの・名前と形式に使うもの）。読む前は null。 */
  target: DragOutTargetDto | null;
  /** 名前を変えている行。 */
  renaming: number | null;
  /** 一覧と持ち出し先を読み直す（重なった呼び出しはまとめる）。 */
  load: () => Promise<void>;
  setRenaming: (id: number | null) => void;
  clear: () => void;
}

let loading: Promise<void> | null = null;
let again = false;

export const useProjectStore = create<ProjectState>((set) => ({
  list: [],
  target: null,
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
          const [list, target] = await Promise.all([listProjects(), dragOutTarget()]);
          set({ list, target });
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
  clear: () => set({ list: [], target: null, renaming: null }),
}));

/** 名前と形式に使う持ち出し先のプロジェクト（なければ null）。 */
export function effectiveTarget(
  list: readonly ProjectDto[],
  target: DragOutTargetDto | null,
): ProjectDto | null {
  if (target?.effective == null) {
    return null;
  }
  return list.find((p) => p.id === target.effective) ?? null;
}
