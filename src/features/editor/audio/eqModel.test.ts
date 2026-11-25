import { describe, expect, it } from 'vitest';

import { EQ_LIMITS } from '../../../ipc/edit';
import {
  approxCompatible,
  bandPoint,
  bandwidthOct,
  clampFreq,
  clampGain,
  clampQ,
  dbText,
  freqText,
  newBell,
  qFromHandle,
  qHandles,
  qOfBandwidth,
  readEq,
  soloBandOf,
} from './eqModel';

describe('イコライザの引数を画面で扱う', () => {
  it('引数を読む（足りない欄は EQ_LIMITS の既定）', () => {
    const p = readEq({
      bands: [
        { type: 'lowCut', freq: 80 },
        { type: 'x', freq: 'a' },
      ],
      phase: 'minimum',
    });
    expect(p.bands[0]).toEqual({
      type: 'lowCut',
      freq: 80,
      gainDb: 0,
      q: EQ_LIMITS.defaultQ,
      slope: EQ_LIMITS.defaultSlope,
      channel: 'all',
      enabled: true,
    });
    expect(p.bands[1]?.type).toBe('bell');
    expect(p.bands[1]?.freq).toBe(1_000);
    expect(p.gainDb).toBe(0);
    expect(readEq(undefined)).toEqual({ bands: [], phase: 'minimum', gainDb: 0 });
  });

  it('範囲に収める（周波数はレートの 0.49 まで・シェルフの Q は 4 まで）', () => {
    expect(clampFreq(5, 48_000)).toBe(EQ_LIMITS.freqMin);
    expect(clampFreq(30_000, 48_000)).toBeCloseTo(48_000 * 0.49, 9);
    expect(clampGain(50)).toBe(30);
    expect(clampGain(-50)).toBe(-30);
    expect(clampQ(10, 'lowShelf')).toBe(4);
    expect(clampQ(10, 'bell')).toBe(10);
    expect(clampQ(0.01, 'bell')).toBe(EQ_LIMITS.qMin);
  });

  it('点の位置：ベル・シェルフ・チルトは利得、カット・ノッチ・バンドパスは 0 dB の線', () => {
    expect(bandPoint(newBell(1_000, -5))).toEqual({ freq: 1_000, db: -5 });
    expect(bandPoint({ ...newBell(100, 6), type: 'lowCut' })).toEqual({ freq: 100, db: 0 });
    expect(bandPoint({ ...newBell(2_000, 3), type: 'tilt' })).toEqual({ freq: 2_000, db: 3 });
  });

  it('Q と帯の幅は往復し、Q の印を動かすと Q が変わる（Q を使わない型は印なし）', () => {
    expect(qOfBandwidth(bandwidthOct(2))).toBeCloseTo(2, 9);
    const b = newBell(1_000, 0, 1.4);
    const h = qHandles(b);
    expect(h).not.toBeNull();
    expect(qFromHandle(b, h?.hi ?? 0)).toBeCloseTo(1.4, 6);
    expect(qFromHandle(b, 1_000 * Math.pow(2, 0.25))).toBeGreaterThan(1.4);
    expect(qHandles({ ...b, type: 'highCut' })).toBeNull();
  });

  it('「前 ＋ 曲線」で後を描けるのは、オンのバンドのチャンネルが表示と合うときだけ', () => {
    const all = newBell(1_000, 3);
    const mid = { ...newBell(1_000, 3), channel: 'mid' as const };
    expect(approxCompatible([all], { type: 'sum' })).toBe(true);
    expect(approxCompatible([all, mid], { type: 'sum' })).toBe(false);
    expect(approxCompatible([all, mid], { type: 'mid' })).toBe(true);
    expect(approxCompatible([{ ...mid, enabled: false }], { type: 'sum' })).toBe(true);
    expect(approxCompatible([{ ...all, channel: 'left' }], { type: 'index', index: 0 })).toBe(true);
    expect(approxCompatible([{ ...all, channel: 'left' }], { type: 'index', index: 1 })).toBe(
      false,
    );
  });

  it('ソロの帯（M17 段 6）：ベル・ノッチ・バンドパスは中心と Q、シェルフ・カットは境で 1 オクターブ、チルトはなし', () => {
    const bell = newBell(2_800, -5, 6);
    expect(soloBandOf(bell)).toEqual({ freq: 2_800, q: 6 });
    expect(soloBandOf({ ...bell, type: 'notch' })).toEqual({ freq: 2_800, q: 6 });
    expect(soloBandOf({ ...bell, type: 'bandPass' })).toEqual({ freq: 2_800, q: 6 });
    for (const type of ['lowShelf', 'highShelf', 'lowCut', 'highCut'] as const) {
      const solo = soloBandOf({ ...bell, type });
      expect(solo?.freq).toBe(2_800);
      expect(bandwidthOct(solo?.q ?? 0)).toBeCloseTo(1, 9);
    }
    expect(soloBandOf({ ...bell, type: 'tilt' })).toBeNull();
  });

  it('書き方：「2.80 kHz」「−5.0 dB」', () => {
    expect(freqText(2_800)).toBe('2.80 kHz');
    expect(dbText(-5)).toBe('−5.0 dB');
    expect(dbText(3.25)).toBe('3.3 dB');
    expect(dbText(-0.01)).toBe('0.0 dB');
  });
});
