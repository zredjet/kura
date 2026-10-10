import { invoke } from '@tauri-apps/api/core';

import {
  commands,
  type ItemDetailDto,
  type ItemPatchDto,
  type ItemSummaryDto,
  type LibraryCountsDto,
  type SelectionInfoDto,
  type TrashEmptiedDto,
} from './bindings';
import { unwrap } from './result';

export type {
  ItemDetailDto,
  ItemPatchDto,
  ItemSummaryDto,
  LibraryCountsDto,
  SelectionInfoDto,
  TrashEmptiedDto,
};

/** 一覧でユニットを表す ID の印（`LIST_FLAG | unitId`。docs/architecture.md §3）。 */
export const LIST_FLAG = 0x8000_0000;

/** 一覧の ID がユニットか。 */
export function isUnitEntry(id: number): boolean {
  return id >= LIST_FLAG;
}

/** 一覧の ID からユニットの ID を取り出す。 */
export function unitIdOf(id: number): number {
  return id - LIST_FLAG;
}

/** ユニットの ID から一覧の ID を作る。 */
export function unitEntry(unitId: number): number {
  return LIST_FLAG + unitId;
}

/** 一覧の種類（サイドバーの項目。`items_query_ids` の `view`）。 */
export type ListView =
  | { kind: 'all' }
  | { kind: 'recent' }
  | { kind: 'unsorted' }
  | { kind: 'untagged' }
  | { kind: 'random'; seed: number }
  | { kind: 'trash' }
  | { kind: 'folder'; id: number }
  | { kind: 'unit'; id: number };

/** 日付の見出しのグループ（`start` 番目から始まる、`day` 日のアイテム）。 */
export interface DayGroup {
  start: number;
  /** 1970-01-01 からの日数（問い合わせたときの時差での日付）。 */
  day: number;
}

/** 一覧（ID 先取り）。詳細は持たない。 */
export interface IdList {
  ids: Uint32Array;
  /** 縦横比×1000（音楽と不明は 1000）。 */
  aspects: Uint16Array;
  groups: DayGroup[];
}

/**
 * `items_query_ids` の生バイトを読む（docs/architecture.md §3）。すべてリトルエンディアン。
 * `[件数 u32][id u32 × N][縦横比×1000 u16 × N][グループ数 u32][開始位置 u32 × G][日 i32 × G]`
 */
export function parseIdList(buffer: ArrayBuffer): IdList {
  const view = new DataView(buffer);
  let offset = 0;
  const count = view.getUint32(offset, true);
  offset += 4;
  const ids = new Uint32Array(count);
  for (let i = 0; i < count; i += 1) {
    ids[i] = view.getUint32(offset, true);
    offset += 4;
  }
  const aspects = new Uint16Array(count);
  for (let i = 0; i < count; i += 1) {
    aspects[i] = view.getUint16(offset, true);
    offset += 2;
  }
  const groupCount = view.getUint32(offset, true);
  offset += 4;
  const starts: number[] = [];
  for (let i = 0; i < groupCount; i += 1) {
    starts.push(view.getUint32(offset, true));
    offset += 4;
  }
  const groups: DayGroup[] = starts.map((start) => {
    const day = view.getInt32(offset, true);
    offset += 4;
    return { start, day };
  });
  return { ids, aspects, groups };
}

/** この端末の UTC との時差（分）。日付の見出しの区切りに使う。 */
export function utcOffsetMinutes(now: Date = new Date()): number {
  return -now.getTimezoneOffset();
}

/** 一覧の ID・縦横比・日付のグループを取る。`collapseUnits` なら set ユニットを 1 件にまとめる。 */
export async function queryIds(
  view: ListView = { kind: 'all' },
  collapseUnits = false,
): Promise<IdList> {
  const buffer = await invoke<ArrayBuffer>('items_query_ids', {
    utcOffsetMinutes: utcOffsetMinutes(),
    view,
    collapseUnits,
  });
  return parseIdList(buffer);
}

/** 見えている分の詳細（最大 200 件）。 */
export function getItems(ids: number[]): Promise<ItemSummaryDto[]> {
  return unwrap(commands.itemsGet(ids));
}

/** インスペクタ用の詳細。 */
export function getItemDetail(id: number): Promise<ItemDetailDto> {
  return unwrap(commands.itemDetail(id));
}

/** サイドバーの「ライブラリ」の件数。 */
export function libraryCounts(): Promise<LibraryCountsDto> {
  return unwrap(commands.libraryCounts());
}

/** 複数選択のインスペクタに出す集計。 */
export function selectionInfo(ids: number[]): Promise<SelectionInfoDto> {
  return unwrap(commands.itemsSelectionInfo(ids));
}

/** タイトル（1 件のとき）・評価・メモを変える。メモの空文字は消す。 */
export function updateItems(ids: number[], patch: ItemPatchDto): Promise<null> {
  return unwrap(commands.itemsUpdate(ids, patch));
}

/** ゴミ箱に入れる。入れた件数を返す。 */
export function trashItems(ids: number[]): Promise<number> {
  return unwrap(commands.itemsTrash(ids));
}

/** ゴミ箱から戻す。戻した件数を返す。 */
export function restoreItems(ids: number[]): Promise<number> {
  return unwrap(commands.itemsRestore(ids));
}

/** ゴミ箱を空にする（実ファイルも消える）。 */
export function emptyTrash(): Promise<TrashEmptiedDto> {
  return unwrap(commands.trashEmpty());
}
