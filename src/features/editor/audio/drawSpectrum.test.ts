import { describe, expect, it } from 'vitest';

import { DEFAULT_SPECTRUM_COLORS, drawSpectrum, type SpectrumScene } from './drawSpectrum';
import { freqAxis, specBands } from './spectrumMath';

/** 呼んだ口を書き残す 2D の文脈のまね（プロパティは受けるだけ）。 */
function recording() {
  const calls: [string, unknown[]][] = [];
  const ctx = new Proxy(
    {},
    {
      get:
        (_t, key) =>
        (...args: unknown[]) => {
          calls.push([String(key), args]);
        },
      set: (_t, key, value) => {
        calls.push([`set:${String(key)}`, [value]]);
        return true;
      },
    },
  ) as unknown as Parameters<typeof drawSpectrum>[0];
  return { ctx, calls };
}

function scene(over: Partial<SpectrumScene> = {}): SpectrumScene {
  const bands = specBands(48_000);
  return {
    width: 800,
    height: 400,
    dpr: 1,
    inner: { x: 40, y: 40, w: 700, h: 300 },
    axis: freqAxis(48_000),
    spec: { top: 0, bottom: -90 },
    eq: { top: 12, bottom: -12 },
    bands,
    post: new Float32Array(bands.length).fill(-40),
    pre: new Float32Array(bands.length).fill(-45),
    hold: new Float32Array(bands.length).fill(-30),
    dimPost: false,
    eqLayer: null,
    colors: { ...DEFAULT_SPECTRUM_COLORS, post: 'P', pre: 'Q', hold: 'H', accent: 'A' },
    font: '11px sans-serif',
    peak: null,
    ...over,
  };
}

describe('スペクトルの面を描く', () => {
  it('目盛り・後の塗りと縁・前の実線・最大の保持の点線を描き、目盛りの数は左がイコライザ、右が dBFS', () => {
    const { ctx, calls } = recording();
    drawSpectrum(ctx, scene());
    const texts = calls.filter(([k]) => k === 'fillText').map(([, a]) => a[0]);
    expect(texts).toEqual(expect.arrayContaining(['20', '1k', '20k', '+12', '0', '−12', '−90']));
    const styles = calls
      .filter(([k]) => k.startsWith('set:'))
      .map(([k, a]) => `${k}=${String(a[0])}`);
    expect(styles).toEqual(
      expect.arrayContaining(['set:fillStyle=P', 'set:strokeStyle=Q', 'set:strokeStyle=H']),
    );
    // 最大の保持は点線
    expect(calls.some(([k, a]) => k === 'setLineDash' && (a[0] as number[]).length === 2)).toBe(
      true,
    );
  });

  it('後を薄く出すとき・値がないとき・描く領域がないとき', () => {
    const dim = recording();
    drawSpectrum(dim.ctx, scene({ dimPost: true }));
    expect(dim.calls.some(([k, a]) => k === 'set:globalAlpha' && (a[0] as number) < 1)).toBe(true);
    const empty = recording();
    drawSpectrum(empty.ctx, scene({ post: null, pre: null, hold: null }));
    expect(empty.calls.filter(([k]) => k === 'fill')).toHaveLength(0);
    const none = recording();
    drawSpectrum(none.ctx, scene({ inner: { x: 0, y: 0, w: 0, h: 0 } }));
    expect(none.calls.filter(([k]) => k === 'fillText')).toHaveLength(0);
  });
});
