/**
 * レシピの操作（M14 段 7。サイドバーの節・レシピの画面・項目のメニューから）：名前・複製・削除・並べ替え・`.kurarecipe` の
 * 書き出しと読み込み・見本で編集。失敗は知らせのカプセル。
 */

import { askConfirm, askPick, type PickerOption } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import { pickRecipeFile, pickRecipeSaveLocation } from '../../ipc/dialogs';
import { getItems, type ItemSummaryDto, queryIds } from '../../ipc/items';
import { kindWord } from '../../ipc/kinds';
import {
  deleteRecipe,
  duplicateRecipe,
  exportRecipe,
  getRecipe,
  importRecipe,
  renameRecipe,
  type RecipeSummaryDto,
} from '../../ipc/recipes';
import { notify } from '../../stores/noticeStore';
import { useViewStore } from '../../stores/viewStore';
import { requestCloseEditor } from '../editor/editorSave';
import { openEditor } from '../editor/editorKinds';
import { recipeMedia, type RecipeMedia } from './recipeModel';
import { useRecipeStore } from './recipeStore';

const t = ja.recipes;

/** 失敗を知らせる。 */
function fail(error: unknown): void {
  notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
}

/** 一覧を読み直す。 */
export function refreshRecipes(): Promise<void> {
  return useRecipeStore.getState().load();
}

/** 名前を変える（重なってよい）。 */
export async function renameRecipeTo(id: number, name: string): Promise<void> {
  try {
    await renameRecipe(id, name);
  } catch (error) {
    fail(error);
  }
  await refreshRecipes();
}

/** 説明を変える。 */
export { describeRecipe } from '../../ipc/recipes';

/** 複製する（元のすぐ後ろに「{名前} のコピー」）。 */
export async function duplicateRecipeAction(recipe: RecipeSummaryDto): Promise<void> {
  try {
    await duplicateRecipe(recipe.id);
    notify(t.duplicated(recipe.name));
  } catch (error) {
    fail(error);
  }
  await refreshRecipes();
}

/** 確かめてから消す（開いていれば、すべての一覧へ）。 */
export async function deleteRecipeWithConfirm(recipe: RecipeSummaryDto): Promise<void> {
  const ok = await askConfirm({
    title: ja.recipeMenu.deleteTitle,
    message: ja.recipeMenu.deleteMessage(recipe.name),
    confirm: ja.recipeMenu.deleteConfirm,
  });
  if (!ok) return;
  try {
    await deleteRecipe(recipe.id);
  } catch (error) {
    fail(error);
  }
  if (useViewStore.getState().recipe === recipe.id) {
    useViewStore.getState().setView({ kind: 'all' });
  }
  await refreshRecipes();
}

/** ファイル名に使えない文字を除いた名前（書き出しの既定の名前）。 */
export function fileStem(name: string): string {
  const stem = Array.from(name, (c) => (c.charCodeAt(0) < 0x20 || '/:\\'.includes(c) ? '-' : c))
    .join('')
    .trim();
  return stem === '' ? 'recipe' : stem;
}

/** `.kurarecipe` に書き出す（保存のダイアログで場所を選ぶ）。 */
export async function exportRecipeAction(recipe: RecipeSummaryDto): Promise<void> {
  const path = await pickRecipeSaveLocation(t.exportTitle, fileStem(recipe.name), t.fileFilter);
  if (!path) return;
  try {
    await exportRecipe(recipe.id, path);
    notify(t.exported(recipe.name));
  } catch (error) {
    fail(error);
  }
}

/** `.kurarecipe` を読み込む（開くダイアログ。`path` を渡せばそれを）。読み込んだらレシピの画面を開く。 */
export async function importRecipeAction(path?: string): Promise<void> {
  const file = path ?? (await pickRecipeFile(t.importTitle, t.fileFilter));
  if (!file) return;
  try {
    const imported = await importRecipe(file);
    await refreshRecipes();
    const name = useRecipeStore.getState().list.find((r) => r.id === imported.id)?.name ?? '';
    notify(imported.notes.length > 0 ? t.importedOld(name) : t.imported(name));
    useViewStore.getState().openRecipe(imported.id);
  } catch (error) {
    fail(error);
  }
}

/** 見本の候補の問い合わせ（種類で絞り、利用者の語を足す。種類の語は kinds.ts から。FilterBar と同じ組み方）。 */
export function sampleQuery(media: RecipeMedia, query: string): string {
  return [`種類:${kindWord(media)}`, query.trim()].filter(Boolean).join(' ');
}

/** 見本の候補（名前で探す。レシピと同じ種類でゴミ箱にないもの。すべての一覧の並びの先頭 40 件）。 */
export async function sampleOptions(
  media: RecipeMedia,
  query: string,
): Promise<PickerOption<ItemSummaryDto>[]> {
  const ids = await queryIds({ kind: 'all' }, { query: sampleQuery(media, query) });
  const head = Array.from(ids.ids.slice(0, 40));
  if (head.length === 0) return [];
  const items = await getItems(head);
  return items
    .filter((item) => item.kind === media)
    .map((item) => ({
      key: String(item.id),
      label: item.title,
      detail: item.ext.toUpperCase(),
      value: item,
    }));
}

/**
 * 見本で編集：見本のアイテムを選んでもらい、その元にレシピを当ててエディタで開く（M14 段 7。保存はレシピへ）。今のエディタは
 * 閉じる確かめを通してから閉じる。
 */
export async function editRecipeWithSample(recipeId: number): Promise<void> {
  let media: RecipeMedia;
  try {
    const recipe = await getRecipe(recipeId);
    if (recipe.problem) {
      notify(t.problem(recipe.problem));
      return;
    }
    media = recipeMedia(recipe.graph.nodes.map((n) => n.op));
  } catch (error) {
    fail(error);
    return;
  }
  const picked = await askPick<ItemSummaryDto>({
    title: t.pickSampleTitle,
    placeholder: t.pickSamplePlaceholder,
    empty: t.pickSampleEmpty,
    search: (query) => sampleOptions(media, query).catch(() => []),
  });
  if (!picked) return;
  const closed = await requestCloseEditor();
  if (!closed) return;
  await openEditor(picked.value, { recipe: recipeId });
}
