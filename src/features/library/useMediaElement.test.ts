import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { readMediaState, sameMediaState, useMediaElement } from './useMediaElement';

describe('readMediaState', () => {
  it('要素から位置・長さ・音量を読む（長さが分からなければ null）', () => {
    expect(
      readMediaState({ paused: false, ended: false, currentTime: 1.5, duration: 10, volume: 0.5 }),
    ).toEqual({ paused: false, ended: false, currentMs: 1500, durationMs: 10_000, volume: 0.5 });
    expect(
      readMediaState({
        paused: true,
        ended: false,
        currentTime: Number.NaN,
        duration: Number.NaN,
        volume: 1,
      }),
    ).toMatchObject({ currentMs: 0, durationMs: null });
    expect(
      readMediaState({
        paused: true,
        ended: false,
        currentTime: 0,
        duration: Number.POSITIVE_INFINITY,
        volume: 1,
      }).durationMs,
    ).toBeNull();
  });

  it('同じ状態か', () => {
    const a = readMediaState({
      paused: true,
      ended: false,
      currentTime: 1,
      duration: 2,
      volume: 1,
    });
    expect(sameMediaState(a, { ...a })).toBe(true);
    expect(sameMediaState(a, { ...a, currentMs: 1001 })).toBe(false);
  });
});

describe('useMediaElement', () => {
  it('イベントで新しい状態になり、変わらなければ同じオブジェクト', () => {
    const el = document.createElement('audio');
    Object.defineProperty(el, 'duration', { configurable: true, value: 30 });
    const { result } = renderHook(() => useMediaElement(el));
    expect(result.current.durationMs).toBe(30_000);
    const first = result.current;
    act(() => {
      el.dispatchEvent(new Event('timeupdate'));
    });
    expect(result.current).toBe(first);
    act(() => {
      el.currentTime = 12;
      el.dispatchEvent(new Event('seeking'));
    });
    expect(result.current.currentMs).toBe(12_000);
    expect(result.current).not.toBe(first);
  });

  it('要素がなければ止まった空の状態', () => {
    const { result } = renderHook(() => useMediaElement(null));
    expect(result.current).toMatchObject({ paused: true, currentMs: 0, durationMs: null });
  });
});
