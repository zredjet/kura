/**
 * 項目のメニューの「レシピを当てる ▸」「まとめて 1 つにする ▸」と ⌥⌘E の選ぶパレット（M14 段 7）。
 */

import type { MenuAction, MenuSubmenu } from '../../components/Menu/menuStore';
import { askPick } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import { isUnitEntry } from '../../ipc/items';
import type { RecipeSummaryDto } from '../../ipc/recipes';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { getCached } from '../../stores/itemCache';
import { openApply } from './applyStore';
import { openMerge } from './mergeStore';
import { useRecipeStore } from './recipeStore';

/** メニューに並べるレシピの数（残りは「レシピを選ぶ…」）。 */
export const MENU_RECIPES = 8;

/** 1 件ずつ当てるレシピ（サイドバーの並び）。 */
export function eachRecipes(): RecipeSummaryDto[] {
  return useRecipeStore.getState().list.filter((r) => r.kind === 'each');
}

/** 選んだものに当てられるものがあるか（画像・音声のアイテムか、ユニット。読んでいない項目は当てられる側に数える）。 */
export function hasApplicable(entries: readonly number[]): boolean {
  return entries.some((id) => {
    if (isUnitEntry(id)) return true;
    const item = getCached(id);
    return !item || item.kind === 'image' || item.kind === 'audio';
  });
}

/** スプライトシートにできるか（画像 2 件以上か、ユニットを含む）。 */
export function canMerge(entries: readonly number[]): boolean {
  let images = 0;
  for (const id of entries) {
    if (isUnitEntry(id)) return true;
    const item = getCached(id);
    if (!item || item.kind === 'image') images++;
  }
  return images >= 2;
}

/** 当てるレシピを選ぶパレット（⌥⌘E・メニューの「レシピを選ぶ…」）。選んだら当てるシートを開く。 */
export async function pickRecipeAndApply(entries: readonly number[]): Promise<void> {
  if (entries.length === 0 || !hasApplicable(entries)) {
    notify(ja.batch.nothing);
    return;
  }
  await useRecipeStore.getState().load();
  const recipes = eachRecipes();
  const t = ja.batch;
  const picked = await askPick<RecipeSummaryDto>({
    title: t.pickTitle,
    placeholder: t.pickPlaceholder,
    empty: t.pickEmpty,
    search: (query) =>
      Promise.resolve(
        recipes
          .filter((r) => r.name.toLowerCase().includes(query.trim().toLowerCase()))
          .map((r) => ({
            key: String(r.id),
            label: r.name,
            detail: ja.recipes.ops(formatCount(r.nodes)),
            value: r,
          })),
      ),
  });
  if (picked) await openApply({ type: 'recipe', id: picked.value.id }, entries);
}

/** 項目のメニューの「レシピを当てる ▸」（当てられるものがなければ null）。 */
export function applyRecipeSubmenu(entries: readonly number[]): MenuSubmenu | null {
  if (!hasApplicable(entries)) return null;
  const recipes = eachRecipes().slice(0, MENU_RECIPES);
  const submenu: MenuAction[] = recipes.map((r) => ({
    label: r.name,
    onSelect: () => void openApply({ type: 'recipe', id: r.id }, entries),
  }));
  submenu.push({
    label: ja.batch.pick,
    shortcut: '⌥⌘E',
    onSelect: () => void pickRecipeAndApply(entries),
  });
  return { label: ja.itemMenu.applyRecipe, submenu };
}

/** 項目のメニューの「まとめて 1 つにする ▸」（画像 2 件以上のときだけ）。 */
export function mergeSubmenu(entries: readonly number[]): MenuSubmenu | null {
  if (!canMerge(entries)) return null;
  return {
    label: ja.merge.menu,
    submenu: [{ label: ja.merge.sprite, onSelect: () => void openMerge(entries) }],
  };
}
