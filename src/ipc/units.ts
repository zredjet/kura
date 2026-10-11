import {
  commands,
  type OrderModeDto,
  type UnitDetailDto,
  type UnitFolderDto,
  type UnitKindDto,
  type UnitPatchDto,
  type UnitRefDto,
  type UnitSummaryDto,
} from './bindings';
import { unwrap } from './result';

export type {
  OrderModeDto,
  UnitDetailDto,
  UnitFolderDto,
  UnitKindDto,
  UnitPatchDto,
  UnitRefDto,
  UnitSummaryDto,
};

/** 見えている分のユニットの要約（最大 200 件）。 */
export function getUnits(ids: number[]): Promise<UnitSummaryDto[]> {
  return unwrap(commands.unitsGet(ids));
}

/** ユニットのインスペクタ用の詳細。 */
export function getUnitDetail(id: number): Promise<UnitDetailDto> {
  return unwrap(commands.unitDetail(id));
}

/** ユニットを選ぶ一覧（名前に `filter` を含むもの）。 */
export function listUnits(
  filter: string,
  kind: UnitKindDto | null = null,
  limit = 50,
): Promise<UnitRefDto[]> {
  return unwrap(commands.unitsList(filter, kind, limit));
}

/** アイテムを set ユニットにまとめる（渡した順が並び順）。 */
export function createSetUnit(name: string, itemIds: number[]): Promise<number> {
  return unwrap(commands.unitCreateSet(name, itemIds));
}

/** フォルダを参照する folder ユニットを作る。 */
export function createFolderUnit(name: string, folders: UnitFolderDto[]): Promise<number> {
  return unwrap(commands.unitCreateFolder(name, folders));
}

/** ユニットの設定を変える（`imageDurationMs` の 0 は既定に戻す、メモの空文字は消す）。 */
export function updateUnit(id: number, patch: UnitPatchDto): Promise<null> {
  return unwrap(commands.unitUpdate(id, patch));
}

/** set ユニットの最後にアイテムを加える。 */
export function addUnitItems(id: number, itemIds: number[]): Promise<null> {
  return unwrap(commands.unitAddItems(id, itemIds));
}

/** set ユニットからアイテムを外す。 */
export function removeUnitItems(id: number, itemIds: number[]): Promise<null> {
  return unwrap(commands.unitRemoveItems(id, itemIds));
}

/** set ユニットのメンバーを渡した順に並べ直す（並び順は手動になる）。 */
export function reorderUnit(id: number, itemIds: number[]): Promise<null> {
  return unwrap(commands.unitReorder(id, itemIds));
}

/** folder ユニットの参照フォルダを置き換える。 */
export function setUnitFolders(id: number, folders: UnitFolderDto[]): Promise<null> {
  return unwrap(commands.unitSetFolders(id, folders));
}

/** ユニットを解除する（アイテムは残る）。 */
export function deleteUnit(id: number): Promise<null> {
  return unwrap(commands.unitDelete(id));
}
