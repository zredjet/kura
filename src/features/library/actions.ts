/**
 * ライブラリの操作（評価・タグ・フォルダ・ユニット・ゴミ箱）。
 * IPC を呼び、終わったら画面のデータを読み直す。失敗は画面下の知らせで伝える。
 */

import {
  askConfirm,
  askPick,
  askText,
  type PickerOption,
} from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import {
  createFolder,
  deleteFolder,
  type FolderNodeDto,
  moveFolder,
  setItemFolders,
  updateFolder,
} from '../../ipc/folders';
import {
  emptyTrash,
  isUnitEntry,
  type ListView,
  queryIds,
  restoreItems,
  trashItems,
  unitEntry,
  unitIdOf,
  updateItems,
} from '../../ipc/items';
import { applyTags } from '../../ipc/tags';
import {
  addUnitItems,
  applyUnitTags,
  createFolderUnit,
  createSetUnit,
  deleteUnit,
  listUnits,
  removeUnitItems,
  reorderUnit,
  setUnitFolders,
  type UnitFolderDto,
  type UnitPatchDto,
  updateUnit,
} from '../../ipc/units';
import { formatCount } from '../../lib/format';
import { getCached, getCachedUnit, invalidateItems, invalidateUnits } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { bumpRevision } from '../../stores/revisionStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { descendantsOf, pathOf } from '../sidebar/tree';
import { insertBefore, moveBlock } from './reorder';
import { selectOne } from './selection';

type ViewKind = ListView['kind'];

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** 操作を実行し、失敗したら知らせる。 */
export async function attempt(action: () => Promise<void>): Promise<void> {
  try {
    await action();
  } catch (error) {
    console.warn('操作できませんでした', error);
    notify(ja.common.failed(messageOf(error)));
  }
}

/** データを変えたあとに読み直す。`listIn` の一覧を開いているときだけ一覧も読み直す。 */
async function refresh(
  items: readonly number[],
  listIn: readonly ViewKind[] | 'always',
): Promise<void> {
  invalidateItems(items);
  invalidateUnits();
  bumpRevision();
  const kind = useViewStore.getState().view.kind;
  const tasks: Promise<void>[] = [useSidebarStore.getState().load()];
  if (listIn === 'always' || listIn.includes(kind)) {
    tasks.push(useLibraryStore.getState().loadList());
  }
  await Promise.all(tasks);
  void useLibraryStore.getState().refresh();
}

/** 選択（ユニットのタイルを含む）をアイテムの ID に開く（ユニットはメンバーに）。 */
export async function expandEntries(entries: readonly number[]): Promise<number[]> {
  const out: number[] = [];
  const seen = new Set<number>();
  for (const id of entries) {
    const ids = isUnitEntry(id)
      ? Array.from((await queryIds({ kind: 'unit', id: unitIdOf(id) })).ids)
      : [id];
    for (const x of ids) {
      if (!seen.has(x)) {
        seen.add(x);
        out.push(x);
      }
    }
  }
  return out;
}

// ---- 評価・タイトル・メモ ----

/** 評価を付ける（ユニットのタイルを 1 つだけ選んでいればユニットの評価）。 */
export function setRating(entries: readonly number[], rating: number): Promise<void> {
  return attempt(async () => {
    const only = entries.length === 1 ? entries[0] : undefined;
    if (only !== undefined && isUnitEntry(only)) {
      await updateUnit(unitIdOf(only), { rating });
      await refresh([], []);
      return;
    }
    const ids = await expandEntries(entries);
    if (ids.length > 0) {
      await updateItems(ids, { rating });
      await refresh(ids, []);
    }
  });
}

/** タイトルを変える（1 件）。 */
export function setTitle(id: number, title: string): Promise<void> {
  return attempt(async () => {
    await updateItems([id], { title });
    await refresh([id], []);
  });
}

/** メモを書き換える（空ならメモを消す）。 */
export function setNote(ids: readonly number[], note: string): Promise<void> {
  return attempt(async () => {
    await updateItems([...ids], { note });
    await refresh(ids, []);
  });
}

// ---- タグ ----

/** タグを付け外しする（名前で付け、ID で外す）。 */
export function changeTags(
  entries: readonly number[],
  add: readonly string[],
  remove: readonly number[],
): Promise<void> {
  return attempt(async () => {
    const ids = await expandEntries(entries);
    if (ids.length === 0) {
      return;
    }
    await applyTags(ids, [...add], [...remove]);
    await refresh(ids, ['untagged']);
  });
}

// ---- ゴミ箱 ----

/** ゴミ箱に入れる。 */
export function trashEntries(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const ids = await expandEntries(entries);
    if (ids.length === 0) {
      return;
    }
    const n = await trashItems(ids);
    notify(ja.notice.trashed(formatCount(n)));
    await refresh(ids, 'always');
  });
}

/** ゴミ箱から戻す。 */
export function restoreEntries(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const ids = entries.filter((id) => !isUnitEntry(id));
    if (ids.length === 0) {
      return;
    }
    const n = await restoreItems(ids);
    notify(ja.notice.restored(formatCount(n)));
    await refresh(ids, 'always');
  });
}

/** 確認してからゴミ箱を空にする。 */
export function emptyTrashWithConfirm(): Promise<void> {
  return attempt(async () => {
    const count = useSidebarStore.getState().counts?.trash ?? 0;
    if (count === 0) {
      return;
    }
    const ok = await askConfirm({
      title: ja.library.emptyTrash,
      message: ja.library.emptyTrashMessage(formatCount(count)),
      confirm: ja.library.emptyTrashConfirm,
    });
    if (!ok) {
      return;
    }
    const result = await emptyTrash();
    notify(
      result.failed > 0
        ? ja.notice.emptiedWithFailures(formatCount(result.items), formatCount(result.failed))
        : ja.notice.emptied(formatCount(result.items)),
    );
    useViewStore.getState().select(null);
    await refresh([], 'always');
  });
}

// ---- フォルダ ----

/** フォルダを選ぶ候補（名前かパスに入力を含むもの）。`create` なら新しく作る候補も足す。 */
export function folderOptions(
  folders: readonly FolderNodeDto[],
  query: string,
  options: { create?: boolean; exclude?: ReadonlySet<number> } = {},
): PickerOption<number | string>[] {
  const q = query.trim().toLowerCase();
  const found: PickerOption<number | string>[] = [];
  for (const folder of folders) {
    if (options.exclude?.has(folder.id)) {
      continue;
    }
    const path = pathOf(folders, folder.id);
    if (q && !path.join('/').toLowerCase().includes(q)) {
      continue;
    }
    found.push({
      key: `folder-${folder.id}`,
      label: folder.name,
      detail: path.length > 1 ? path.slice(0, -1).join(' / ') : undefined,
      value: folder.id,
    });
  }
  found.sort((a, b) => {
    const exact = Number(b.label.toLowerCase() === q) - Number(a.label.toLowerCase() === q);
    return (
      exact || (a.detail ?? '').localeCompare(b.detail ?? '') || a.label.localeCompare(b.label)
    );
  });
  const name = query.trim();
  if (
    options.create &&
    name &&
    !name.includes('/') &&
    !folders.some((f) => f.name === name && f.parentId === null)
  ) {
    found.push({ key: 'new', label: ja.picker.newFolder(name), value: name });
  }
  return found;
}

function folderName(id: number): string {
  return useSidebarStore.getState().folders.find((f) => f.id === id)?.name ?? '';
}

/** アイテムをフォルダに入れる。 */
export function addToFolder(entries: readonly number[], folderId: number): Promise<void> {
  return attempt(async () => {
    const ids = await expandEntries(entries);
    if (ids.length === 0) {
      return;
    }
    await setItemFolders(ids, [folderId], []);
    notify(ja.notice.addedToFolder(formatCount(ids.length), folderName(folderId)));
    await refresh(ids, ['unsorted', 'folder']);
  });
}

/** フォルダを選んでもらって入れる（F）。新しいフォルダもその場で作れる。 */
export function pickFolderFor(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    if (entries.length === 0) {
      return;
    }
    const folders = useSidebarStore.getState().folders;
    const picked = await askPick<number | string>({
      title: ja.picker.folderTitle,
      placeholder: ja.picker.folderPlaceholder,
      empty: ja.picker.noFolder,
      search: (q) => Promise.resolve(folderOptions(folders, q, { create: true })),
    });
    if (!picked) {
      return;
    }
    const folderId =
      typeof picked.value === 'number' ? picked.value : await createFolder(null, picked.value);
    if (typeof picked.value === 'string') {
      await useSidebarStore.getState().load();
    }
    await addToFolder(entries, folderId);
  });
}

/** 開いているフォルダ（と中のフォルダ）から外す（フォルダを開いているときの ⌫）。 */
export function removeFromViewedFolder(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const view = useViewStore.getState().view;
    if (view.kind !== 'folder') {
      return;
    }
    const ids = await expandEntries(entries);
    if (ids.length === 0) {
      return;
    }
    const folders = [...descendantsOf(useSidebarStore.getState().folders, view.id)];
    await setItemFolders(ids, [], folders);
    notify(ja.notice.removedFromFolder(formatCount(ids.length), folderName(view.id)));
    await refresh(ids, 'always');
  });
}

/** フォルダから外す（インスペクタのフォルダの ×）。 */
export function removeFromFolder(entries: readonly number[], folderId: number): Promise<void> {
  return attempt(async () => {
    const ids = await expandEntries(entries);
    if (ids.length === 0) {
      return;
    }
    await setItemFolders(ids, [], [folderId]);
    await refresh(ids, ['unsorted', 'folder']);
  });
}

/** 新しいフォルダを作る（親を開いておく）。 */
export function createFolderNamed(parentId: number | null, name: string): Promise<void> {
  return attempt(async () => {
    await createFolder(parentId, name);
    if (parentId !== null) {
      useSidebarStore.getState().setExpanded(parentId, true);
    }
    await refresh([], []);
  });
}

/** フォルダの名前を変える。 */
export function renameFolder(id: number, name: string): Promise<void> {
  return attempt(async () => {
    await updateFolder(id, { name });
    await refresh([], []);
  });
}

/** フォルダの色を変える（null で外す）。 */
export function setFolderColor(id: number, color: string | null): Promise<void> {
  return attempt(async () => {
    await updateFolder(id, { color: color ?? '' });
    await refresh([], []);
  });
}

/** フォルダを移す（ドラッグ・移動先の選択）。 */
export function moveFolderTo(
  id: number,
  parentId: number | null,
  beforeId: number | null,
): Promise<void> {
  return attempt(async () => {
    await moveFolder(id, parentId, beforeId);
    if (parentId !== null) {
      useSidebarStore.getState().setExpanded(parentId, true);
    }
    await refresh([], ['folder']);
  });
}

/** 移動先を選んでもらってフォルダを移す。 */
export function pickFolderDestination(folder: FolderNodeDto): Promise<void> {
  return attempt(async () => {
    const folders = useSidebarStore.getState().folders;
    const exclude = descendantsOf(folders, folder.id);
    const top: PickerOption<number | string> = {
      key: 'top',
      label: ja.folderMenu.top,
      detail: ja.folderMenu.topDetail,
      value: 'top',
    };
    const picked = await askPick<number | string>({
      title: ja.folderMenu.moveTitle(folder.name),
      placeholder: ja.picker.folderPlaceholder,
      empty: ja.picker.noFolder,
      search: (q) =>
        Promise.resolve([...(q ? [] : [top]), ...folderOptions(folders, q, { exclude })]),
    });
    if (!picked) {
      return;
    }
    await moveFolderTo(folder.id, typeof picked.value === 'number' ? picked.value : null, null);
  });
}

/** 確認してからフォルダを消す（中のアイテムは残る）。 */
export function deleteFolderWithConfirm(folder: FolderNodeDto): Promise<void> {
  return attempt(async () => {
    const removed = descendantsOf(useSidebarStore.getState().folders, folder.id);
    const ok = await askConfirm({
      title: ja.folderMenu.deleteTitle,
      message: ja.folderMenu.deleteMessage(folder.name, removed.size - 1),
      confirm: ja.folderMenu.deleteConfirm,
    });
    if (!ok) {
      return;
    }
    await deleteFolder(folder.id);
    const view = useViewStore.getState().view;
    if (view.kind === 'folder' && removed.has(view.id)) {
      useViewStore.getState().setView({ kind: 'all' });
    }
    await refresh([], ['unsorted']);
  });
}

// ---- ユニット ----

/** 選んだものを set ユニットにまとめる（⌘G）。名前の初期値は先頭のアイテムの名前。 */
export function groupEntries(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const ids = await expandEntries(entries);
    const first = ids[0];
    if (first === undefined) {
      return;
    }
    const name = await askText({
      title: ja.group.title,
      label: ja.group.label,
      initial: getCached(first)?.title ?? '',
      confirm: ja.group.confirm,
    });
    if (!name) {
      return;
    }
    const unitId = await createSetUnit(name, ids);
    notify(ja.notice.grouped(name));
    await refresh(ids, 'always');
    const view = useViewStore.getState();
    if (view.collapseUnits && view.view.kind !== 'unit') {
      view.setSelection(selectOne(unitEntry(unitId)));
    }
  });
}

/** ユニットを解除する（⌘⇧G）。ユニットの中を開いていればそのユニット、でなければ選んだユニットのタイル。 */
export function dissolveUnits(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const view = useViewStore.getState();
    const unitIds =
      view.view.kind === 'unit' ? [view.view.id] : entries.filter(isUnitEntry).map(unitIdOf);
    if (unitIds.length === 0) {
      return;
    }
    const names = unitIds.map((id) => getCachedUnit(id)?.name ?? '');
    for (const id of unitIds) {
      await deleteUnit(id);
    }
    if (view.view.kind === 'unit') {
      view.back();
    }
    notify(ja.notice.dissolved(names.filter(Boolean).join('」「')));
    await refresh([], 'always');
  });
}

/** ユニットを選んでもらってアイテムを加える。 */
export function pickUnitFor(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const ids = await expandEntries(entries);
    if (ids.length === 0) {
      return;
    }
    const picked = await askPick<number>({
      title: ja.picker.unitTitle,
      placeholder: ja.picker.unitPlaceholder,
      empty: ja.picker.noUnit,
      search: async (q) =>
        (await listUnits(q, 'set')).map((unit) => ({
          key: `unit-${unit.id}`,
          label: unit.name,
          value: unit.id,
        })),
    });
    if (!picked) {
      return;
    }
    await addUnitItems(picked.value, ids);
    notify(ja.notice.addedToUnit(formatCount(ids.length), picked.label));
    await refresh(ids, 'always');
  });
}

/** 開いているユニットから外す（ユニットを開いているときの ⌫）。 */
export function removeFromViewedUnit(entries: readonly number[]): Promise<void> {
  return attempt(async () => {
    const view = useViewStore.getState().view;
    const ids = entries.filter((id) => !isUnitEntry(id));
    if (view.kind !== 'unit' || ids.length === 0) {
      return;
    }
    await removeUnitItems(view.id, ids);
    notify(ja.notice.removedFromUnit(formatCount(ids.length), getCachedUnit(view.id)?.name ?? ''));
    await refresh(ids, 'always');
  });
}

function unitOrder(): { unitId: number; order: number[] } | null {
  const view = useViewStore.getState().view;
  const list = useLibraryStore.getState().list;
  if (view.kind !== 'unit' || !list) {
    return null;
  }
  return { unitId: view.id, order: Array.from(list.ids) };
}

/** 開いているユニットの中で、選んだものを前後に 1 つずらす（⌥⌘← →）。 */
export function moveWithinUnit(entries: readonly number[], delta: -1 | 1): Promise<void> {
  return attempt(async () => {
    const current = unitOrder();
    if (!current || entries.length === 0) {
      return;
    }
    const next = moveBlock(current.order, new Set(entries), delta);
    if (next.every((id, i) => id === current.order[i])) {
      return;
    }
    await reorderUnit(current.unitId, next);
    await refresh([], 'always');
  });
}

/** 開いているユニットの中で、ドラッグしたものを `before` の前（null は最後）へ移す。 */
export function dropWithinUnit(entries: readonly number[], before: number | null): Promise<void> {
  return attempt(async () => {
    const current = unitOrder();
    if (!current || entries.length === 0) {
      return;
    }
    await reorderUnit(current.unitId, insertBefore(current.order, new Set(entries), before));
    await refresh([], 'always');
  });
}

/** 開いているユニットの表紙にする。 */
export function makeCover(itemId: number): Promise<void> {
  return attempt(async () => {
    const view = useViewStore.getState().view;
    if (view.kind !== 'unit') {
      return;
    }
    await updateUnit(view.id, { coverItemId: itemId });
    await refresh([], []);
  });
}

/** フォルダを folder ユニットとして使う（子フォルダを含める）。 */
export function makeFolderUnit(folder: FolderNodeDto): Promise<void> {
  return attempt(async () => {
    await createFolderUnit(folder.name, [{ folderId: folder.id, recursive: true, path: [] }]);
    notify(ja.notice.folderUnit(folder.name));
    await refresh([], []);
  });
}

/** folder ユニットをやめる。 */
export function stopUnit(unitId: number): Promise<void> {
  return attempt(async () => {
    await deleteUnit(unitId);
    await refresh([], 'always');
  });
}

/** ユニットの設定を変える（インスペクタ）。 */
export function updateUnitWith(unitId: number, patch: UnitPatchDto): Promise<void> {
  return attempt(async () => {
    await updateUnit(unitId, patch);
    // 並び順を変えたら、開いているユニットの中の並びも変わる
    await refresh([], patch.orderMode ? ['unit'] : []);
  });
}

/** ユニット自体のタグを付け外しする。 */
export function changeUnitTags(
  unitId: number,
  add: readonly string[],
  remove: readonly number[],
): Promise<void> {
  return attempt(async () => {
    await applyUnitTags(unitId, [...add], [...remove]);
    await refresh([], []);
  });
}

/** folder ユニットの参照フォルダを置き換える。 */
export function setUnitFolderRefs(
  unitId: number,
  folders: readonly UnitFolderDto[],
): Promise<void> {
  return attempt(async () => {
    await setUnitFolders(unitId, [...folders]);
    await refresh([], ['unit']);
  });
}

/** 参照するフォルダを選んでもらって folder ユニットに足す。 */
export function pickRefFolder(unitId: number, current: readonly UnitFolderDto[]): Promise<void> {
  return attempt(async () => {
    const folders = useSidebarStore.getState().folders;
    const exclude = new Set(current.map((f) => f.folderId));
    const picked = await askPick<number | string>({
      title: ja.picker.refFolderTitle,
      placeholder: ja.picker.folderPlaceholder,
      empty: ja.picker.noFolder,
      search: (q) => Promise.resolve(folderOptions(folders, q, { exclude })),
    });
    if (!picked || typeof picked.value !== 'number') {
      return;
    }
    await setUnitFolderRefs(unitId, [
      ...current,
      { folderId: picked.value, recursive: true, path: [] },
    ]);
  });
}
