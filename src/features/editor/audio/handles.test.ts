import { describe, expect, it } from 'vitest';

import type { EditNodeDto } from '../../../ipc/edit';
import type { EditorDoc } from '../editorStore';
import {
  dragSteps,
  fadeInHandle,
  fadeOutHandle,
  handleStep,
  inOut,
  loopHandle,
  targetNode,
  trimHandle,
} from './handles';
import { beginTrim, clampTrim, confirmTrimSteps } from './trim';

function doc(nodes: Partial<EditNodeDto>[]): EditorDoc {
  const all = nodes.map((n, i) => ({
    id: n.id ?? i + 1,
    op: n.op ?? 'audio.gain',
    v: 1,
    enabled: n.enabled ?? true,
    params: n.params ?? {},
    log: null,
  }));
  return {
    order: all.map((n) => n.id),
    nodes: Object.fromEntries(all.map((n) => [n.id, n])),
    outputs: [],
  };
}

describe('ハンドルの手', () => {
  it('表示している出力のフレームを整数にして送る', () => {
    expect(handleStep(trimHandle({ start: 10.4, end: 99.6 }), 3, 'step')).toEqual({
      type: 'handle',
      handle: { type: 'trim', start: 10, end: 100 },
      node: 3,
      mode: 'step',
    });
    expect(fadeInHandle(-5)).toEqual({ type: 'fadeIn', end: 0 });
    expect(fadeOutHandle(42)).toEqual({ type: 'fadeOut', start: 42 });
    expect(loopHandle(null)).toEqual({ type: 'loop', range: null });
    expect(loopHandle({ start: 1, end: 2 })).toEqual({ type: 'loop', range: { start: 1, end: 2 } });
  });

  it('ドラッグ：途中は preview、離したら preview と commitPreview、やめたら cancelPreview', () => {
    const h = fadeInHandle(10);
    expect(dragSteps(h, null, 'move')).toEqual([
      { type: 'handle', handle: h, node: null, mode: 'preview' },
    ]);
    expect(dragSteps(h, 4, 'end')).toEqual([
      { type: 'handle', handle: h, node: 4, mode: 'preview' },
      { type: 'commitPreview', mode: 'step' },
    ]);
    expect(dragSteps(h, 4, 'cancel')).toEqual([{ type: 'cancelPreview' }]);
  });

  it('対象のノード：選んだ操作がその種類ならそれ、なければ最後のその種類、なければ null', () => {
    const d = doc([
      { id: 1, op: 'audio.fade' },
      { id: 2, op: 'audio.gain' },
      { id: 3, op: 'audio.fade', enabled: false },
    ]);
    expect(targetNode(d, 1, 'audio.fade')).toBe(1);
    expect(targetNode(d, 2, 'audio.fade')).toBe(3);
    expect(targetNode(d, null, 'audio.trim')).toBeNull();
  });
});

describe('I／O', () => {
  const now = {
    total: 1000,
    selection: { start: 100, end: 300 },
    trim: { start: 50, end: 950 },
    loop: { start: 200, end: 400 },
  };

  it('道具ごとの始め・終わりを位置にする（越えたらもう一方を端に寄せる）', () => {
    expect(inOut('select', 'in', 150, now)).toEqual({
      kind: 'selection',
      span: { start: 150, end: 300 },
    });
    expect(inOut('select', 'out', 500, now)).toEqual({
      kind: 'selection',
      span: { start: 100, end: 500 },
    });
    expect(inOut('select', 'in', 500, now)).toEqual({
      kind: 'selection',
      span: { start: 500, end: 1000 },
    });
    expect(inOut('select', 'out', 50, now)).toEqual({
      kind: 'selection',
      span: { start: 0, end: 50 },
    });
    expect(inOut('trim', 'in', 60, now)).toEqual({ kind: 'trim', span: { start: 60, end: 950 } });
    expect(inOut('fade', 'in', 70, now)).toEqual({ kind: 'fadeIn', end: 70 });
    expect(inOut('fade', 'out', 900, now)).toEqual({ kind: 'fadeOut', start: 900 });
    expect(inOut('loop', 'out', 350, now)).toEqual({
      kind: 'loop',
      span: { start: 200, end: 350 },
    });
  });

  it('選択がなければ全体から、切り詰めの枠がなければ何もしない', () => {
    const none = { ...now, selection: null, trim: null };
    expect(inOut('select', 'in', 10, none)).toEqual({
      kind: 'selection',
      span: { start: 10, end: 1000 },
    });
    expect(inOut('trim', 'in', 10, none)).toBeNull();
    expect(inOut('select', 'out', 0, none)).toBeNull();
  });
});

describe('切り詰めの道具の決め方', () => {
  it('切り詰めがなければ、全体の枠で末尾に足す', () => {
    const b = beginTrim(doc([{ id: 1, op: 'audio.gain' }]), null, [], 1000);
    expect(b).toEqual({
      kind: 'frame',
      trim: { node: null, start: 0, end: 1000, bounds: 1000, previewed: false },
      preview: null,
    });
  });

  it('有効な切り詰めは仮に無効にして入力を表示し、印の位置に枠（長さは入力が届いてから）', () => {
    const d = doc([{ id: 5, op: 'audio.trim' }]);
    const nodes = [
      {
        id: 5,
        op: 'audio.trim',
        enabled: true,
        inRate: 48_000,
        inFrames: 2000,
        marks: { type: 'trim' as const, start: 100, end: 900 },
      },
    ];
    expect(beginTrim(d, null, nodes, 800)).toEqual({
      kind: 'frame',
      trim: { node: 5, start: 100, end: 900, bounds: null, previewed: true },
      preview: { type: 'preview', edit: { type: 'setEnabled', node: 5, enabled: false } },
    });
    // 外した切り詰めは、今の表示がもう入力
    const off = doc([{ id: 5, op: 'audio.trim', enabled: false }]);
    expect(beginTrim(off, null, nodes, 2000)).toMatchObject({
      trim: { bounds: 2000, previewed: false },
      preview: null,
    });
  });

  it('印が null（後ろの操作で写せない）なら引数パネルで直す', () => {
    const d = doc([{ id: 5, op: 'audio.trim' }]);
    const nodes = [
      { id: 5, op: 'audio.trim', enabled: true, inRate: 48_000, inFrames: 2000, marks: null },
    ];
    expect(beginTrim(d, 5, nodes, 800)).toEqual({ kind: 'params', node: 5 });
  });

  it('枠を収め、↵ で送る手（全体の新しい枠は何も足さない）', () => {
    expect(clampTrim(-5, 5000, 1000)).toEqual({ start: 0, end: 1000 });
    expect(clampTrim(500, 400, 1000)).toEqual({ start: 500, end: 501 });
    expect(
      confirmTrimSteps({ node: null, start: 0, end: 1000, bounds: 1000, previewed: false }),
    ).toEqual({
      steps: [],
      create: false,
    });
    expect(
      confirmTrimSteps({ node: null, start: 10, end: 900, bounds: 1000, previewed: false }),
    ).toEqual({
      steps: [
        { type: 'handle', handle: { type: 'trim', start: 10, end: 900 }, node: null, mode: 'step' },
      ],
      create: true,
    });
    expect(
      confirmTrimSteps({ node: 5, start: 10, end: 900, bounds: null, previewed: true }).steps,
    ).toEqual([]);
  });
});
