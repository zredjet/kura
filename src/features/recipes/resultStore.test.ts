import { describe, expect, it, vi } from 'vitest';

import { askConfirm } from '../../components/Overlay/overlayStore';
import type { BatchResultDto } from '../../ipc/batch';
import { batchResult, undoBatch } from '../../ipc/batch';
import { openApply } from './applyStore';
import { useBatchJobsStore } from './batchJobsStore';
import { openResult, retryBatch, retryItems, undoFromResult, useResultStore } from './resultStore';

vi.mock('../../ipc/batch', () => ({ batchResult: vi.fn(), undoBatch: vi.fn(async () => 99) }));
vi.mock('./applyStore', () => ({ openApply: vi.fn(async () => undefined) }));
vi.mock('../../components/Overlay/overlayStore', () => ({ askConfirm: vi.fn() }));
vi.mock('../../stores/noticeStore', () => ({ notify: vi.fn() }));

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

  it('［元に戻す…］は確かめてから積み、閉じる。やめたら積まない', async () => {
    vi.mocked(askConfirm).mockResolvedValueOnce(false);
    await undoFromResult(result({}));
    expect(undoBatch).not.toHaveBeenCalled();
    useResultStore.setState({ job: 8, result: result({}) });
    vi.mocked(askConfirm).mockResolvedValueOnce(true);
    await undoFromResult(result({}));
    expect(undoBatch).toHaveBeenCalledWith(8);
    expect(useResultStore.getState().job).toBeNull();
  });

  it('シートの新しい結果を、ほかの一括の進みでカプセルの古い写しに戻さない', async () => {
    const old = result({ undoAvailable: 1 });
    const fresh = result({ undoAvailable: 0, undoneBy: [12] });
    useBatchJobsStore.setState({
      jobs: { 8: { id: 8, state: 'done', progress: 1, message: null, result: old } },
    });
    vi.mocked(batchResult).mockResolvedValueOnce(fresh);
    openResult(8);
    await new Promise((r) => setTimeout(r, 0));
    expect(useResultStore.getState().result).toBe(fresh);
    // 別の一括の進みでストアが変わる
    useBatchJobsStore.setState((st) => ({
      jobs: {
        ...st.jobs,
        9: { id: 9, state: 'running', progress: 0.5, message: null, result: null },
      },
    }));
    expect(useResultStore.getState().result).toBe(fresh);
  });
});
