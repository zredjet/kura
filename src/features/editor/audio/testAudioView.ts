/**
 * 音声エディタのテストの補助（表示の結果・タイル・PCM の偽物と、差し替える時計）。テストからだけ読む。
 */

import type { EditAudioView, EditPeaksInfoDto } from '../../../ipc/edit';
import { writePcmWav } from '../../../lib/asset/wav';
import { CHUNK } from './pcmChunks';
import { TILE_BINS } from './peakTiles';
import type { WaveScheduler } from './waveController';

export const BINS = [256, 1024, 4096, 16384, 65536];

/** 表示の結果（音声。`peaks` は既定に重ねる）。 */
export type AudioViewOver = Partial<Omit<EditAudioView, 'peaks'>> & {
  peaks?: Partial<EditPeaksInfoDto>;
};

/** 表示の結果（音声）。 */
export function audioView(over: AudioViewOver = {}): EditAudioView {
  const frames = over.frames ?? 48_000 * 60;
  const peaks: EditPeaksInfoDto = {
    rev: 1,
    rate: 48_000,
    channels: 2,
    frames,
    bins: BINS,
    tileBins: TILE_BINS,
    changed: { type: 'all' },
    pending: false,
    ...(over.peaks ?? {}),
  };
  return {
    type: 'audio',
    rev: 1,
    rate: 48_000,
    channels: 2,
    frames,
    loop: null,
    markers: [],
    samplesId: '00112233aabbccdd',
    // 試聴の重ねがなければ鳴らす音の id は samplesId と同じ（M17 段 6）
    auditionId: over.samplesId ?? '00112233aabbccdd',
    audition: null,
    changed: { type: 'all' },
    metaChanged: false,
    valid: [],
    sourceMap: [{ outStart: 0, outEnd: frames, inStart: 0, inRate: 48_000, outRate: 48_000 }],
    nodes: [],
    loudness: { type: 'idle' },
    work: false,
    workStalled: false,
    warnings: [],
    drawnNodes: 0,
    ms: 1,
    solving: null,
    spec: [],
    specAverage: [],
    ...over,
    peaks,
  } as EditAudioView;
}

/** タイルのバイト列（`value` を全ビンの [−value, value] に）。 */
export function tileBytes(
  bin: number,
  tile: number,
  frames: number,
  channels: number,
  value = 1000,
): ArrayBuffer {
  const levelBins = Math.ceil(frames / bin);
  const count = Math.min(TILE_BINS, levelBins - tile * TILE_BINS);
  const buf = new ArrayBuffer(32 + count * channels * 4);
  const v = new DataView(buf);
  [0x6b, 0x70, 0x74, 0x31].forEach((b, i) => v.setUint8(i, b));
  v.setUint16(4, channels, true);
  v.setUint32(8, bin, true);
  v.setUint32(12, tile, true);
  v.setUint32(16, count, true);
  v.setBigUint64(24, BigInt(frames), true);
  for (let i = 0; i < count * channels; i++) {
    v.setInt16(32 + i * 4, -value, true);
    v.setInt16(32 + i * 4 + 2, value, true);
  }
  return buf;
}

/** 出力の PCM のチャンクの WAV（`fill(frame, ch)` の値）。 */
export function pcmBytes(
  index: number,
  total: number,
  channels: number,
  decoder: string,
  fill: (frame: number, ch: number) => number = () => 0.25,
  rate = 48_000,
): ArrayBuffer {
  const a = index * CHUNK;
  const b = Math.min(total, a + CHUNK);
  const planes = Array.from({ length: channels }, (_, ch) => {
    const p = new Float32Array(b - a);
    for (let k = 0; k < p.length; k++) p[k] = fill(a + k, ch);
    return p;
  });
  return writePcmWav({
    rate,
    format: 'f32',
    planes,
    info: { start: a, total, loop: null, decoder },
  });
}

/** 差し替える時計（描く時刻と、止まってからの待ち）。 */
export function fakeClock() {
  let now = 0;
  let id = 0;
  const frames = new Map<number, () => void>();
  const timers = new Map<number, { at: number; cb: () => void }>();
  const scheduler: WaveScheduler = {
    frame(cb) {
      frames.set(++id, cb);
      return id;
    },
    cancelFrame(i) {
      frames.delete(i);
    },
    later(cb, ms) {
      timers.set(++id, { at: now + ms, cb });
      return id;
    },
    cancelLater(i) {
      timers.delete(i);
    },
  };
  return {
    scheduler,
    now: () => now,
    pendingFrames: () => frames.size,
    flushFrames() {
      const fs = [...frames.values()];
      frames.clear();
      for (const f of fs) f();
    },
    advance(ms: number) {
      now += ms;
      for (;;) {
        const due = [...timers.entries()]
          .filter(([, t]) => t.at <= now)
          .sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        due[1].cb();
      }
    },
  };
}

/** 取る口の偽物（URL ごとに後から応える）。 */
export function fakeFetch() {
  const calls: string[] = [];
  const waiting = new Map<
    string,
    { resolve: (b: ArrayBuffer) => void; reject: (e: unknown) => void }[]
  >();
  const fetch = (url: string) =>
    new Promise<ArrayBuffer>((resolve, reject) => {
      calls.push(url);
      const list = waiting.get(url) ?? [];
      list.push({ resolve, reject });
      waiting.set(url, list);
    });
  return {
    fetch,
    calls,
    pending: () => [...waiting.keys()],
    resolve(url: string, body: ArrayBuffer) {
      const list = waiting.get(url) ?? [];
      waiting.delete(url);
      for (const w of list) w.resolve(body);
    },
    reject(url: string, error: unknown) {
      const list = waiting.get(url) ?? [];
      waiting.delete(url);
      for (const w of list) w.reject(error);
    },
  };
}

/** 約束の続きを流す。 */
export async function settle(): Promise<void> {
  for (let i = 0; i < 6; i++) await Promise.resolve();
}

/** スペクトルのタイル（`kst1`）のバイト列（列 j・帯 k の u8 を `value(j, k)` に）。 */
export function specTileBytes(t: {
  rate: number;
  n: number;
  hop: number;
  bands: number;
  code: number;
  firstFrame: number;
  frames: number;
  value: (frame: number, band: number) => number;
}): ArrayBuffer {
  const buf = new ArrayBuffer(40 + t.frames * t.bands);
  const v = new DataView(buf);
  [0x6b, 0x73, 0x74, 0x31].forEach((b, i) => v.setUint8(i, b));
  v.setUint16(4, 1, true);
  v.setUint16(6, t.code, true);
  v.setUint32(8, t.rate, true);
  v.setUint32(12, t.n, true);
  v.setUint32(16, t.hop, true);
  v.setUint16(20, t.bands, true);
  v.setUint16(22, 1, true);
  v.setBigUint64(24, BigInt(t.firstFrame), true);
  v.setUint32(32, t.frames, true);
  v.setUint32(36, 0, true);
  const data = new Uint8Array(buf, 40);
  for (let j = 0; j < t.frames; j++) {
    for (let k = 0; k < t.bands; k++) data[j * t.bands + k] = t.value(t.firstFrame + j, k);
  }
  return buf;
}
