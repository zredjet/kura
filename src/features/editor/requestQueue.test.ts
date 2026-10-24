import { describe, expect, it, vi } from 'vitest';

import type { EditStepDto, EditStepResultDto, EditViewportDto } from '../../ipc/edit';
import { RequestQueue, type SentRequest } from './requestQueue';

function vp(x: number): EditViewportDto {
  return { frame: 0, rect: { x, y: 0, w: 10, h: 10 }, scale: 1, interacting: false, focus: null };
}

const RESULT = { status: { rev: 1 } } as unknown as EditStepResultDto;

/** 応えを手で返せる偽の IPC。 */
function fakeIpc() {
  const calls: {
    kind: 'step' | 'view';
    steps: EditStepDto[];
    viewport: EditViewportDto;
    resolve: (r: EditStepResultDto) => void;
    reject: (e: unknown) => void;
  }[] = [];
  const ipc = {
    step: vi.fn(
      (steps: EditStepDto[], viewport: EditViewportDto) =>
        new Promise<EditStepResultDto>((resolve, reject) =>
          calls.push({ kind: 'step', steps, viewport, resolve, reject }),
        ),
    ),
    view: vi.fn(
      (viewport: EditViewportDto) =>
        new Promise<EditStepResultDto>((resolve, reject) =>
          calls.push({ kind: 'view', steps: [], viewport, resolve, reject }),
        ),
    ),
  };
  return { ipc, calls };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe('要求の振り分け', () => {
  it('要求は 1 つだけ出し、待っている間の手はまとめて、ビューポートは最新だけを送る', async () => {
    const { ipc, calls } = fakeIpc();
    const done: SentRequest<string>[] = [];
    const q = new RequestQueue<string>(ipc, {
      done: (req) => done.push(req),
      error: () => undefined,
    });
    q.push([{ step: { type: 'undo' } }], vp(1));
    q.push([{ step: { type: 'redo' }, tag: 'b' }], vp(2));
    q.view(vp(3));
    q.push([{ step: { type: 'seal' } }], vp(4));
    expect(calls).toHaveLength(1);
    expect(calls[0]?.steps).toEqual([{ type: 'undo' }]);
    expect(q.busy).toBe(true);
    expect(q.waiting).toBe(2);

    calls[0]?.resolve(RESULT);
    await flush();
    expect(calls).toHaveLength(2);
    expect(calls[1]).toMatchObject({
      kind: 'step',
      steps: [{ type: 'redo' }, { type: 'seal' }],
      viewport: vp(4),
    });
    expect(done.map((r) => r.seq)).toEqual([1]);
    calls[1]?.resolve(RESULT);
    await flush();
    expect(done.map((r) => [r.seq, r.steps.map((s) => s.tag)])).toEqual([
      [1, [undefined]],
      [2, ['b', undefined]],
    ]);
    expect(q.busy).toBe(false);
  });

  it('手のない表示の要求は edit_view。前に送ったビューポートと同じなら送らない（force で送る）', async () => {
    const { ipc, calls } = fakeIpc();
    const q = new RequestQueue(ipc, { done: () => undefined, error: () => undefined });
    q.assumeSent(vp(1));
    q.view(vp(1));
    expect(calls).toHaveLength(0);
    q.view(vp(2));
    expect(calls[0]?.kind).toBe('view');
    calls[0]?.resolve(RESULT);
    await flush();
    q.view(vp(2));
    expect(calls).toHaveLength(1);
    q.view(vp(2), true);
    expect(calls).toHaveLength(2);
  });

  it('送るときに通し番号と手を知らせる（仮の筆に番号を付ける）', () => {
    const { ipc } = fakeIpc();
    const sent = vi.fn();
    const q = new RequestQueue<number>(ipc, {
      sent,
      done: () => undefined,
      error: () => undefined,
    });
    q.push([{ step: { type: 'undo' }, tag: 7 }], vp(1));
    expect(sent).toHaveBeenCalledWith(
      expect.objectContaining({ seq: 1, steps: [{ step: { type: 'undo' }, tag: 7 }] }),
    );
    expect(q.lastSeq).toBe(1);
  });

  it('失敗は error に渡し、次の要求へ進む', async () => {
    const { ipc, calls } = fakeIpc();
    const error = vi.fn();
    const done = vi.fn();
    const q = new RequestQueue(ipc, { done, error });
    q.push([{ step: { type: 'undo' } }], vp(1));
    q.push([{ step: { type: 'redo' } }], vp(1));
    calls[0]?.reject(new Error('だめ'));
    await flush();
    expect(error).toHaveBeenCalledWith(expect.objectContaining({ seq: 1 }), expect.any(Error));
    expect(calls).toHaveLength(2);
  });

  it('閉じたら、ためた手を捨て、出している要求の応えも捨てる', async () => {
    const { ipc, calls } = fakeIpc();
    const done = vi.fn();
    const q = new RequestQueue(ipc, { done, error: () => undefined });
    q.push([{ step: { type: 'undo' } }], vp(1));
    q.push([{ step: { type: 'redo' } }], vp(1));
    q.close();
    calls[0]?.resolve(RESULT);
    await flush();
    expect(done).not.toHaveBeenCalled();
    expect(calls).toHaveLength(1);
    q.push([{ step: { type: 'undo' } }], vp(1));
    expect(calls).toHaveLength(1);
  });

  it('手をためた番号を要求に付け、応えの来ていない手があるかと、なくなるのを待つ口を持つ', async () => {
    const { ipc, calls } = fakeIpc();
    const done: SentRequest<string>[] = [];
    const q = new RequestQueue<string>(ipc, {
      done: (req) => done.push(req),
      error: () => undefined,
    });
    expect(q.holds()).toBe(false);
    await q.settled();
    q.push([{ step: { type: 'undo' } }], vp(1));
    q.push([{ step: { type: 'seal' } }], vp(1));
    q.push([{ step: { type: 'redo' } }], vp(1));
    expect(q.pushedCount).toBe(3);
    const isUndo = (s: EditStepDto) => s.type === 'undo';
    expect(q.holds(isUndo)).toBe(true);
    let settled = false;
    void q.settled().then(() => {
      settled = true;
    });
    calls[0]?.resolve(RESULT);
    await flush();
    expect(done[0]?.upto).toBe(1);
    // 送った undo の応えは来たので、残りは seal と redo
    expect(q.holds(isUndo)).toBe(false);
    expect(q.holds()).toBe(true);
    expect(settled).toBe(false);
    calls[1]?.resolve(RESULT);
    await flush();
    expect(done[1]?.upto).toBe(3);
    expect(settled).toBe(true);
    // 表示だけの要求は前の番号のまま
    q.view(vp(9));
    calls[2]?.resolve(RESULT);
    await flush();
    expect(done[2]?.upto).toBe(3);
  });

  it('閉じたら待っている呼び手を起こす', async () => {
    const { ipc } = fakeIpc();
    const q = new RequestQueue(ipc, { done: () => undefined, error: () => undefined });
    q.push([{ step: { type: 'undo' } }], vp(1));
    let settled = false;
    void q.settled().then(() => {
      settled = true;
    });
    q.close();
    await flush();
    expect(settled).toBe(true);
  });
});
