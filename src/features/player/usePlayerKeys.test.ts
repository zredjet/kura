import { beforeEach, describe, expect, it, vi } from 'vitest';

import { toggleFullscreen } from './playerActions';
import { usePlayerStore } from './playerStore';
import { handlePlayerKey } from './usePlayerKeys';

vi.mock('./playerActions', () => ({
  cycleRepeat: vi.fn(),
  playerController: () => ({}),
  toggleFullscreen: vi.fn(),
  toggleShuffle: vi.fn(),
}));

const press = (key: string) =>
  handlePlayerKey(new KeyboardEvent('keydown', { key, cancelable: true }));

describe('handlePlayerKey（フルスクリーン。ADR-128）', () => {
  beforeEach(() => {
    vi.mocked(toggleFullscreen).mockClear();
    usePlayerStore.setState({ mode: 'theater', fullscreen: false });
  });

  it('F でフルスクリーンを切り替える', () => {
    expect(press('f')).toBe(true);
    expect(toggleFullscreen).toHaveBeenCalledOnce();
  });

  it('esc はフルスクリーンならまず抜け、そうでなければライブラリへ戻る', () => {
    usePlayerStore.setState({ fullscreen: true });
    press('Escape');
    expect(toggleFullscreen).toHaveBeenCalledOnce();
    expect(usePlayerStore.getState().mode).toBe('theater');

    usePlayerStore.setState({ fullscreen: false });
    press('Escape');
    expect(usePlayerStore.getState().mode).toBe('mini');
  });
});

describe('setFullscreen', () => {
  it('入ると右のパネルを閉じ、出ると戻す', () => {
    usePlayerStore.setState({
      fullscreen: false,
      panel: 'queue',
      panelBeforeFullscreen: undefined,
    });
    usePlayerStore.getState().setFullscreen(true);
    expect(usePlayerStore.getState().panel).toBeNull();
    usePlayerStore.getState().setFullscreen(false);
    expect(usePlayerStore.getState().panel).toBe('queue');
  });
});
