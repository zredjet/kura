import { describe, expect, it } from 'vitest';

import audioRs from '../../../../crates/kura-engine/src/value/audio.rs?raw';

import {
  alignSpan,
  CHUNK,
  chunkOf,
  chunkSpan,
  covered,
  fillPcmColumns,
  type PcmChunk,
  PcmChunkCache,
} from './pcmChunks';

function chunk(index: number, values: number[][], rev = 1): PcmChunk {
  return {
    rev,
    start: index * CHUNK,
    frames: values[0]?.length ?? 0,
    planes: values.map((v) => Float32Array.from(v)),
  };
}

describe('チャンクの格子', () => {
  it('チャンクのフレーム数は Rust の AUDIO_CHUNK と同じ', () => {
    expect(audioRs).toMatch(/pub const AUDIO_CHUNK: usize = 1 << 16;/);
    expect(CHUNK).toBe(1 << 16);
  });

  it('番号・区間・境にそろえる', () => {
    expect(chunkOf(0)).toBe(0);
    expect(chunkOf(CHUNK - 1)).toBe(0);
    expect(chunkOf(CHUNK)).toBe(1);
    expect(chunkSpan(1, CHUNK + 10)).toEqual([CHUNK, CHUNK + 10]);
    expect(chunkSpan(2, CHUNK + 10)).toBeNull();
    expect(chunkSpan(-1, CHUNK)).toBeNull();
    expect(alignSpan(10, CHUNK + 1, CHUNK * 3)).toEqual({ start: 0, end: CHUNK * 2 });
    expect(alignSpan(10, CHUNK * 9, CHUNK + 5)).toEqual({ start: 0, end: CHUNK + 5 });
    expect(alignSpan(5, 5, 100)).toBeNull();
  });

  it('valid のどれか 1 つに覆われる区間だけ取れる', () => {
    const valid = [
      { start: 0, end: CHUNK },
      { start: CHUNK * 2, end: CHUNK * 4 },
    ];
    expect(covered(valid, 0, CHUNK)).toBe(true);
    expect(covered(valid, CHUNK * 3, CHUNK * 4)).toBe(true);
    expect(covered(valid, CHUNK, CHUNK * 2)).toBe(false);
    expect(covered(valid, 0, CHUNK * 2)).toBe(false);
  });
});

describe('手元の PCM の表', () => {
  it('変わった区間に掛かるチャンクを捨てる（捨てた番号を返す）', () => {
    const cache = new PcmChunkCache();
    cache.put(0, chunk(0, [[0]]));
    cache.put(1, chunk(1, [[0]]));
    cache.put(2, chunk(2, [[0]]));
    expect(
      cache.invalidate({ type: 'spans', spans: [{ start: CHUNK + 5, end: CHUNK * 2 + 1 }] }),
    ).toEqual([1, 2]);
    expect(cache.has(0)).toBe(true);
    expect(cache.invalidate({ type: 'spans', spans: [] })).toEqual([]);
    expect(cache.invalidate({ type: 'all' })).toEqual([0]);
    expect(cache.size).toBe(0);
    expect(cache.bytes).toBe(0);
  });

  it('上限を超えたら長く使っていないものから捨てる', () => {
    const one = chunk(0, [new Array(100).fill(0)]);
    const size = 400 + 64;
    const cache = new PcmChunkCache(size * 2);
    cache.put(0, one);
    cache.put(1, { ...one, start: CHUNK });
    cache.get(0);
    cache.put(2, { ...one, start: CHUNK * 2 });
    expect(cache.has(1)).toBe(false);
    expect(cache.has(0)).toBe(true);
    expect(cache.bytes).toBe(size * 2);
  });
});

describe('PCM から列を埋める', () => {
  it('列ごと・チャンネルごとの [min, max]、拡大して 1 列が 1 フレームより細かいときはそのフレーム', () => {
    const c0 = chunk(0, [
      [0.1, -0.5, 0.3, 0.9],
      [0, 0, -1, 1],
    ]);
    const get = (i: number) => (i === 0 ? c0 : undefined);
    const out = new Float32Array(2 * 2 * 2);
    const has = new Uint8Array(2);
    fillPcmColumns({ get, channels: 2, frames: 4, start: 0, span: 4, columns: 2 }, out, has);
    expect(Array.from(has)).toEqual([1, 1]);
    expect(out[0]).toBeCloseTo(-0.5, 6);
    expect(out[1]).toBeCloseTo(0.1, 6);
    expect(out[2]).toBe(0);
    expect(out[3]).toBe(0);
    expect(out[4]).toBeCloseTo(0.3, 6);
    expect(out[5]).toBeCloseTo(0.9, 6);
    expect(out[6]).toBe(-1);
    expect(out[7]).toBe(1);
    const mono = chunk(0, [[0.1, -0.5, 0.3, 0.9]]);
    const fine = new Float32Array(8 * 2);
    const fineHas = new Uint8Array(8);
    fillPcmColumns(
      { get: () => mono, channels: 1, frames: 4, start: 1, span: 0.5, columns: 4 },
      fine,
      fineHas,
    );
    // 1 フレーム目（−0.5）を 4 列とも
    expect(Array.from(fine.subarray(0, 8)).map((v) => Math.round(v * 10) / 10)).toEqual([
      -0.5, -0.5, -0.5, -0.5, -0.5, -0.5, -0.5, -0.5,
    ]);
  });

  it('チャンクがない列は埋めない', () => {
    const out = new Float32Array(4);
    const has = new Uint8Array(2);
    fillPcmColumns(
      {
        get: () => undefined,
        channels: 1,
        frames: CHUNK * 2,
        start: 0,
        span: CHUNK * 2,
        columns: 2,
      },
      out,
      has,
    );
    expect(Array.from(has)).toEqual([0, 0]);
  });
});
