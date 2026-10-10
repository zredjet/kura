import { create } from 'zustand';

import type { PlayerPrefsDto } from '../../ipc/player';
import type { PlayerSnapshot, Queue } from './engine/types';

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
  repeat: false,
};

interface PlayerState {
  /** 再生キューがあるか（ミニプレーヤー・シアターを出すか）。 */
  active: boolean;
  mode: PlayerMode;
  /** どこから再生したか（「すべて」・フォルダ名など）。 */
  title: string;
  queue: Queue | null;
  snapshot: PlayerSnapshot;
  panel: PlayerPanel;
  tab: 'queue' | 'history';
  /** このセッションで再生済みになったアイテム（新しい順）。 */
  history: number[];
  prefs: PlayerPrefsDto;
  /** タグの入力に移るよう頼まれたか（シアターの T・「タグを編集」）。 */
  focusTags: number;
  open: (queue: Queue, title: string) => void;
  close: () => void;
  setMode: (mode: PlayerMode) => void;
  setPanel: (panel: PlayerPanel) => void;
  togglePanel: (panel: Exclude<PlayerPanel, null>) => void;
  setTab: (tab: 'queue' | 'history') => void;
  addHistory: (itemId: number) => void;
  setPrefs: (prefs: PlayerPrefsDto) => void;
  requestTagFocus: () => void;
}

export const usePlayerStore = create<PlayerState>((set) => ({
  active: false,
  mode: 'theater',
  title: '',
  queue: null,
  snapshot: IDLE,
  panel: 'queue',
  tab: 'queue',
  history: [],
  prefs: DEFAULT_PREFS,
  focusTags: 0,
  open: (queue, title) => set({ active: true, mode: 'theater', title, queue, history: [] }),
  close: () => set({ active: false, queue: null, snapshot: IDLE, history: [] }),
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
}));
