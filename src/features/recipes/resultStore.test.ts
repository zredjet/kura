import { describe, expect, it, vi } from 'vitest';

import type { BatchResultDto } from '../../ipc/batch';
import { openApply } from './applyStore';
import { retryBatch, retryItems } from './resultStore';

vi.mock('../../ipc/batch', () => ({ batchResult: vi.fn(), undoBatch: vi.fn() }));
vi.mock('./applyStore', () => ({ openApply: vi.fn(async () => undefined) }));

function result(over: Partial<BatchResultDto>): BatchResultDto {
  return {
    job: 8,
    state: 'done',
    action: { type: 'overwrite' },
    recipe: 'r',
    includeDrafts: false,
    total: 5,
    groups: [
      { state: 'saved', code: null, items: [1] },
      { state: 'skipped', code: 'Open', items: [2] },
      { state: 'failed', code: 'Invalid', items: [3] },
      { state: 'canceled', code: null, items: [4] },
      { state: 'existing', code: 'Existing', items: [5] },
    ],
    created: [],
    undoneBy: [],
    undoAvailable: 1,
    warnings: [],
    ...over,
  };
}

describe('resultStore', () => {
  it('もう一度当てるのは飛ばした・失敗・中止・待ちの行（1 件ずつの一括だけ）', () => {
    expect(retryItems(result({}))).toEqual([2, 3, 4]);
    expect(
      retryItems(result({ action: { type: 'undo', of: 1, saveAs: false, merge: false } })),
    ).toEqual([]);
    expect(retryItems(result({ action: { type: 'merge', name: 'x', sheet: {} } }))).toEqual([]);
  });

  it('［もう一度…］はその一括の写しで、同じ保存のしかたから始める', () => {
    retryBatch(result({ action: { type: 'saveAs', name: '{名前} 2x' } }));
    expect(openApply).toHaveBeenCalledWith({ type: 'job', job: 8 }, [2, 3, 4], {
      retry: true,
      mode: 'saveAs',
      nameTemplate: '{名前} 2x',
    });
  });
});
