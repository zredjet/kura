import { describe, expect, it, vi } from 'vitest';

import { watchFrames } from './frameProbe';

/** rVFC を手で回せる偽の動画。 */
function fakeVideo() {
  const video = document.createElement('video');
  let cb: VideoFrameRequestCallback | null = null;
  let next = 1;
  const request = vi.fn((fn: VideoFrameRequestCallback) => {
    cb = fn;
    return next++;
  });
  const cancel = vi.fn(() => {
    cb = null;
  });
  Object.assign(video, { requestVideoFrameCallback: request, cancelVideoFrameCallback: cancel });
  const present = (frames: number, mediaTime: number) => {
    const fn = cb;
    cb = null;
    fn?.(0, {
      presentedFrames: frames,
      mediaTime,
      expectedDisplayTime: 0,
      height: 1,
      width: 1,
      presentationTime: 0,
    });
  };
  return { video, present, request, cancel, waiting: () => cb !== null };
}

describe('watchFrames', () => {
  it('rVFC がなければ何もしない', () => {
    const video = document.createElement('video');
    const watch = watchFrames(video);
    expect(watch.frameMs()).toBeNull();
    watch.stop();
  });

  it('1 つずつ進んだ組の差から推定し、落ちたコマの組は捨てる', () => {
    const v = fakeVideo();
    const watch = watchFrames(v.video);
    let t = 0;
    v.present(1, t);
    for (let i = 2; i <= 7; i++) {
      t += 1 / 24;
      v.present(i, t);
    }
    // 2 コマ飛んだ組（差は 2 コマ分）は数えない
    t += 2 / 24;
    v.present(9, t);
    expect(watch.frameMs()).toBeCloseTo(1000 / 24, 6);
    watch.stop();
    expect(v.cancel).toHaveBeenCalled();
  });

  it('シークで組を切り、loadstart で集め直す', () => {
    const v = fakeVideo();
    const watch = watchFrames(v.video);
    v.present(1, 0);
    v.video.dispatchEvent(new Event('seeking'));
    // シークのあとの最初のコマは前と組にしない（差は 5 秒）
    v.present(2, 5);
    for (let i = 3; i <= 8; i++) {
      v.present(i, 5 + (i - 2) / 30);
    }
    expect(watch.frameMs()).toBeCloseTo(1000 / 30, 6);
    v.video.dispatchEvent(new Event('loadstart'));
    expect(watch.frameMs()).toBeNull();
    watch.stop();
  });

  it('集まったら止め、止めたあとは頼まない', () => {
    const v = fakeVideo();
    const watch = watchFrames(v.video);
    for (let i = 1; i <= 40; i++) {
      v.present(i, i / 25);
    }
    expect(watch.frameMs()).toBeCloseTo(40, 6);
    expect(v.waiting()).toBe(false);
    expect(v.request.mock.calls.length).toBeLessThanOrEqual(31);
    watch.stop();
  });
});
