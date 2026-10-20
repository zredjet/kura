import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { useViewStore } from '../../stores/viewStore';
import { DEFAULT_PREFS, usePlayerStore } from '../player/playerStore';
import { AssetSection } from './AssetSection';
import {
  type AudioRuntime,
  closeAudio,
  IDLE_CLOSE_MS,
  setAudioRuntimeForTest,
} from './audioRuntime';
import { useAuditionStore } from './auditionStore';
import { audioAsset, summary } from './testAssets';

vi.mock('../../ipc/asset', () => ({
  setAssetSettings: vi.fn(async () => 1),
  retryAnalysis: vi.fn(async () => 1),
  getAsset: vi.fn(),
}));
vi.mock('../player/playerActions', () => ({
  playerController: vi.fn(() => ({ pause: vi.fn() })),
}));
vi.mock('./WaveformView', () => ({ WaveformView: () => <div /> }));

/** 偽の音の口（試聴は止めるまで鳴っている）。 */
function fakeRuntime() {
  const state = { playing: false };
  const audition = {
    startProbed: vi.fn(async () => {
      state.playing = true;
      return {
        status: 'playing' as const,
        plan: { mode: 'loop' as const, intro: null, body: [0, 10] as [number, number], offset: 0 },
        facts: { frames: 10, rate: 44_100, channels: 2, loop: null },
      };
    }),
    stop: vi.fn(() => {
      state.playing = false;
    }),
    position: vi.fn(() => null),
    get playing() {
      return state.playing;
    },
  };
  const pool = { closeAll: vi.fn() };
  const runtime = { audition, sfx: { play: vi.fn(), stop: vi.fn() }, pool };
  return { runtime: runtime as unknown as AudioRuntime, audition, pool };
}

// 素材の欄はシアターの情報のパネルにも出る（プレイリストの画面から再生したときなど、ライブラリの画面がないところ）
describe('素材の欄で始めた試聴の持ち主（本物の試聴の状態）', () => {
  let fake: ReturnType<typeof fakeRuntime>;

  beforeEach(() => {
    vi.useFakeTimers();
    fake = fakeRuntime();
    setAudioRuntimeForTest(fake.runtime);
    usePlayerStore.setState({ active: false, prefs: DEFAULT_PREFS });
    useViewStore.setState({ quickLook: false });
    useAuditionStore.setState({ itemId: null, status: 'idle', mode: null, fromStart: false });
  });

  afterEach(() => {
    closeAudio();
    setAudioRuntimeForTest(null);
    vi.useRealTimers();
  });

  it('欄が消えたら試聴を止め、15 秒たてば文脈を閉じる', async () => {
    const asset = audioAsset();
    const view = render(
      <AssetSection
        data={{ asset, anim: { status: 'none' }, reload: vi.fn() }}
        item={summary({ id: asset.itemId, kind: 'audio', hash: asset.hash, durationMs: 90_000 })}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: ja.asset.audition }));
    await act(async () => {
      await Promise.resolve();
    });
    expect(useAuditionStore.getState().status).toBe('playing');
    view.unmount();
    expect(fake.audition.stop).toHaveBeenCalled();
    expect(useAuditionStore.getState().status).toBe('idle');
    expect(fake.pool.closeAll).not.toHaveBeenCalled();
    vi.advanceTimersByTime(IDLE_CLOSE_MS);
    expect(fake.pool.closeAll).toHaveBeenCalledTimes(1);
  });
});
