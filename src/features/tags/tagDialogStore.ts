import { create } from 'zustand';

/** 開いているタグの編集ダイアログ（タグの ID）。 */
export const useTagDialogStore = create<{ tagId: number | null }>(() => ({ tagId: null }));

/** タグの編集ダイアログを開く。 */
export function openTagDialog(tagId: number): void {
  useTagDialogStore.setState({ tagId });
}

/** タグの編集ダイアログを閉じる。 */
export function closeTagDialog(): void {
  useTagDialogStore.setState({ tagId: null });
}
