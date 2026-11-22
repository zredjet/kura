/**
 * 当てるシート・結果のシート・まとめて 1 つにするシートで使う部品と純粋な関数（M14 段 7）：理由の見出し・形・失うもの・
 * 読む値の例・先頭のアイテムの名前・一覧で選ぶ。
 */

import { useEffect, useState } from 'react';

import { ja } from '../../i18n/ja';
import type { LossDto, ShapeInfoDto } from '../../ipc/batch';
import type { JsonValue } from '../../ipc/edit';
import { getItems } from '../../ipc/items';
import { formatCount, formatDuration } from '../../lib/format';
import { useViewStore } from '../../stores/viewStore';
import { opSpec, paramLabel, valueSummary } from '../editor/opsCatalog';
import { selectAll } from '../library/selection';
import { formatLabel } from './recipeModel';
import styles from './Recipes.module.css';

const t = ja.batch;

/** 理由の見出し（知らない code は Rust の言葉か code のまま）。 */
export function codeLabel(code: string | null, message?: string | null): string {
  if (code && t.codes[code]) return t.codes[code];
  return message || code || '';
}

/** 状態の見出し（向きで言い方を変える）。 */
export function stateLabel(state: string, action: 'apply' | 'undo' | 'merge' = 'apply'): string {
  const special = action === 'undo' ? t.undoStates : action === 'merge' ? t.mergeStates : null;
  return special?.[state] ?? t.states[state] ?? state;
}

/** 形の短い表記（「64 × 64」「64 × 64 · 8 コマ」「48 kHz · 2 ch · 0:03.500」）。 */
export function shapeText(shape: ShapeInfoDto): string {
  if (shape.kind === 'image') {
    if (shape.w === null || shape.h === null) return t.shapeUnknown;
    const [w, h] = [formatCount(shape.w), formatCount(shape.h)];
    return shape.frames !== null
      ? t.shapeFrames(w, h, formatCount(shape.frames))
      : t.shapeImage(w, h);
  }
  const rate = String(Math.round(shape.rate / 100) / 10);
  const len =
    shape.frames === null ? t.shapeUnknown : formatDuration((shape.frames / shape.rate) * 1000);
  return t.shapeAudio(rate, String(shape.channels), len);
}

/** 失うものの言い方。 */
export function lossText(loss: LossDto, format: string): string {
  const f = format ? formatLabel(format) : '';
  switch (loss.type) {
    case 'lossy':
      return t.loss.lossy(f);
    case 'alpha':
      return t.loss.alpha(f);
    case 'semiAlpha':
      return t.loss.semiAlpha(f);
    case 'highBits':
      return t.loss.highBits(f);
    case 'colors':
      return t.loss.colors(f, formatCount(loss.max));
    case 'loopPoints':
      return t.loss.loopPoints(f);
    case 'settings':
      return t.loss.settings;
  }
}

/** 結び付けで読んだ値の記録（`bound_json`）を「格子：16 × 16」の行にする。読めなければ空。 */
export function boundText(json: string): string[] {
  let list: unknown;
  try {
    list = JSON.parse(json);
  } catch {
    return [];
  }
  if (!Array.isArray(list)) return [];
  const out: string[] = [];
  for (const raw of list as Record<string, JsonValue>[]) {
    const op = typeof raw.op === 'string' ? raw.op : '';
    const param = typeof raw.param === 'string' ? raw.param : '';
    const v = typeof raw.v === 'number' ? raw.v : undefined;
    const spec = opSpec(op, v)?.schema.params.find((p) => p.key === param);
    const part =
      raw.part === 'start'
        ? ` · ${ja.saveRecipe.rangeStart}`
        : raw.part === 'end'
          ? ` · ${ja.saveRecipe.rangeEnd}`
          : '';
    const value = raw.value ?? null;
    // 区間の片方は書いたフレームの数だけ
    const text =
      raw.part && typeof value === 'number'
        ? formatCount(value)
        : spec
          ? (valueSummary(op, spec, value) ?? JSON.stringify(value))
          : JSON.stringify(value);
    out.push(`${paramLabel(op, param)}${part}：${text}`);
  }
  return out;
}

/** 先頭のアイテムの名前を読む（`limit` 件まで。読めなければ空）。 */
export function useItemTitles(ids: readonly number[], limit = 3): Map<number, string> {
  const key = ids.slice(0, limit).join(',');
  const [titles, setTitles] = useState<Map<number, string>>(new Map());
  useEffect(() => {
    const head = key === '' ? [] : key.split(',').map(Number);
    if (head.length === 0) return;
    let alive = true;
    getItems(head)
      .then((items) => {
        if (alive) setTitles(new Map(items.map((i) => [i.id, i.title])));
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [key]);
  return titles;
}

/** 先頭 3 件の名前と残りの件数（「hero、walk、run ほか 12 件」）。 */
export function ItemNames({ ids }: { ids: readonly number[] }) {
  const titles = useItemTitles(ids);
  const names = ids
    .slice(0, 3)
    .map((id) => titles.get(id))
    .filter((n): n is string => n !== undefined);
  if (names.length === 0) return null;
  const rest = ids.length - names.length;
  return (
    <span className={styles.names}>
      {names.join('、')}
      {rest > 0 && ` ${t.more(formatCount(rest))}`}
    </span>
  );
}

/** アイテムをすべての一覧で選ぶ（シートは呼び手が閉じる）。 */
export function selectInList(ids: readonly number[]): void {
  if (ids.length === 0) return;
  const view = useViewStore.getState();
  view.setView({ kind: 'all' });
  useViewStore.getState().setSelection(selectAll(ids));
}

/** 群の行：見出し・件数・［一覧で選ぶ］（`onSelect` でシートを閉じてから選ぶ）・Rust の言葉・先頭の名前。 */
export function GroupRow({
  title,
  ids,
  note,
  onSelect,
}: {
  title: string;
  ids: number[];
  note?: string | null;
  onSelect: () => void;
}) {
  return (
    <li className={styles.group}>
      <div className={styles.groupHead}>
        <span className={styles.groupTitle}>{title}</span>
        <span className={styles.groupCount}>{t.count(formatCount(ids.length))}</span>
        <button
          type="button"
          className={styles.link}
          onClick={() => {
            onSelect();
            selectInList(ids);
          }}
        >
          {t.selectInList}
        </button>
      </div>
      {note && note !== title && <p className={styles.note}>{note}</p>}
      <ItemNames ids={ids} />
    </li>
  );
}
