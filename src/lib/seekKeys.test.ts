import { describe, expect, it, vi } from 'vitest';

import { markReplayed } from './keys';
import { END_GUARD_MS } from './seek';
import { applySeekAction, seekKeyAction, type SeekTarget } from './seekKeys';

function key(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent('keydown', init);
}

describe('seekKeyAction', () => {
  it(', . は 1 コマ、⇧ つき（< >）は 1 秒', () => {
    expect(seekKeyAction(key({ key: ',', code: 'Comma' }))).toEqual({ kind: 'frame', dir: -1 });
    expect(seekKeyAction(key({ key: '.', code: 'Period' }))).toEqual({ kind: 'frame', dir: 1 });
    expect(seekKeyAction(key({ key: '<', code: 'Comma', shiftKey: true }))).toEqual({
      kind: 'by',
      ms: -1000,
    });
    expect(seekKeyAction(key({ key: '>', code: 'Period', shiftKey: true }))).toEqual({
      kind: 'by',
      ms: 1000,
    });
    // code がなくても文字で読む
    expect(seekKeyAction(key({ key: '>' }))).toEqual({ kind: 'by', ms: 1000 });
  });

  it('J・L は 5 秒、K は再生／一時停止', () => {
    expect(seekKeyAction(key({ key: 'j', code: 'KeyJ' }))).toEqual({ kind: 'by', ms: -5000 });
    expect(seekKeyAction(key({ key: 'l', code: 'KeyL' }))).toEqual({ kind: 'by', ms: 5000 });
    expect(seekKeyAction(key({ key: 'K', code: 'KeyK' }))).toEqual({ kind: 'toggle' });
    expect(seekKeyAction(key({ key: 'L', code: 'KeyL', shiftKey: true }))).toBeNull();
  });

  it('修飾キー・メニューが送ったキー・日本語入力・ほかのキーは受けない', () => {
    expect(seekKeyAction(key({ key: ',', code: 'Comma', metaKey: true }))).toBeNull();
    expect(seekKeyAction(key({ key: 'j', ctrlKey: true }))).toBeNull();
    expect(seekKeyAction(key({ key: 'l', altKey: true }))).toBeNull();
    expect(seekKeyAction(key({ key: '.', code: 'Period', isComposing: true }))).toBeNull();
    const replayed = key({ key: 'l', code: 'KeyL' });
    markReplayed(replayed);
    expect(seekKeyAction(replayed)).toBeNull();
    expect(seekKeyAction(key({ key: 'ArrowRight' }))).toBeNull();
    expect(seekKeyAction(key({ key: ' ' }))).toBeNull();
  });
});

function target(over: Partial<SeekTarget> & { pos?: number; end?: number | null } = {}) {
  const t = {
    positionMs: () => over.pos ?? 10_000,
    endMs: () => (over.end === undefined ? 60_000 : over.end),
    seek: vi.fn(),
    pause: vi.fn(),
    togglePlay: vi.fn(),
    frameMs: over.frameMs ?? (() => null),
  };
  return t;
}

describe('applySeekAction', () => {
  it('前後に動かし、0 と終わりの手前で止める', () => {
    let t = target();
    applySeekAction({ kind: 'by', ms: 5000 }, t);
    expect(t.seek).toHaveBeenCalledWith(15_000);
    t = target({ pos: 2_000 });
    applySeekAction({ kind: 'by', ms: -5000 }, t);
    expect(t.seek).toHaveBeenCalledWith(0);
    t = target({ pos: 58_000 });
    applySeekAction({ kind: 'by', ms: 5000 }, t);
    expect(t.seek).toHaveBeenCalledWith(60_000 - END_GUARD_MS);
  });

  it('コマ送りは止めてから、コマが分かればコマの真ん中へ、分からなければ 0.1 秒', () => {
    let t = target({ pos: 1_000, frameMs: () => 40 });
    applySeekAction({ kind: 'frame', dir: 1 }, t);
    expect(t.pause).toHaveBeenCalled();
    expect(t.seek).toHaveBeenCalledWith(1_060);
    t = target({ pos: 1_000 });
    applySeekAction({ kind: 'frame', dir: -1 }, t);
    expect(t.pause).toHaveBeenCalled();
    expect(t.seek).toHaveBeenCalledWith(900);
  });

  it('K は再生／一時停止', () => {
    const t = target();
    applySeekAction({ kind: 'toggle' }, t);
    expect(t.togglePlay).toHaveBeenCalledTimes(1);
    expect(t.seek).not.toHaveBeenCalled();
  });
});
