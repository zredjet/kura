import { ja } from '../../i18n/ja';
import { getItems } from '../../ipc/items';
import { playUrl } from '../../ipc/media';
import {
  getPlayerPrefs,
  openExternal,
  playerQueue,
  type PlayerPrefsDto,
  reportPlayback,
  requestProxies,
  setPlayerPrefs,
} from '../../ipc/player';
import { notify } from '../../stores/noticeStore';
import { PlaybackController } from './engine/PlaybackController';
import { realClock } from './engine/types';
import { usePlayerStore } from './playerStore';

/** 設定（音量など）を保存するまで待つ時間。 */
const SAVE_DELAY_MS = 500;
/** 先に decode した画像を持っておく数。 */
const KEEP_IMAGES = 4;

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
  controller.subscribe((snapshot) => usePlayerStore.setState({ snapshot }));
  controller.onRecorded((itemId) => usePlayerStore.getState().addHistory(itemId));
  applyPrefs(controller, usePlayerStore.getState().prefs);
  instance = controller;
  return controller;
}

function applyPrefs(controller: PlaybackController, prefs: PlayerPrefsDto): void {
  controller.setVolume(prefs.volume / 100);
  controller.setPrefs({
    throughImages: prefs.bgmThroughImages,
    loop: prefs.bgmLoop,
    imageMs: prefs.imageDurationMs,
  });
}

/** 選択（並び順のまま）を再生する。`title` はシアターの見出し。 */
export async function playEntries(entries: number[], title: string): Promise<void> {
  if (entries.length === 0) {
    return;
  }
  try {
    const queue = await playerQueue(entries);
    if (queue.itemIds.length === 0) {
      notify(ja.player.nothingToPlay);
      return;
    }
    const controller = playerController();
    usePlayerStore.getState().open(queue, title);
    controller.start(queue, 0);
  } catch (error) {
    notify(error instanceof Error ? error.message : String(error));
  }
}

/** 再生を終える。 */
export function closePlayer(): void {
  instance?.close();
  usePlayerStore.getState().close();
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
