import type { KeyboardEvent, MouseEvent } from 'react';

import { InlineName } from '../../components/InlineName/InlineName';
import { type MenuEntry, openMenuAt, openMenuBelow } from '../../components/Menu/menuStore';
import { ImportIcon } from '../../components/icons/ImportIcon';
import { RecipeIcon } from '../../components/icons/RecipeIcon';
import { ja } from '../../i18n/ja';
import type { RecipeSummaryDto } from '../../ipc/recipes';
import { useViewStore } from '../../stores/viewStore';
import { useDragStore, watchDrag } from '../library/dragState';
import {
  deleteRecipeWithConfirm,
  duplicateRecipeAction,
  editRecipeWithSample,
  exportRecipeAction,
  importRecipeAction,
  renameRecipeTo,
} from '../recipes/recipeActions';
import { moveRecipe, useRecipeStore } from '../recipes/recipeStore';
import { ROW_ATTR } from './Sidebar';
import styles from './Sidebar.module.css';

const t = ja.recipeMenu;

/** レシピの行の右クリック（⌃↩）の項目。 */
export function recipeMenu(recipe: RecipeSummaryDto, rename: () => void): MenuEntry[] {
  return [
    {
      label: t.editWithSample,
      disabled: recipe.kind !== 'each',
      onSelect: () => void editRecipeWithSample(recipe.id),
    },
    'separator',
    { label: t.rename, shortcut: '↩', onSelect: rename },
    { label: t.duplicate, onSelect: () => void duplicateRecipeAction(recipe) },
    { label: t.export, onSelect: () => void exportRecipeAction(recipe) },
    'separator',
    {
      label: t.delete,
      shortcut: '⌘⌫',
      danger: true,
      onSelect: () => void deleteRecipeWithConfirm(recipe),
    },
  ];
}

/**
 * サイドバーのレシピ（M14 段 7。ADR-190 (5)）。行を押すとレシピの画面、右クリックで見本で編集・名前・複製・書き出す・削除。
 * 見出しのボタンで `.kurarecipe` を読み込む（作るのはエディタの［保存 ▾］→［レシピとして保存…］）。グリッドのタイルを行に
 * 落とすと、そのレシピを当てるシートを開く。行をドラッグするか ⌥⌘↑↓ で並べ替える。
 */
export function RecipeList() {
  const recipes = useRecipeStore((s) => s.list);
  const renaming = useRecipeStore((s) => s.renaming);
  const setRenaming = useRecipeStore((s) => s.setRenaming);
  const current = useViewStore((s) => s.recipe);
  const openRecipe = useViewStore((s) => s.openRecipe);
  const dropTarget = useDragStore((s) => s.target);

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, recipe: RecipeSummaryDto) => {
    const plain = !e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey;
    let handled = true;
    if (e.key === 'Enter' && plain) {
      setRenaming(recipe.id);
    } else if (e.key === 'Enter' && e.ctrlKey) {
      openMenuBelow(
        e.currentTarget,
        t.label(recipe.name),
        recipeMenu(recipe, () => setRenaming(recipe.id)),
      );
    } else if (e.key === 'Backspace' && e.metaKey) {
      void deleteRecipeWithConfirm(recipe);
    } else if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && e.metaKey && e.altKey) {
      const index = recipes.findIndex((r) => r.id === recipe.id);
      const up = e.key === 'ArrowUp';
      const neighbor = recipes[index + (up ? -1 : 1)];
      if (neighbor) {
        const row = e.currentTarget;
        void moveRecipe(recipe.id, neighbor.id, up ? 'before' : 'after').then(() => row.focus());
      }
    } else {
      handled = false;
    }
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const onContextMenu = (e: MouseEvent, recipe: RecipeSummaryDto) => {
    e.preventDefault();
    openMenuAt(
      e.clientX,
      e.clientY,
      t.label(recipe.name),
      recipeMenu(recipe, () => setRenaming(recipe.id)),
    );
  };

  return (
    <section className={styles.section} aria-label={ja.sidebar.recipes}>
      <div className={styles.sectionHeader}>
        <h2 className={styles.sectionTitle}>{ja.sidebar.recipes}</h2>
        <button
          type="button"
          className={styles.addButton}
          aria-label={ja.sidebar.importRecipe}
          title={ja.sidebar.importRecipe}
          onClick={() => void importRecipeAction()}
        >
          <ImportIcon size={14} strokeWidth={2} />
        </button>
      </div>
      {recipes.map((recipe) => {
        const selected = current === recipe.id;
        if (renaming === recipe.id) {
          return (
            <div key={recipe.id} className={styles.row}>
              <span className={styles.rowIcon}>
                <RecipeIcon size={16} strokeWidth={1.8} />
              </span>
              <InlineName
                initial={recipe.name}
                label={ja.sidebar.recipeName}
                onCommit={(name) => {
                  setRenaming(null);
                  void renameRecipeTo(recipe.id, name);
                }}
                onCancel={() => setRenaming(null)}
              />
            </div>
          );
        }
        return (
          <button
            key={recipe.id}
            type="button"
            className={styles.row}
            aria-current={selected ? 'page' : undefined}
            {...{ [ROW_ATTR]: '' }}
            data-drop-recipe={recipe.kind === 'each' ? recipe.id : undefined}
            data-recipe-row={recipe.id}
            data-drop={
              dropTarget?.kind === 'recipe' && dropTarget.id === recipe.id
                ? 'inside'
                : dropTarget?.kind === 'recipeOrder' && dropTarget.id === recipe.id
                  ? dropTarget.side
                  : undefined
            }
            onPointerDown={(e) =>
              watchDrag(e, () => ({
                payload: { kind: 'recipe', id: recipe.id },
                label: recipe.name,
              }))
            }
            onClick={(e) => {
              e.currentTarget.focus();
              if (!selected) openRecipe(recipe.id);
            }}
            onKeyDown={(e) => onKeyDown(e, recipe)}
            onContextMenu={(e) => onContextMenu(e, recipe)}
          >
            <span className={styles.rowIcon}>
              <RecipeIcon size={16} strokeWidth={1.8} />
            </span>
            <span className={styles.rowLabel}>{recipe.name}</span>
          </button>
        );
      })}
    </section>
  );
}
