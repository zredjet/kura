import { create } from 'zustand';

import {
  type ImportOptionsDto,
  type ImportPreviewDto,
  previewImport,
  startImport,
} from '../ipc/import';

type Phase = 'idle' | 'checking' | 'sheet';

interface ImportState {
  /** ファイルをウィンドウの上でドラッグ中か（重ね表示）。 */
  dragging: boolean;
  phase: Phase;
  paths: string[];
  preview: ImportPreviewDto | null;
  error: string | null;
  setDragging: (dragging: boolean) => void;
  /** ドロップ・選択されたものの確認を始め、取り込みシートを開く。 */
  begin: (paths: string[]) => Promise<void>;
  /** シートを閉じる。 */
  close: () => void;
  /** 取り込みを始めてシートを閉じる。 */
  confirm: (options: ImportOptionsDto) => Promise<void>;
}

function message(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

export const useImportStore = create<ImportState>((set, get) => ({
  dragging: false,
  phase: 'idle',
  paths: [],
  preview: null,
  error: null,
  setDragging: (dragging) => set({ dragging }),
  begin: async (paths) => {
    if (paths.length === 0 || get().phase !== 'idle') {
      return;
    }
    set({ phase: 'checking', paths, preview: null, error: null });
    try {
      const preview = await previewImport(paths);
      // 確認中に閉じられていたら何もしない
      if (get().phase === 'checking' && get().paths === paths) {
        set({ phase: 'sheet', preview });
      }
    } catch (error) {
      if (get().paths === paths) {
        set({ phase: 'sheet', error: message(error) });
      }
    }
  },
  close: () => set({ phase: 'idle', paths: [], preview: null, error: null }),
  confirm: async (options) => {
    const { paths } = get();
    try {
      await startImport(paths, options);
      get().close();
    } catch (error) {
      set({ error: message(error) });
    }
  },
}));
