import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  type AudioRuntime,
  closeAudio,
  IDLE_CLOSE_MS,
  setAudioRuntimeForTest,
  touchAudio,
} from './audioRuntime';

function fakeRuntime() {
  const state = { playing: false };
  const pool = { closeAll: vi.fn() };
  const audition = {
    get playing() {
      return state.playing;
    },
    stop: vi.fn(),
  };
  const sfx = { stop: vi.fn() };
  const runtime = { pool, audition, sfx } as unknown as AudioRuntime;
  return { runtime, pool, audition, sfx, state };
}

// critic 23：試聴の文脈は元のレートごとに 1 つを使い回し、使い終えたら閉じる
describe('使い終えた文脈を閉じる', () => {
  let fake: ReturnType<typeof fakeRuntime>;

  beforeEach(() => {
    vi.useFakeTimers();
    fake = fakeRuntime();
    setAudioRuntimeForTest(fake.runtime);
  });

  afterEach(() => {
    closeAudio();
    setAudioRuntimeForTest(null);
    vi.useRealTimers();
  });

  it('最後に鳴らしてから 15 秒、何も鳴らさなければ閉じる（鳴らすたびに数え直す）', () => {
    touchAudio();
    vi.advanceTimersByTime(IDLE_CLOSE_MS - 1);
    touchAudio();
    vi.advanceTimersByTime(IDLE_CLOSE_MS - 1);
    expect(fake.pool.closeAll).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fake.pool.closeAll).toHaveBeenCalledTimes(1);
  });

  it('試聴している間は閉じず、止めてから 15 秒で閉じる', () => {
    fake.state.playing = true;
    touchAudio();
    vi.advanceTimersByTime(IDLE_CLOSE_MS * 3);
    expect(fake.pool.closeAll).not.toHaveBeenCalled();
    fake.state.playing = false;
    vi.advanceTimersByTime(IDLE_CLOSE_MS);
    expect(fake.pool.closeAll).toHaveBeenCalledTimes(1);
  });

  it('画面を離れたらすぐ止めて閉じ、待っていた閉じる時機は取り消す', () => {
    touchAudio();
    closeAudio();
    expect(fake.audition.stop).toHaveBeenCalled();
    expect(fake.sfx.stop).toHaveBeenCalled();
    expect(fake.pool.closeAll).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(IDLE_CLOSE_MS);
    expect(fake.pool.closeAll).toHaveBeenCalledTimes(1);
  });
});
