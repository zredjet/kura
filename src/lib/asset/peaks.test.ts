import { describe, expect, it } from 'vitest';

import invalidText from '../../../crates/kura-engine/tests/vectors/peaks/invalid.json?raw';
import validText from '../../../crates/kura-engine/tests/vectors/peaks/valid.json?raw';

import { columns, parsePeaks, pickLevel, regionX, waveformPath } from './peaks';

interface ValidCase {
  name: string;
  rate: number;
  channels: number;
  frames: number;
  analysisVersion: number;
  levels: { binFrames: number; bins: number; data: number[] }[];
  hex: string;
}

const valid = (JSON.parse(validText) as { cases: ValidCase[] }).cases;
const invalid = (JSON.parse(invalidText) as { cases: { name: string; hex: string }[] }).cases;

function fromHex(hex: string): ArrayBuffer {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out.buffer;
}

function mustParse(hex: string) {
  const p = parsePeaks(fromHex(hex));
  if (!p) throw new Error('読めない');
  return p;
}

// エンジンが凍結したベクタ（audio.peaks の正規の形 kura-peaks 版 1）と同じに読む
describe('波形の段を読む（エンジンと共有のベクタ）', () => {
  it('ベクタがある', () => {
    expect(valid.length).toBeGreaterThan(4);
    expect(invalid.length).toBeGreaterThan(10);
  });

  for (const c of valid) {
    it(`読める：${c.name}`, () => {
      const p = mustParse(c.hex);
      expect(p.rate).toBe(c.rate);
      expect(p.channels).toBe(c.channels);
      expect(p.frames).toBe(c.frames);
      expect(p.analysisVersion).toBe(c.analysisVersion);
      expect(
        p.levels.map((l) => ({ binFrames: l.binFrames, bins: l.bins, data: [...l.data] })),
      ).toEqual(c.levels);
    });
  }

  for (const c of invalid) {
    it(`読まない：${c.name}`, () => {
      expect(parsePeaks(fromHex(c.hex))).toBeNull();
    });
  }

  it('フレーム数が JS の整数で正確に表せないものは読まない', () => {
    const first = valid[0];
    if (!first) throw new Error('ベクタがない');
    const buf = fromHex(first.hex);
    new DataView(buf).setBigUint64(24, 2n ** 53n + 2n, true);
    expect(parsePeaks(buf)).toBeNull();
  });

  it('解析の版は見ない（並びの版だけを見る）', () => {
    const first = valid[0];
    if (!first) throw new Error('ベクタがない');
    const buf = fromHex(first.hex);
    new DataView(buf).setUint16(10, 9, true);
    expect(parsePeaks(buf)?.analysisVersion).toBe(9);
  });
});

describe('波形を描く', () => {
  const p = mustParse(valid.find((c) => c.name === 'default-five-levels')?.hex ?? '');

  it('描く幅以上のビンを持つ段のうち、いちばん粗いもの（なければいちばん細かい段）', () => {
    expect(pickLevel(p, p.frames, 1)?.binFrames).toBe(65_536);
    expect(pickLevel(p, p.frames, 2)?.binFrames).toBe(65_536);
    expect(pickLevel(p, p.frames, 3)?.binFrames).toBe(16_384);
    expect(pickLevel(p, p.frames, 272)?.binFrames).toBe(256);
    expect(pickLevel(p, p.frames, 600)?.binFrames).toBe(256);
    // 区間が短ければ細かい段
    expect(pickLevel(p, 4_096, 16)?.binFrames).toBe(256);
    expect(pickLevel(p, 16_384, 16)?.binFrames).toBe(1_024);
  });

  it('列ごとに掛かるビンの min の min・max の max（チャンネルもまとめる）', () => {
    const stereo = mustParse(valid.find((c) => c.name === 'three-channels')?.hex ?? '');
    const level = stereo.levels[0];
    if (!level) throw new Error('段がない');
    const d = [...level.data];
    // 3 ビン × 3 チャンネル。列 1 つなら全体の min・max
    const all = [...columns(level, 3, stereo.frames, 0, stereo.frames, 1)];
    const mins = d.filter((_, i) => i % 2 === 0);
    const maxs = d.filter((_, i) => i % 2 === 1);
    expect(all).toEqual([Math.min(...mins), Math.max(...maxs)]);
    // 列がビンより細かいとき：同じビンの値が続く
    const fine = [...columns(level, 3, stereo.frames, 0, 256, 4)];
    const bin0 = [
      Math.min(d[0] ?? 0, d[2] ?? 0, d[4] ?? 0),
      Math.max(d[1] ?? 0, d[3] ?? 0, d[5] ?? 0),
    ];
    expect(fine).toEqual([...bin0, ...bin0, ...bin0, ...bin0]);
  });

  it('区間の外・空の区間・列 0 は 0 の列', () => {
    const level = p.levels[0];
    if (!level) throw new Error('段がない');
    expect([...columns(level, 1, p.frames, 0, 0, 3)]).toEqual([0, 0, 0, 0, 0, 0]);
    expect([...columns(level, 1, p.frames, p.frames, p.frames + 100, 2)]).toEqual([0, 0, 0, 0]);
    expect(columns(level, 1, p.frames, 0, p.frames, 0)).toHaveLength(0);
  });

  it('NaN だけのビンは 0、端に止まった値はそのまま', () => {
    const q = mustParse(valid.find((c) => c.name === 'nan-inf-and-clip')?.hex ?? '');
    const level = q.levels[0];
    if (!level) throw new Error('段がない');
    // ビン 0 は ±1.5（端に止まる）、ビン 1 は ±∞、ビン 2（512〜599）は全部が NaN
    expect([...columns(level, 1, q.frames, 0, 256, 1)]).toEqual([-32768, 32767]);
    expect([...columns(level, 1, q.frames, 256, 512, 1)]).toEqual([-32768, 32767]);
    expect([...columns(level, 1, q.frames, 512, 600, 2)]).toEqual([0, 0, 0, 0]);
  });

  it('path は列の数だけ縦の線を引き、無音にも 1 単位の線を引く', () => {
    const d = waveformPath(new Int16Array([0, 0, -32768, 32767]), 20);
    expect(d).toBe('M0.5 9.50V10.50M1.5 0.00V20.00');
    expect(waveformPath(new Int16Array([]), 48)).toBe('');
  });

  it('ループの区間の矩形（描く範囲で切る）', () => {
    expect(regionX(250, 750, 0, 1000, 200)).toEqual({ x: 50, width: 100 });
    expect(regionX(0, 2000, 500, 1500, 100)).toEqual({ x: 0, width: 100 });
    expect(regionX(1500, 2000, 0, 1000, 100)).toBeNull();
    expect(regionX(10, 10, 0, 1000, 100)).toBeNull();
    expect(regionX(0, 10, 0, 0, 100)).toBeNull();
  });
});
