import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { useEffect } from 'react';

import { ja } from '../../i18n/ja';
import { pickImportFiles, RECIPE_EXTENSION } from '../../ipc/dialogs';
import { useImportStore } from '../../stores/importStore';
import { isOwnDrop } from '../library/dragOut';
import { importRecipeAction } from '../recipes/recipeActions';

/** レシピのファイル（`.kurarecipe`）か。 */
export function isRecipeFile(path: string): boolean {
  return path.toLowerCase().endsWith(`.${RECIPE_EXTENSION}`);
}

/** 落としたレシピのファイルを 1 つずつ読み込む（M14 段 7）。 */
async function importRecipes(paths: readonly string[]): Promise<void> {
  for (const path of paths) await importRecipeAction(path);
}

/** ドラッグ＆ドロップのイベント（Tauri の DragDropEvent の必要な部分）。 */
export type DropPayload =
  | { type: 'enter'; paths: string[] }
  | { type: 'over' }
  | { type: 'drop'; paths: string[] }
  | { type: 'leave' };

/** ドラッグ＆ドロップのイベントを取り込みの状態に反映する。シートを開いている間と、Kura から出したドラッグは受け付けない。 */
export function handleDragDrop(payload: DropPayload): void {
  const store = useImportStore.getState();
  // Kura から出したドラッグ（一時置き場のファイル）が戻ってきたら取り込まない（M15 段 5）
  if ((payload.type === 'enter' || payload.type === 'drop') && isOwnDrop(payload.paths)) {
    if (store.dragging) {
      store.setDragging(false);
    }
    return;
  }
  if (store.phase !== 'idle') {
    if (store.dragging) {
      store.setDragging(false);
    }
    return;
  }
  switch (payload.type) {
    case 'enter':
      store.setDragging(payload.paths.length > 0);
      break;
    case 'leave':
      store.setDragging(false);
      break;
    case 'drop': {
      store.setDragging(false);
      // レシピのファイルは読み込み、ほかは取り込みのシートへ
      const recipes = payload.paths.filter(isRecipeFile);
      const media = payload.paths.filter((p) => !isRecipeFile(p));
      if (recipes.length > 0) void importRecipes(recipes);
      if (media.length > 0 || recipes.length === 0) void store.begin(media);
      break;
    }
    case 'over':
      break;
  }
}

/** ウィンドウへのドラッグ＆ドロップを受け付ける（docs/architecture.md §11）。 */
export function useFileDrop(): void {
  useEffect(() => {
    if (!isTauri()) {
      return;
    }
    let unlisten: (() => void) | null = null;
    let disposed = false;
    getCurrentWebview()
      .onDragDropEvent((event) => handleDragDrop(event.payload))
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((error: unknown) => console.warn('ドラッグ＆ドロップを受け付けられません', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}

/** ファイルを選んで取り込みシートを開く（⌘O・取り込みボタン）。 */
export async function pickAndImport(): Promise<void> {
  if (useImportStore.getState().phase !== 'idle') {
    return;
  }
  const paths = await pickImportFiles(ja.import.pickTitle, ja.import.pickFilter);
  await useImportStore.getState().begin(paths);
}

/** ⌘O で取り込む。 */
export function useImportShortcut(): void {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey && !e.shiftKey && !e.altKey && !e.ctrlKey && e.key.toLowerCase() === 'o') {
        e.preventDefault();
        pickAndImport().catch((error: unknown) =>
          console.warn('ファイルを選べませんでした', error),
        );
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);
}
