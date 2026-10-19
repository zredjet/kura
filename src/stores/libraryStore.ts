import { create } from 'zustand';

import { prune } from '../features/library/selection';
import { joinClauses } from '../features/search/clauses';
import { type IdList, queryIds } from '../ipc/items';
import {
  type AppError,
  createLibrary,
  libraryInfo,
  type LibraryInfoDto,
  openLibrary,
  restoreLibrary,
} from '../ipc/library';
import { isIpcError } from '../ipc/result';
import { clearItems } from './itemCache';
import { notify } from './noticeStore';
import { useSidebarStore } from './sidebarStore';
import { settleUpgrade, watchUpgrades } from './upgradeStore';
import { useViewStore } from './viewStore';

type Status = 'loading' | 'none' | 'open';

interface LibraryState {
  status: Status;
  info: LibraryInfoDto | null;
  /** 前回のライブラリが見つからなかったときの場所。 */
  missingPath: string | null;
  /**
   * 前回のライブラリ（か `KURA_LIBRARY`）を開けなかった理由と場所（新しい版の Kura のライブラリ・空き容量・
   * バックアップの失敗・新しい版にできない、など。M11）。
   */
  restoreError: { path: string | null; error: AppError } | null;
  /** 一覧（ID・縦横比・日付のグループ）。 */
  list: IdList | null;
  /** 一覧を読み直す。 */
  loadList: () => Promise<void>;
  /** 起動時に前回のライブラリを開く（起動の間に 1 回だけ。続けて呼ばれたら同じ 1 回を待つ）。 */
  restore: () => Promise<void>;
  /** 新しいライブラリを作って開く（失敗は呼び出し側へ）。 */
  create: (path: string) => Promise<void>;
  /** 既存のライブラリを開く（失敗は呼び出し側へ）。 */
  open: (path: string) => Promise<void>;
  /** 件数などを読み直す。 */
  refresh: () => Promise<void>;
}

/** 開いている途中の「前回のライブラリを開く」。 */
let restoring: Promise<void> | null = null;

export const useLibraryStore = create<LibraryState>((set, get) => ({
  status: 'loading',
  info: null,
  missingPath: null,
  restoreError: null,
  list: null,
  loadList: async () => {
    const { view, collapseUnits, filter, sort } = useViewStore.getState();
    try {
      const list = await queryIds(view, { collapseUnits, query: joinClauses(filter), sort });
      // 読んでいるあいだに一覧・条件・並び順を変えていたら捨てる
      const now = useViewStore.getState();
      if (
        now.view !== view ||
        now.collapseUnits !== collapseUnits ||
        now.filter !== filter ||
        now.sort !== sort
      ) {
        return;
      }
      set({ list });
      now.setSelection(prune(now.selection, list.ids));
    } catch (error) {
      if (isIpcError(error, 'NotFound')) {
        // 開いていたフォルダ・ユニット・スマートフォルダがなくなったら「すべて」に戻る
        useViewStore.getState().setView({ kind: 'all' });
        return;
      }
      if (isIpcError(error, 'Invalid') || isIpcError(error, 'InvalidQuery')) {
        // スマートフォルダの条件が壊れているときなど
        notify(error.message);
        set({ list: { ids: new Uint32Array(), aspects: new Uint16Array(), groups: [] } });
        return;
      }
      console.warn('一覧を取得できませんでした', error);
    }
  },
  restore: () => {
    // 開発時は StrictMode で effect が 2 回走る。同じライブラリを並べて 2 回開くと、DB を新しい版にする前の
    // バックアップがぶつかって両方失敗する（実測）ので、開いている途中ならそれを待ち、開き終えたら何もしない
    if (restoring) {
      return restoring;
    }
    if (get().status !== 'loading') {
      return Promise.resolve();
    }
    restoring = restoreOnce(set).finally(() => {
      restoring = null;
    });
    return restoring;
  },
  create: async (path) => {
    const info = await createLibrary(path);
    resetForLibrary();
    set({ status: 'open', info, missingPath: null, restoreError: null, list: null });
  },
  open: async (path) => {
    await watchUpgrades();
    try {
      const info = await openLibrary(path);
      resetForLibrary();
      set({ status: 'open', info, missingPath: null, restoreError: null, list: null });
    } finally {
      settleUpgrade();
    }
  },
  refresh: async () => {
    try {
      set({ info: await libraryInfo() });
    } catch (error) {
      console.warn('ライブラリの概要を読めませんでした', error);
    }
  },
}));

/** 前回のライブラリを開く（`restore` の中身）。 */
async function restoreOnce(set: (state: Partial<LibraryState>) => void): Promise<void> {
  // 開く途中で DB を新しい版にするときの知らせを落とさないよう、受け取りを始めてから開く
  await watchUpgrades();
  try {
    const result = await restoreLibrary();
    if (result.info) {
      set({ status: 'open', info: result.info, missingPath: null, restoreError: null });
    } else if (result.error) {
      set({
        status: 'none',
        info: null,
        missingPath: null,
        restoreError: { path: result.lastPath, error: result.error },
      });
    } else {
      set({ status: 'none', info: null, missingPath: result.lastPath, restoreError: null });
    }
  } catch (error) {
    console.warn('前回のライブラリを開けませんでした', error);
    set({ status: 'none', info: null, missingPath: null, restoreError: null });
  } finally {
    settleUpgrade();
  }
}

/** ライブラリを切り替えたときに、前のライブラリの表示を捨てる。 */
function resetForLibrary(): void {
  clearItems();
  useSidebarStore.getState().clear();
  useViewStore.getState().setView({ kind: 'all' });
}
