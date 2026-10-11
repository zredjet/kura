import {
  type AddedDto,
  type AddSourceDto,
  commands,
  type EntryDto,
  type EntryRowDto,
  type EntryTargetDto,
  type GranularityDto,
  type LimitDto,
  type PlaylistContentsDto,
  type PlaylistDto,
  type PlaylistKindDto,
  type PlaylistPatchDto,
  type PlaylistSummaryDto,
  type PlayStateDto,
  type RepeatModeDto,
  type SmartRuleDto,
  type UnitMatchDto,
} from './bindings';
import { utcOffsetMinutes } from './items';
import { unwrap } from './result';

export type {
  AddedDto,
  AddSourceDto,
  EntryDto,
  EntryRowDto,
  EntryTargetDto,
  GranularityDto,
  LimitDto,
  PlaylistContentsDto,
  PlaylistDto,
  PlaylistKindDto,
  PlaylistPatchDto,
  PlaylistSummaryDto,
  PlayStateDto,
  RepeatModeDto,
  SmartRuleDto,
  UnitMatchDto,
};

/** すべてのプレイリスト（サイドバー）。 */
export function listPlaylists(): Promise<PlaylistSummaryDto[]> {
  return unwrap(commands.playlistsList());
}

/** 設定と、続きから再生できる位置。 */
export function getPlaylist(id: number): Promise<PlaylistDto> {
  return unwrap(commands.playlistGet(id));
}

/** 作る。ID を返す。 */
export function createPlaylist(
  name: string,
  kind: PlaylistKindDto,
  granularity: GranularityDto,
): Promise<number> {
  return unwrap(commands.playlistCreate(name, kind, granularity));
}

/** 設定を変える（省いた項目は変えない）。 */
export function updatePlaylist(id: number, patch: PlaylistPatchDto): Promise<null> {
  return unwrap(commands.playlistUpdate(id, patch));
}

/** サイドバーの並びを変える（`ids` の順）。 */
export function reorderPlaylists(ids: number[]): Promise<null> {
  return unwrap(commands.playlistsReorder(ids));
}

export function deletePlaylist(id: number): Promise<null> {
  return unwrap(commands.playlistDelete(id));
}

/** 単位を切り替える（エントリは変えない）。 */
export function setGranularity(id: number, granularity: GranularityDto): Promise<null> {
  return unwrap(commands.playlistSetGranularity(id, granularity));
}

/** 中身（エントリの行と、展開したアイテムの列）。 */
export function playlistContents(id: number): Promise<PlaylistContentsDto> {
  return unwrap(commands.playlistContents(id));
}

/** 追加する（docs/playlists.md §3.5）。 */
export function addToPlaylist(
  id: number,
  sources: AddSourceDto[],
  options: { invert?: boolean; groupName?: string | null; before?: number | null } = {},
): Promise<AddedDto> {
  return unwrap(
    commands.playlistAdd(
      id,
      sources,
      options.invert ?? false,
      options.groupName ?? null,
      options.before ?? null,
    ),
  );
}

/** エントリを `before` の前（null なら末尾）へ移す。 */
export function moveEntries(id: number, entryIds: number[], before: number | null): Promise<null> {
  return unwrap(commands.playlistMove(id, entryIds, before));
}

export function removeEntries(id: number, entryIds: number[]): Promise<null> {
  return unwrap(commands.playlistRemove(id, entryIds));
}

/** 「ファイルに固定」。取り消し用に前のエントリを返す。 */
export function snapshotToFiles(id: number, entryIds: number[] | null): Promise<EntryDto[]> {
  return unwrap(commands.playlistSnapshotToFiles(id, entryIds));
}

/** 前のエントリに戻す。 */
export function restoreEntries(id: number, entries: EntryDto[]): Promise<null> {
  return unwrap(commands.playlistRestoreEntries(id, entries));
}

/** スマートの並びを作り直す。 */
export function regeneratePlaylist(id: number): Promise<null> {
  return unwrap(commands.playlistRegenerate(id, utcOffsetMinutes()));
}

/** 再開の位置を保存する（null なら消す）。 */
export function savePlayState(id: number, state: PlayStateDto | null): Promise<null> {
  return unwrap(commands.playlistSaveState(id, state));
}
