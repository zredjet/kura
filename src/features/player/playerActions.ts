import { ja } from '../../i18n/ja';
import { getItems } from '../../ipc/items';
import { playUrl } from '../../ipc/media';
import {
  getPlayerPrefs,
  openExternal,
  type PlayerPrefsDto,
  playQueue,
  type PlayRequestDto,
  type PlaySourceDto,
  reportPlayback,
  requestProxies,
  setPlayerPrefs,
} from '../../ipc/player';
import { savePlayState, updatePlaylist } from '../../ipc/playlists';
import { notify } from '../../stores/noticeStore';
import { bumpRevision } from '../../stores/revisionStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { PlaybackController } from './engine/PlaybackController';
import { type PlayerSnapshot, type Queue, realClock, type RepeatMode } from './engine/types';
import { usePlayerStore } from './playerStore';

/** 設定（音量など）を保存するまで待つ時間。 */
const SAVE_DELAY_MS = 500;
/** 先に decode した画像を持っておく数。 */
const KEEP_IMAGES = 4;
/** 再開の位置を保存する間隔（再生中）。 */
const STATE_SAVE_MS = 5000;

let instance: PlaybackController | null = null;
const decoded: HTMLImageElement[] = [];

/** 再生のコントローラ（アプリに 1 つ）。 */
export function playerController(): PlaybackController {
  if (instance) {
    return instance;
  }
  const controller = new PlaybackController({
    clock: realClock,
    createMedia: (kind) => document.createElement(kind),
    fetchItems: getItems,
    report: reportPlayback,
    requestProxies,
    // 作り直したプロキシは別の URL にして、前の失敗を読み直さないようにする
    playUrl: (hash, attempt) => (attempt > 0 ? `${playUrl(hash)}?a=${attempt}` : playUrl(hash)),
    preloadImage: (url) => {
      const img = new Image();
      img.src = url;
      img.decode().catch(() => undefined);
      decoded.push(img);
      decoded.splice(0, Math.max(0, decoded.length - KEEP_IMAGES));
    },
  });
  controller.subscribe((snapshot) => {
    usePlayerStore.setState({ snapshot });
    void keepPlayState(snapshot);
  });
  controller.onRecorded((itemId) => usePlayerStore.getState().addHistory(itemId));
  controller.onWrap(() => void wrapAround());
  applyPrefs(controller, usePlayerStore.getState().prefs);
  instance = controller;
  return controller;
}

/** 再生の設定を反映する。プレイリストを再生しているときは、画像の間の音楽はプレイリストの設定。 */
function applyPrefs(controller: PlaybackController, prefs: PlayerPrefsDto): void {
  const playlist = usePlayerStore.getState().queue?.playlist ?? null;
  controller.setVolume(prefs.volume / 100);
  controller.setPrefs({
    throughImages: playlist ? playlist.bgmThroughImages : prefs.bgmThroughImages,
    loop: prefs.bgmLoop,
    imageMs: prefs.imageDurationMs,
  });
}

function fail(error: unknown): void {
  notify(error instanceof Error ? error.message : String(error));
}

/** キューのシャッフル（プレイリストはその設定、その場のキューは今の状態）。 */
function isShuffled(queue: Queue | null): boolean {
  return queue?.playlist ? queue.playlist.shuffle : usePlayerStore.getState().shuffle;
}

/**
 * 再生を始める。`title` はシアターの見出し。
 * プレイリストはその設定どおり（`request.resume` なら続きから）、選択はそのまま、一覧はシャッフルして。
 */
export async function startPlayback(
  source: PlaySourceDto,
  title: string,
  request: Partial<PlayRequestDto> = {},
): Promise<void> {
  try {
    const queue = await playQueue(source, request);
    if (queue.itemIds.length === 0) {
      notify(ja.player.nothingToPlay);
      return;
    }
    const controller = playerController();
    const shuffle = request.shuffle ?? source.kind === 'view';
    usePlayerStore.getState().open(queue, queue.playlist?.name ?? title, source, shuffle);
    controller.resetImageOverride();
    controller.setRepeat((queue.playlist?.repeatMode as RepeatMode | undefined) ?? 'off');
    applyPrefs(controller, usePlayerStore.getState().prefs);
    controller.start(queue, queue.startIndex, queue.startPositionMs);
    lastSaved = { index: -1, at: 0 };
  } catch (error) {
    fail(error);
  }
}

/** 選択（並び順のまま）を再生する。 */
export function playEntries(entries: number[], title: string): Promise<void> {
  if (entries.length === 0) {
    return Promise.resolve();
  }
  return startPlayback({ kind: 'selection', entries }, title);
}

/** 今のキューを作り直す：シャッフルの切り替え（S）・並べ直す。今のアイテム（のユニット）を先頭にする。 */
export async function rebuildQueue(change: { shuffle?: boolean; reseed?: boolean }): Promise<void> {
  const { source, queue, snapshot } = usePlayerStore.getState();
  if (!source || !queue || !instance) {
    return;
  }
  const shuffle = change.shuffle ?? isShuffled(queue);
  try {
    const next = await playQueue(source, {
      // その場のキューは毎回シャッフルを指定する（プレイリストは切り替えたときだけ）
      shuffle: queue.playlist ? (change.shuffle ?? null) : shuffle,
      reseed: change.reseed ?? false,
      startItem: snapshot.itemId,
    });
    if (next.itemIds.length === 0) {
      return;
    }
    usePlayerStore.getState().replaceQueue(next, shuffle);
    instance.start(next, next.startIndex, change.shuffle === undefined ? 0 : snapshot.positionMs);
    if (next.playlist) {
      // プレイリストのシャッフル・シードが変わったので、開いている画面を読み直す
      bumpRevision();
    }
  } catch (error) {
    fail(error);
  }
}

/** 全体を繰り返すとき、キューの終わりで：シャッフルでシードを固定していなければ新しいシードで作り直す（§5.2）。 */
async function wrapAround(): Promise<void> {
  const { source, queue } = usePlayerStore.getState();
  if (!source || !queue || !instance) {
    return;
  }
  const reseed = isShuffled(queue) && !queue.playlist?.seedLocked;
  if (!reseed) {
    instance.start(queue, 0);
    return;
  }
  try {
    const next = await playQueue(source, {
      reseed: true,
      shuffle: queue.playlist ? null : true,
    });
    usePlayerStore.getState().replaceQueue(next, true);
    instance.start(next, 0);
  } catch (error) {
    fail(error);
  }
}

/** シャッフルを切り替える（今のアイテムを先頭に作り直す。§6.3 の S）。 */
export function toggleShuffle(): Promise<void> {
  return rebuildQueue({ shuffle: !isShuffled(usePlayerStore.getState().queue) });
}

/** 繰り返しを次へ（オフ → 全体 → 1 曲）。プレイリストならその設定も変える。 */
export function cycleRepeat(): void {
  const { snapshot, queue } = usePlayerStore.getState();
  const order: RepeatMode[] = ['off', 'all', 'one'];
  const next = order[(order.indexOf(snapshot.repeat) + 1) % order.length] ?? 'off';
  instance?.setRepeat(next);
  if (queue?.playlist) {
    usePlayerStore
      .getState()
      .replaceQueue(
        { ...queue, playlist: { ...queue.playlist, repeatMode: next } },
        isShuffled(queue),
      );
    updatePlaylist(queue.playlist.id, { repeatMode: next }).then(bumpRevision, fail);
  }
}

/** 再生しているプレイリストの設定を変える（画像の間の音楽・同じ種類を続けない）。 */
export function updatePlayingPlaylist(patch: {
  bgmThroughImages?: boolean;
  maxSameKindRun?: number;
}): void {
  const queue = usePlayerStore.getState().queue;
  if (!queue?.playlist) {
    return;
  }
  const playlist = { ...queue.playlist, ...patch };
  usePlayerStore.getState().replaceQueue({ ...queue, playlist }, isShuffled(queue));
  if (instance) {
    applyPrefs(instance, usePlayerStore.getState().prefs);
  }
  updatePlaylist(queue.playlist.id, patch).then(bumpRevision, fail);
}

// ===== 再開の位置（docs/playlists.md §6.5） =====

let lastSaved = { index: -1, at: 0 };

/**
 * プレイリストを再生しているあいだ、スロットが変わったときと 5 秒ごとに位置を保存する。終わりまで再生したら消す。
 * `force` なら間隔に関係なく保存する（閉じるとき）。保存したときはその完了を返す。
 */
function keepPlayState(snapshot: PlayerSnapshot, force = false): Promise<unknown> | undefined {
  const queue = usePlayerStore.getState().queue;
  const playlistId = queue?.playlist?.id;
  if (!queue || playlistId === undefined || snapshot.itemId === null) {
    return undefined;
  }
  if (snapshot.phase === 'ended') {
    if (lastSaved.index === -2 && !force) {
      return undefined;
    }
    lastSaved = { index: -2, at: Date.now() };
    return savePlayState(playlistId, null).catch(() => undefined);
  }
  const now = Date.now();
  const changed = snapshot.index !== lastSaved.index;
  if (!force && !changed && (!snapshot.playing || now - lastSaved.at < STATE_SAVE_MS)) {
    return undefined;
  }
  lastSaved = { index: snapshot.index, at: now };
  return savePlayState(playlistId, {
    seed: queue.seed,
    slotIndex: snapshot.index,
    itemId: snapshot.itemId,
    positionMs: Math.round(snapshot.positionMs),
  }).catch(() => undefined);
}

/** 再生を終える（プレイリストなら今の位置を保存しておく）。 */
export function closePlayer(): void {
  const saved = instance ? keepPlayState(instance.snapshot(), true) : undefined;
  instance?.close();
  usePlayerStore.getState().close();
  // 保存が済んでから、サイドバーの件数・プレイリストの「続きから」を読み直す
  void Promise.resolve(saved).then(() => {
    bumpRevision();
    void useSidebarStore.getState().load();
  });
}

/** 今のアイテムを外部プレイヤーで開く。 */
export async function openCurrentExternally(): Promise<void> {
  const { itemId } = usePlayerStore.getState().snapshot;
  const item = itemId !== null ? instance?.item(itemId) : undefined;
  if (!item) {
    return;
  }
  try {
    instance?.pause();
    const app = await openExternal(item.hash);
    notify(ja.player.openedIn(app === 'mpv' ? ja.player.mpv : ja.player.quickTime));
  } catch (error) {
    notify(error instanceof Error ? error.message : String(error));
  }
}

/** 保存してある再生の設定を読む。 */
export async function loadPlayerPrefs(): Promise<void> {
  try {
    const prefs = await getPlayerPrefs();
    usePlayerStore.getState().setPrefs(prefs);
    if (instance) {
      applyPrefs(instance, prefs);
    }
  } catch (error) {
    console.warn('再生の設定を読めませんでした', error);
  }
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;

/** 再生の設定を変える（すぐ反映し、少し待ってから保存する）。 */
export function updatePlayerPrefs(patch: Partial<PlayerPrefsDto>): void {
  const prefs = { ...usePlayerStore.getState().prefs, ...patch };
  usePlayerStore.getState().setPrefs(prefs);
  if (instance) {
    applyPrefs(instance, prefs);
  }
  if (saveTimer !== null) {
    clearTimeout(saveTimer);
  }
  saveTimer = setTimeout(() => {
    saveTimer = null;
    setPlayerPrefs(usePlayerStore.getState().prefs).catch((error: unknown) =>
      console.warn('再生の設定を保存できませんでした', error),
    );
  }, SAVE_DELAY_MS);
}
