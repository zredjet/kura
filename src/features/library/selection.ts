/**
 * グリッドの選択（純粋関数）。選んだ順を保つ（⌘G のユニットの並び順に使う）。
 * - クリック：1 件だけ選ぶ
 * - ⌘クリック：足す・外す
 * - ⇧クリック・⇧←→：基点から範囲を選ぶ（範囲の中は基点から近い順）
 */

export interface Selection {
  /** 選んだ順の ID。 */
  ids: number[];
  /** 範囲選択の基点。 */
  anchor: number | null;
  /** いま注目している ID（インスペクタ・クイックルック・←→ の起点）。 */
  focus: number | null;
}

export const EMPTY_SELECTION: Selection = { ids: [], anchor: null, focus: null };

/** 1 件だけ選ぶ。 */
export function selectOne(id: number): Selection {
  return { ids: [id], anchor: id, focus: id };
}

/** 足す・外す（⌘クリック）。 */
export function toggle(selection: Selection, id: number): Selection {
  if (selection.ids.includes(id)) {
    const ids = selection.ids.filter((x) => x !== id);
    const last = ids[ids.length - 1] ?? null;
    return { ids, anchor: last, focus: last };
  }
  return { ids: [...selection.ids, id], anchor: id, focus: id };
}

/** 基点から `id` までを選ぶ（⇧クリック）。基点がなければ 1 件だけ。 */
export function extendTo(selection: Selection, id: number, order: ArrayLike<number>): Selection {
  const anchor = selection.anchor;
  const from = anchor === null ? -1 : indexIn(order, anchor);
  const to = indexIn(order, id);
  if (anchor === null || from < 0 || to < 0) {
    return selectOne(id);
  }
  const step = to >= from ? 1 : -1;
  const ids: number[] = [];
  for (let i = from; i !== to + step; i += step) {
    const value = order[i];
    if (value !== undefined) {
      ids.push(value);
    }
  }
  return { ids, anchor, focus: id };
}

/** 注目を `delta` だけ動かして 1 件だけ選ぶ（←→）。何も選んでいなければ先頭。 */
export function moveFocus(
  selection: Selection,
  delta: number,
  order: ArrayLike<number>,
): Selection {
  const next = neighbor(selection, delta, order);
  return next === null ? selection : selectOne(next);
}

/** 注目を `delta` だけ動かして範囲を広げる（⇧←→）。 */
export function extendBy(selection: Selection, delta: number, order: ArrayLike<number>): Selection {
  const next = neighbor(selection, delta, order);
  if (next === null) {
    return selection;
  }
  return selection.anchor === null ? selectOne(next) : extendTo(selection, next, order);
}

/** すべて選ぶ（⌘A）。 */
export function selectAll(order: ArrayLike<number>): Selection {
  const ids = Array.from(order);
  const first = ids[0] ?? null;
  return { ids, anchor: first, focus: ids[ids.length - 1] ?? null };
}

/** 一覧からなくなった ID を外す（読み直したあと）。 */
export function prune(selection: Selection, order: ArrayLike<number>): Selection {
  const present = new Set(Array.from(order));
  const ids = selection.ids.filter((id) => present.has(id));
  if (ids.length === selection.ids.length) {
    return selection;
  }
  const keep = (id: number | null) => (id !== null && present.has(id) ? id : null);
  const last = ids[ids.length - 1] ?? null;
  return {
    ids,
    anchor: keep(selection.anchor) ?? last,
    focus: keep(selection.focus) ?? last,
  };
}

function indexIn(order: ArrayLike<number>, id: number): number {
  for (let i = 0; i < order.length; i += 1) {
    if (order[i] === id) {
      return i;
    }
  }
  return -1;
}

function neighbor(selection: Selection, delta: number, order: ArrayLike<number>): number | null {
  if (order.length === 0) {
    return null;
  }
  const current = selection.focus === null ? -1 : indexIn(order, selection.focus);
  if (current < 0) {
    return order[0] ?? null;
  }
  const next = Math.min(order.length - 1, Math.max(0, current + delta));
  return order[next] ?? null;
}
