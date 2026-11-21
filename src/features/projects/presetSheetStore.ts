import { create } from 'zustand';

/** 持ち出しの設定のシート（開いているプロジェクト。M15 段 6）。 */
interface PresetSheetState {
  project: number | null;
}

export const usePresetSheetStore = create<PresetSheetState>(() => ({ project: null }));

/** プロジェクトの持ち出しの設定を開く。 */
export function openPresetSheet(project: number): void {
  usePresetSheetStore.setState({ project });
}

/** 閉じる。 */
export function closePresetSheet(): void {
  usePresetSheetStore.setState({ project: null });
}
