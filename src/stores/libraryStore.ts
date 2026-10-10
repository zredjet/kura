import { create } from 'zustand';

import { type IdList, queryIds } from '../ipc/items';
import {
  createLibrary,
  libraryInfo,
  type LibraryInfoDto,
  openLibrary,
  restoreLibrary,
} from '../ipc/library';
import { clearItems } from './itemCache';

type Status = 'loading' | 'none' | 'open';

interface LibraryState {
  status: Status;
  info: LibraryInfoDto | null;
  /** 前回のライブラリが見つからなかったときの場所。 */
  missingPath: string | null;
  /** 一覧（ID・縦横比・日付のグループ）。 */
  list: IdList | null;
  /** 一覧を読み直す。 */
  loadList: () => Promise<void>;
  /** 起動時に前回のライブラリを開く。 */
  restore: () => Promise<void>;
  /** 新しいライブラリを作って開く（失敗は呼び出し側へ）。 */
  create: (path: string) => Promise<void>;
  /** 既存のライブラリを開く（失敗は呼び出し側へ）。 */
  open: (path: string) => Promise<void>;
  /** 件数などを読み直す。 */
  refresh: () => Promise<void>;
}

export const useLibraryStore = create<LibraryState>((set) => ({
  status: 'loading',
  info: null,
  missingPath: null,
  list: null,
  loadList: async () => {
    try {
      set({ list: await queryIds() });
    } catch (error) {
      console.warn('一覧を取得できませんでした', error);
    }
  },
  restore: async () => {
    try {
      const result = await restoreLibrary();
      if (result.info) {
        set({ status: 'open', info: result.info, missingPath: null });
      } else {
        set({ status: 'none', info: null, missingPath: result.lastPath });
      }
    } catch (error) {
      console.warn('前回のライブラリを開けませんでした', error);
      set({ status: 'none', info: null, missingPath: null });
    }
  },
  create: async (path) => {
    const info = await createLibrary(path);
    clearItems();
    set({ status: 'open', info, missingPath: null, list: null });
  },
  open: async (path) => {
    const info = await openLibrary(path);
    clearItems();
    set({ status: 'open', info, missingPath: null, list: null });
  },
  refresh: async () => {
    try {
      set({ info: await libraryInfo() });
    } catch (error) {
      console.warn('ライブラリの概要を読めませんでした', error);
    }
  },
}));
