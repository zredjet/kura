import { describe, expect, it } from 'vitest';

import { drawWave } from './drawWave';
import { audioView, fakeClock, settle, tileBytes } from './testAudioView';
import { WaveController } from './waveController';

/** 呼んだ口を書き残す 2D の文脈のまね（プロパティは受けるだけ）。 */
function recordingCanvas() {
  const calls: [string, unknown[]][] = [];
  const ctx = new Proxy(
    {},
    {
      get: (_t, key) => {
        if (key === 'canvas') return undefined;
        return (...args: unknown[]) => {
          calls.push([String(key), args]);
        };
      },
      set: (_t, key, value) => {
        calls.push([`set:${String(key)}`, [value]]);
        return true;
      },
    },
  );
  const canvas = {
    width: 1000,
    height: 300,
    getContext: () => ctx,
  } as unknown as HTMLCanvasElement;
  return { canvas, calls };
}

describe('波形を描く', () => {
  it('段の列・L R の印・ループ・選択・切り詰めの外・フェード・カーソル・目盛り・全体図の枠を描く', async () => {
    const clock = fakeClock();
    const total = 48_000 * 10;
    const c = new WaveController({
      scheduler: clock.scheduler,
      fetch: async () => tileBytes(65536, 0, total, 2, 1000),
    });
    c.resize(1000, 300, 1);
    c.setSession('ab');
    c.applyView(audioView({ frames: total, peaks: { frames: total, rev: 1 } }));
    await settle();
    c.setOverlay({
      cursor: 1000,
      selection: { start: 0, end: 48_000 },
      trim: { start: 48_000, end: 96_000 },
      fade: {
        inStart: 0,
        inEnd: 4800,
        outStart: total - 4800,
        outEnd: total,
        curve: 'equalPower',
        handles: true,
      },
      loop: { start: 96_000, end: 192_000 },
      loopFlags: true,
      markers: [{ pos: 10, name: 'a' }],
    });
    const { canvas, calls } = recordingCanvas();
    drawWave(canvas, c.scene());
    const names = calls.map(([n]) => n);
    expect(names.filter((n) => n === 'fillRect').length).toBeGreaterThan(500);
    const texts = calls.filter(([n]) => n === 'fillText').map(([, a]) => a[0]);
    expect(texts).toContain('L');
    expect(texts).toContain('R');
    expect(texts).toContain('0:01');
    expect(names).toContain('strokeRect');
    expect(names).toContain('stroke');
  });

  it('フェードの見かけの始まり・終わりが null なら曲線を描かず三角だけ。負の見かけの始まりは頭のゲインから引く', async () => {
    const clock = fakeClock();
    const total = 48_000 * 10;
    const c = new WaveController({
      scheduler: clock.scheduler,
      fetch: async () => tileBytes(65536, 0, total, 2, 1000),
    });
    c.resize(1000, 300, 1);
    c.setSession('ab');
    c.applyView(audioView({ frames: total, peaks: { frames: total, rev: 1 } }));
    await settle();
    const overlay = (inStart: number | null, outEnd: number | null) => ({
      cursor: null,
      selection: null,
      trim: null,
      fade: {
        inStart,
        inEnd: 4800,
        outStart: total - 4800,
        outEnd,
        curve: 'linear',
        handles: true,
      },
      loop: null,
      loopFlags: false,
      markers: [],
    });
    c.setOverlay(overlay(null, null));
    const none = recordingCanvas();
    drawWave(none.canvas, c.scene());
    const names = none.calls.map(([n]) => n);
    expect(names).not.toContain('stroke');
    expect(names.filter((n) => n === 'fill').length).toBe(2);
    // 見かけの始まり −4,800（頭のゲインは 1/2）：曲線の最初の点は段の下ではなく、半分の高さ
    c.setOverlay(overlay(-4800, null));
    const half = recordingCanvas();
    drawWave(half.canvas, c.scene());
    expect(half.calls.filter(([n]) => n === 'stroke').length).toBe(1);
    // 曲線の点（stroke の前の moveTo とそのあとの lineTo）
    const strokeAt = half.calls.findIndex(([n]) => n === 'stroke');
    const before = half.calls.slice(0, strokeAt);
    const startAt = before.map(([n]) => n).lastIndexOf('moveTo');
    const path = before.slice(startAt).filter(([n]) => n === 'moveTo' || n === 'lineTo');
    const scene = c.scene();
    const lanes = scene.layout.lanes;
    const top = scene.inner.y + (lanes[0]?.y ?? 0);
    const lastLane = lanes[lanes.length - 1];
    const bottom = scene.inner.y + (lastLane ? lastLane.y + lastLane.h : 0);
    const [x0, y0] = (path[0]?.[1] ?? []) as number[];
    const [x1, y1] = (path[path.length - 1]?.[1] ?? []) as number[];
    // 最初の点は出力の頭（x は面の左端）でゲイン 1/2、最後の点は三角の位置でゲイン 1
    expect(x0).toBeCloseTo(scene.inner.x, 6);
    expect(y0).toBeCloseTo(bottom - (bottom - top) / 2, 6);
    expect(x1).toBeCloseTo(scene.inner.x + (4800 / total) * scene.inner.w, 6);
    expect(y1).toBeCloseTo(top, 6);
  });

  it('文脈が取れなければ何もしない', () => {
    const canvas = { getContext: () => null } as unknown as HTMLCanvasElement;
    const c = new WaveController();
    expect(() => drawWave(canvas, c.scene())).not.toThrow();
  });
});
