/**
 * まとめて 1 つにする（スプライトシート。M14 段 7。docs/editing.md §14.2「N → 1」）：並べる順（選んだ順・名前順・一覧の並び）・
 * 1 行の数・すき間・何もない所の色・セルの中の揃え・名前を選び、事前チェックでシートの大きさと飛ばすものを見せてから積む。
 */

import { create } from 'zustand';

import { ja } from '../../i18n/ja';
import {
  type Anchor,
  type BatchPrecheckDto,
  cancelPrecheck,
  DEFAULT_SHEET_NAME,
  type MergeRequestDto,
  onPrecheckProgress,
  precheckMerge,
  type SheetParams,
  startMerge,
} from '../../ipc/batch';
import { getItems } from '../../ipc/items';
import { notify } from '../../stores/noticeStore';
import { useLibraryStore } from '../../stores/libraryStore';
import { expandEntries, validNameTemplate } from './applyStore';

export type MergeOrder = 'selected' | 'name' | 'list';

interface MergeState {
  phase: 'closed' | 'preparing' | 'checking' | 'ready' | 'error';
  /** 選んだ順に広げたアイテム。 */
  selected: number[];
  order: MergeOrder;
  /** 1 行の数（null は自動）。 */
  columns: number | null;
  gapX: number;
  gapY: number;
  /** 何もない所の色（null は透明）。 */
  fill: [number, number, number, number] | null;
  anchor: Anchor;
  name: string;
  done: number;
  total: number;
  precheck: BatchPrecheckDto | null;
  error: string | null;
  starting: boolean;
}

const CLOSED: MergeState = {
  phase: 'closed',
  selected: [],
  order: 'selected',
  columns: null,
  gapX: 0,
  gapY: 0,
  fill: null,
  anchor: 'center',
  name: DEFAULT_SHEET_NAME,
  done: 0,
  total: 0,
  precheck: null,
  error: null,
  starting: false,
};

export const useMergeStore = create<MergeState>(() => ({ ...CLOSED }));

let token: string | null = null;
let serial = 0;
let opened = 0;
let listening: Promise<() => void> | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
/** 名前順の並び（開くたびに読む）。 */
let byName: number[] | null = null;

/** すき間・1 行の数の上限（Rust の `MAX_GAP`・`MAX_COLUMNS`）。 */
export const MAX_SHEET_NUMBER = 4096;

function listen(): void {
  listening ??= onPrecheckProgress((e) => {
    if (e.token !== token) return;
    useMergeStore.setState({ done: e.done, total: e.total });
  }).catch(() => {
    listening = null;
    return () => undefined;
  });
}

/** 一覧の並びに並べ替える（一覧にないものは後ろに選んだ順で）。 */
export function inListOrder(items: readonly number[], list: ArrayLike<number> | null): number[] {
  if (!list) return [...items];
  const at = new Map<number, number>();
  for (let i = 0; i < list.length; i++) at.set(list[i] as number, i);
  const inList = items.filter((id) => at.has(id));
  const rest = items.filter((id) => !at.has(id));
  inList.sort((a, b) => (at.get(a) ?? 0) - (at.get(b) ?? 0));
  return [...inList, ...rest];
}

/** 名前順（自然な順・日本語の照合。同じ名前は選んだ順）。 */
export function inNameOrder(
  items: readonly number[],
  titles: ReadonlyMap<number, string>,
): number[] {
  const collator = new Intl.Collator('ja', { numeric: true, sensitivity: 'base' });
  return items
    .map((id, i) => ({ id, i, title: titles.get(id) ?? '' }))
    .sort((a, b) => collator.compare(a.title, b.title) || a.i - b.i)
    .map((x) => x.id);
}

/** 今の並べる順のアイテム。 */
export function orderedItems(s: Pick<MergeState, 'selected' | 'order'>): number[] {
  if (s.order === 'name') return byName ?? s.selected;
  if (s.order === 'list')
    return inListOrder(s.selected, useLibraryStore.getState().list?.ids ?? null);
  return s.selected;
}

/** シートの引数。 */
export function sheetOf(
  s: Pick<MergeState, 'columns' | 'gapX' | 'gapY' | 'fill' | 'anchor'>,
): SheetParams {
  return { columns: s.columns, gapX: s.gapX, gapY: s.gapY, fill: s.fill, anchor: s.anchor };
}

function requestOf(s: MergeState): MergeRequestDto {
  return {
    items: orderedItems(s),
    name: s.name.trim() || DEFAULT_SHEET_NAME,
    sheet: sheetOf(s),
  };
}

function stopCheck(): void {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  const running = token;
  token = null;
  if (running) cancelPrecheck(running).catch(() => undefined);
}

async function check(): Promise<void> {
  stopCheck();
  const s = useMergeStore.getState();
  if (s.phase === 'closed') return;
  const mine = `merge-${++serial}`;
  token = mine;
  listen();
  useMergeStore.setState({
    phase: 'checking',
    done: 0,
    total: s.selected.length,
    error: null,
  });
  try {
    const precheck = await precheckMerge(mine, requestOf(s));
    if (token !== mine) return;
    token = null;
    useMergeStore.setState({ phase: 'ready', precheck });
  } catch (error) {
    if (token !== mine) return;
    token = null;
    useMergeStore.setState({
      phase: 'error',
      precheck: null,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

/** 少し待ってから確かめ直す（数の欄を打っている間に何度も確かめない）。 */
function checkSoon(): void {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    void check();
  }, 300);
}

/** 名前を読んで名前順の並びを作る（200 件ずつ）。 */
async function loadNameOrder(items: number[]): Promise<number[]> {
  const titles = new Map<number, string>();
  for (let i = 0; i < items.length; i += 200) {
    const chunk = await getItems(items.slice(i, i + 200));
    for (const item of chunk) titles.set(item.id, item.title);
  }
  return inNameOrder(items, titles);
}

/** まとめて 1 つにするシートを開く（ユニットは中のアイテムに広げる）。 */
export async function openMerge(entries: readonly number[]): Promise<void> {
  stopCheck();
  const mine = ++opened;
  byName = null;
  useMergeStore.setState({ ...CLOSED, phase: 'preparing' });
  try {
    const { items } = await expandEntries(entries);
    if (mine !== opened) return;
    if (items.length === 0) {
      useMergeStore.setState({ ...CLOSED });
      notify(ja.merge.nothing);
      return;
    }
    useMergeStore.setState({ selected: items });
    await check();
  } catch (error) {
    if (mine !== opened) return;
    useMergeStore.setState({ ...CLOSED });
    notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
  }
}

/** 並べる順を変える（名前順は名前を読んでから）。 */
export async function setMergeOrder(order: MergeOrder): Promise<void> {
  const s = useMergeStore.getState();
  if (s.order === order || s.phase === 'closed') return;
  if (order === 'name' && byName === null) {
    const mine = opened;
    try {
      const sorted = await loadNameOrder(s.selected);
      if (mine !== opened) return;
      byName = sorted;
    } catch (error) {
      notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
      return;
    }
  }
  useMergeStore.setState({ order });
  void check();
}

/** シートの引数を変える（少し待って確かめ直す）。 */
export function setMergeParams(
  patch: Partial<Pick<MergeState, 'columns' | 'gapX' | 'gapY' | 'fill' | 'anchor'>>,
): void {
  useMergeStore.setState(patch);
  checkSoon();
}

/** 名前の型（確かめの結果は変わらない）。 */
export function setMergeName(name: string): void {
  useMergeStore.setState({ name });
}

export function closeMerge(): void {
  opened++;
  stopCheck();
  byName = null;
  useMergeStore.setState({ ...CLOSED });
}

/** 一括を積む（積めたらシートを閉じる）。 */
export async function startMergeJob(): Promise<void> {
  const s = useMergeStore.getState();
  if (s.phase !== 'ready' || s.starting || !validNameTemplate(s.name)) return;
  useMergeStore.setState({ starting: true, error: null });
  try {
    await startMerge(requestOf(s));
    notify(ja.merge.started);
    closeMerge();
  } catch (error) {
    useMergeStore.setState({
      starting: false,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}
