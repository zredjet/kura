import {
  type ClauseDto,
  commands,
  type QueryCheckDto,
  type SmartFolderDto,
  type SuggestionDto,
} from './bindings';
import { type ListView, utcOffsetMinutes } from './items';
import { unwrap } from './result';

export type { ClauseDto, QueryCheckDto, SmartFolderDto, SuggestionDto };

/** 入力を解析し、開いている一覧の中で一致する件数を数える。 */
export function queryParse(text: string, view: ListView | null): Promise<QueryCheckDto> {
  return unwrap(commands.queryParse(text, view, utcOffsetMinutes()));
}

/** 入力中の語の候補。 */
export function querySuggest(word: string, limit = 8): Promise<SuggestionDto[]> {
  return unwrap(commands.querySuggest(word, limit));
}

/** すべてのスマートフォルダ（`withCounts` なら件数も）。 */
export function smartFolders(withCounts = false): Promise<SmartFolderDto[]> {
  return unwrap(commands.smartFoldersList(withCounts, utcOffsetMinutes()));
}

/** 検索をスマートフォルダとして保存する。 */
export function saveSmartFolder(name: string, query: string): Promise<number> {
  return unwrap(commands.smartFolderSave(name, query));
}

/** スマートフォルダの名前・条件を変える。 */
export function updateSmartFolder(
  id: number,
  patch: { name?: string; query?: string },
): Promise<null> {
  return unwrap(commands.smartFolderUpdate(id, patch.name ?? null, patch.query ?? null));
}

/** スマートフォルダを消す。 */
export function deleteSmartFolder(id: number): Promise<null> {
  return unwrap(commands.smartFolderDelete(id));
}
