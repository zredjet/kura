import type { ItemSummaryDto } from '../../../ipc/items';
import type { QueueDto } from '../../../ipc/player';

/** スロットの種類。 */
export type SlotKind = 'image' | 'video' | 'audio';

/** 再生キュー（Rust の Slot 列を列ごとの配列で持つ。docs/playlists.md §5.1）。 */
export type Queue = QueueDto;

/** 再生に使う要素の共通部分（HTMLMediaElement をそのまま渡せる。テストでは偽物）。 */
export type MediaPort = Pick<
  HTMLMediaElement,
  | 'src'
  | 'currentTime'
  | 'duration'
  | 'paused'
  | 'ended'
  | 'volume'
  | 'preload'
  | 'play'
  | 'pause'
  | 'load'
  | 'removeAttribute'
  | 'addEventListener'
  | 'removeEventListener'
>;

/** 時計（テストでは偽のタイマーで動かす）。 */
export interface Clock {
  now(): number;
  setTimeout(fn: () => void, ms: number): number;
  clearTimeout(id: number): void;
}

/** 本物の時計。 */
export const realClock: Clock = {
  now: () => performance.now(),
  setTimeout: (fn, ms) => window.setTimeout(fn, ms),
  clearTimeout: (id) => window.clearTimeout(id),
};

/** 1 スロット分の再生の報告（docs/playlists.md §6.4。判定は Rust がする）。 */
export interface SlotReport {
  itemId: number;
  unitId: number | null;
  unitStart: boolean;
  playedMs: number;
  completed: boolean;
  durationMs: number | null;
}

/** 今のスロットの状態。 */
export type SlotPhase =
  /** 読み込み中。 */
  | 'loading'
  | 'playing'
  /** プロキシの作成を待っている。 */
  | 'waiting'
  /** 再生できない（外部プレイヤーで開く）。 */
  | 'failed'
  /** キューの終わりまで再生した。 */
  | 'ended';

/** 画像の間に流している BGM（直前の音楽スロットの曲）。 */
export interface BgmInfo {
  itemId: number;
  positionMs: number;
  durationMs: number | null;
}

/** 画面に出す再生の状態。 */
export interface PlayerSnapshot {
  index: number;
  length: number;
  /** 再生中か（一時停止なら false）。 */
  playing: boolean;
  phase: SlotPhase;
  kind: SlotKind | null;
  itemId: number | null;
  positionMs: number;
  durationMs: number | null;
  /** 画像のスロットの表示時間（このセッションでの変更を反映したもの）。 */
  imageMs: number;
  /** 画像の表示時間をこのセッションで変えたか。 */
  imageOverridden: boolean;
  /** 背景で流している音楽（今のスロットが画像のときだけ）。 */
  bgm: BgmInfo | null;
  repeat: boolean;
}

/** コントローラが外とやり取りするもの（テストでは偽物）。 */
export interface ControllerDeps {
  clock: Clock;
  /** BGM 用・先読み用の要素を作る。 */
  createMedia: (kind: 'audio' | 'video') => MediaPort;
  /** アイテムの概要を取る。 */
  fetchItems: (ids: number[]) => Promise<ItemSummaryDto[]>;
  /** 再生の報告。再生済みとして記録したら true。 */
  report: (report: SlotReport) => Promise<boolean>;
  /** プロキシを先に作ってもらう。 */
  requestProxies: (ids: number[], force: boolean) => Promise<unknown>;
  /** 再生用の URL。`attempt` は作り直したプロキシを読み直すための印。 */
  playUrl: (hash: string, attempt: number) => string;
  /** 画像を先に decode する。 */
  preloadImage: (url: string) => void;
}
