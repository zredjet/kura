import { create } from 'zustand';

import type { PlayerPrefsDto, PlaySourceDto } from '../../ipc/player';
import type { PlayerSnapshot, Queue } from './engine/types';
import { onFullscreenChange } from './theaterChrome';

/** シアター（全画面）か、ライブラリの上のミニプレーヤーか。 */
export type PlayerMode = 'theater' | 'mini';
/** シアターの右パネル（キュー・情報、閉じていれば null）。 */
export type PlayerPanel = 'queue' | 'info' | null;

/** 履歴に残す数。 */
const HISTORY_LIMIT = 200;

export const DEFAULT_PREFS: PlayerPrefsDto = {
  volume: 80,
  bgmThroughImages: true,
  bgmLoop: false,
  imageDurationMs: 8000,
  sfxAudition: true,
};

const IDLE: PlayerSnapshot = {
  index: 0,
  length: 0,
  playing: false,
  phase: 'loading',
  kind: null,
  itemId: null,
  positionMs: 0,
  durationMs: null,
  imageMs: DEFAULT_PREFS.imageDurationMs,
  imageOverridden: false,
  bgm: null,
  repeat: 'off',
  groupIndex: null,
  groupCount: 0,
  indexInUnit: 0,
  unitLen: 1,
};

interface PlayerState {
  /** 再生キューがあるか（ミニプレーヤー・シアターを出すか）。 */
  active: boolean;
  mode: PlayerMode;
  /** どこから再生したか（「すべて」・フォルダ名・プレイリスト名など）。 */
  title: string;
  /** キューの元（シャッフルの切り替え・並べ直す・リピートで作り直すときに使う）。 */
  source: PlaySourceDto | null;
  /** その場のキュー（選択・一覧）でシャッフルしているか（プレイリストはプレイリストの設定）。 */
  shuffle: boolean;
  queue: Queue | null;
  snapshot: PlayerSnapshot;
  panel: PlayerPanel;
  tab: 'queue' | 'history';
  /** このセッションで再生済みになったアイテム（新しい順）。 */
  history: number[];
  prefs: PlayerPrefsDto;
  /** タグの入力に移るよう頼まれたか（シアターの T・「タグを編集」）。 */
  focusTags: number;
  /** ウィンドウがフルスクリーンか（シアターの操作とパネルの出し方に使う。ADR-128）。 */
  fullscreen: boolean;
  /** フルスクリーンに入る前のパネル（出たら戻す）。 */
  panelBeforeFullscreen: PlayerPanel | undefined;
  open: (queue: Queue, title: string, source: PlaySourceDto, shuffle: boolean) => void;
  /** キューを作り直した（同じ元から）。 */
  replaceQueue: (queue: Queue, shuffle: boolean) => void;
  close: () => void;
  setMode: (mode: PlayerMode) => void;
  setPanel: (panel: PlayerPanel) => void;
  togglePanel: (panel: Exclude<PlayerPanel, null>) => void;
  setTab: (tab: 'queue' | 'history') => void;
  addHistory: (itemId: number) => void;
  setPrefs: (prefs: PlayerPrefsDto) => void;
  requestTagFocus: () => void;
  /** ウィンドウのフルスクリーンが変わった（入ったらパネルを閉じ、出たら戻す）。 */
  setFullscreen: (on: boolean) => void;
}

export const usePlayerStore = create<PlayerState>((set) => ({
  active: false,
  mode: 'theater',
  title: '',
  source: null,
  shuffle: false,
  queue: null,
  snapshot: IDLE,
  panel: 'queue',
  tab: 'queue',
  history: [],
  prefs: DEFAULT_PREFS,
  focusTags: 0,
  fullscreen: false,
  panelBeforeFullscreen: undefined,
  open: (queue, title, source, shuffle) =>
    set({ active: true, mode: 'theater', title, source, shuffle, queue, history: [] }),
  replaceQueue: (queue, shuffle) => set({ queue, shuffle }),
  close: () => set({ active: false, queue: null, source: null, snapshot: IDLE, history: [] }),
  setMode: (mode) => set({ mode }),
  setPanel: (panel) => set({ panel }),
  togglePanel: (panel) => set((s) => ({ panel: s.panel === panel ? null : panel })),
  setTab: (tab) => set({ tab }),
  addHistory: (itemId) =>
    set((s) => ({
      history: [itemId, ...s.history.filter((id) => id !== itemId)].slice(0, HISTORY_LIMIT),
    })),
  setPrefs: (prefs) => set({ prefs }),
  requestTagFocus: () => set((s) => ({ panel: 'info', focusTags: s.focusTags + 1 })),
  setFullscreen: (on) =>
    set((s) => {
      const next = onFullscreenChange(s, on);
      return next === s
        ? {}
        : {
            fullscreen: next.fullscreen,
            panel: next.panel,
            panelBeforeFullscreen: next.panelBeforeFullscreen,
          };
    }),
}));
