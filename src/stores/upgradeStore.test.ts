import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { LibraryUpgradeEvent } from '../ipc/library';
import { onLibraryUpgrade } from '../ipc/library';
import {
  receiveUpgrade,
  settleUpgrade,
  UPGRADE_NOTICE_MS,
  useUpgradeStore,
  watchUpgrades,
} from './upgradeStore';

vi.mock('../ipc/library', () => ({
  onLibraryUpgrade: vi.fn(async () => () => {}),
}));

function event(phase: LibraryUpgradeEvent['phase'], path = '/L.kuralib'): LibraryUpgradeEvent {
  return {
    phase,
    path,
    fromVersion: 6,
    toVersion: 9,
    dbBytes: phase === 'started' ? 51_834_880 : null,
    elapsedMs: phase === 'started' ? null : 1961,
    error:
      phase === 'failed' ? { code: 'NoSpace', message: 'ディスクの空き容量が足りないため…' } : null,
  };
}

beforeEach(() => {
  vi.useFakeTimers();
  settleUpgrade();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('ライブラリの更新の知らせ（決定 1）', () => {
  it('始まってから 300ms を超えたら「更新しています」を出し、終わったら消す', () => {
    expect(UPGRADE_NOTICE_MS).toBe(300);
    receiveUpgrade(event('started'));
    expect(useUpgradeStore.getState().running?.path).toBe('/L.kuralib');
    vi.advanceTimersByTime(UPGRADE_NOTICE_MS - 1);
    expect(useUpgradeStore.getState().visible).toBe(false);
    vi.advanceTimersByTime(1);
    expect(useUpgradeStore.getState().visible).toBe(true);
    receiveUpgrade(event('finished'));
    expect(useUpgradeStore.getState()).toMatchObject({ running: null, visible: false });
  });

  it('300ms のうちに終われば一度も出さない', () => {
    receiveUpgrade(event('started'));
    vi.advanceTimersByTime(200);
    receiveUpgrade(event('finished'));
    vi.advanceTimersByTime(1000);
    expect(useUpgradeStore.getState().visible).toBe(false);
  });

  it('失敗の知らせでも消す（理由は開く IPC の戻り値で出す）', () => {
    receiveUpgrade(event('started'));
    vi.advanceTimersByTime(500);
    receiveUpgrade(event('failed'));
    expect(useUpgradeStore.getState()).toMatchObject({ running: null, visible: false });
  });

  it('別のライブラリを当て始めたら数え直す', () => {
    receiveUpgrade(event('started', '/A.kuralib'));
    vi.advanceTimersByTime(250);
    receiveUpgrade(event('finished', '/A.kuralib'));
    receiveUpgrade(event('started', '/B.kuralib'));
    vi.advanceTimersByTime(250);
    expect(useUpgradeStore.getState().visible).toBe(false);
    vi.advanceTimersByTime(50);
    expect(useUpgradeStore.getState()).toMatchObject({
      running: { path: '/B.kuralib' },
      visible: true,
    });
  });

  it('開く IPC が返ったら、知らせが欠けても消す', () => {
    receiveUpgrade(event('started'));
    vi.advanceTimersByTime(400);
    settleUpgrade();
    expect(useUpgradeStore.getState()).toMatchObject({ running: null, visible: false });
    vi.advanceTimersByTime(1000);
    expect(useUpgradeStore.getState().visible).toBe(false);
  });

  it('受け取りは一度だけ始め、始められなくても開くのを止めない', async () => {
    vi.mocked(onLibraryUpgrade).mockRejectedValueOnce(new Error('not in tauri'));
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await expect(watchUpgrades()).resolves.toBeUndefined();
    await watchUpgrades();
    expect(onLibraryUpgrade).toHaveBeenCalledTimes(1);
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });
});
