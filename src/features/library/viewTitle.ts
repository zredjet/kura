import { ja } from '../../i18n/ja';
import type { FolderNodeDto } from '../../ipc/folders';
import type { ListView } from '../../ipc/items';
import type { SmartFolderDto } from '../../ipc/search';
import { getCachedUnit } from '../../stores/itemCache';

/** 一覧の見出し（サイドバーの項目名・フォルダ名・ユニット名・スマートフォルダ名）。 */
export function viewTitle(
  view: ListView,
  folders: readonly FolderNodeDto[],
  smart: readonly SmartFolderDto[] = [],
): string {
  switch (view.kind) {
    case 'all':
      return ja.sidebar.all;
    case 'recent':
      return ja.sidebar.recent;
    case 'unsorted':
      return ja.sidebar.unsorted;
    case 'untagged':
      return ja.sidebar.untagged;
    case 'random':
      return ja.sidebar.random;
    case 'trash':
      return ja.sidebar.trash;
    case 'folder':
      return folders.find((f) => f.id === view.id)?.name ?? ja.sidebar.folders;
    case 'unit':
      return getCachedUnit(view.id)?.name ?? ja.library.unitTitle;
    case 'smart':
      return smart.find((s) => s.id === view.id)?.name ?? ja.library.smartTitle;
  }
}
