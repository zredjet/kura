/**
 * 素材の音の口（アプリに 1 つ）：ループの試聴（Audition）と効果音の即再生（SfxPlayer）が、元のレートごとの文脈
 * （ContextPool）を分け合う。文脈は使うときに作り、ライブラリの画面を離れるときに `closeAudio` で閉じる。
 */

import { pcmUrl, playUrl } from '../../ipc/media';
import {
  Audition,
  browserContext,
  ContextPool,
  type ScheduledAudition,
  SfxPlayer,
} from '../../lib/assetAudio';
import { usePlayerStore } from '../player/playerStore';

/** kura:// の応答が 2xx でなかった（/pcm の 503 は混んでいる、416 は範囲の外）。 */
export class MediaHttpError extends Error {
  constructor(readonly status: number) {
    super(`HTTP ${status}`);
    this.name = 'MediaHttpError';
  }
}

async function fetchBytes(url: string): Promise<ArrayBuffer> {
  const res = await fetch(url);
  if (!res.ok) {
    throw new MediaHttpError(res.status);
  }
  return res.arrayBuffer();
}

/** 試聴を始めたときに呼ぶもの（dev の継ぎ目の自己点検。VITE_KURA_CHECK=1 のときだけ読み込む）。 */
let seamHook: ((scheduled: ScheduledAudition) => void) | null = null;

// 通常のビルドでは import.meta.env.VITE_KURA_CHECK が undefined に置き換わり、この枝ごと消える
if (import.meta.env.VITE_KURA_CHECK === '1') {
  void import('../../dev/seamCheck').then(
    (m) => {
      seamHook = m.checkSeam;
    },
    () => undefined,
  );
}

export interface AudioRuntime {
  pool: ContextPool;
  audition: Audition;
  sfx: SfxPlayer;
}

let runtime: AudioRuntime | null = null;
let auditionStopped: () => void = () => undefined;

/** 再生の設定の音量（0〜1）。 */
function volume(): number {
  return usePlayerStore.getState().prefs.volume / 100;
}

/** 音の口（初めて使うときに作る。文脈はまだ作らない）。 */
export function audioRuntime(): AudioRuntime {
  if (runtime) {
    return runtime;
  }
  const pool = new ContextPool(browserContext, 3);
  runtime = {
    pool,
    audition: new Audition({
      pool,
      fetchPcm: (hash, from, to) => fetchBytes(pcmUrl(hash, from, to)),
      volume,
      onStop: () => auditionStopped(),
      onScheduled: (scheduled) => seamHook?.(scheduled),
    }),
    sfx: new SfxPlayer({ pool, fetchAudio: (hash) => fetchBytes(playUrl(hash)), volume }),
  };
  return runtime;
}

/** 試聴が止まったとき（止めた・継ぎ目だけの試聴が終わった）に呼ぶものを決める（試聴の状態の持ち主）。 */
export function onAuditionStopped(listener: () => void): void {
  auditionStopped = listener;
}

/** 音を止め、文脈をすべて閉じる（ライブラリの画面を離れるとき）。 */
export function closeAudio(): void {
  if (!runtime) {
    return;
  }
  runtime.audition.stop();
  runtime.sfx.stop();
  runtime.pool.closeAll();
}

/** テストで偽物の口に差し替える（null で元に戻す）。 */
export function setAudioRuntimeForTest(next: AudioRuntime | null): void {
  runtime = next;
}
