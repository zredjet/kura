import { useEffect, useId, useMemo, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { openMenuBelow } from '../../components/Menu/menuStore';
import { MoreIcon } from '../../components/icons/MoreIcon';
import { ja } from '../../i18n/ja';
import { getRecipe, type RecipeDto } from '../../ipc/recipes';
import { relativeDateTime } from '../../lib/dates';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { useViewStore } from '../../stores/viewStore';
import { recipeMenu } from '../sidebar/RecipeList';
import {
  describeRecipe,
  editRecipeWithSample,
  exportRecipeAction,
  refreshRecipes,
} from './recipeActions';
import { formatLabel, outputFormat, recipeMedia, recipeRows } from './recipeModel';
import { useRecipeStore } from './recipeStore';
import styles from './RecipeView.module.css';

const t = ja.recipes;

/** サイドバーの行で名前を変える（サイドバーを隠していれば出してから）。 */
function renameInSidebar(id: number): void {
  const view = useViewStore.getState();
  if (!view.sidebar) view.toggleSidebar();
  useRecipeStore.getState().setRenaming(id);
}

/**
 * レシピの画面（M14 段 7。サイドバーの行を押すとグリッドの代わりに出す）：名前・種類・操作の数・説明（その場で直す）・
 * 操作の並び（名前と引数の要約、アイテムごとに決まる値の印）・出力の形式・当て方の説明と［見本で編集…］［書き出す…］。
 * 中身を直すのは見本で編集（ADR-190 (5)）。
 */
export function RecipeView({ id }: { id: number }) {
  const summary = useRecipeStore((s) => s.list.find((r) => r.id === id) ?? null);
  const updatedAt = summary?.updatedAt ?? null;
  const key = `${id}:${updatedAt ?? ''}`;
  // 読んだレシピ（読んだときの鍵ごと。読み直す間は前のものを出したまま。読めなければ recipe が null）
  const [loaded, setLoaded] = useState<{ key: string; recipe: RecipeDto | null } | null>(null);
  const [description, setDescription] = useState('');
  const descId = useId();

  useEffect(() => {
    let alive = true;
    getRecipe(id)
      .then((r) => {
        if (!alive) return;
        setLoaded({ key, recipe: r });
        setDescription(r.description);
      })
      .catch(() => {
        if (alive) setLoaded({ key, recipe: null });
      });
    return () => {
      alive = false;
    };
  }, [id, key]);
  const recipe = loaded?.recipe ?? null;
  const failed = loaded?.key === key && loaded.recipe === null;

  const rows = useMemo(
    () => (recipe ? recipeRows(recipe.graph.nodes, recipe.bindings) : []),
    [recipe],
  );

  if (failed && !recipe) {
    return (
      <div className={styles.view}>
        <div className={styles.empty}>
          <p className={styles.emptyTitle}>{summary ? t.loadFailed : t.gone}</p>
        </div>
      </div>
    );
  }
  if (!recipe) {
    return <div className={styles.view} aria-busy="true" />;
  }
  const media = recipeMedia(recipe.graph.nodes.map((n) => n.op));
  const kindLabel =
    recipe.kind === 'merge' ? t.kindMerge : media === 'audio' ? t.kindAudio : t.kindImage;
  const format = outputFormat(recipe.graph.outputs);
  const saveDescription = () => {
    if (description === recipe.description) return;
    describeRecipe(recipe.id, description)
      .then(() => refreshRecipes())
      .catch((e: unknown) => {
        notify(ja.common.failed(e instanceof Error ? e.message : String(e)));
        setDescription(recipe.description);
      });
  };

  return (
    <div className={styles.view}>
      <header className={styles.head}>
        <div className={styles.heading}>
          <h1 className={styles.title}>{recipe.name}</h1>
          <span className={styles.summary}>
            {[
              kindLabel,
              rows.length > 0 ? t.ops(formatCount(rows.length)) : t.noOps,
              t.updated(relativeDateTime(recipe.updatedAt)),
            ].join(' · ')}
          </span>
        </div>
        <div className={styles.buttons}>
          <Button
            variant="primary"
            disabled={recipe.kind !== 'each' || recipe.problem !== null}
            onClick={() => void editRecipeWithSample(recipe.id)}
          >
            {t.editWithSample}
          </Button>
          <Button onClick={() => summary && void exportRecipeAction(summary)}>
            {t.exportFile}
          </Button>
          {summary && (
            <button
              type="button"
              className={styles.more}
              aria-label={ja.recipeMenu.label(recipe.name)}
              aria-haspopup="menu"
              onClick={(e) =>
                openMenuBelow(
                  e.currentTarget,
                  ja.recipeMenu.label(recipe.name),
                  recipeMenu(summary, () => renameInSidebar(recipe.id)),
                )
              }
            >
              <MoreIcon size={16} />
            </button>
          )}
        </div>
      </header>
      <div className={styles.scroll}>
        {recipe.problem && (
          <p className={styles.problem} role="alert">
            {t.problem(recipe.problem)}
          </p>
        )}
        {recipe.notes.length > 0 && <p className={styles.notice}>{t.notes}</p>}
        <div className={styles.field}>
          <label htmlFor={descId} className={styles.label}>
            {t.description}
          </label>
          <textarea
            id={descId}
            className={styles.description}
            value={description}
            placeholder={t.descriptionPlaceholder}
            onChange={(e) => setDescription(e.target.value)}
            onBlur={saveDescription}
            onKeyDown={(e) => e.stopPropagation()}
          />
        </div>
        <ol className={styles.ops}>
          {rows.map((row, i) => (
            <li key={row.id} className={styles.op} data-disabled={!row.enabled || undefined}>
              <span className={styles.index}>{i + 1}</span>
              <span className={styles.opBody}>
                <span className={styles.opLine}>
                  <span className={styles.opName}>{row.name}</span>
                  {row.summary && <span className={styles.opSummary}>{row.summary}</span>}
                  {!row.enabled && <span className={styles.badge}>{t.disabled}</span>}
                </span>
                {row.bound.map((b) => (
                  <span key={b} className={styles.bound}>
                    <span className={styles.badge}>{t.perItem}</span>
                    {b}
                  </span>
                ))}
              </span>
            </li>
          ))}
        </ol>
        <p className={styles.format}>{format ? t.format(formatLabel(format)) : t.formatKeep}</p>
        <p className={styles.hint}>{t.howToApply}</p>
        <p className={styles.dates}>{t.created(relativeDateTime(recipe.createdAt))}</p>
      </div>
    </div>
  );
}
