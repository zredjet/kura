import { create } from 'zustand';

/** 知らせを出しておく時間（ms）。取り消しのボタンがあるときは長め。 */
const SHOW_MS = 4000;
const SHOW_WITH_ACTION_MS = 8000;

/** 知らせに付けるボタン（「元に戻す」など）。 */
export interface NoticeAction {
  label: string;
  run: () => void;
}

interface NoticeState {
  /** 画面下に出す一行（操作の失敗・完了の知らせ）。 */
  message: string | null;
  action: NoticeAction | null;
  /** 何回目の知らせか（同じ文言でも出し直すため）。 */
  serial: number;
  show: (message: string, action?: NoticeAction) => void;
  clear: () => void;
}

let timer: ReturnType<typeof setTimeout> | null = null;

export const useNoticeStore = create<NoticeState>((set, get) => ({
  message: null,
  action: null,
  serial: 0,
  show: (message, action) => {
    if (timer !== null) {
      clearTimeout(timer);
    }
    set({ message, action: action ?? null, serial: get().serial + 1 });
    timer = setTimeout(
      () => {
        timer = null;
        set({ message: null, action: null });
      },
      action ? SHOW_WITH_ACTION_MS : SHOW_MS,
    );
  },
  clear: () => set({ message: null, action: null }),
}));

/** 知らせを出す（`action` は「元に戻す」などのボタン）。 */
export function notify(message: string, action?: NoticeAction): void {
  useNoticeStore.getState().show(message, action);
}
