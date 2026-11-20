import { useEffect, useId, useMemo, useRef, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { Select, type SelectOption } from '../../components/Select/Select';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import type { EditNodeDto } from '../../ipc/edit';
import { createRecipe, replaceRecipe } from '../../ipc/recipes';
import { isImeKey } from '../../lib/ime';
import { notify } from '../../stores/noticeStore';
import { dismissSheet, recipeSaved } from '../editor/editorSave';
import { type AfterClose, useEditorStore } from '../editor/editorStore';
import { opName, summarizeNode } from '../editor/opsCatalog';
import {
  type BindChoice,
  type BindSlot,
  bindingsFrom,
  bindSlots,
  choicesOf,
  defaultKeep,
  defaultRecipeName,
  defaultSeconds,
  formatLabel,
  outputFormat,
  slotActive,
} from './recipeModel';
import { useRecipeStore } from './recipeStore';
import styles from './Recipes.module.css';

const t = ja.saveRecipe;

/** 欄の選び方を選択の値にする。 */
function choiceValue(c: BindChoice): string {
  if (c.type === 'setting') return `setting:${c.key}`;
  if (c.type === 'time') return `time:${c.anchor}`;
  return 'keep';
}

/** 欄の選択肢（見本の値のまま・素材の設定・頭から／終わりからの秒）。 */
function slotOptions(slot: BindSlot): SelectOption[] {
  const out: SelectOption[] = [{ value: 'keep', label: t.keep }];
  for (const key of slot.settings) {
    out.push({ value: `setting:${key}`, label: t.fromSetting(ja.recipes.from[key]) });
  }
  if (slot.time) {
    out.push({ value: 'time:start', label: t.fromTimeStart });
    out.push({ value: 'time:end', label: t.fromTimeEnd });
  }
  return out;
}

/**
 * レシピとして保存…（エディタの ▾）と、見本で編集の［レシピを保存］（M14 段 7。docs/editing.md §4.3）：含める操作（無効・描画・
 * パレットの置き換えは初めから外す）・出力の形式を含めるか・アイテムごとに決める値（結び付け）。作るときは名前と説明も。
 */
export function SaveRecipeSheet({ closeAfter }: { closeAfter: AfterClose | null }) {
  const doc = useEditorStore((s) => s.doc);
  const recipe = useEditorStore((s) => s.recipe);
  const session = useEditorStore((s) => s.session);
  const rate = useEditorStore((s) => s.source?.audio?.rate ?? null);
  const pending = useEditorStore((s) => s.pendingSteps);
  const titleId = useId();
  const nameId = useId();
  const descId = useId();
  const nodes = useMemo(
    () => doc.order.map((id) => doc.nodes[id]).filter((n): n is EditNodeDto => n !== undefined),
    [doc],
  );
  const initialKeep = useMemo(() => defaultKeep(nodes), [nodes]);
  const [keep, setKeep] = useState<number[]>(initialKeep);
  const [name, setName] = useState(() =>
    defaultRecipeName(nodes.filter((n) => initialKeep.includes(n.id)).map((n) => n.op)),
  );
  const [description, setDescription] = useState('');
  const format = outputFormat(doc.outputs);
  const [withFormat, setWithFormat] = useState(() =>
    recipe ? recipe.withFormat && format !== null : format !== null,
  );
  const slots = useMemo(() => bindSlots(nodes, keep), [nodes, keep]);
  const [choices, setChoices] = useState<Record<string, BindChoice>>(() =>
    choicesOf(bindSlots(nodes, initialKeep), recipe?.bindings ?? []),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    nameRef.current?.focus();
    nameRef.current?.select();
  }, []);

  const excluded = nodes.some((n) => n.op === 'paint.actions' || n.op === 'palette.replace');
  const disabled = nodes.some((n) => !n.enabled);
  const toggle = (id: number, on: boolean) => {
    setKeep((k) =>
      on ? doc.order.filter((x) => x === id || k.includes(x)) : k.filter((x) => x !== id),
    );
  };
  const choose = (slot: BindSlot, value: string) => {
    let next: BindChoice = { type: 'keep' };
    if (value.startsWith('setting:')) {
      next = { type: 'setting', key: value.slice(8) as BindSlot['settings'][number] };
    } else if (value.startsWith('time:')) {
      const prev = choices[slot.key];
      next = {
        type: 'time',
        anchor: value === 'time:end' ? 'end' : 'start',
        seconds:
          prev?.type === 'time'
            ? prev.seconds
            : defaultSeconds(
                nodes.find((n) => n.id === slot.node),
                slot,
                rate,
              ),
      };
    }
    setChoices((c) => ({ ...c, [slot.key]: next }));
  };

  const submit = async () => {
    if (busy || !session) return;
    if (keep.length === 0) {
      setError(t.noOps);
      return;
    }
    const trimmed = name.trim();
    if (!recipe && trimmed === '') {
      setError(t.nameRequired);
      return;
    }
    const { bindings, invalid } = bindingsFrom(slots, choices);
    if (invalid !== null) {
      setError(t.secondsInvalid(invalid));
      return;
    }
    const pick = {
      session,
      keep: doc.order.filter((id) => keep.includes(id)),
      withFormat: withFormat && format !== null,
      bindings,
    };
    setBusy(true);
    setError(null);
    try {
      if (recipe) {
        await replaceRecipe(recipe.id, pick);
        notify(ja.recipes.replaced(recipe.name));
      } else {
        await createRecipe(trimmed, description, pick);
        notify(ja.recipes.saved(trimmed));
      }
      void useRecipeStore.getState().load();
      recipeSaved(closeAfter);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="md">
      <div className={styles.sheet}>
        <h2 id={titleId} className={styles.title}>
          {recipe ? t.replaceTitle(recipe.name) : t.title}
        </h2>
        <div className={styles.body}>
          {!recipe && (
            <>
              <div className={styles.field}>
                <label htmlFor={nameId} className={styles.label}>
                  {t.name}
                </label>
                <input
                  ref={nameRef}
                  id={nameId}
                  className={styles.input}
                  value={name}
                  spellCheck={false}
                  autoComplete="off"
                  onChange={(e) => setName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !isImeKey(e)) {
                      e.preventDefault();
                      void submit();
                    }
                  }}
                />
              </div>
              <div className={styles.field}>
                <label htmlFor={descId} className={styles.label}>
                  {t.description}
                </label>
                <textarea
                  id={descId}
                  className={styles.textarea}
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                />
              </div>
            </>
          )}
          <section className={styles.field} aria-label={t.ops}>
            <h3 className={styles.sectionTitle}>{t.ops}</h3>
            <ul className={styles.checkList}>
              {nodes.map((node) => (
                <li key={node.id}>
                  <label className={styles.check}>
                    <input
                      type="checkbox"
                      checked={keep.includes(node.id)}
                      onChange={(e) => toggle(node.id, e.target.checked)}
                    />
                    <span className={styles.checkName}>{opName(node.op)}</span>
                    <span className={styles.checkSummary}>
                      {node.enabled ? summarizeNode(node) : ja.recipes.disabled}
                    </span>
                  </label>
                </li>
              ))}
            </ul>
            <p className={styles.note}>{t.opsHint}</p>
            {excluded && <p className={styles.note}>{t.paintOff}</p>}
            {disabled && <p className={styles.note}>{t.disabledOff}</p>}
          </section>
          {format !== null && (
            <div className={styles.switchRow}>
              <span className={styles.switchText}>
                <span>{t.withFormat(formatLabel(format))}</span>
                <span className={styles.note}>{t.withFormatOff}</span>
              </span>
              <Switch
                checked={withFormat}
                onChange={setWithFormat}
                aria-label={t.withFormat(formatLabel(format))}
              />
            </div>
          )}
          {slots.length > 0 && (
            <section className={styles.field} aria-label={t.perItem}>
              <h3 className={styles.sectionTitle}>{t.perItem}</h3>
              <p className={styles.note}>{t.perItemHint}</p>
              <div className={styles.bindList}>
                {slots
                  .filter((slot) => slotActive(slot, choices))
                  .map((slot) => {
                    const c = choices[slot.key] ?? { type: 'keep' };
                    return (
                      <div key={slot.key} className={styles.bindRow}>
                        <span className={styles.bindLabel} title={slot.label}>
                          {slot.label}
                        </span>
                        <span className={styles.bindControls}>
                          <Select
                            aria-label={slot.label}
                            value={choiceValue(c)}
                            options={slotOptions(slot)}
                            onChange={(v) => choose(slot, v)}
                          />
                          {c.type === 'time' && (
                            <>
                              <input
                                className={styles.seconds}
                                inputMode="decimal"
                                aria-label={`${slot.label}（${t.seconds}）`}
                                value={c.seconds}
                                onChange={(e) =>
                                  setChoices((all) => ({
                                    ...all,
                                    [slot.key]: { ...c, seconds: e.target.value },
                                  }))
                                }
                              />
                              <span className={styles.unit}>{t.seconds}</span>
                            </>
                          )}
                        </span>
                      </div>
                    );
                  })}
              </div>
            </section>
          )}
          {error && (
            <p className={styles.error} role="alert">
              {error}
            </p>
          )}
        </div>
        <footer className={styles.actions}>
          <Button onClick={dismissSheet}>{t.cancel}</Button>
          <Button variant="primary" disabled={busy || pending} onClick={() => void submit()}>
            {t.save}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
