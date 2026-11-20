import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import type { BatchResultDto } from '../../ipc/batch';
import type { BatchJob } from './batchJobsStore';
import { batchCounts, batchProgressView, keepsCapsule } from './batchView';

const t = ja.batch;

function result(over: Partial<BatchResultDto> = {}): BatchResultDto {
  return {
    job: 5,
    state: 'running',
    action: { type: 'overwrite' },
    recipe: 'ドット絵 4 倍',
    includeDrafts: false,
    total: 10,
    groups: [
      { state: 'saved', code: null, items: [1, 2, 3] },
      { state: 'unchanged', code: null, items: [4] },
      { state: 'skipped', code: 'Open', items: [5] },
      { state: 'failed', code: 'Invalid', items: [6] },
      { state: 'pending', code: null, items: [7, 8, 9, 10] },
    ],
    created: [],
    undoneBy: [],
    undoAvailable: 0,
    warnings: [],
    ...over,
  };
}

function job(over: Partial<BatchJob> = {}): BatchJob {
  return { id: 5, state: 'running', progress: 0.6, message: null, result: result(), ...over };
}

describe('batchView', () => {
  it('件数：当てた（版・変わらない・同じ中身）・飛ばした・失敗・待ち', () => {
    expect(batchCounts(result())).toEqual({
      applied: 4,
      skipped: 1,
      failed: 1,
      canceled: 0,
      pending: 4,
      total: 10,
    });
    expect(batchCounts(null).total).toBe(0);
  });

  it('動いている間は「当てています」と済んだ数・飛ばした・失敗、一時停止と中止', () => {
    const view = batchProgressView(job());
    expect(view.title).toBe(t.running('ドット絵 4 倍'));
    expect(view.detail).toBe(
      [t.progressCount('6', '10'), t.skipped('1'), t.failures('1')].join(' · '),
    );
    expect(view.progress).toBe(0.6);
    expect(view.actions).toEqual(['pause', 'cancel']);
    expect(view.finished).toBe(false);
  });

  it('一時停止は続きから進めると言い、再開と中止', () => {
    const view = batchProgressView(job({ state: 'paused' }));
    expect(view.title).toBe(t.paused('ドット絵 4 倍'));
    expect(view.detail).toBe(t.resumeNote);
    expect(view.actions).toEqual(['resume', 'cancel']);
  });

  it('終わったら「N 件に当てました」と結果・閉じる。飛ばした・失敗があれば残す', () => {
    const done = job({
      state: 'done',
      result: result({
        state: 'done',
        groups: [
          { state: 'saved', code: null, items: [1, 2] },
          { state: 'skipped', code: 'Open', items: [3] },
        ],
        total: 3,
      }),
    });
    const view = batchProgressView(done);
    expect(view.title).toBe(t.done('2'));
    expect(view.detail).toBe(t.skipped('1'));
    expect(view.actions).toEqual(['result', 'close']);
    expect(view.finished).toBe(true);
    expect(keepsCapsule(done)).toBe(true);
    const clean = job({
      state: 'done',
      result: result({ groups: [{ state: 'saved', code: null, items: [1] }], total: 1 }),
    });
    expect(keepsCapsule(clean)).toBe(false);
    expect(keepsCapsule(job({ state: 'failed' }))).toBe(true);
  });

  it('元に戻す・N → 1 は言い方を変える', () => {
    const undo = job({
      state: 'done',
      result: result({
        action: { type: 'undo', of: 3, saveAs: false, merge: false },
        recipe: null,
        groups: [{ state: 'saved', code: null, items: [1, 2] }],
      }),
    });
    expect(batchProgressView(undo).title).toBe(t.undoDone('2'));
    const merge = job({
      state: 'done',
      result: result({
        action: { type: 'merge', name: '{名前} のシート', sheet: {} },
        recipe: null,
        groups: [{ state: 'existing', code: 'Existing', items: [1, 2] }],
      }),
    });
    expect(batchProgressView(merge).title).toBe(t.mergeExisting);
    expect(batchProgressView({ ...merge, state: 'running' }).title).toBe(t.mergeRunning);
  });

  it('中止したら当て済みの数を添える。結果をまだ読めていなければ名前は「レシピ」', () => {
    const view = batchProgressView(job({ state: 'canceled' }));
    expect(view.title).toBe(t.canceled('ドット絵 4 倍'));
    expect(view.detail).toBe([t.canceledDetail('4'), t.skipped('1'), t.failures('1')].join(' · '));
    expect(batchProgressView(job({ state: 'queued', result: null })).title).toBe(
      t.queued(t.unnamed),
    );
  });

  it('中止した一括も、飛ばした・失敗があれば残し、その数を添える', () => {
    const canceled = job({
      state: 'canceled',
      result: result({
        groups: [
          { state: 'saved', code: null, items: [1] },
          { state: 'failed', code: 'Invalid', items: [2] },
          { state: 'canceled', code: null, items: [3] },
        ],
        total: 3,
      }),
    });
    expect(keepsCapsule(canceled)).toBe(true);
    expect(batchProgressView(canceled).detail).toBe(
      [t.canceledDetail('1'), t.failures('1')].join(' · '),
    );
    const clean = job({
      state: 'canceled',
      result: result({ groups: [{ state: 'canceled', code: null, items: [1] }], total: 1 }),
    });
    expect(keepsCapsule(clean)).toBe(false);
  });

  it('N → 1 が何も作れずに終わったら「作れませんでした」と言い、残す', () => {
    const none = job({
      state: 'done',
      result: result({
        action: { type: 'merge', name: '{名前} のシート', sheet: {} },
        recipe: null,
        groups: [{ state: 'failed', code: 'Invalid', items: [1, 2] }],
      }),
    });
    expect(batchProgressView(none).title).toBe(t.mergeFailed);
    expect(keepsCapsule(none)).toBe(true);
    const made = job({
      state: 'done',
      result: result({
        action: { type: 'merge', name: '{名前} のシート', sheet: {} },
        recipe: null,
        groups: [{ state: 'saved', code: null, items: [1, 2] }],
      }),
    });
    expect(batchProgressView(made).title).toBe(t.mergeDone);
    expect(keepsCapsule(made)).toBe(false);
  });
});
