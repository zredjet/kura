import { create } from 'zustand';

/** 知らせを出しておく時間（ms）。 */
const SHOW_MS = 4000;

interface NoticeState {
  /** 画面下に出す一行（操作の失敗・完了の知らせ）。 */
  message: string | null;
  /** 何回目の知らせか（同じ文言でも出し直すため）。 */
  serial: number;
  show: (message: string) => void;
  clear: () => void;
}

let timer: ReturnType<typeof setTimeout> | null = null;

export const useNoticeStore = create<NoticeState>((set, get) => ({
  message: null,
  serial: 0,
  show: (message) => {
    if (timer !== null) {
      clearTimeout(timer);
    }
    set({ message, serial: get().serial + 1 });
    timer = setTimeout(() => {
      timer = null;
      set({ message: null });
    }, SHOW_MS);
  },
  clear: () => set({ message: null }),
}));

/** 知らせを出す。 */
export function notify(message: string): void {
  useNoticeStore.getState().show(message);
}
