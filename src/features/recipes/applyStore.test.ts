import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { BatchPrecheckDto } from '../../ipc/batch';
import { cancelPrecheck, precheckBatch, startBatch } from '../../ipc/batch';
import { LIST_FLAG, queryIds } from '../../ipc/items';
import { notify } from '../../stores/noticeStore';
import {
  closeApply,
  expandEntries,
  expandName,
  openApply,
  requestOf,
  setApplyMode,
  setIncludeDrafts,
  startApply,
  useApplyStore,
  validNameTemplate,
} from './applyStore';

vi.mock('../../ipc/batch', () => ({
  DEFAULT_SAVE_AS_NAME: '{名前} のコピー',
  precheckBatch: vi.fn(),
  cancelPrecheck: vi.fn(async () => null),
  onPrecheckProgress: vi.fn(async () => () => undefined),
  startBatch: vi.fn(async () => 42),
  batchResult: vi.fn(async () => ({ recipe: '写しのレシピ' })),
}));
vi.mock('../../ipc/recipes', () => ({
  getRecipe: vi.fn(async (id: number) => ({ id, name: 'ドット絵 4 倍', problem: null })),
}));
vi.mock('../../ipc/items', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../ipc/items')>();
  return { ...actual, queryIds: vi.fn() };
});
vi.mock('../../stores/noticeStore', () => ({ notify: vi.fn() }));

function precheck(over: Partial<BatchPrecheckDto> = {}): BatchPrecheckDto {
  return {
    total: 3,
    missing: [],
    apply: [1, 2],
    skips: [{ state: 'skipped', code: 'Draft', message: '下書き', items: [3] }],
    unchecked: [],
    outputs: [],
    losses: [],
    composed: [],
    disk: { newBytes: 0, replacedBytes: 0 },
    memoryPeak: 0,
    memoryBudget: 0,
    bound: [],
    notes: [],
    sheet: null,
    ...over,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe('applyStore', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    closeApply();
  });

  it('選んだものを広げる：ユニットは中のアイテムに、重なりは最初だけ', async () => {
    vi.mocked(queryIds).mockResolvedValue({
      ids: new Uint32Array([7, 2, 8]),
      aspects: new Uint16Array(),
      groups: [],
    });
    const unit = (LIST_FLAG | 4) >>> 0;
    const out = await expandEntries([2, unit, 9]);
    expect(out).toEqual({ items: [2, 7, 8, 9], hadUnits: true });
    expect(queryIds).toHaveBeenCalledWith({ kind: 'unit', id: 4 });
    expect(await expandEntries([5])).toEqual({ items: [5], hadUnits: false });
  });

  it('頼みの形：別名は名前の型、下書きは上書きのときだけ', () => {
    const base = {
      source: { type: 'recipe' as const, id: 1 },
      nameTemplate: '{名前} 4x',
      includeDrafts: true,
    };
    expect(requestOf({ ...base, mode: 'overwrite' }, [1])).toEqual({
      source: base.source,
      items: [1],
      mode: { type: 'overwrite' },
      includeDrafts: true,
    });
    expect(requestOf({ ...base, mode: 'saveAs' }, [1])?.mode).toEqual({
      type: 'saveAs',
      name: '{名前} 4x',
    });
    expect(requestOf({ ...base, mode: 'saveAs' }, [1])?.includeDrafts).toBe(false);
    expect(requestOf({ ...base, mode: 'saveAs', nameTemplate: '  ' }, [1])?.mode).toEqual({
      type: 'saveAs',
      name: '{名前} のコピー',
    });
  });

  it('名前の型の確かめと展開', () => {
    expect(validNameTemplate('{名前} のコピー')).toBe(true);
    expect(validNameTemplate(' ')).toBe(false);
    expect(validNameTemplate('a/b')).toBe(false);
    expect(expandName('{名前} のコピー', 'hero')).toBe('hero のコピー');
    expect(expandName('{名前}-{名前}', 'a')).toBe('a-a');
  });

  it('開くと確かめ、保存のしかた・下書きを変えると止めて確かめ直す。積むのは当てる見込みだけ', async () => {
    vi.mocked(precheckBatch).mockResolvedValue(precheck());
    await openApply({ type: 'recipe', id: 3 }, [1, 2, 3]);
    let s = useApplyStore.getState();
    expect(s.phase).toBe('ready');
    expect(s.recipeName).toBe('ドット絵 4 倍');
    expect(precheckBatch).toHaveBeenLastCalledWith(expect.any(String), {
      source: { type: 'recipe', id: 3 },
      items: [1, 2, 3],
      mode: { type: 'overwrite' },
      includeDrafts: false,
    });

    let resolve: (v: BatchPrecheckDto) => void = () => undefined;
    vi.mocked(precheckBatch).mockImplementationOnce(
      () => new Promise<BatchPrecheckDto>((r) => (resolve = r)),
    );
    setIncludeDrafts(true);
    expect(useApplyStore.getState().phase).toBe('checking');
    // 確かめの途中で別の確かめを始めたら、前のものは止める
    vi.mocked(precheckBatch).mockResolvedValueOnce(precheck({ apply: [1, 2, 3], skips: [] }));
    setApplyMode('saveAs');
    expect(cancelPrecheck).toHaveBeenCalledTimes(1);
    await flush();
    resolve(precheck({ apply: [] }));
    await flush();
    s = useApplyStore.getState();
    expect(s.phase).toBe('ready');
    expect(s.precheck?.apply).toEqual([1, 2, 3]);

    await startApply();
    expect(startBatch).toHaveBeenCalledWith({
      source: { type: 'recipe', id: 3 },
      items: [1, 2, 3],
      mode: { type: 'saveAs', name: '{名前} のコピー' },
      includeDrafts: false,
    });
    expect(useApplyStore.getState().phase).toBe('closed');
  });

  it('当てる画像・音声がなければ開かずに知らせる。閉じると確かめを止める', async () => {
    await openApply({ type: 'recipe', id: 3 }, []);
    expect(notify).toHaveBeenCalled();
    expect(useApplyStore.getState().phase).toBe('closed');

    vi.mocked(precheckBatch).mockImplementationOnce(() => new Promise(() => undefined));
    void openApply({ type: 'job', job: 9 }, [1]);
    await flush();
    await flush();
    expect(useApplyStore.getState().phase).toBe('checking');
    expect(useApplyStore.getState().recipeName).toBe('写しのレシピ');
    closeApply();
    expect(cancelPrecheck).toHaveBeenCalled();
    expect(useApplyStore.getState().phase).toBe('closed');
  });
});
