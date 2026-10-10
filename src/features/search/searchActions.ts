/** 検索とスマートフォルダの操作（保存・上書き・改名・削除）。 */

import { askConfirm, askText } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import {
  deleteSmartFolder,
  saveSmartFolder,
  type SmartFolderDto,
  updateSmartFolder,
} from '../../ipc/search';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { attempt } from '../library/actions';

/** 名前を聞いて、条件をスマートフォルダとして保存し、そのスマートフォルダを開く。保存したら true。 */
export async function saveAsSmartFolder(query: string): Promise<boolean> {
  let saved = false;
  await attempt(async () => {
    if (!query.trim()) {
      notify(ja.search.needQuery);
      return;
    }
    const name = await askText({
      title: ja.search.saveTitle,
      label: ja.search.saveLabel,
      initial: '',
      confirm: ja.search.saveConfirm,
    });
    if (!name) {
      return;
    }
    const id = await saveSmartFolder(name, query);
    notify(ja.search.saved(name));
    await useSidebarStore.getState().load();
    useViewStore.getState().setView({ kind: 'smart', id });
    saved = true;
  });
  return saved;
}

/** スマートフォルダの条件を上書きする。 */
export async function updateSmartQuery(id: number, name: string, query: string): Promise<boolean> {
  let saved = false;
  await attempt(async () => {
    if (!query.trim()) {
      notify(ja.search.needQuery);
      return;
    }
    await updateSmartFolder(id, { query });
    notify(ja.search.updated(name));
    await useSidebarStore.getState().load();
    const view = useViewStore.getState().view;
    if (view.kind === 'smart' && view.id === id) {
      await useLibraryStore.getState().loadList();
    } else {
      useViewStore.getState().setView({ kind: 'smart', id });
    }
    saved = true;
  });
  return saved;
}

/** スマートフォルダの名前を変える。 */
export function renameSmartFolder(id: number, name: string): Promise<void> {
  return attempt(async () => {
    await updateSmartFolder(id, { name });
    await useSidebarStore.getState().load();
  });
}

/** 確認してからスマートフォルダを消す（アイテムは残る）。 */
export function deleteSmartWithConfirm(folder: SmartFolderDto): Promise<void> {
  return attempt(async () => {
    const ok = await askConfirm({
      title: ja.smartMenu.deleteTitle,
      message: ja.smartMenu.deleteMessage(folder.name),
      confirm: ja.smartMenu.deleteConfirm,
    });
    if (!ok) {
      return;
    }
    await deleteSmartFolder(folder.id);
    const view = useViewStore.getState().view;
    if (view.kind === 'smart' && view.id === folder.id) {
      useViewStore.getState().setView({ kind: 'all' });
    }
    await useSidebarStore.getState().load();
  });
}
