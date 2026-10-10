import { create } from 'zustand';

/**
 * ライブラリのデータを変えるたびに増える番号。
 * インスペクタ・サイドバーの件数など、開いている画面の内容を読み直すきっかけに使う。
 */
export const useRevisionStore = create<{ revision: number }>(() => ({ revision: 0 }));

/** データを変えたことを知らせる。 */
export function bumpRevision(): void {
  useRevisionStore.setState((s) => ({ revision: s.revision + 1 }));
}
