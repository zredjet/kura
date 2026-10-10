import { commands, type FolderNodeDto } from './bindings';
import { unwrap } from './result';

export type { FolderNodeDto };

/** すべての仮想フォルダ。 */
export function foldersTree(): Promise<FolderNodeDto[]> {
  return unwrap(commands.foldersTree());
}
