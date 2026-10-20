/**
 * ループの試聴の状態と操作（design.md §3.1「ループの試聴」・§6 の L）。素材の欄のボタン・L のキー・メニューの
 * 「ループを試聴」が同じ口を通す。一度に 1 つだけで、始めるとプレーヤーを一時停止し、効果音を止める。
 * 鳴らし方（本体だけのバッファを loop で回す。loopStart・loopEnd を使わない）は src/lib/assetAudio.ts の Audition。
 */

import { create } from 'zustand';

import { ja } from '../../i18n/ja';
import type { AssetDto } from '../../ipc/asset';
import { getAsset } from '../../ipc/asset';
import { isIpcError } from '../../ipc/result';
import { MAX_RATE, MIN_RATE } from '../../lib/asset/wav';
import { notify } from '../../stores/noticeStore';
import { playerController } from '../player/playerActions';
import { usePlayerStore } from '../player/playerStore';
import { audioRuntime, MediaHttpError, onAuditionStopped } from './audioRuntime';

const t = ja.asset;

export type AuditionStatus = 'idle' | 'loading' | 'playing';

interface AuditionState {
  /** 試聴しているアイテム（していなければ null）。 */
  itemId: number | null;
  status: AuditionStatus;
  /** 鳴らし方（本体を回す・継ぎ目の前後だけ）。鳴り始めるまでは null。 */
  mode: 'loop' | 'seam' | null;
  /** ［はじめから］で始めたか。 */
  fromStart: boolean;
}

const IDLE: AuditionState = { itemId: null, status: 'idle', mode: null, fromStart: false };

export const useAuditionStore = create<AuditionState>(() => IDLE);

onAuditionStopped(() => useAuditionStore.setState(IDLE));

/** 試聴に要るもの（解析の事実）。 */
export interface AuditionTarget {
  itemId: number;
  hash: string;
  rate: number;
  channels: number;
}

/**
 * 試聴できない理由（できるなら null）。音声でないもの・解析の事実（レート・チャンネル）がないもの・作れない
 * レート（3〜384kHz の外。文脈を元のレートで作れないので補間になる）。
 */
export function auditionBlocker(asset: AssetDto): string | null {
  if (asset.kind !== 'audio') {
    return t.auditionUnsupported;
  }
  const audio = asset.facts?.ok ? asset.facts.audio : null;
  if (!audio?.sampleRate || !audio.channels) {
    return asset.analysisState === 'failed' ? t.auditionUnsupported : t.auditionNoFacts;
  }
  if (audio.sampleRate < MIN_RATE || audio.sampleRate > MAX_RATE) {
    return t.auditionRate;
  }
  return null;
}

/** 試聴の対象（試聴できなければ null）。 */
export function auditionTarget(asset: AssetDto): AuditionTarget | null {
  const audio = asset.facts?.audio;
  if (auditionBlocker(asset) !== null || !audio?.sampleRate || !audio.channels) {
    return null;
  }
  return {
    itemId: asset.itemId,
    hash: asset.hash,
    rate: audio.sampleRate,
    channels: audio.channels,
  };
}

/** 読んだ素材の欄（L のキーで、インスペクタが読んだものをすぐ使うため。新しいものを後ろへ、16 件まで）。 */
const recent = new Map<number, AssetDto>();
const RECENT_MAX = 16;

/** 読んだ素材の欄を覚える（useAssetData が呼ぶ）。 */
export function rememberAsset(asset: AssetDto): void {
  recent.delete(asset.itemId);
  recent.set(asset.itemId, asset);
  while (recent.size > RECENT_MAX) {
    const oldest = recent.keys().next();
    if (oldest.done) break;
    recent.delete(oldest.value);
  }
}

/** 覚えている素材の欄。 */
export function recentAsset(itemId: number): AssetDto | undefined {
  return recent.get(itemId);
}

/** 再生中のプレーヤーを一時停止する（試聴は明示の操作なので、ほかの音より先に立てる）。 */
function pausePlayer(): void {
  const player = usePlayerStore.getState();
  if (player.active && player.snapshot.playing) {
    playerController().pause();
  }
}

function failureText(error: unknown): string {
  if ((error instanceof MediaHttpError && error.status === 503) || isIpcError(error, 'Busy')) {
    return t.auditionBusy;
  }
  return t.auditionFailed(error instanceof Error ? error.message : String(error));
}

/**
 * 試聴を始める。キー・クリックの処理の中から呼ぶ（文脈の resume を、取る前に呼ぶため）。
 * 長さ・ループ点は /pcm の応答（同じ読み取り）で決める（ループ点がなければ全体を回す）。
 */
export function startAudition(target: AuditionTarget, fromStart = false): void {
  const { audition, sfx } = audioRuntime();
  audition.stop();
  sfx.stop();
  pausePlayer();
  useAuditionStore.setState({ itemId: target.itemId, status: 'loading', mode: null, fromStart });
  const isCurrent = () => {
    const s = useAuditionStore.getState();
    return s.itemId === target.itemId && s.fromStart === fromStart && s.status === 'loading';
  };
  void audition
    .startProbed(target.hash, { rate: target.rate, channels: target.channels }, { fromStart })
    .then((out) => {
      if (out.status === 'superseded' || !isCurrent()) {
        return;
      }
      if (out.status === 'playing') {
        useAuditionStore.setState({ status: 'playing', mode: out.plan.mode });
        return;
      }
      useAuditionStore.setState(IDLE);
      notify(out.status === 'failed' ? failureText(out.error) : t.auditionUnsupported);
    });
}

/** 試聴を止める（していなければ何もしない）。 */
export function stopAudition(): void {
  audioRuntime().audition.stop();
  useAuditionStore.setState(IDLE);
}

/**
 * ボタン：同じアイテムを同じ鳴らし方で試聴していれば止め、そうでなければ始める（［ループを試聴］と［はじめから］）。
 */
export function toggleAudition(target: AuditionTarget, fromStart = false): void {
  const s = useAuditionStore.getState();
  if (s.status !== 'idle' && s.itemId === target.itemId && s.fromStart === fromStart) {
    stopAudition();
  } else {
    startAudition(target, fromStart);
  }
}

/**
 * L のキー・メニュー：試聴していれば止める。していなければ、選んでいる音声を継ぎ目の手前から試聴する。
 * インスペクタが読んだ素材の欄があればすぐ始め、なければ読んでから始める。扱ったら true。
 */
export function toggleAuditionKey(itemId: number | null, kind: string | undefined): boolean {
  if (useAuditionStore.getState().status !== 'idle') {
    stopAudition();
    return true;
  }
  if (itemId === null || kind !== 'audio') {
    return false;
  }
  const begin = (asset: AssetDto) => {
    const target = auditionTarget(asset);
    if (target) {
      startAudition(target);
    } else {
      notify(auditionBlocker(asset) ?? t.auditionUnsupported);
    }
  };
  const known = recentAsset(itemId);
  if (known) {
    begin(known);
  } else {
    getAsset(itemId).then(
      (asset) => {
        rememberAsset(asset);
        begin(asset);
      },
      (error: unknown) => notify(failureText(error)),
    );
  }
  return true;
}
