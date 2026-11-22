import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { getAnim, getAsset, requestAnalysis } from '../../ipc/asset';
import type { AnimDto } from '../../ipc/asset';
import { onJobProgress, type JobProgress } from '../../ipc/jobs';
import { IpcError } from '../../ipc/result';
import { bumpRevision } from '../../stores/revisionStore';
import { recentAsset } from './auditionStore';
import { audioAsset, imageAsset } from './testAssets';
import { useAssetData } from './useAssetData';

vi.mock('../../ipc/asset', () => ({
  getAsset: vi.fn(),
  getAnim: vi.fn(),
  requestAnalysis: vi.fn(async () => true),
}));
vi.mock('../../ipc/jobs', () => ({ onJobProgress: vi.fn() }));
vi.mock('../player/playerActions', () => ({ playerController: vi.fn() }));

const ANIM = { hash: 'ab', rev: 0, frameCount: 8 } as AnimDto;

describe('useAssetData（素材の欄とアニメの帯を読む）', () => {
  let progress: ((e: JobProgress) => void) | null;
  const unlisten = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    vi.useRealTimers();
    progress = null;
    vi.mocked(onJobProgress).mockImplementation(async (handler) => {
      progress = handler;
      return unlisten;
    });
  });

  afterEach(() => vi.useRealTimers());

  it('素材の欄を読み、覚えておく（L のキーがすぐ使う）。アニメでなければ帯は読まない', async () => {
    vi.mocked(getAsset).mockResolvedValue(audioAsset());
    const { result } = renderHook(() => useAssetData(2));
    await waitFor(() => expect(result.current.asset?.itemId).toBe(2));
    expect(result.current.anim).toEqual({ status: 'none' });
    expect(getAnim).not.toHaveBeenCalled();
    expect(recentAsset(2)?.hash).toBe('cd'.repeat(32));
    expect(requestAnalysis).not.toHaveBeenCalled();
  });

  it('アニメなら帯を読む。データが変わっても格子が同じなら作り直さない', async () => {
    vi.mocked(getAsset).mockResolvedValue(imageAsset());
    vi.mocked(getAnim).mockResolvedValue(ANIM);
    const { result } = renderHook(() => useAssetData(1));
    await waitFor(() => expect(result.current.anim).toEqual({ status: 'ready', anim: ANIM }));
    act(() => bumpRevision());
    await waitFor(() => expect(getAsset).toHaveBeenCalledTimes(2));
    expect(getAnim).toHaveBeenCalledTimes(1);
    // 格子を変えたら作り直す
    vi.mocked(getAsset).mockResolvedValue(
      imageAsset({
        settings: {
          ...imageAsset().settings,
          grid: { cellW: 8, cellH: 8, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0, count: null },
        },
      }),
    );
    act(() => result.current.reload());
    await waitFor(() => expect(getAnim).toHaveBeenCalledTimes(2));
  });

  it('大きすぎる帯は tooLarge、混んでいれば待って 2 回までやり直す', async () => {
    vi.mocked(getAsset).mockResolvedValue(imageAsset());
    vi.mocked(getAnim).mockRejectedValue(new IpcError({ code: 'TooLarge', message: '大きい' }));
    const big = renderHook(() => useAssetData(1));
    await waitFor(() => expect(big.result.current.anim).toEqual({ status: 'tooLarge' }));
    big.unmount();

    vi.mocked(getAnim).mockReset();
    vi.mocked(getAnim)
      .mockRejectedValueOnce(new IpcError({ code: 'Busy', message: '混んでいる' }))
      .mockResolvedValueOnce(ANIM);
    const busy = renderHook(() => useAssetData(1));
    await waitFor(() => expect(busy.result.current.anim).toEqual({ status: 'ready', anim: ANIM }), {
      timeout: 3000,
    });
    expect(getAnim).toHaveBeenCalledTimes(2);
  });

  it('解析を待っているものは先に解析してもらい、解析のジョブが進んだら読み直す', async () => {
    vi.mocked(getAsset).mockResolvedValueOnce(
      audioAsset({ analysisState: 'pending', facts: null }),
    );
    vi.mocked(getAsset).mockResolvedValue(audioAsset());
    const { result } = renderHook(() => useAssetData(2));
    await waitFor(() => expect(result.current.asset?.analysisState).toBe('pending'));
    expect(requestAnalysis).toHaveBeenCalledWith([2]);
    await waitFor(() => expect(progress).not.toBeNull());
    // ほかのジョブでは読み直さない
    act(() => progress?.({ id: 1, kind: 'thumbnail', state: 'running' } as JobProgress));
    act(() => progress?.({ id: 2, kind: 'analyze', state: 'running' } as JobProgress));
    await waitFor(() => expect(result.current.asset?.analysisState).toBe('ready'), {
      timeout: 2000,
    });
    expect(getAsset).toHaveBeenCalledTimes(2);
    // 解析が済んだら受け取りをやめる
    await waitFor(() => expect(unlisten).toHaveBeenCalled());
  });
});
