import { create } from 'zustand';

import { type LicenseDto, listLicenses } from '../../ipc/credits';

/** ライセンスの表（インスペクタの選択・設定のタブ。M15 段 6）。 */
interface LicenseState {
  list: LicenseDto[];
  /** 読み直す（重なった呼び出しはまとめる）。 */
  load: () => Promise<void>;
  clear: () => void;
}

let loading: Promise<void> | null = null;
let again = false;

export const useLicenseStore = create<LicenseState>((set) => ({
  list: [],
  load: () => {
    if (loading) {
      again = true;
      return loading;
    }
    loading = (async () => {
      try {
        do {
          again = false;
          set({ list: await listLicenses() });
        } while (again);
      } catch {
        // 読めなければ前の表のまま（ライブラリを閉じた直後など）
      } finally {
        loading = null;
      }
    })();
    return loading;
  },
  clear: () => set({ list: [] }),
}));
