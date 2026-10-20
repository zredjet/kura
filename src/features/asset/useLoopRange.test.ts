import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { getAsset } from '../../ipc/asset';
import { AUDIO_FACTS, audioAsset } from './testAssets';
import { loopOf, useLoopRange } from './useLoopRange';

vi.mock('../../ipc/asset', () => ({ getAsset: vi.fn() }));

beforeEach(() => {
  vi.mocked(getAsset).mockReset();
});

describe('loopOf', () => {
  it('解析の事実のループ点を [始まり, 終わり) のフレームにする。なければ null', () => {
    const asset = audioAsset();
    expect(loopOf(asset)).toEqual([185_220, 4_057_200]);
    const facts = asset.facts && { ...asset.facts, audio: { ...AUDIO_FACTS, loopRange: null } };
    expect(loopOf({ ...asset, facts })).toBeNull();
    expect(loopOf(audioAsset({ facts: null }))).toBeNull();
    expect(loopOf(null)).toBeNull();
  });
});

describe('useLoopRange（シアターの波形）', () => {
  it('概要がループありのときだけ素材の欄を読み、ループの区間を返す', async () => {
    vi.mocked(getAsset).mockResolvedValue(audioAsset());
    const { result } = renderHook(() => useLoopRange(2, true));
    expect(result.current).toBeNull();
    await waitFor(() => expect(result.current).toEqual([185_220, 4_057_200]));
    expect(getAsset).toHaveBeenCalledWith(2);
  });

  it('ループのないもの・アイテムがないときは読まない', () => {
    renderHook(() => useLoopRange(2, false));
    renderHook(() => useLoopRange(null, true));
    expect(getAsset).not.toHaveBeenCalled();
  });

  it('アイテムが替わったら、前のアイテムの区間を出さない', async () => {
    vi.mocked(getAsset).mockImplementation((id) =>
      id === 2 ? Promise.resolve(audioAsset()) : new Promise(() => {}),
    );
    const { result, rerender } = renderHook(({ id }) => useLoopRange(id, true), {
      initialProps: { id: 2 },
    });
    await waitFor(() => expect(result.current).not.toBeNull());
    rerender({ id: 3 });
    expect(result.current).toBeNull();
  });
});
