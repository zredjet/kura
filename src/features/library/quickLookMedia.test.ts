import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  beginMediaScrub,
  endMediaScrub,
  quickLookSeekTarget,
  registerQuickLookMedia,
} from './quickLookMedia';

/** 止まっている・鳴っているを手で決められる要素。 */
function media(over: { paused: boolean; autoplay?: boolean; played?: number }) {
  const el = document.createElement('audio');
  el.autoplay = over.autoplay ?? false;
  Object.defineProperty(el, 'paused', { configurable: true, get: () => over.paused });
  Object.defineProperty(el, 'played', {
    configurable: true,
    value: { length: over.played ?? 0 },
  });
  const pause = vi.fn();
  const play = vi.fn(() => Promise.resolve());
  Object.assign(el, { pause, play });
  return { el, pause, play };
}

afterEach(() => vi.restoreAllMocks());

describe('ドラッグの止め方（ADR-230）', () => {
  it('鳴っていれば止めて、離したら鳴らす', () => {
    const m = media({ paused: false, played: 1 });
    beginMediaScrub(m.el);
    expect(m.pause).toHaveBeenCalledOnce();
    endMediaScrub(m.el);
    expect(m.play).toHaveBeenCalledOnce();
  });

  it('止まっていれば pause を呼ばず、離しても鳴らさない', () => {
    const m = media({ paused: true, played: 1 });
    beginMediaScrub(m.el);
    endMediaScrub(m.el);
    expect(m.pause).not.toHaveBeenCalled();
    expect(m.play).not.toHaveBeenCalled();
  });

  it('自動再生でまだ鳴っていなければ、pause を呼ばずに離したら鳴らす', () => {
    const m = media({ paused: true, autoplay: true, played: 0 });
    beginMediaScrub(m.el);
    expect(m.pause).not.toHaveBeenCalled();
    endMediaScrub(m.el);
    expect(m.play).toHaveBeenCalledOnce();
  });
});

describe('シークのキーの当て先', () => {
  it('登録した間だけ取り出せ、外すと消える（あとから登録したものは消さない）', () => {
    const a = {
      positionMs: () => 0,
      endMs: () => null,
      seek: vi.fn(),
      pause: vi.fn(),
      togglePlay: vi.fn(),
      frameMs: () => null,
    };
    const b = { ...a };
    const offA = registerQuickLookMedia(a);
    const offB = registerQuickLookMedia(b);
    offA();
    expect(quickLookSeekTarget()).toBe(b);
    offB();
    expect(quickLookSeekTarget()).toBeNull();
  });
});
