import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { LIST_FLAG } from '../../ipc/items';
import { getCached } from '../../stores/itemCache';
import type { MenuAction } from '../../components/Menu/menuStore';
import { openApply } from './applyStore';
import { openMerge } from './mergeStore';
import {
  applyRecipeSubmenu,
  canMerge,
  hasApplicable,
  MENU_RECIPES,
  mergeSubmenu,
} from './recipeMenus';
import { useRecipeStore } from './recipeStore';

vi.mock('../../stores/itemCache', () => ({ getCached: vi.fn() }));
vi.mock('./applyStore', () => ({ openApply: vi.fn(async () => undefined) }));
vi.mock('./mergeStore', () => ({ openMerge: vi.fn(async () => undefined) }));
vi.mock('../../ipc/recipes', () => ({ listRecipes: vi.fn(async () => []) }));

const kinds: Record<number, string> = { 1: 'image', 2: 'image', 3: 'video', 4: 'audio' };

function recipe(id: number, kind: 'each' | 'merge' = 'each') {
  return {
    id,
    name: `レシピ ${id}`,
    description: '',
    kind,
    nodes: 1,
    bindings: 0,
    createdAt: 0,
    updatedAt: 0,
  };
}

describe('recipeMenus', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(getCached).mockImplementation(
      (id) => (kinds[id] ? { id, kind: kinds[id] } : undefined) as ReturnType<typeof getCached>,
    );
    useRecipeStore.setState({ list: [] });
  });

  it('当てられるもの：画像・音声・ユニット（読んでいない項目も当てる側）', () => {
    expect(hasApplicable([3])).toBe(false);
    expect(hasApplicable([3, 4])).toBe(true);
    expect(hasApplicable([(LIST_FLAG | 2) >>> 0])).toBe(true);
    expect(hasApplicable([99])).toBe(true);
  });

  it('レシピを当てる ▸：1 件ずつのレシピを 8 件までと「レシピを選ぶ…」', () => {
    useRecipeStore.setState({
      list: [
        recipe(1),
        recipe(2, 'merge'),
        ...Array.from({ length: 10 }, (_, i) => recipe(10 + i)),
      ],
    });
    const menu = applyRecipeSubmenu([1]);
    expect(menu?.label).toBe(ja.itemMenu.applyRecipe);
    const labels = menu?.submenu.map((m) => m.label) ?? [];
    expect(labels).toHaveLength(MENU_RECIPES + 1);
    expect(labels).not.toContain('レシピ 2');
    expect(labels[labels.length - 1]).toBe(ja.batch.pick);
    (menu?.submenu[0] as MenuAction).onSelect();
    expect(openApply).toHaveBeenCalledWith({ type: 'recipe', id: 1 }, [1]);
    expect(applyRecipeSubmenu([3])).toBeNull();
  });

  it('まとめて 1 つにする ▸ は画像 2 件以上かユニット', () => {
    expect(canMerge([1])).toBe(false);
    expect(canMerge([1, 3])).toBe(false);
    expect(canMerge([1, 2])).toBe(true);
    expect(canMerge([(LIST_FLAG | 2) >>> 0])).toBe(true);
    expect(mergeSubmenu([1])).toBeNull();
    const menu = mergeSubmenu([1, 2]);
    expect(menu?.submenu.map((m) => m.label)).toEqual([ja.merge.sprite]);
    (menu?.submenu[0] as MenuAction).onSelect();
    expect(openMerge).toHaveBeenCalledWith([1, 2]);
  });
});
