import { describe, expect, it } from 'vitest';

import type { OutputSpec } from '../../ipc/edit';
import { moveSteps, outputSteps, paramSteps, sameChoice } from './paramSend';

describe('引数の送り方の振り分け', () => {
  it('ドラッグ中は preview、離したら commitPreview（1 手）、esc で cancelPreview', () => {
    expect(paramSteps(3, 'colors', { kind: 'drag', value: 16 })).toEqual([
      { type: 'preview', edit: { type: 'setParam', node: 3, key: 'colors', value: 16 } },
    ]);
    expect(paramSteps(3, 'colors', { kind: 'release' })).toEqual([
      { type: 'commitPreview', mode: 'step' },
    ]);
    expect(paramSteps(3, 'colors', { kind: 'cancel' })).toEqual([{ type: 'cancelPreview' }]);
  });

  it('矢印と数の入力は merge、Enter・離れたら seal、スイッチ・選択は 1 手', () => {
    expect(paramSteps(3, 'colors', { kind: 'nudge', value: 17 })).toEqual([
      {
        type: 'commit',
        edit: { type: 'setParam', node: 3, key: 'colors', value: 17 },
        mode: 'merge',
      },
    ]);
    expect(paramSteps(3, 'colors', { kind: 'seal' })).toEqual([{ type: 'seal' }]);
    expect(paramSteps(4, 'axis', { kind: 'set', value: 'vertical' })).toEqual([
      {
        type: 'commit',
        edit: { type: 'setParam', node: 4, key: 'axis', value: 'vertical' },
        mode: 'step',
      },
    ]);
  });
});

describe('操作の並びの手', () => {
  it('並べ替えは範囲の中だけ', () => {
    expect(moveSteps([1, 2, 3], 2, -1)).toEqual([
      { type: 'commit', edit: { type: 'moveNode', node: 2, to: 0 }, mode: 'step' },
    ]);
    expect(moveSteps([1, 2, 3], 1, -1)).toEqual([]);
    expect(moveSteps([1, 2, 3], 3, 1)).toEqual([]);
    expect(moveSteps([1, 2, 3], 9, 1)).toEqual([]);
  });

  it('出力の形式は、つなぐ先・展開を変えずに形式だけ替える', () => {
    const outputs = [
      { id: 0, from: { from: 'node', id: 3 }, format: { type: 'keep' }, expand: { type: 'one' } },
    ] as unknown as OutputSpec[];
    expect(outputSteps(outputs, { type: 'to', format: 'webp' })).toEqual([
      {
        type: 'commit',
        edit: {
          type: 'setOutput',
          output: 0,
          format: { type: 'to', format: 'webp' },
          quantize: null,
          expand: { type: 'one' },
        },
        mode: 'step',
      },
    ]);
    expect(outputSteps([], { type: 'keep' })).toEqual([]);
    expect(sameChoice({ type: 'keep' }, { type: 'keep' })).toBe(true);
    expect(sameChoice({ type: 'to', format: 'png' }, { type: 'to', format: 'png' })).toBe(true);
    expect(sameChoice({ type: 'to', format: 'png' }, { type: 'keep' })).toBe(false);
  });
});
