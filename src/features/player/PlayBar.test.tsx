import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { Queue } from './engine/types';
import { PlayBar, slotEnd } from './PlayBar';
import { usePlayerStore } from './playerStore';

const controller = vi.hoisted(() => ({
  item: vi.fn(() => undefined),
  seek: vi.fn(),
  beginScrub: vi.fn(),
  endScrub: vi.fn(),
  togglePlay: vi.fn(),
  prev: vi.fn(),
  next: vi.fn(),
  stepImage: vi.fn(),
}));

vi.mock('./playerActions', () => ({
  cycleRepeat: vi.fn(),
  playerController: () => controller,
  toggleShuffle: vi.fn(),
  updatePlayerPrefs: vi.fn(),
}));
vi.mock('../../ipc/items', () => ({ getItemDetail: vi.fn(() => new Promise(() => undefined)) }));

beforeEach(() => {
  Object.values(controller).forEach((fn) => fn.mockClear());
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
    left: 0,
    top: 0,
    right: 500,
    bottom: 28,
    width: 500,
    height: 28,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
  Object.assign(HTMLElement.prototype, { setPointerCapture: vi.fn() });
  usePlayerStore.setState({
    snapshot: {
      ...usePlayerStore.getState().snapshot,
      index: 0,
      itemId: 5,
      kind: 'video',
      positionMs: 20_000,
      durationMs: 100_000,
      playing: true,
    },
    queue: null,
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('再生バーのシーク（ADR-230）', () => {
  it('今の時刻・長さ・シークバーを出す', () => {
    render(<PlayBar />);
    expect(screen.getByRole('button', { name: ja.seek.jump('0:20') })).toHaveTextContent('0:20');
    expect(screen.getByRole('slider', { name: ja.seek.label })).toHaveAttribute(
      'aria-valuetext',
      ja.seek.valueText('0:20', '1:40'),
    );
    expect(screen.getByText('1:40')).toBeInTheDocument();
  });

  it('ドラッグはコントローラのスクラブとシークにつなぐ', () => {
    render(<PlayBar />);
    const bar = screen.getByRole('slider', { name: ja.seek.label }).parentElement as HTMLElement;
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 250 });
    expect(controller.beginScrub).toHaveBeenCalledOnce();
    expect(controller.seek).toHaveBeenLastCalledWith(50_000);
    fireEvent.pointerUp(bar, { pointerId: 1, clientX: 250 });
    expect(controller.endScrub).toHaveBeenCalledOnce();
  });

  it('時刻を打ち込んで飛ぶ', () => {
    render(<PlayBar />);
    fireEvent.click(screen.getByRole('button', { name: ja.seek.jump('0:20') }));
    const input = screen.getByLabelText(ja.seek.field);
    fireEvent.change(input, { target: { value: '1:05.5' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(controller.seek).toHaveBeenLastCalledWith(65_500);
  });

  it('時刻の欄を開いたままバーを押したら、欄を閉じる', () => {
    render(<PlayBar />);
    fireEvent.click(screen.getByRole('button', { name: ja.seek.jump('0:20') }));
    expect(screen.getByLabelText(ja.seek.field)).toBeInTheDocument();
    const bar = screen.getByRole('slider', { name: ja.seek.label }).parentElement as HTMLElement;
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 250 });
    expect(screen.queryByLabelText(ja.seek.field)).toBeNull();
    fireEvent.pointerUp(bar, { pointerId: 1, clientX: 250 });
  });

  it('ドラッグ中にスロットが替わったら、時刻の 0.1 秒の表示を戻す', () => {
    render(<PlayBar />);
    const bar = screen.getByRole('slider', { name: ja.seek.label }).parentElement as HTMLElement;
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 250 });
    expect(screen.getByRole('button', { name: ja.seek.jump('0:20') })).toHaveTextContent('0:20.0');
    act(() => {
      usePlayerStore.setState({
        snapshot: { ...usePlayerStore.getState().snapshot, index: 1, itemId: 6, positionMs: 3000 },
      });
    });
    expect(screen.getByRole('button', { name: ja.seek.jump('0:03') })).toHaveTextContent(/^0:03$/);
  });

  it('トリムの終わりがあれば、シークの範囲と長さの表示はそこまで', () => {
    usePlayerStore.setState({ queue: { endMs: [50_000] } as unknown as Queue });
    render(<PlayBar />);
    const input = screen.getByRole('slider', { name: ja.seek.label });
    expect(input).toHaveAttribute('aria-valuetext', ja.seek.valueText('0:20', '0:50'));
    expect(screen.getByText('0:50')).toBeInTheDocument();
    const bar = input.parentElement as HTMLElement;
    // 右端（500px）を押すと、トリムの終わりの手前
    fireEvent.pointerDown(bar, { button: 0, pointerId: 1, clientX: 500 });
    fireEvent.pointerUp(bar, { pointerId: 1, clientX: 500 });
    expect(controller.seek).toHaveBeenLastCalledWith(49_900);
    // 真ん中は 0:25（長さ全体の 0:50 ではない）
    fireEvent.pointerDown(bar, { button: 0, pointerId: 2, clientX: 250 });
    expect(controller.seek).toHaveBeenLastCalledWith(25_000);
    fireEvent.pointerUp(bar, { pointerId: 2, clientX: 250 });
  });

  it('時刻の欄を開いている間にスロットが替わったら、欄を閉じる', () => {
    render(<PlayBar />);
    fireEvent.click(screen.getByRole('button', { name: ja.seek.jump('0:20') }));
    const input = screen.getByLabelText(ja.seek.field);
    fireEvent.change(input, { target: { value: '0:30' } });
    act(() => {
      usePlayerStore.setState({
        snapshot: { ...usePlayerStore.getState().snapshot, index: 1, itemId: 6, positionMs: 0 },
      });
    });
    expect(screen.queryByLabelText(ja.seek.field)).toBeNull();
    expect(controller.seek).not.toHaveBeenCalled();
  });

  it('今のアイテムがなければ押せない', () => {
    usePlayerStore.setState({
      snapshot: { ...usePlayerStore.getState().snapshot, itemId: null, durationMs: null },
    });
    render(<PlayBar />);
    expect(screen.getByRole('slider', { name: ja.seek.label })).toBeDisabled();
  });
});

describe('slotEnd', () => {
  it('長さとトリムの終わりの早いほう', () => {
    expect(slotEnd(100_000, 50_000)).toBe(50_000);
    expect(slotEnd(40_000, 50_000)).toBe(40_000);
    expect(slotEnd(100_000, null)).toBe(100_000);
    expect(slotEnd(null, 50_000)).toBe(50_000);
    expect(slotEnd(null, null)).toBeNull();
  });
});
