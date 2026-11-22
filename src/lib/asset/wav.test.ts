import { describe, expect, it } from 'vitest';

import wavText from '../../../crates/kura-core/tests/vectors/pcm/wav.json?raw';

import { MAX_RATE, MIN_RATE, type Pcm, readPcmWav, writePcmWav } from './wav';

interface WavCase {
  name: string;
  format: 'i16' | 'f32';
  rate: number;
  channels: number;
  frames: number;
  start: number;
  total: number;
  loop: [number, number] | null;
  decoder: string;
  input: number[][];
  decoded: number[][];
  hex: string;
}

const cases = (JSON.parse(wavText) as { cases: WavCase[] }).cases;

function fromHex(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out.buffer;
}

function toHex(buf: ArrayBuffer): string {
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** f32 のビットの列 → Float32Array（NaN の中身も保つ）。 */
function planeOf(bits: number[]): Float32Array<ArrayBuffer> {
  return new Float32Array(new Uint32Array(bits).buffer);
}

function caseOf(name: string): WavCase {
  const c = cases.find((x) => x.name === name);
  if (!c) throw new Error(`ベクタがない：${name}`);
  return c;
}

function pcmOf(c: WavCase, planes = c.input): Omit<Pcm, 'frames' | 'channels'> {
  return {
    rate: c.rate,
    format: c.format,
    planes: planes.map(planeOf),
    info: { start: c.start, total: c.total, loop: c.loop, decoder: c.decoder },
  };
}

/** 書き換えた写し。 */
function edited(hex: string, f: (v: DataView, buf: ArrayBuffer) => void): ArrayBuffer {
  const buf = fromHex(hex);
  f(new DataView(buf), buf);
  return buf;
}

// /pcm の WAV（kura-core/tests/pcm_vectors.rs の並び）を Rust の書き方と同じに読み書きする
describe('試聴の PCM（Rust と共有のベクタ）', () => {
  it('ベクタがある', () => {
    expect(cases.length).toBeGreaterThan(4);
  });

  for (const c of cases) {
    it(`書くと同じバイト列：${c.name}`, () => {
      expect(toHex(writePcmWav(pcmOf(c)))).toBe(c.hex);
    });

    it(`読むと同じ値：${c.name}`, () => {
      const pcm = readPcmWav(fromHex(c.hex));
      expect(pcm).not.toBeNull();
      if (!pcm) return;
      expect(pcm.rate).toBe(c.rate);
      expect(pcm.channels).toBe(c.channels);
      expect(pcm.frames).toBe(c.frames);
      expect(pcm.format).toBe(c.format);
      expect(pcm.info).toEqual({
        start: c.start,
        total: c.total,
        loop: c.loop,
        decoder: c.decoder,
      });
      pcm.planes.forEach((plane, ch) => {
        const want = planeOf(c.decoded[ch] ?? []);
        expect(plane).toHaveLength(want.length);
        plane.forEach((x, i) => {
          const w = want[i] ?? 0;
          if (Number.isNaN(w)) {
            expect(x).toBeNaN();
          } else {
            // ビットで比べる（−0 と 0 を分ける）
            expect(Object.is(x, w), `${ch}:${i} ${x} ${w}`).toBe(true);
          }
        });
      });
      // 読んだ値を同じ形式で書くと元に戻る（16 ビットは格子の上なので）
      expect(toHex(writePcmWav(pcm))).toBe(c.hex);
    });
  }
});

describe('試聴の PCM を読む：断るもの', () => {
  const f32 = caseOf('f32-mono-specials-odd-decoder');
  const i16 = caseOf('i16-stereo-loop');
  // i16-stereo-loop の並び：fmt は 12、kura は 36（本体は 44〜、L = 14）、data は 36 + 8 + 54 = 98
  const KURA = 44;
  const DATA = 98;

  it('並びの位置の前提', () => {
    const v = new DataView(fromHex(i16.hex));
    const tag = (at: number) => String.fromCharCode(...new Uint8Array(v.buffer, at, 4));
    expect(tag(12)).toBe('fmt ');
    expect(tag(KURA - 8)).toBe('kura');
    expect(tag(DATA)).toBe('data');
  });

  const broken: [string, ArrayBuffer][] = [
    ['短すぎる', fromHex(i16.hex).slice(0, 11)],
    ['途中で切れている', fromHex(i16.hex).slice(0, fromHex(i16.hex).byteLength - 1)],
    [
      'RIFF の大きさが合わない',
      edited(i16.hex, (v) => v.setUint32(4, v.getUint32(4, true) + 2, true)),
    ],
    ['RIFF でない', edited(i16.hex, (v) => v.setUint8(0, 0x58))],
    ['24 ビットの整数', edited(i16.hex, (v) => v.setUint16(34, 24, true))],
    ['64 ビットの浮動小数', edited(f32.hex, (v) => v.setUint16(34, 64, true))],
    ['知らない形式（拡張の fmt）', edited(i16.hex, (v) => v.setUint16(20, 0xfffe, true))],
    [`レートが ${MIN_RATE} 未満`, edited(i16.hex, (v) => v.setUint32(24, MIN_RATE - 1, true))],
    [`レートが ${MAX_RATE} を超える`, edited(i16.hex, (v) => v.setUint32(24, MAX_RATE + 1, true))],
    ['チャンネルが 0', edited(i16.hex, (v) => v.setUint16(22, 0, true))],
    ['ブロックが合わない', edited(i16.hex, (v) => v.setUint16(32, 2, true))],
    ['バイト毎秒が合わない', edited(i16.hex, (v) => v.setUint32(28, 1, true))],
    ['kura の版が違う', edited(i16.hex, (v) => v.setUint16(KURA, 2, true))],
    ['kura の印に知らないビット', edited(i16.hex, (v) => v.setUint32(KURA + 4, 3, true))],
    [
      'ループの終わりが元の終わりを超える',
      edited(i16.hex, (v) => v.setBigUint64(KURA + 32, 11n, true)),
    ],
    ['ループの始まりが終わり以上', edited(i16.hex, (v) => v.setBigUint64(KURA + 24, 7n, true))],
    ['印がないのにループの値がある', edited(i16.hex, (v) => v.setUint32(KURA + 4, 0, true))],
    ['応答が元の終わりを超える', edited(i16.hex, (v) => v.setBigUint64(KURA + 16, 9n, true))],
    ['デコーダの識別子の長さが合わない', edited(i16.hex, (v) => v.setUint16(KURA + 2, 15, true))],
    ['デコーダの識別子が UTF-8 でない', edited(i16.hex, (v) => v.setUint8(KURA + 40, 0xff))],
    [
      'フレーム数が JS の整数で表せない',
      edited(i16.hex, (v) => v.setBigUint64(KURA + 16, 2n ** 53n + 2n, true)),
    ],
    [
      'data がブロックの倍数でない',
      edited(i16.hex, (v) => v.setUint32(DATA + 4, v.getUint32(DATA + 4, true) - 2, true)),
    ],
    ['data がない', edited(i16.hex, (v) => v.setUint8(DATA, 0x78))],
    ['kura がない', edited(i16.hex, (v) => v.setUint8(KURA - 8, 0x78))],
    [
      'fmt が 2 つ',
      edited(i16.hex, (v) =>
        [...'fmt '].forEach((c, i) => v.setUint8(KURA - 8 + i, c.charCodeAt(0))),
      ),
    ],
  ];

  for (const [name, buf] of broken) {
    it(name, () => {
      expect(readPcmWav(buf)).toBeNull();
    });
  }

  it('上限（浮動小数にしたときのバイト数）を超えるものは確保の前に断る', () => {
    const buf = fromHex(i16.hex);
    // 10 フレーム × 2 チャンネル × 4 バイト = 80
    expect(readPcmWav(buf, 80)).not.toBeNull();
    expect(readPcmWav(buf, 79)).toBeNull();
  });

  it('知らないチャンクは読み飛ばす', () => {
    const base = new Uint8Array(fromHex(i16.hex));
    const extra = new Uint8Array([0x4c, 0x49, 0x53, 0x54, 3, 0, 0, 0, 1, 2, 3, 0]);
    const out = new Uint8Array(base.length + extra.length);
    out.set(base);
    out.set(extra, base.length);
    new DataView(out.buffer).setUint32(4, out.length - 8, true);
    expect(readPcmWav(out.buffer)?.frames).toBe(10);
  });
});

describe('試聴の PCM を書く', () => {
  it('浮動小数の書いて読む往復（開始フレーム・NaN・ループ点も）', () => {
    const l = new Float32Array([0, 0.5, -1, 1e-30]);
    const r = new Float32Array([1, -0.25, 0.125, Number.NaN]);
    const buf = writePcmWav({
      rate: 44_100,
      format: 'f32',
      planes: [l, r],
      info: {
        start: 123_456_789,
        total: 123_456_800,
        loop: [123_456_790, 123_456_800],
        decoder: 'x/1',
      },
    });
    const pcm = readPcmWav(buf);
    expect(pcm?.info).toEqual({
      start: 123_456_789,
      total: 123_456_800,
      loop: [123_456_790, 123_456_800],
      decoder: 'x/1',
    });
    expect([...(pcm?.planes[0] ?? [])]).toEqual([...l]);
    expect(pcm?.planes[1]?.[3]).toBeNaN();
  });

  it('チャンネルの長さがそろわない・知らないレートは書かない', () => {
    const info = { start: 0, total: 2, loop: null, decoder: '' };
    expect(() =>
      writePcmWav({
        rate: 44_100,
        format: 'f32',
        planes: [new Float32Array(2), new Float32Array(1)],
        info,
      }),
    ).toThrow(RangeError);
    expect(() =>
      writePcmWav({ rate: 1, format: 'f32', planes: [new Float32Array(2)], info }),
    ).toThrow(RangeError);
    expect(() => writePcmWav({ rate: 44_100, format: 'f32', planes: [], info })).toThrow(
      RangeError,
    );
  });
});
