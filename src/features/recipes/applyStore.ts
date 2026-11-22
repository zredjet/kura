/**
 * レシピを当てるシート（M14 段 7。docs/editing.md §14.2「事前チェック」）：選んだもの（ユニットは中のアイテムに広げる）に
 * レシピを当てる見込みを確かめ（描かない。進みのイベントと中止）、上書きか別名で一括のジョブを積む。保存のしかた・下書きを
 * 含めるかを変えると確かめ直す。シートを閉じたら確かめを止める。
 */

import { create } from 'zustand';

import { ja } from '../../i18n/ja';
import {
  type BatchModeDto,
  type BatchPrecheckDto,
  type BatchRequestDto,
  batchResult,
  cancelPrecheck,
  DEFAULT_SAVE_AS_NAME,
  onPrecheckProgress,
  precheckBatch,
  type RecipeSourceDto,
  startBatch,
} from '../../ipc/batch';
import { isUnitEntry, queryIds, unitIdOf } from '../../ipc/items';
import { getRecipe } from '../../ipc/recipes';
import { notify } from '../../stores/noticeStore';

export type ApplyMode = 'overwrite' | 'saveAs';

interface ApplyState {
  phase: 'closed' | 'preparing' | 'checking' | 'ready' | 'error';
  source: RecipeSourceDto | null;
  recipeName: string;
  /** 結果のシートの［もう一度当てる］から開いた。 */
  retry: boolean;
  /** 当てるアイテム（選んだ順。ユニットは中のアイテムに広げたもの）。 */
  items: number[];
  /** 選んだものにユニットがあった。 */
  hadUnits: boolean;
  mode: ApplyMode;
  nameTemplate: string;
  includeDrafts: boolean;
  /**
   * 保存していない下書きのあるアイテムの数（上書きで、下書きを含めずに確かめたときの Draft の群。含めて確かめ直すと群が
   * なくなるので、前の数を使い回す）。
   */
  draftCount: number;
  /** 確かめの進み。 */
  done: number;
  total: number;
  precheck: BatchPrecheckDto | null;
  error: string | null;
  starting: boolean;
}

const CLOSED: Omit<ApplyState, never> = {
  phase: 'closed',
  source: null,
  recipeName: '',
  retry: false,
  items: [],
  hadUnits: false,
  mode: 'overwrite',
  nameTemplate: DEFAULT_SAVE_AS_NAME,
  includeDrafts: false,
  draftCount: 0,
  done: 0,
  total: 0,
  precheck: null,
  error: null,
  starting: false,
};

export const useApplyStore = create<ApplyState>(() => ({ ...CLOSED }));

/** 動いている確かめの印。 */
let token: string | null = null;
let serial = 0;
let listening: Promise<() => void> | null = null;
/** 開くたびに増える（閉じた・開き直したあとの応えを捨てる）。 */
let opened = 0;

function listen(): void {
  listening ??= onPrecheckProgress((e) => {
    if (e.token !== token) return;
    useApplyStore.setState({ done: e.done, total: e.total });
  }).catch(() => {
    listening = null;
    return () => undefined;
  });
}

/** 一度に読むユニットの数（広げる問い合わせを並べる数）。 */
const EXPAND_CHUNK = 16;

/**
 * 選んだもの（アイテムとユニット）をアイテムの列に広げる（選んだ順。ユニットは中の並び。重なりは最初のものだけ）。
 * `origins` は広げたアイテムがどの選んだもの（ユニットの印）から来たか（一覧の並びで並べ直すときに使う）。
 */
export async function expandEntries(entries: readonly number[]): Promise<{
  items: number[];
  hadUnits: boolean;
  origins: Map<number, number>;
}> {
  const units = entries.filter(isUnitEntry);
  const members = new Map<number, number[]>();
  for (let i = 0; i < units.length; i += EXPAND_CHUNK) {
    const chunk = units.slice(i, i + EXPAND_CHUNK);
    const lists = await Promise.all(
      chunk.map((entry) => queryIds({ kind: 'unit', id: unitIdOf(entry) })),
    );
    chunk.forEach((entry, k) => members.set(entry, Array.from(lists[k]?.ids ?? [])));
  }
  const seen = new Set<number>();
  const items: number[] = [];
  const origins = new Map<number, number>();
  for (const entry of entries) {
    for (const id of members.get(entry) ?? [entry]) {
      if (!seen.has(id)) {
        seen.add(id);
        items.push(id);
        origins.set(id, entry);
      }
    }
  }
  return { items, hadUnits: units.length > 0, origins };
}

/** 頼みの形。 */
export function requestOf(
  s: Pick<ApplyState, 'source' | 'mode' | 'nameTemplate' | 'includeDrafts'>,
  items: number[],
): BatchRequestDto | null {
  if (!s.source) return null;
  const mode: BatchModeDto =
    s.mode === 'saveAs'
      ? { type: 'saveAs', name: s.nameTemplate.trim() || DEFAULT_SAVE_AS_NAME }
      : { type: 'overwrite' };
  return {
    source: s.source,
    items,
    mode,
    includeDrafts: s.mode === 'overwrite' && s.includeDrafts,
  };
}

/** 確かめを止める（動いていれば）。 */
function stopCheck(): void {
  const running = token;
  token = null;
  if (running) cancelPrecheck(running).catch(() => undefined);
}

/** 今の保存のしかたで確かめ直す。 */
async function check(): Promise<void> {
  stopCheck();
  const s = useApplyStore.getState();
  const request = requestOf(s, s.items);
  if (!request) return;
  const mine = `apply-${++serial}`;
  token = mine;
  listen();
  useApplyStore.setState({
    phase: 'checking',
    done: 0,
    total: s.items.length,
    precheck: null,
    error: null,
  });
  try {
    const precheck = await precheckBatch(mine, request);
    if (token !== mine) return;
    token = null;
    const now = useApplyStore.getState();
    const draftCount =
      now.mode === 'overwrite' && !now.includeDrafts
        ? (precheck.skips.find((g) => g.code === 'Draft')?.items.length ?? 0)
        : now.draftCount;
    useApplyStore.setState({ phase: 'ready', precheck, draftCount });
  } catch (error) {
    if (token !== mine) return;
    token = null;
    useApplyStore.setState({
      phase: 'error',
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

/**
 * 当てるシートを開く：レシピ（または一括の写し）の名前を読み、選んだものを広げて確かめ始める。当てる画像・音声がなければ
 * 知らせるだけ。
 */
export async function openApply(
  source: RecipeSourceDto,
  entries: readonly number[],
  options: { retry?: boolean; mode?: ApplyMode; nameTemplate?: string } = {},
): Promise<void> {
  if (entries.length === 0) {
    notify(ja.batch.nothing);
    return;
  }
  stopCheck();
  const mine = ++opened;
  useApplyStore.setState({
    ...CLOSED,
    phase: 'preparing',
    source,
    retry: options.retry ?? false,
    mode: options.mode ?? 'overwrite',
    nameTemplate: options.nameTemplate ?? DEFAULT_SAVE_AS_NAME,
  });
  try {
    const [name, expanded] = await Promise.all([
      source.type === 'recipe'
        ? getRecipe(source.id).then((r) => {
            if (r.problem) throw new Error(ja.recipes.problem(r.problem));
            return r.name;
          })
        : batchResult(source.job).then((r) => r.recipe ?? ja.batch.unnamed),
      expandEntries(entries),
    ]);
    if (mine !== opened) return;
    if (expanded.items.length === 0) {
      useApplyStore.setState({ ...CLOSED });
      notify(ja.batch.nothing);
      return;
    }
    useApplyStore.setState({
      recipeName: name,
      items: expanded.items,
      hadUnits: expanded.hadUnits,
    });
    await check();
  } catch (error) {
    if (mine !== opened) return;
    useApplyStore.setState({ ...CLOSED });
    notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
  }
}

/** 保存のしかたを変える（確かめ直す）。 */
export function setApplyMode(mode: ApplyMode): void {
  if (useApplyStore.getState().mode === mode) return;
  useApplyStore.setState({ mode });
  void check();
}

/** 下書きのあるアイテムも上書きするか（確かめ直す）。 */
export function setIncludeDrafts(on: boolean): void {
  if (useApplyStore.getState().includeDrafts === on) return;
  useApplyStore.setState({ includeDrafts: on });
  void check();
}

/** 別名の名前の型（確かめの結果は変わらないので確かめ直さない）。 */
export function setNameTemplate(name: string): void {
  useApplyStore.setState({ nameTemplate: name });
}

/** シートを閉じる（確かめを止める）。 */
export function closeApply(): void {
  opened++;
  stopCheck();
  useApplyStore.setState({ ...CLOSED });
}

/** 名前の型を使えるか（空でなく `/` を含まない。Rust と同じ）。 */
export function validNameTemplate(name: string): boolean {
  const n = name.trim();
  return n.length > 0 && !n.includes('/') && !n.includes('\0');
}

/** 名前の型を 1 件の名前にする（`{名前}` を表示名に）。 */
export function expandName(template: string, title: string): string {
  return template.trim().split('{名前}').join(title);
}

/** 当てる見込みのアイテムに一括を積む（積めたらシートを閉じる）。 */
export async function startApply(): Promise<void> {
  const s = useApplyStore.getState();
  if (s.phase !== 'ready' || s.starting || !s.precheck) return;
  if (s.mode === 'saveAs' && !validNameTemplate(s.nameTemplate)) return;
  const items = s.precheck.apply;
  const request = requestOf(s, items);
  if (!request || items.length === 0) return;
  useApplyStore.setState({ starting: true, error: null });
  try {
    await startBatch(request);
    notify(ja.batch.started(s.recipeName));
    closeApply();
  } catch (error) {
    useApplyStore.setState({
      starting: false,
      error: error instanceof Error ? error.message : String(error),
    });
  }
}
