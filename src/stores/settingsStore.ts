import { create } from 'zustand';

/** 設定画面のタブ。 */
export type SettingsTab = 'general' | 'library' | 'downloads';

interface SettingsState {
  open: boolean;
  tab: SettingsTab;
  /** 開く（タブを指定すればそのタブ）。 */
  show: (tab?: SettingsTab) => void;
  setTab: (tab: SettingsTab) => void;
  close: () => void;
}

/** 設定画面（⌘,・サイドバーの歯車）の開閉。 */
export const useSettingsStore = create<SettingsState>((set) => ({
  open: false,
  tab: 'general',
  show: (tab) => set((s) => ({ open: true, tab: tab ?? s.tab })),
  setTab: (tab) => set({ tab }),
  close: () => set({ open: false }),
}));
