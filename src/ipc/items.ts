import { invoke } from '@tauri-apps/api/core';

import {
  commands,
  type EntriesUpdatedDto,
  type EntryPatchDto,
  type ItemDetailDto,
  type ItemPatchDto,
  type ItemSummaryDto,
  type ItemViewDto,
  type LibraryCountsDto,
  type SelectionInfoDto,
  type TrashEmptiedDto,
} from './bindings';
import { unwrap } from './result';

export type {
  EntriesUpdatedDto,
  EntryPatchDto,
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

/** 一覧の種類（サイドバーの項目・ユニットの中・スマートフォルダ。`items_query_ids` の `view`）。 */
export type ListView = ItemViewDto;

/** 並べる基準。 */
export type SortKey = 'imported' | 'captured' | 'name' | 'rating' | 'duration' | 'size';

/** 並び順（`desc` は大きい・新しい順）。 */
export interface ItemSort {
  key: SortKey;
  desc: boolean;
}

export const DEFAULT_SORT: ItemSort = { key: 'imported', desc: true };

/** 撮影日で並べたとき、撮影日のないもののグループの日（Rust の `NO_DATE_DAY`）。 */
export const NO_DATE_DAY = -2147483648;

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

/** 一覧の取り方。 */
export interface ListOptions {
  /** set ユニットを 1 件にまとめる。 */
  collapseUnits?: boolean;
  /** 検索の条件（docs/search-query.md）。 */
  query?: string;
  sort?: ItemSort;
}

/** 一覧の ID・縦横比・日付のグループを取る。 */
export async function queryIds(
  view: ListView = { kind: 'all' },
  options: ListOptions = {},
): Promise<IdList> {
  const buffer = await invoke<ArrayBuffer>('items_query_ids', {
    utcOffsetMinutes: utcOffsetMinutes(),
    view,
    collapseUnits: options.collapseUnits ?? false,
    query: options.query ?? null,
    sort: options.sort ?? null,
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

/** 一覧の ID（ユニットのタイルを含む）を、アイテムの ID とユニットの ID に分ける。 */
export function splitEntries(entries: readonly number[]): { itemIds: number[]; unitIds: number[] } {
  const itemIds: number[] = [];
  const unitIds: number[] = [];
  for (const id of entries) {
    if (isUnitEntry(id)) {
      unitIds.push(unitIdOf(id));
    } else {
      itemIds.push(id);
    }
  }
  return { itemIds, unitIds };
}

/**
 * 評価・メモ・タグをまとめて付け外しする。ユニットのタイルはユニット自体とメンバー全員に付ける
 * （docs/playlists.md §2.1）。メモの空文字は消す。
 */
export function updateEntries(
  entries: readonly number[],
  patch: EntryPatchDto,
): Promise<EntriesUpdatedDto> {
  const { itemIds, unitIds } = splitEntries(entries);
  return unwrap(commands.entriesUpdate(itemIds, unitIds, patch));
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
