import { describe, expect, it } from 'vitest';

import vectorText from '../../../../crates/kura-core/tests/vectors/edit/spec_bands.json?raw';
import {
  curveAt,
  dbToY,
  decayToward,
  findPeak,
  freqAxis,
  freqParts,
  freqToX,
  holdMax,
  noteOf,
  specBands,
  tickLabel,
  tiltDb,
  xToFreq,
  yToDb,
} from './spectrumMath';

const vector = JSON.parse(vectorText) as {
  rates: { rate: number; bands: [number, number, boolean][] }[];
};

describe('スペクトルの面の計算', () => {
  it.each(vector.rates)('帯の端は Rust と同じ（$rate Hz）', ({ rate, bands }) => {
    const got = specBands(rate);
    expect(got.length).toBe(bands.length);
    got.forEach((b, k) => {
      const [lo, hi, long] = bands[k] ?? [0, 0, false];
      expect(Math.abs(b.lo - lo) / lo).toBeLessThan(1e-12);
      expect(Math.abs(b.hi - hi) / hi).toBeLessThan(1e-12);
      expect(b.long).toBe(long);
      expect(b.center).toBeCloseTo(Math.sqrt(lo * hi), 9);
    });
  });

  it('48 kHz は 491 帯で最後はナイキスト。0 Hz は空', () => {
    const b = specBands(48_000);
    expect(b.length).toBe(491);
    expect(b[b.length - 1]?.hi).toBe(24_000);
    expect(specBands(0)).toEqual([]);
  });

  it('対数の軸と dB の写しは往復する', () => {
    const axis = freqAxis(48_000);
    expect(axis).toEqual({ lo: 20, hi: 24_000 });
    expect(freqToX(20, axis, 600)).toBeCloseTo(0, 9);
    expect(freqToX(24_000, axis, 600)).toBeCloseTo(600, 9);
    expect(xToFreq(freqToX(1_000, axis, 600), axis, 600)).toBeCloseTo(1_000, 6);
    const range = { top: 0, bottom: -90 };
    expect(dbToY(-45, range, 300)).toBeCloseTo(150, 9);
    expect(yToDb(dbToY(-12.5, range, 300), range, 300)).toBeCloseTo(-12.5, 9);
  });

  it('傾きは 1 kHz で 0、1 オクターブ上で +dB/oct', () => {
    expect(tiltDb(1_000, 4.5)).toBe(0);
    expect(tiltDb(2_000, 4.5)).toBeCloseTo(4.5, 9);
    expect(tiltDb(250, 4.5)).toBeCloseTo(-9, 9);
    expect(tiltDb(5_000, 0)).toBe(0);
  });

  it('落ちる速さ：上がるときはすぐ、下がるときは dB/s × 秒まで。保持は最大', () => {
    const shown = new Float32Array([-10, -10, -40]);
    decayToward(shown, [-5, -40, -60], 20, 0.5);
    expect(Array.from(shown)).toEqual([-5, -20, -50]);
    const hold = new Float32Array([-30, -30, -30]);
    holdMax(hold, [-40, -10, -30]);
    expect(Array.from(hold)).toEqual([-30, -10, -30]);
  });

  it('山：周りより 3 dB 以上高い極大のうち最も高いもの', () => {
    const bands = specBands(48_000);
    const db = new Float32Array(bands.length).fill(-60);
    // 1 kHz あたりに 10 dB の山、少し上に 2 dB だけの小さな出っ張り
    const k = bands.findIndex((b) => b.center >= 1_000);
    db[k] = -50;
    db[k + 4] = -58;
    const peak = findPeak(bands, db, 1_050);
    expect(peak?.band).toBe(k);
    expect(peak?.db).toBe(-50);
    // 平らなら山はない・遠ければ見つからない
    expect(findPeak(bands, new Float32Array(bands.length).fill(-60), 1_000)).toBeNull();
    expect(findPeak(bands, db, 5_000)).toBeNull();
  });

  it('音名とセント（A4 = 440 Hz）', () => {
    expect(noteOf(440)).toEqual({ name: 'A4', cents: 0 });
    expect(noteOf(261.6256)).toEqual({ name: 'C4', cents: 0 });
    expect(noteOf(450)?.name).toBe('A4');
    expect(noteOf(450)?.cents).toBe(39);
    expect(noteOf(0)).toBeNull();
  });

  it('周波数の書き方と目盛り', () => {
    expect(freqParts(2_800)).toEqual({ value: '2.80', unit: 'kHz' });
    expect(freqParts(12_500)).toEqual({ value: '12.5', unit: 'kHz' });
    expect(freqParts(440)).toEqual({ value: '440', unit: 'Hz' });
    expect(freqParts(62.5)).toEqual({ value: '62.5', unit: 'Hz' });
    expect(tickLabel(2_000)).toBe('2k');
    expect(tickLabel(50)).toBe('50');
  });

  it('曲線を対数の周波数で補う（格子の外は端、null は null）', () => {
    const freqs = [100, 1_000, 10_000];
    const values = [0, 6, 0];
    expect(curveAt(freqs, values, 100)).toBe(0);
    expect(curveAt(freqs, values, Math.sqrt(100 * 1_000))).toBeCloseTo(3, 9);
    expect(curveAt(freqs, values, 50)).toBe(0);
    expect(curveAt(freqs, values, 20_000)).toBe(0);
    expect(curveAt(freqs, [0, null, 0], 500)).toBeNull();
    expect(curveAt([], [], 100)).toBeNull();
  });
});
