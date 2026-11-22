import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { getAsset } from '../../ipc/asset';
import { AUDIO_FACTS, audioAsset, withAuditionLoop } from './testAssets';
import { fileLoopOf, loopOf, settingLoopOf, useLoopRange } from './useLoopRange';

vi.mock('../../ipc/asset', () => ({ getAsset: vi.fn() }));

beforeEach(() => {
  vi.mocked(getAsset).mockReset();
});

describe('loopOf', () => {
  it('解析の事実のループ点を [始まり, 終わり) のフレームにする。なければ null', () => {
    const asset = audioAsset();
    expect(loopOf(asset)).toEqual([185_220, 4_057_200]);
    expect(fileLoopOf(asset)).toEqual([185_220, 4_057_200]);
    const facts = asset.facts && { ...asset.facts, audio: { ...AUDIO_FACTS, loopRange: null } };
    expect(loopOf({ ...asset, facts })).toBeNull();
    expect(loopOf(audioAsset({ facts: null }))).toBeNull();
    expect(loopOf(null)).toBeNull();
  });

  it('試聴の設定があれば先に使う（設定 ?? ファイル）。長さ（解析の読み取り）を超えればファイルのループ点', () => {
    const asset = withAuditionLoop(audioAsset(), 44_100, 88_200);
    expect(settingLoopOf(asset)).toEqual([44_100, 88_200]);
    expect(loopOf(asset)).toEqual([44_100, 88_200]);
    expect(fileLoopOf(asset)).toEqual([185_220, 4_057_200]);
    // 長さちょうどまではよい。超えればファイルのループ点（なければ null）
    expect(loopOf(withAuditionLoop(audioAsset(), 0, AUDIO_FACTS.frames!))).toEqual([
      0,
      AUDIO_FACTS.frames,
    ]);
    const past = withAuditionLoop(audioAsset(), 0, AUDIO_FACTS.frames! + 1);
    expect(loopOf(past)).toEqual([185_220, 4_057_200]);
    const facts = past.facts && { ...past.facts, audio: { ...AUDIO_FACTS, loopRange: null } };
    expect(loopOf({ ...past, facts })).toBeNull();
    // 長さが分からなければ設定を使う
    const unknown = past.facts && { ...past.facts, audio: { ...AUDIO_FACTS, frames: null } };
    expect(loopOf({ ...past, facts: unknown })).toEqual([0, AUDIO_FACTS.frames! + 1]);
  });
});

describe('useLoopRange（シアターの波形）', () => {
  it('素材の欄を読み、ループの区間を返す', async () => {
    vi.mocked(getAsset).mockResolvedValue(audioAsset());
    const { result } = renderHook(() => useLoopRange(2));
    expect(result.current).toBeNull();
    await waitFor(() => expect(result.current).toEqual([185_220, 4_057_200]));
    expect(getAsset).toHaveBeenCalledWith(2);
  });

  it('ファイルにループ点がなくても、試聴の設定があればその区間を返す', async () => {
    const plain = audioAsset();
    const facts = plain.facts && { ...plain.facts, audio: { ...AUDIO_FACTS, loopRange: null } };
    vi.mocked(getAsset).mockResolvedValue(withAuditionLoop({ ...plain, facts }, 100, 200));
    const { result } = renderHook(() => useLoopRange(2));
    await waitFor(() => expect(result.current).toEqual([100, 200]));
  });

  it('アイテムがないときは読まない', () => {
    renderHook(() => useLoopRange(null));
    expect(getAsset).not.toHaveBeenCalled();
  });

  it('アイテムが替わったら、前のアイテムの区間を出さない', async () => {
    vi.mocked(getAsset).mockImplementation((id) =>
      id === 2 ? Promise.resolve(audioAsset()) : new Promise(() => {}),
    );
    const { result, rerender } = renderHook(({ id }) => useLoopRange(id), {
      initialProps: { id: 2 },
    });
    await waitFor(() => expect(result.current).not.toBeNull());
    rerender({ id: 3 });
    expect(result.current).toBeNull();
  });
});
