/**
 * 偽の Web Audio（テストで共有する。jsdom に AudioContext がないので、`src/lib/assetAudio.ts` の差し替えられる口
 * `ContextLike` を満たす偽物で回す）。M11 の試聴のテスト（`assetAudio.test.ts`）から移した（M13 の段 6。エディタの再生の
 * テストも使う）。
 *
 * - `loopStart`・`loopEnd` に触ったら投げて `touched` に残す（試聴もエディタの再生も使わない約束を見張る）。
 * - 音量の時刻つきの変化（`setValueAtTime`・`linearRampToValueAtTime`）は `sets`・`ramps` に残す（交差フェードの傾き）。
 * - バッファに写した標本は `channels` に残す（継ぎ目・切り出しを照らす）。
 * - 文脈の時刻 `currentTime` はテストが進める。
 */

import type {
  AudioParamLike,
  BufferLike,
  ContextLike,
  GainLike,
  SourceLike,
} from '../lib/assetAudio';

export interface FakeSource extends SourceLike {
  started: [number, number | undefined] | null;
  stopped: boolean;
  /** stop に渡した時刻（すぐ止めたら undefined）。 */
  stoppedAt: number | undefined;
  /** つないだ先（GainNode）。 */
  dest: unknown;
}

export interface FakeBuffer extends BufferLike {
  /** 写した標本（チャンネルごと）。 */
  channels: Float32Array[];
}

export interface FakeParam extends AudioParamLike {
  /** `setValueAtTime(value, when)` の記録。 */
  sets: [number, number][];
  /** `linearRampToValueAtTime(value, when)` の記録。 */
  ramps: [number, number][];
}

export interface FakeGain extends GainLike {
  readonly gain: FakeParam;
  /** `gain.linearRampToValueAtTime` の記録（`gain.ramps` と同じもの。M11 のテストの書き方）。 */
  ramps: [number, number][];
  dest: unknown;
  disconnected: boolean;
}

export interface FakeContext extends ContextLike {
  /** 文脈の時刻（テストが進める）。 */
  currentTime: number;
  requested: number | undefined;
  sources: FakeSource[];
  gains: FakeGain[];
  buffers: FakeBuffer[];
  closed: boolean;
  /** 触ったら失敗にする（loopStart・loopEnd）。 */
  touched: string[];
}

/**
 * 偽の文脈を作る口。`honor` が偽なら、頼んだレートではなく 48kHz で作る（作れなかった環境のまね）。`log` に
 * `create:{rate}`・`resume`・`decode:{bytes}` を残す。
 */
export function fakeFactory(log: string[] = [], honor = true) {
  const contexts: FakeContext[] = [];
  const create = (rate?: number): FakeContext => {
    log.push(`create:${rate ?? 'device'}`);
    const ctx: FakeContext = {
      requested: rate,
      sampleRate: honor ? (rate ?? 48_000) : 48_000,
      currentTime: 0.123_456_7,
      destination: { disconnect: () => undefined },
      sources: [],
      gains: [],
      buffers: [],
      closed: false,
      touched: [],
      createBuffer(channels, frames, r): FakeBuffer {
        const b: FakeBuffer = {
          numberOfChannels: channels,
          length: frames,
          sampleRate: r,
          channels: Array.from({ length: channels }, () => new Float32Array(frames)),
          copyToChannel(source, channel) {
            b.channels[channel]?.set(source.subarray(0, frames));
          },
        };
        ctx.buffers.push(b);
        return b;
      },
      createBufferSource() {
        const s: FakeSource = {
          buffer: null,
          loop: false,
          onended: null,
          started: null,
          stopped: false,
          stoppedAt: undefined,
          dest: null,
          connect: (dest) => {
            s.dest = dest;
          },
          disconnect: () => undefined,
          start(when: number, offset?: number) {
            s.started = [when, offset];
          },
          stop(when?: number) {
            s.stopped = true;
            s.stoppedAt = when;
          },
        };
        for (const key of ['loopStart', 'loopEnd']) {
          Object.defineProperty(s, key, {
            get: () => {
              ctx.touched.push(key);
              throw new Error(`${key} に触れた`);
            },
            set: () => {
              ctx.touched.push(key);
              throw new Error(`${key} に触れた`);
            },
          });
        }
        ctx.sources.push(s);
        return s;
      },
      createGain() {
        const ramps: [number, number][] = [];
        const param: FakeParam = {
          value: 1,
          sets: [],
          ramps,
          setValueAtTime: (v: number, t: number) => {
            param.value = v;
            param.sets.push([v, t]);
          },
          linearRampToValueAtTime: (v: number, t: number) => {
            ramps.push([v, t]);
          },
        };
        const g: FakeGain = {
          ramps,
          gain: param,
          dest: null,
          disconnected: false,
          connect: (dest) => {
            g.dest = dest;
          },
          disconnect: () => {
            g.disconnected = true;
          },
        };
        ctx.gains.push(g);
        return g;
      },
      async decodeAudioData(data: ArrayBuffer): Promise<BufferLike> {
        log.push(`decode:${data.byteLength}`);
        if (data.byteLength === 0) throw new Error('読めない');
        return {
          numberOfChannels: 2,
          length: data.byteLength,
          sampleRate: ctx.sampleRate,
          copyToChannel: () => undefined,
        };
      },
      async resume() {
        log.push('resume');
      },
      async close() {
        ctx.closed = true;
      },
    };
    contexts.push(ctx);
    return ctx;
  };
  return { create, contexts };
}
