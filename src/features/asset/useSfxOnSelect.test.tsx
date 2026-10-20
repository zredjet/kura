import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ItemSummaryDto } from '../../ipc/items';
import { unitEntry } from '../../ipc/items';
import { getCached } from '../../stores/itemCache';
import { useViewStore } from '../../stores/viewStore';
import { EMPTY_SELECTION } from '../library/selection';
import { DEFAULT_PREFS, usePlayerStore } from '../player/playerStore';
import { type AudioRuntime, setAudioRuntimeForTest } from './audioRuntime';
import { useAuditionStore } from './auditionStore';
import { summary } from './testAssets';
import { useSfxOnSelect } from './useSfxOnSelect';

vi.mock('../../stores/itemCache', () => ({ getCached: vi.fn() }));
vi.mock('../player/playerActions', () => ({ playerController: vi.fn() }));

const items: Record<number, ItemSummaryDto> = {
  1: summary({ id: 1, hash: 'h1', kind: 'audio', durationMs: 800 }),
  2: summary({ id: 2, hash: 'h2', kind: 'audio', durationMs: 10_000 }),
  3: summary({ id: 3, hash: 'h3', kind: 'audio', durationMs: 10_001 }),
  4: summary({ id: 4, hash: 'h4', kind: 'image' }),
  5: summary({ id: 5, hash: 'h5', kind: 'video', durationMs: 800 }),
};

function fakeRuntime() {
  const sfx = { play: vi.fn(async () => 'playing' as const), stop: vi.fn() };
  const audition = { stop: vi.fn(), position: vi.fn(() => null) };
  const pool = { closeAll: vi.fn() };
  const runtime = { sfx, audition, pool } as unknown as AudioRuntime;
  return { runtime, sfx, audition, pool };
}

function select(id: number | null) {
  act(() => useViewStore.getState().select(id));
}

describe('useSfxOnSelect（効果音の即再生。決定 13）', () => {
  let fake: ReturnType<typeof fakeRuntime>;

  beforeEach(() => {
    fake = fakeRuntime();
    setAudioRuntimeForTest(fake.runtime);
    vi.mocked(getCached).mockImplementation((id) => items[id]);
    useViewStore.setState({ selection: EMPTY_SELECTION, quickLook: false });
    usePlayerStore.setState({
      prefs: DEFAULT_PREFS,
      active: false,
      snapshot: { ...usePlayerStore.getState().snapshot, playing: false },
    });
    useAuditionStore.setState({ itemId: null, status: 'idle', mode: null, fromStart: false });
  });

  afterEach(() => setAudioRuntimeForTest(null));

  it('10 秒以下の音声を 1 件だけ選んだら鳴らし、前の音は止める（既定でオン）', () => {
    expect(DEFAULT_PREFS.sfxAudition).toBe(true);
    renderHook(() => useSfxOnSelect());
    select(1);
    expect(fake.sfx.play).toHaveBeenCalledWith('h1');
    select(2);
    expect(fake.sfx.play).toHaveBeenLastCalledWith('h2');
    expect(fake.sfx.stop).toHaveBeenCalledTimes(2);
  });

  it('10 秒を超える音声・画像・動画・ユニットのタイル・選択を外したときは鳴らさない（止めはする）', () => {
    renderHook(() => useSfxOnSelect());
    for (const id of [3, 4, 5, unitEntry(7), null]) {
      select(id);
    }
    expect(fake.sfx.play).not.toHaveBeenCalled();
    expect(fake.sfx.stop).toHaveBeenCalledTimes(5);
  });

  it('設定がオフ・プレーヤーの再生中・クイックルック中・複数選択では鳴らさない', () => {
    renderHook(() => useSfxOnSelect());
    usePlayerStore.setState({ prefs: { ...DEFAULT_PREFS, sfxAudition: false } });
    select(1);
    usePlayerStore.setState({
      prefs: DEFAULT_PREFS,
      active: true,
      snapshot: { ...usePlayerStore.getState().snapshot, playing: true },
    });
    select(2);
    usePlayerStore.setState({
      active: true,
      snapshot: { ...usePlayerStore.getState().snapshot, playing: false },
    });
    act(() => useViewStore.setState({ quickLook: true }));
    select(1);
    act(() => useViewStore.setState({ quickLook: false }));
    act(() => useViewStore.getState().setSelection({ ids: [1, 2], anchor: 1, focus: 2 }));
    expect(fake.sfx.play).not.toHaveBeenCalled();
    // 一時停止中のプレーヤーなら鳴らす
    select(1);
    expect(fake.sfx.play).toHaveBeenCalledWith('h1');
  });

  it('クイックルックを開いたら効果音と試聴を止める（クイックルックは自分で鳴らす）', () => {
    renderHook(() => useSfxOnSelect());
    select(1);
    useAuditionStore.setState({ itemId: 1, status: 'playing', mode: 'loop', fromStart: false });
    const stops = fake.sfx.stop.mock.calls.length;
    act(() => useViewStore.getState().setQuickLook(true));
    expect(fake.sfx.stop).toHaveBeenCalledTimes(stops + 1);
    expect(fake.audition.stop).toHaveBeenCalled();
    expect(useAuditionStore.getState().status).toBe('idle');
  });

  it('選ぶものが変わったら、ほかのアイテムの試聴を止める', () => {
    renderHook(() => useSfxOnSelect());
    select(1);
    useAuditionStore.setState({ itemId: 1, status: 'playing', mode: 'loop', fromStart: false });
    select(1);
    expect(fake.audition.stop).not.toHaveBeenCalled();
    select(2);
    expect(fake.audition.stop).toHaveBeenCalled();
    expect(useAuditionStore.getState().status).toBe('idle');
  });

  it('画面を離れたら音を止めて文脈を閉じ、もう選択を見ない', () => {
    const { unmount } = renderHook(() => useSfxOnSelect());
    unmount();
    expect(fake.pool.closeAll).toHaveBeenCalled();
    select(1);
    expect(fake.sfx.play).not.toHaveBeenCalled();
  });
});
