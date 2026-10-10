import { commands, type FolderNodeDto, type FolderPatchDto } from './bindings';
import { unwrap } from './result';

export type { FolderNodeDto, FolderPatchDto };

/** すべての仮想フォルダ。`withCounts` なら件数（子孫フォルダの分を含む）も付ける。 */
export function foldersTree(withCounts = false): Promise<FolderNodeDto[]> {
  return unwrap(commands.foldersTree(withCounts));
}

/** フォルダを作る（`parentId` が null ならいちばん上）。作ったフォルダの ID を返す。 */
export function createFolder(parentId: number | null, name: string): Promise<number> {
  return unwrap(commands.folderCreate(parentId, name));
}

/** 名前・色を変える（色の空文字は色を外す）。 */
export function updateFolder(id: number, patch: FolderPatchDto): Promise<null> {
  return unwrap(commands.folderUpdate(id, patch));
}

/** フォルダを `parentId` の下の `beforeId` の前（null なら最後）へ移す。 */
export function moveFolder(
  id: number,
  parentId: number | null,
  beforeId: number | null,
): Promise<null> {
  return unwrap(commands.folderMove(id, parentId, beforeId));
}

/** フォルダと中のフォルダを消す（アイテムは残る）。 */
export function deleteFolder(id: number): Promise<number> {
  return unwrap(commands.folderDelete(id));
}

/** アイテムを `add` のフォルダに入れ、`remove` のフォルダから外す。 */
export function setItemFolders(itemIds: number[], add: number[], remove: number[]): Promise<null> {
  return unwrap(commands.itemsSetFolders(itemIds, add, remove));
}
