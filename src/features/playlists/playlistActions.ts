import {
  type AddModeValue,
  askAddMode,
  askConfirm,
  askNewPlaylist,
  askPick,
} from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import { getCached } from '../../stores/itemCache';
import { isUnitEntry, unitIdOf } from '../../ipc/items';
import {
  addToPlaylist,
  type AddSourceDto,
  createPlaylist,
  deletePlaylist,
  type GranularityDto,
  type PlaylistPatchDto,
  type PlaylistSummaryDto,
  regeneratePlaylist,
  removeEntries,
  restoreEntries,
  setGranularity,
  snapshotToFiles,
  updatePlaylist,
} from '../../ipc/playlists';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { bumpRevision } from '../../stores/revisionStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { closePlayer, startPlayback } from '../player/playerActions';
import { usePlayerStore } from '../player/playerStore';

/** 「1 つのユニットにまとめる／1 件ずつ」の前回の選び方（見た目の設定なので localStorage）。 */
const ADD_MODE_KEY = 'kura.playlistAddMode';

function fail(error: unknown): void {
  notify(error instanceof Error ? error.message : String(error));
}

/** サイドバー・プレイリストの画面を読み直す。 */
function refreshPlaylists(): void {
  bumpRevision();
  void useSidebarStore.getState().load();
}

/** 新しいプレイリストを作って開く。 */
export async function createPlaylistInteractive(): Promise<void> {
  const value = await askNewPlaylist({
    name: ja.newPlaylist.defaultName,
    kind: 'manual',
    granularity: 'file',
  });
  if (!value) {
    return;
  }
  try {
    const id = await createPlaylist(value.name, value.kind, value.granularity);
    refreshPlaylists();
    useViewStore.getState().openPlaylist(id);
  } catch (error) {
    fail(error);
  }
}

export async function renamePlaylist(id: number, name: string): Promise<void> {
  try {
    await updatePlaylist(id, { name });
    refreshPlaylists();
  } catch (error) {
    fail(error);
  }
}

/** 確かめてから削除する。開いていればライブラリに戻り、再生していれば止める。 */
export async function deletePlaylistWithConfirm(playlist: PlaylistSummaryDto): Promise<void> {
  const t = ja.playlistMenu;
  const ok = await askConfirm({
    title: t.deleteTitle,
    message: t.deleteMessage(playlist.name),
    confirm: t.deleteConfirm,
  });
  if (!ok) {
    return;
  }
  try {
    if (usePlayerStore.getState().queue?.playlist?.id === playlist.id) {
      closePlayer();
    }
    await deletePlaylist(playlist.id);
    if (useViewStore.getState().playlist === playlist.id) {
      useViewStore.getState().setView({ kind: 'all' });
    }
    refreshPlaylists();
  } catch (error) {
    fail(error);
  }
}

/** 設定を変える。 */
export async function updatePlaylistSettings(
  id: number,
  patch: PlaylistPatchDto,
): Promise<boolean> {
  try {
    await updatePlaylist(id, patch);
    bumpRevision();
    return true;
  } catch (error) {
    fail(error);
    return false;
  }
}

/** 単位を切り替える（エントリは変えない。§3.4）。 */
export async function switchGranularity(id: number, granularity: GranularityDto): Promise<void> {
  try {
    await setGranularity(id, granularity);
    refreshPlaylists();
  } catch (error) {
    fail(error);
  }
}

/** スマートの並びを作り直す。 */
export async function regenerate(id: number): Promise<void> {
  try {
    await regeneratePlaylist(id);
    refreshPlaylists();
  } catch (error) {
    fail(error);
  }
}

/** プレイリストを再生する（`resume` なら続きから、`fromItem` ならそのアイテムから）。 */
export function playPlaylist(
  id: number,
  name: string,
  resume = false,
  fromItem?: number,
): Promise<void> {
  return startPlayback({ kind: 'playlist', id }, name, { resume, fromItem: fromItem ?? null });
}

function loadAddMode(): { mode: 'group' | 'each'; remember: boolean } {
  try {
    const raw = JSON.parse(localStorage.getItem(ADD_MODE_KEY) ?? '{}') as {
      mode?: unknown;
      remember?: unknown;
    };
    return {
      mode: raw.mode === 'each' ? 'each' : 'group',
      remember: raw.remember === true,
    };
  } catch {
    return { mode: 'group', remember: false };
  }
}

function saveAddMode(value: AddModeValue): void {
  try {
    localStorage.setItem(
      ADD_MODE_KEY,
      JSON.stringify({ mode: value.mode, remember: value.remember }),
    );
  } catch {
    // 保存できなくても追加はそのまま続ける
  }
}

/**
 * 一覧の選択（アイテムと `0x8000_0000 | unit_id`）をプレイリストに入れる（§3.5）。
 * ユニット単位に複数のアイテムを入れるときは、まとめるか 1 件ずつか選んでもらう（前回の選び方が既定）。
 */
export async function addEntriesToPlaylist(
  playlist: PlaylistSummaryDto,
  entries: readonly number[],
  invert = false,
): Promise<void> {
  const sources: AddSourceDto[] = entries.map((id) =>
    isUnitEntry(id) ? { kind: 'unit', id: unitIdOf(id) } : { kind: 'item', id },
  );
  const items = entries.filter((id) => !isUnitEntry(id));
  let groupName: string | null = null;
  if (playlist.granularity === 'unit' && items.length > 1) {
    const saved = loadAddMode();
    let choice: AddModeValue | null = {
      mode: saved.mode,
      name: getCached(items[0] ?? -1)?.title ?? '',
      remember: saved.remember,
    };
    if (!saved.remember) {
      choice = await askAddMode(items.length, choice);
      if (!choice) {
        return;
      }
      saveAddMode(choice);
    }
    if (choice.mode === 'group') {
      groupName = choice.name || ja.newPlaylist.defaultName;
    }
  }
  await addSources(playlist, sources, invert, groupName);
}

/** フォルダをプレイリストに入れる（ファイル単位では展開、ユニット単位では folder エントリ。⌥ で逆）。 */
export function addFolderToPlaylist(
  playlist: PlaylistSummaryDto,
  folderId: number,
  invert = false,
): Promise<void> {
  return addSources(playlist, [{ kind: 'folder', id: folderId }], invert, null);
}

async function addSources(
  playlist: PlaylistSummaryDto,
  sources: AddSourceDto[],
  invert: boolean,
  groupName: string | null,
): Promise<void> {
  try {
    const added = await addToPlaylist(playlist.id, sources, { invert, groupName });
    notify(ja.playlist.added(formatCount(added.entryIds.length), playlist.name));
    refreshPlaylists();
  } catch (error) {
    fail(error);
  }
}

/** 入れる先の手動プレイリストを選んでから入れる（P・右クリックの「プレイリストに追加…」）。 */
export async function pickPlaylistFor(entries: readonly number[]): Promise<void> {
  if (entries.length === 0) {
    return;
  }
  const t = ja.playlist;
  const manual = useSidebarStore.getState().playlists.filter((p) => p.kind === 'manual');
  const picked = await askPick<PlaylistSummaryDto>({
    title: t.addTitle,
    placeholder: t.addPlaceholder,
    empty: t.addEmpty,
    search: (query) =>
      Promise.resolve(
        manual
          .filter((p) => p.name.toLowerCase().includes(query.trim().toLowerCase()))
          .map((p) => ({
            key: String(p.id),
            label: p.name,
            detail: `${p.granularity === 'unit' ? ja.playlist.unit : ja.playlist.file} · ${t.items(formatCount(p.itemCount))}`,
            value: p,
          })),
      ),
  });
  if (picked) {
    await addEntriesToPlaylist(picked.value, entries);
  }
}

/** エントリを外す。 */
export async function removeFromPlaylist(id: number, entryIds: number[]): Promise<void> {
  if (entryIds.length === 0) {
    return;
  }
  try {
    await removeEntries(id, entryIds);
    notify(ja.playlist.removed(formatCount(entryIds.length)));
    refreshPlaylists();
  } catch (error) {
    fail(error);
  }
}

/** 「ファイルに固定」（取り消せる。§3.4）。 */
export async function snapshotPlaylist(id: number, entryIds: number[] | null): Promise<void> {
  try {
    const previous = await snapshotToFiles(id, entryIds);
    refreshPlaylists();
    notify(ja.playlist.snapshotDone, {
      label: ja.playlist.undo,
      run: () => {
        restoreEntries(id, previous).then(refreshPlaylists).catch(fail);
      },
    });
  } catch (error) {
    fail(error);
  }
}
