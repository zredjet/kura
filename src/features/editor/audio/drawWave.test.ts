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
      fade: { inEnd: 4800, outStart: total - 4800, curve: 'equalPower', handles: true },
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

  it('文脈が取れなければ何もしない', () => {
    const canvas = { getContext: () => null } as unknown as HTMLCanvasElement;
    const c = new WaveController();
    expect(() => drawWave(canvas, c.scene())).not.toThrow();
  });
});
