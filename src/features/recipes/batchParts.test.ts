import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import { boundText, codeLabel, lossText, shapeText, stateLabel } from './batchParts';

const t = ja.batch;

describe('batchParts', () => {
  it('理由の見出しは表の言葉、知らない code は Rust の言葉か code', () => {
    expect(codeLabel('Open')).toBe(t.codes.Open);
    expect(codeLabel('Mystery', 'なにかの理由')).toBe('なにかの理由');
    expect(codeLabel('Mystery')).toBe('Mystery');
    expect(codeLabel(null)).toBe('');
  });

  it('状態の見出しは向きで言い方を変える', () => {
    expect(stateLabel('saved')).toBe(t.states.saved);
    expect(stateLabel('saved', 'undo')).toBe(t.undoStates.saved);
    expect(stateLabel('saved', 'merge')).toBe(t.mergeStates.saved);
    expect(stateLabel('skipped', 'undo')).toBe(t.states.skipped);
  });

  it('形の表記', () => {
    expect(shapeText({ kind: 'image', w: 64, h: 32, frames: null })).toBe(t.shapeImage('64', '32'));
    expect(shapeText({ kind: 'image', w: 64, h: 32, frames: 8 })).toBe(
      t.shapeFrames('64', '32', '8'),
    );
    expect(shapeText({ kind: 'image', w: null, h: null, frames: null })).toBe(t.shapeUnknown);
    expect(shapeText({ kind: 'audio', rate: 48000, channels: 2, frames: null })).toBe(
      t.shapeAudio('48', '2', t.shapeUnknown),
    );
  });

  it('失うものの言い方', () => {
    expect(lossText({ type: 'alpha' }, 'jpeg')).toBe(t.loss.alpha('JPEG'));
    expect(lossText({ type: 'colors', max: 256 }, 'gif')).toBe(t.loss.colors('GIF', '256'));
    expect(lossText({ type: 'settings' }, '')).toBe(t.loss.settings);
  });

  it('読む値の記録を引数の名前と値の行にする（読めなければ空）', () => {
    const json = JSON.stringify([
      {
        node: 3,
        op: 'meta.set_grid',
        v: 1,
        param: 'grid',
        from: { type: 'setting', key: 'grid' },
        read: {},
        value: { cellW: 16, cellH: 24, gapX: 0, gapY: 0, offsetX: 0, offsetY: 0 },
      },
      {
        node: 4,
        op: 'audio.set_loop',
        v: 1,
        param: 'loop',
        part: 'start',
        from: { type: 'time', ms: 1000, anchor: 'start' },
        read: { rate: 44100, frames: 44100 },
        value: 44100,
      },
    ]);
    const lines = boundText(json);
    expect(lines).toHaveLength(2);
    expect(lines[0]).toContain('16');
    expect(lines[0]).toContain('24');
    expect(lines[1]).toContain(ja.saveRecipe.rangeStart);
    expect(lines[1]).toContain('44,100');
    expect(boundText('not json')).toEqual([]);
    expect(boundText('{}')).toEqual([]);
  });
});
