import { describe, expect, it } from 'vitest';

import vectorText from '../../../../crates/kura-core/tests/vectors/edit/spec_tile.json?raw';
import { editSpecUrl } from '../../../ipc/media';
import {
  SPEC_TILE_HEADER,
  SpecTileCache,
  isReady,
  parseEditSpecTile,
  specChunkRange,
  specDb,
  type SpecTile,
} from './specTiles';

interface VectorTile {
  rate: number;
  n: number;
  hop: number;
  bands: number;
  channel: number;
  channelType: 'index' | 'mid' | 'side' | 'sum';
  token: string;
  chunk: number;
  firstFrame: number;
  frames: number;
  hex: string;
}

const vector = JSON.parse(vectorText) as { tiles: VectorTile[] };

function bytesOf(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out.buffer;
}

function mutate(hex: string, at: number, f: (v: DataView) => void): ArrayBuffer {
  const buf = bytesOf(hex);
  f(new DataView(buf, at));
  return buf;
}

describe('スペクトルのタイル（kst1）', () => {
  it.each(vector.tiles)('共有のベクタ $token/$chunk を読む', (t) => {
    const tile = parseEditSpecTile(bytesOf(t.hex));
    expect(tile).not.toBeNull();
    const got = tile as SpecTile;
    expect(got.channel.type).toBe(t.channelType);
    if (got.channel.type === 'index') expect(got.channel.index).toBe(t.channel);
    expect([got.rate, got.n, got.hop, got.bands]).toEqual([t.rate, t.n, t.hop, t.bands]);
    expect([got.chunk, got.firstFrame, got.frames]).toEqual([t.chunk, t.firstFrame, t.frames]);
    expect(got.data.length).toBe(t.frames * t.bands);
    expect(Array.from(got.data.slice(0, 4))).toEqual([11, 48, 85, 122]);
    expect(got.data[got.data.length - 1]).toBe(((got.data.length - 1) * 37 + 11) % 256);
    // URL のチャンネルの綴り
    expect(
      editSpecUrl('ab', '0123456789abcdef0123456789abcdef', got.channel, got.chunk).split('/')[7],
    ).toBe(t.token);
  });

  it('壊れたタイルは読まない', () => {
    const t = vector.tiles[0];
    if (!t) throw new Error('ベクタが空です');
    const bad: ArrayBuffer[] = [
      bytesOf(t.hex).slice(0, SPEC_TILE_HEADER - 1),
      bytesOf(t.hex).slice(0, bytesOf(t.hex).byteLength - 1),
      mutate(t.hex, 0, (v) => v.setUint8(0, 0x6a)),
      mutate(t.hex, 4, (v) => v.setUint16(0, 2, true)),
      mutate(t.hex, 12, (v) => v.setUint32(0, 500, true)),
      mutate(t.hex, 12, (v) => v.setUint32(0, 128, true)),
      mutate(t.hex, 16, (v) => v.setUint32(0, 256, true)),
      mutate(t.hex, 22, (v) => v.setUint16(0, 2, true)),
      mutate(t.hex, 24, (v) => v.setBigUint64(0, 1537n, true)),
      mutate(t.hex, 24, (v) => v.setBigUint64(0, 1n << 60n, true)),
      mutate(t.hex, 32, (v) => v.setUint32(0, 0, true)),
      mutate(t.hex, 36, (v) => v.setUint32(0, 1, true)),
    ];
    for (const buf of bad) expect(parseEditSpecTile(buf)).toBeNull();
  });

  it('確かめは 1 つずつ効く（ほかの確かめに隠れない）', () => {
    const t = vector.tiles[0];
    if (!t) throw new Error('ベクタが空です');
    // 列の数 0（頭だけで長さはちょうど）
    const empty = bytesOf(t.hex).slice(0, SPEC_TILE_HEADER);
    new DataView(empty).setUint32(32, 0, true);
    expect(parseEditSpecTile(empty)).toBeNull();
    // N = 128 で hop = 32（hop は N/4 のまま、N だけが下限より小さい）
    const small = bytesOf(t.hex);
    new DataView(small).setUint32(12, 128, true);
    new DataView(small).setUint32(16, 32, true);
    expect(parseEditSpecTile(small)).toBeNull();
    // 1 チャンクの列の数を超える（hop 128 なら 512 列まで）
    const many = bytesOf(t.hex);
    new DataView(many).setUint32(32, 513, true);
    expect(parseEditSpecTile(many)).toBeNull();
  });

  it('dB・チャンクの範囲・置き場の区間', () => {
    expect(specDb(0)).toBe(-120);
    expect(specDb(240)).toBe(0);
    expect(specDb(255)).toBe(7.5);
    expect(specChunkRange(5, 65_536 * 2 + 1, 65_536 * 3)).toEqual([0, 3]);
    expect(specChunkRange(65_536, 65_536 * 9, 65_536 * 2 + 3)).toEqual([1, 3]);
    expect(specChunkRange(-5, 0, 100)).toEqual([0, 0]);
    expect(isReady([{ start: 2, end: 4 }], 3)).toBe(true);
    expect(isReady([{ start: 2, end: 4 }], 4)).toBe(false);
  });

  it('手元の表は上限を超えたら長く使っていないものから捨てる', () => {
    const tile = (chunk: number): SpecTile => ({
      channel: { type: 'sum' },
      rate: 8_000,
      n: 512,
      hop: 128,
      bands: 10,
      chunk,
      firstFrame: chunk * 512,
      frames: 1,
      data: new Uint8Array(60),
    });
    const cache = new SpecTileCache(300);
    for (let c = 0; c < 3; c++) cache.put('k', tile(c));
    expect(cache.size).toBe(3);
    expect(cache.get('k', { type: 'sum' }, 0)).toBeDefined();
    cache.put('k', tile(3));
    expect(cache.get('k', { type: 'sum' }, 1)).toBeUndefined();
    expect(cache.get('k', { type: 'sum' }, 0)).toBeDefined();
    expect(cache.get('k', { type: 'index', index: 0 }, 0)).toBeUndefined();
    expect(cache.bytes).toBeLessThanOrEqual(300);
    cache.clear();
    expect(cache.size).toBe(0);
  });
});
