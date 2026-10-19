import { create } from 'zustand';

import { type LibraryUpgradeEvent, onLibraryUpgrade } from '../ipc/library';

/** 「ライブラリを更新しています」を出すまでの時間（M11 の決定 1：始まってから 300ms を超えたら出す）。 */
export const UPGRADE_NOTICE_MS = 300;

/** 新しい版にしている途中のライブラリ。 */
export interface RunningUpgrade {
  path: string;
  fromVersion: number;
  toVersion: number;
}

interface UpgradeState {
  /** 新しい版にしている途中のライブラリ（`started` から `finished`・`failed` まで）。 */
  running: RunningUpgrade | null;
  /** 始まってから `UPGRADE_NOTICE_MS` を超えた（「ライブラリを更新しています」を出す）。 */
  visible: boolean;
}

/**
 * ライブラリの DB を新しい版にしている間の表示の状態（イベント `library://upgrade`。docs/architecture.md §3）。
 * 失敗の理由は知らせではなく、開く IPC の戻り値（`LibraryRestoreDto.error`・例外の AppError）で出す。
 */
export const useUpgradeStore = create<UpgradeState>(() => ({ running: null, visible: false }));

let timer: ReturnType<typeof setTimeout> | null = null;

function clearTimer(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
}

/** 知らせを 1 つ受ける：`started` から 300ms を超えたら出し、`finished`・`failed` で消す。 */
export function receiveUpgrade(event: LibraryUpgradeEvent): void {
  clearTimer();
  if (event.phase !== 'started') {
    useUpgradeStore.setState({ running: null, visible: false });
    return;
  }
  const running: RunningUpgrade = {
    path: event.path,
    fromVersion: event.fromVersion,
    toVersion: event.toVersion,
  };
  useUpgradeStore.setState({ running, visible: false });
  timer = setTimeout(() => {
    timer = null;
    if (useUpgradeStore.getState().running === running) {
      useUpgradeStore.setState({ visible: true });
    }
  }, UPGRADE_NOTICE_MS);
}

/** 開く IPC が返った：知らせが欠けても表示を残さない。 */
export function settleUpgrade(): void {
  clearTimer();
  useUpgradeStore.setState({ running: null, visible: false });
}

let watching: Promise<void> | null = null;

/**
 * 知らせの受け取りを始める（一度だけ）。開く IPC より先に待つ（知らせはその IPC の中で来る）。
 * 受け取れなくても（Tauri の外など）ライブラリを開くのは止めない。
 */
export function watchUpgrades(): Promise<void> {
  watching ??= (async () => {
    try {
      await onLibraryUpgrade(receiveUpgrade);
    } catch (error) {
      console.warn('ライブラリの更新の知らせを受け取れません', error);
    }
  })();
  return watching;
}
