import type { ReactNode } from 'react';
import { create } from 'zustand';

/** 選ぶダイアログの候補。 */
export interface PickerOption<T = unknown> {
  key: string;
  label: string;
  /** 2 行目の補足（フォルダのパスなど）。 */
  detail?: string;
  icon?: ReactNode;
  value: T;
}

interface PromptOverlay {
  kind: 'prompt';
  title: string;
  label: string;
  initial: string;
  confirm: string;
  resolve: (value: string | null) => void;
}

interface ConfirmOverlay {
  kind: 'confirm';
  title: string;
  message: string;
  confirm: string;
  resolve: (ok: boolean) => void;
}

interface PickerOverlay {
  kind: 'picker';
  title: string;
  placeholder: string;
  empty: string;
  search: (query: string) => Promise<PickerOption[]>;
  resolve: (option: PickerOption | null) => void;
}

export type Overlay = PromptOverlay | ConfirmOverlay | PickerOverlay;

interface OverlayState {
  overlay: Overlay | null;
}

export const useOverlayStore = create<OverlayState>(() => ({ overlay: null }));

/** 開いているものを取り消して閉じ、新しいものを出す。 */
function show(overlay: Overlay): void {
  cancelOverlay();
  useOverlayStore.setState({ overlay });
}

/** 開いているダイアログを「キャンセル」で閉じる。 */
export function cancelOverlay(): void {
  const current = useOverlayStore.getState().overlay;
  if (!current) {
    return;
  }
  useOverlayStore.setState({ overlay: null });
  if (current.kind === 'prompt') {
    current.resolve(null);
  } else if (current.kind === 'confirm') {
    current.resolve(false);
  } else {
    current.resolve(null);
  }
}

/** 名前を入力してもらう（キャンセルなら null）。 */
export function askText(options: Omit<PromptOverlay, 'kind' | 'resolve'>): Promise<string | null> {
  return new Promise((resolve) =>
    show({
      kind: 'prompt',
      ...options,
      resolve: (value) => {
        useOverlayStore.setState({ overlay: null });
        resolve(value);
      },
    }),
  );
}

/** 確認してもらう（取り消せない操作の前）。 */
export function askConfirm(options: Omit<ConfirmOverlay, 'kind' | 'resolve'>): Promise<boolean> {
  return new Promise((resolve) =>
    show({
      kind: 'confirm',
      ...options,
      resolve: (ok) => {
        useOverlayStore.setState({ overlay: null });
        resolve(ok);
      },
    }),
  );
}

/** 候補から選んでもらう（フォルダ・ユニットなど。キャンセルなら null）。 */
export function askPick<T>(
  options: Omit<PickerOverlay, 'kind' | 'resolve' | 'search'> & {
    search: (query: string) => Promise<PickerOption<T>[]>;
  },
): Promise<PickerOption<T> | null> {
  return new Promise((resolve) =>
    show({
      kind: 'picker',
      ...options,
      search: options.search as (query: string) => Promise<PickerOption[]>,
      resolve: (option) => {
        useOverlayStore.setState({ overlay: null });
        resolve(option as PickerOption<T> | null);
      },
    }),
  );
}
