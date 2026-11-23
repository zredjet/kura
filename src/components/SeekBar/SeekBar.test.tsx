import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { END_GUARD_MS } from '../../lib/seek';
import { SeekBar } from './SeekBar';

const WIDTH = 600;
const LENGTH = 60_000; // 1px = 100ms

/** バーの外側の要素（ポインタを受ける）。 */
function barOf(): HTMLElement {
  const input = screen.getByRole('slider', { name: ja.seek.label });
  const bar = input.parentElement;
  if (!bar) throw new Error('バーがない');
  return bar;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
    left: 100,
    top: 0,
    right: 100 + WIDTH,
    bottom: 28,
    width: WIDTH,
    height: 28,
    x: 100,
    y: 0,
    toJSON: () => ({}),
  });
  Object.assign(HTMLElement.prototype, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function setup(props: Partial<Parameters<typeof SeekBar>[0]> = {}) {
  const onSeek = vi.fn();
  const onScrubStart = vi.fn();
  const onScrubEnd = vi.fn();
  const view = render(
    <SeekBar
      label={ja.seek.label}
      positionMs={12_000}
      durationMs={LENGTH}
      onSeek={onSeek}
      onScrubStart={onScrubStart}
      onScrubEnd={onScrubEnd}
      {...props}
    />,
  );
  return { onSeek, onScrubStart, onScrubEnd, ...view };
}

describe('SeekBar', () => {
  it('スライダーの名前と「今 / 長さ」の読み上げ', () => {
    setup();
    const input = screen.getByRole('slider', { name: ja.seek.label });
    expect(input).toHaveAttribute('aria-valuetext', ja.seek.valueText('0:12', '1:00'));
  });

  it('長さがなければ押せない', () => {
    const { onSeek, onScrubStart } = setup({ durationMs: null });
    expect(screen.getByRole('slider')).toBeDisabled();
    fireEvent.pointerDown(barOf(), { button: 0, pointerId: 1, clientX: 400 });
    expect(onScrubStart).not.toHaveBeenCalled();
    expect(onSeek).not.toHaveBeenCalled();
  });

  it('押すとその位置へ飛び、ドラッグで追い、離すと終わる', () => {
    const { onSeek, onScrubStart, onScrubEnd } = setup();
    const bar = barOf();
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 400 });
    expect(onScrubStart).toHaveBeenCalledTimes(1);
    expect(onSeek).toHaveBeenLastCalledWith(30_000);
    expect(bar).toHaveAttribute('data-scrubbing', 'true');
    fireEvent.pointerMove(bar, { pointerId: 1, clientX: 450 });
    act(() => {
      vi.advanceTimersByTime(20);
    });
    expect(onSeek).toHaveBeenLastCalledWith(35_000);
    fireEvent.pointerMove(bar, { pointerId: 1, clientX: 460 });
    fireEvent.pointerUp(bar, { pointerId: 1, clientX: 460 });
    expect(onSeek).toHaveBeenLastCalledWith(36_000);
    expect(onScrubEnd).toHaveBeenCalledTimes(1);
    expect(bar).not.toHaveAttribute('data-scrubbing');
    // 離したあとの捕まえの外れでは二度終わらない
    fireEvent(bar, new Event('lostpointercapture', { bubbles: true }));
    expect(onScrubEnd).toHaveBeenCalledTimes(1);
  });

  it('⌥ を押しながらは今の位置から 1/10 の速さ', () => {
    const { onSeek } = setup();
    const bar = barOf();
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 400, altKey: true });
    expect(onSeek).toHaveBeenLastCalledWith(12_000);
    fireEvent.pointerMove(bar, { pointerId: 1, clientX: 500, altKey: true });
    fireEvent.pointerUp(bar, { pointerId: 1, clientX: 500 });
    expect(onSeek.mock.lastCall?.[0]).toBeCloseTo(13_000, 6);
  });

  it('終わりへドラッグしても終わりの手前で止める', () => {
    const { onSeek } = setup();
    const bar = barOf();
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 900 });
    fireEvent.pointerUp(bar, { pointerId: 1, clientX: 900 });
    expect(onSeek).toHaveBeenLastCalledWith(LENGTH - END_GUARD_MS);
  });

  it('ホバーでポインタの下の時刻、ドラッグ中は 0.1 秒まで出す', () => {
    const { container } = setup();
    const bar = barOf();
    fireEvent.pointerMove(bar, { pointerId: 1, clientX: 223 });
    expect(container).toHaveTextContent('0:12');
    fireEvent.pointerLeave(bar);
    expect(container.textContent).toBe('');
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 223 });
    expect(container).toHaveTextContent('0:12.3');
  });

  it('右ボタン・ほかのポインタは無視する', () => {
    const { onSeek, onScrubEnd } = setup();
    const bar = barOf();
    fireEvent.pointerDown(bar, { button: 2, pointerId: 1, clientX: 400 });
    expect(onSeek).not.toHaveBeenCalled();
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 400 });
    fireEvent.pointerUp(bar, { pointerId: 2, clientX: 400 });
    expect(onScrubEnd).not.toHaveBeenCalled();
  });

  it('フォーカス中は ← → で ±5 秒、Home・End、PageUp で 30 秒', () => {
    const { onSeek } = setup();
    const input = screen.getByRole('slider');
    fireEvent.keyDown(input, { key: 'ArrowRight' });
    expect(onSeek).toHaveBeenLastCalledWith(17_000);
    fireEvent.keyDown(input, { key: 'ArrowLeft' });
    expect(onSeek).toHaveBeenLastCalledWith(7_000);
    fireEvent.keyDown(input, { key: 'Home' });
    expect(onSeek).toHaveBeenLastCalledWith(0);
    fireEvent.keyDown(input, { key: 'End' });
    expect(onSeek).toHaveBeenLastCalledWith(LENGTH - END_GUARD_MS);
    fireEvent.keyDown(input, { key: 'PageUp' });
    expect(onSeek).toHaveBeenLastCalledWith(42_000);
    onSeek.mockClear();
    fireEvent.keyDown(input, { key: ' ' });
    fireEvent.keyDown(input, { key: 'ArrowRight', metaKey: true });
    expect(onSeek).not.toHaveBeenCalled();
  });

  it('アイテムが替わったら、そのドラッグは無かったことにする', () => {
    const { onSeek, onScrubEnd, rerender } = setup({ resetKey: 1 });
    const bar = barOf();
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 400 });
    onSeek.mockClear();
    rerender(
      <SeekBar
        label={ja.seek.label}
        positionMs={0}
        durationMs={LENGTH}
        onSeek={onSeek}
        onScrubEnd={onScrubEnd}
        resetKey={2}
      />,
    );
    expect(barOf()).not.toHaveAttribute('data-scrubbing');
    fireEvent.pointerMove(barOf(), { pointerId: 1, clientX: 500 });
    fireEvent.pointerUp(barOf(), { pointerId: 1, clientX: 500 });
    act(() => {
      vi.advanceTimersByTime(20);
    });
    expect(onSeek).not.toHaveBeenCalled();
    expect(onScrubEnd).not.toHaveBeenCalled();
  });
});
