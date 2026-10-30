import { describe, expect, it } from 'vitest';

import peaksRs from '../../../../crates/kura-core/src/edit/editor/audio/peaks.rs?raw';
import vectorText from '../../../../crates/kura-core/tests/vectors/edit/peaks_tile.json?raw';

import {
  fillPeakColumns,
  parseEditPeaksTile,
  PeakTileCache,
  type PeakTile,
  TILE_BINS,
  tileCount,
  tileRange,
} from './peakTiles';

interface Vector {
  peaks: {
    rate: number;
    channels: number;
    frames: number;
    levels: { bin: number; data: number[] }[];
  };
  tiles: { bin: number; tile: number; hex: string | null }[];
}

const vector = JSON.parse(vectorText) as Vector;

function fromHex(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out.buffer;
}

/** タイルを書く（テスト用。Rust の `tile_bytes` と同じ並び）。 */
function writeTile(t: Omit<PeakTile, 'count'> & { count?: number }): ArrayBuffer {
  const count = t.count ?? t.data.length / (t.channels * 2);
  const buf = new ArrayBuffer(32 + t.data.length * 2);
  const v = new DataView(buf);
  [0x6b, 0x70, 0x74, 0x31].forEach((b, i) => v.setUint8(i, b));
  v.setUint16(4, t.channels, true);
  v.setUint32(8, t.bin, true);
  v.setUint32(12, t.tile, true);
  v.setUint32(16, count, true);
  v.setBigUint64(24, BigInt(t.frames), true);
  t.data.forEach((x, i) => v.setInt16(32 + i * 2, x, true));
  return buf;
}

describe('波形の段のタイルを読む（Rust と共有のベクタ）', () => {
  it('タイル 1 枚のビンの数は Rust の TILE_BINS と同じ', () => {
    expect(peaksRs).toMatch(/pub const TILE_BINS: u32 = 1_024;/);
    expect(TILE_BINS).toBe(1024);
  });

  it('ベクタのタイルを読むと、段の値の切り出しと同じ', () => {
    const tiles = vector.tiles.filter((t) => t.hex !== null);
    expect(tiles.length).toBeGreaterThanOrEqual(2);
    for (const t of tiles) {
      const got = parseEditPeaksTile(fromHex(t.hex ?? ''));
      const level = vector.peaks.levels.find((l) => l.bin === t.bin);
      expect(got).not.toBeNull();
      expect(got?.channels).toBe(vector.peaks.channels);
      expect(got?.frames).toBe(vector.peaks.frames);
      expect(got?.bin).toBe(t.bin);
      expect(got?.tile).toBe(t.tile);
      expect(Array.from(got?.data ?? [])).toEqual(level?.data);
    }
  });
});

describe('壊れたタイルは断る', () => {
  const good = (vector.tiles[0]?.hex ?? '') as string;
  const bytes = () => new Uint8Array(fromHex(good));
  const cases: [string, () => ArrayBuffer][] = [
    ['短い', () => bytes().slice(0, 20).buffer],
    [
      'magic',
      () => {
        const b = bytes();
        b[0] = 0;
        return b.buffer;
      },
    ],
    [
      'チャンネル 0',
      () => {
        const b = bytes();
        b[4] = 0;
        return b.buffer;
      },
    ],
    [
      '予約が 0 でない',
      () => {
        const b = bytes();
        b[20] = 1;
        return b.buffer;
      },
    ],
    ['長さが合わない', () => bytes().slice(0, bytes().length - 2).buffer],
    [
      'min > max',
      () => {
        const b = bytes();
        const v = new DataView(b.buffer);
        v.setInt16(32, 100, true);
        v.setInt16(34, -100, true);
        return b.buffer;
      },
    ],
    [
      '段の外のタイル',
      () => {
        const b = bytes();
        new DataView(b.buffer).setUint32(12, 5, true);
        return b.buffer;
      },
    ],
    [
      '最後でないのに満杯でない',
      () =>
        writeTile({
          channels: 1,
          bin: 256,
          tile: 0,
          frames: 256 * 2000,
          data: new Int16Array(4),
          count: 2,
        }),
    ],
  ];
  it.each(cases)('%s', (_name, make) => {
    expect(parseEditPeaksTile(make())).toBeNull();
  });
});

describe('タイルの番号', () => {
  it('段のタイルの数と、区間に掛かる範囲', () => {
    // 256 のビン × 1024 = 262,144 フレームで 1 枚
    expect(tileCount(262_144, 256)).toBe(1);
    expect(tileCount(262_145, 256)).toBe(2);
    expect(tileCount(0, 256)).toBe(0);
    expect(tileRange(0, 262_144, 256, 1_000_000)).toEqual([0, 1]);
    expect(tileRange(262_143, 262_145, 256, 1_000_000)).toEqual([0, 2]);
    expect(tileRange(-10, 2e9, 256, 1_000_000)).toEqual([0, 4]);
  });
});

function tile(bin: number, index: number, values: number[], frames: number): PeakTile {
  return {
    channels: 1,
    bin,
    tile: index,
    count: values.length / 2,
    frames,
    data: Int16Array.from(values),
  };
}

describe('手元のタイルの表', () => {
  it('変わった区間に掛かるタイルだけ捨て、all は全部捨てる', () => {
    const cache = new PeakTileCache();
    const per = 256 * TILE_BINS;
    cache.put(256, 0, 1, tile(256, 0, [0, 1], per * 3));
    cache.put(256, 1, 1, tile(256, 1, [0, 1], per * 3));
    cache.put(1024, 0, 1, tile(1024, 0, [0, 1], per * 3));
    cache.invalidate({ type: 'spans', spans: [{ start: per + 10, end: per + 20 }] });
    expect(cache.get(256, 0)?.rev).toBe(1);
    expect(cache.get(256, 1)).toBeUndefined();
    // 1024 のタイル 0 は 1024 × 1024 フレームを受け持つので掛かる
    expect(cache.get(1024, 0)).toBeUndefined();
    cache.invalidate({ type: 'spans', spans: [] });
    expect(cache.size).toBe(1);
    cache.invalidate({ type: 'all' });
    expect(cache.size).toBe(0);
    expect(cache.bytes).toBe(0);
  });

  it('上限を超えたら長く使っていないものから捨てる', () => {
    const one = tile(256, 0, new Array(200).fill(0), 1e6);
    const size = one.data.byteLength + 64;
    const cache = new PeakTileCache(size * 2);
    cache.put(256, 0, 1, one);
    cache.put(256, 1, 1, { ...one, tile: 1 });
    cache.get(256, 0); // 0 を新しくする
    cache.put(256, 2, 1, { ...one, tile: 2 });
    expect(cache.peek(256, 1)).toBeUndefined();
    expect(cache.peek(256, 0)).toBeDefined();
    expect(cache.peek(256, 2)).toBeDefined();
    expect(cache.bytes).toBe(size * 2);
  });
});

describe('列を埋める', () => {
  it('選んだ段のタイルで列ごとの [min, max] を出し、ないタイルは粗い段で埋める', () => {
    const frames = 256 * 8;
    const cache = new PeakTileCache();
    // 256 の段：8 ビン（タイル 0 だけ持つ）
    cache.put(
      256,
      0,
      1,
      tile(256, 0, [-100, 100, -200, 200, 0, 0, 0, 0, -300, 300, 0, 0, 0, 0, 0, 0], frames),
    );
    const out = new Float32Array(4 * 2);
    const has = new Uint8Array(4);
    const n = fillPeakColumns(
      { cache, bins: [256, 1024], channels: 1, frames, start: 0, span: frames, columns: 4 },
      out,
      has,
    );
    expect(n).toBe(4);
    expect(Array.from(has)).toEqual([1, 1, 1, 1]);
    expect(out[0]).toBeCloseTo(-200 / 32768, 9);
    expect(out[1]).toBeCloseTo(200 / 32768, 9);
    expect(out[4]).toBeCloseTo(-300 / 32768, 9);
  });

  it('どの段にもタイルがない列は埋めない（0）・区間の外の列は 0 で埋めたことにする', () => {
    const frames = 1024 * 2;
    const cache = new PeakTileCache();
    cache.put(1024, 0, 1, tile(1024, 0, [-5, 5, -7, 7], frames));
    const out = new Float32Array(4 * 2).fill(9);
    const has = new Uint8Array(4);
    // 256 の段はない → 1024 の段で埋める
    fillPeakColumns(
      { cache, bins: [256, 1024], channels: 1, frames, start: 0, span: frames * 2, columns: 4 },
      out,
      has,
    );
    expect(Array.from(has)).toEqual([1, 1, 1, 1]);
    expect(out[2]).toBeCloseTo(-7 / 32768, 9);
    expect(out[6]).toBe(0);
    const empty = new PeakTileCache();
    fillPeakColumns(
      { cache: empty, bins: [256], channels: 1, frames, start: 0, span: frames, columns: 2 },
      out,
      has,
    );
    expect(Array.from(has.subarray(0, 2))).toEqual([0, 0]);
    expect(out[0]).toBe(0);
  });
});
