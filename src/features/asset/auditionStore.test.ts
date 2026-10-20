import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { getAsset } from '../../ipc/asset';
import type { AuditionOutcome } from '../../lib/assetAudio';
import { notify } from '../../stores/noticeStore';
import { useViewStore } from '../../stores/viewStore';
import { playerController } from '../player/playerActions';
import { DEFAULT_PREFS, usePlayerStore } from '../player/playerStore';
import { type AudioRuntime, MediaHttpError, setAudioRuntimeForTest } from './audioRuntime';
import {
  auditionBlocker,
  auditionTarget,
  rememberAsset,
  startAudition,
  stopAudition,
  toggleAudition,
  toggleAuditionKey,
  useAuditionStore,
} from './auditionStore';
import { audioAsset, AUDIO_FACTS, imageAsset } from './testAssets';

vi.mock('../../ipc/asset', () => ({ getAsset: vi.fn() }));
vi.mock('../../stores/noticeStore', () => ({ notify: vi.fn() }));
vi.mock('../player/playerActions', () => ({ playerController: vi.fn() }));

const t = ja.asset;
const PLAN = { mode: 'loop', intro: null, body: [0, 10], offset: 0 } as const;
const FACTS = { frames: 10, rate: 44_100, channels: 2, loop: null };

function fakeRuntime(outcome: () => Promise<AuditionOutcome>) {
  const audition = {
    startProbed: vi.fn(outcome),
    stop: vi.fn(),
    position: vi.fn(() => null),
  };
  const sfx = { play: vi.fn(), stop: vi.fn() };
  const runtime = { audition, sfx, pool: { closeAll: vi.fn() } } as unknown as AudioRuntime;
  return { runtime, audition, sfx };
}

const playing = async (): Promise<AuditionOutcome> => ({
  status: 'playing',
  plan: { ...PLAN, body: [0, 10] },
  facts: FACTS,
});

/** 待っている約束をすべて片付ける。 */
const settle = () => new Promise((r) => setTimeout(r, 0));

describe('試聴の状態と口', () => {
  let fake: ReturnType<typeof fakeRuntime>;

  beforeEach(() => {
    vi.clearAllMocks();
    fake = fakeRuntime(playing);
    setAudioRuntimeForTest(fake.runtime);
    useAuditionStore.setState({ itemId: null, status: 'idle', mode: null, fromStart: false });
    usePlayerStore.setState({ active: false, prefs: DEFAULT_PREFS });
    useViewStore.setState({ quickLook: false });
  });

  afterEach(() => setAudioRuntimeForTest(null));

  it('試聴できない理由：音声でない・解析の前・作れないレート', () => {
    expect(auditionBlocker(audioAsset())).toBeNull();
    expect(auditionTarget(audioAsset())).toEqual({
      itemId: 2,
      hash: 'cd'.repeat(32),
      rate: 44_100,
      channels: 2,
    });
    expect(auditionBlocker(imageAsset())).toBe(t.auditionUnsupported);
    expect(auditionBlocker(audioAsset({ analysisState: 'pending', facts: null }))).toBe(
      t.auditionNoFacts,
    );
    expect(
      auditionBlocker(
        audioAsset({
          analysisState: 'failed',
          facts: { ...audioAsset().facts!, ok: false, audio: { ...AUDIO_FACTS, sampleRate: null } },
        }),
      ),
    ).toBe(t.auditionUnsupported);
    const rate = (sampleRate: number) =>
      audioAsset({ facts: { ...audioAsset().facts!, audio: { ...AUDIO_FACTS, sampleRate } } });
    expect(auditionBlocker(rate(2_999))).toBe(t.auditionRate);
    expect(auditionBlocker(rate(768_000))).toBe(t.auditionRate);
    expect(auditionBlocker(rate(384_000))).toBeNull();
    expect(auditionTarget(rate(768_000))).toBeNull();
  });

  it('始めると効果音を止め、再生中のプレーヤーを一時停止し、応答で長さを知ってから鳴らす', async () => {
    const pause = vi.fn();
    vi.mocked(playerController).mockReturnValue({ pause } as never);
    usePlayerStore.setState({
      active: true,
      snapshot: { ...usePlayerStore.getState().snapshot, playing: true },
    });
    const target = auditionTarget(audioAsset());
    if (!target) throw new Error('対象がない');
    startAudition(target, true);
    expect(fake.sfx.stop).toHaveBeenCalled();
    expect(pause).toHaveBeenCalled();
    expect(fake.audition.startProbed).toHaveBeenCalledWith(
      target.hash,
      { rate: 44_100, channels: 2 },
      { fromStart: true },
    );
    expect(useAuditionStore.getState()).toMatchObject({
      itemId: 2,
      status: 'loading',
      fromStart: true,
    });
    await settle();
    expect(useAuditionStore.getState()).toMatchObject({ status: 'playing', mode: 'loop' });
  });

  it('混んでいて読めなければ（503）知らせのカプセルに「混んでいます…」', async () => {
    fake.audition.startProbed.mockImplementation(async () => ({
      status: 'failed',
      error: new MediaHttpError(503),
    }));
    startAudition(auditionTarget(audioAsset())!);
    await settle();
    expect(notify).toHaveBeenCalledWith(t.auditionBusy);
    expect(useAuditionStore.getState().status).toBe('idle');
    fake.audition.startProbed.mockImplementation(async () => ({ status: 'unsupported' }));
    startAudition(auditionTarget(audioAsset())!);
    await settle();
    expect(notify).toHaveBeenLastCalledWith(t.auditionUnsupported);
  });

  it('待つ間に止めたら、あとから鳴り始めの知らせが来ても状態を戻さない', async () => {
    let resolve: (o: AuditionOutcome) => void = () => undefined;
    fake.audition.startProbed.mockImplementation(
      () => new Promise<AuditionOutcome>((r) => (resolve = r)),
    );
    startAudition(auditionTarget(audioAsset())!);
    stopAudition();
    resolve({ status: 'playing', plan: PLAN as never, facts: FACTS });
    await settle();
    expect(useAuditionStore.getState().status).toBe('idle');
  });

  it('ボタン：同じものを同じ鳴らし方でもう一度押すと止める', async () => {
    const target = auditionTarget(audioAsset())!;
    toggleAudition(target);
    await settle();
    expect(useAuditionStore.getState().status).toBe('playing');
    toggleAudition(target, true);
    expect(fake.audition.startProbed).toHaveBeenCalledTimes(2);
    await settle();
    toggleAudition(target, true);
    expect(useAuditionStore.getState().status).toBe('idle');
    expect(fake.audition.startProbed).toHaveBeenCalledTimes(2);
  });

  it('L：試聴していれば止める。音声なら覚えている素材の欄ですぐ始める', async () => {
    rememberAsset(audioAsset());
    expect(toggleAuditionKey(2, 'audio')).toBe(true);
    expect(fake.audition.startProbed).toHaveBeenCalledTimes(1);
    expect(getAsset).not.toHaveBeenCalled();
    await settle();
    expect(toggleAuditionKey(null, undefined)).toBe(true);
    expect(useAuditionStore.getState().status).toBe('idle');
    // 音声でなければ扱わない
    expect(toggleAuditionKey(4, 'image')).toBe(false);
    expect(toggleAuditionKey(null, undefined)).toBe(false);
  });

  it('L：素材の欄を読んでいなければ読んでから始め、試聴できなければ理由を知らせる', async () => {
    vi.mocked(getAsset).mockResolvedValueOnce(audioAsset({ itemId: 7 }));
    expect(toggleAuditionKey(7, 'audio')).toBe(true);
    await settle();
    expect(getAsset).toHaveBeenCalledWith(7);
    expect(fake.audition.startProbed).toHaveBeenCalledTimes(1);
    stopAudition();
    vi.mocked(getAsset).mockResolvedValueOnce(
      audioAsset({ itemId: 8, analysisState: 'pending', facts: null }),
    );
    toggleAuditionKey(8, 'audio');
    await settle();
    expect(notify).toHaveBeenCalledWith(t.auditionNoFacts);
    expect(fake.audition.startProbed).toHaveBeenCalledTimes(1);
  });

  it('クイックルック中は L・メニューで始めない（クイックルックが自分で鳴らす。効果音と同じ条件）', async () => {
    rememberAsset(audioAsset());
    useViewStore.setState({ quickLook: true });
    expect(toggleAuditionKey(2, 'audio')).toBe(false);
    expect(fake.audition.startProbed).not.toHaveBeenCalled();
    expect(useAuditionStore.getState().status).toBe('idle');
    // 素材の欄を読んでいる間にクイックルックを開いた：読めても始めない
    useViewStore.setState({ quickLook: false });
    let resolve: (asset: ReturnType<typeof audioAsset>) => void = () => undefined;
    vi.mocked(getAsset).mockImplementationOnce(() => new Promise((r) => (resolve = r)));
    expect(toggleAuditionKey(9, 'audio')).toBe(true);
    useViewStore.setState({ quickLook: true });
    resolve(audioAsset({ itemId: 9 }));
    await settle();
    expect(fake.audition.startProbed).not.toHaveBeenCalled();
    expect(useAuditionStore.getState().status).toBe('idle');
  });
});
