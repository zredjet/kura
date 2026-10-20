import { describe, expect, it } from 'vitest';

import { planAudition } from '../lib/asset/audition';
import type { Pcm } from '../lib/asset/wav';
import {
  compareSamples,
  expectedSamples,
  type RenderWindow,
  runSeamCheck,
  type SeamWindow,
  seamWindows,
} from './seamCheck';

const R = 8_000;

/** 区間 [from, to) の PCM（標本の値はフレームの番号から決める。チャンネルごとに違う）。 */
function pcm(from: number, to: number, channels = 2): Pcm {
  const frames = to - from;
  return {
    rate: R,
    channels,
    frames,
    format: 'f32',
    planes: Array.from({ length: channels }, (_, ch) =>
      Float32Array.from({ length: frames }, (_, i) => ((from + i) % 1000) / 1000 + ch),
    ),
    info: { start: from, total: R * 20, loop: null, decoder: 'test' },
  } as Pcm;
}

/** 本物の組み立ての意味（loop・始める時刻・バッファの中の始まり）で描く偽物。`glitch` のフレームだけ値をずらす。 */
function idealRender(glitch: number[] = []): RenderWindow {
  return async (window, pcms, _rate, channels) => {
    const out = expectedSamples(window, pcms, channels);
    for (const k of glitch) {
      const plane = out[0];
      if (plane && k < plane.length) plane[k] = (plane[k] ?? 0) + 1e-3;
    }
    return out;
  };
}

describe('継ぎ目の自己点検（VITE_KURA_CHECK）', () => {
  it('本体だけ：本物と同じく継ぎ目の 3 秒前から、継ぎ目のあと 1 秒まで', () => {
    const plan = planAudition({ frames: R * 20, rate: R, channels: 2, loop: [R, R * 11] });
    if (!plan) throw new Error('計画がない');
    const [w] = seamWindows(plan, [R * 10], R);
    expect(w).toEqual({
      parts: [{ pcm: 0, loop: true, at: 0, offset: R * 7 }],
      frames: R * 4,
      seams: [R * 3],
    });
  });

  it('短いループは窓の中の折り返しをすべて数える', () => {
    const plan = planAudition({ frames: 1000, rate: R, channels: 1, loop: [100, 600] });
    if (!plan) throw new Error('計画がない');
    const [w] = seamWindows(plan, [500], R);
    expect(w?.seams[0]).toBe(500);
    expect(w?.seams).toHaveLength(Math.ceil((500 + R - 500) / 500));
  });

  it('前奏つき：前奏から本体へのつなぎ目と本体の折り返し。長ければ 2 つの窓に分ける', () => {
    const plan = planAudition(
      { frames: R * 20, rate: R, channels: 2, loop: [R, R * 11] },
      { fromStart: true },
    );
    if (!plan) throw new Error('計画がない');
    const [w] = seamWindows(plan, [R, R * 10], R);
    expect(w?.parts).toEqual([
      { pcm: 0, loop: false, at: 0, offset: 0 },
      { pcm: 1, loop: true, at: R, offset: 0 },
    ]);
    expect(w?.seams).toEqual([R, R * 11]);
    // 前奏 40 秒・本体 10 秒は 30 秒の窓に収まらない
    const long = seamWindows(plan, [R * 40, R * 10], R);
    expect(long).toHaveLength(2);
    expect(long[0]?.parts).toEqual([
      { pcm: 0, loop: false, at: 0, offset: R * 39 },
      { pcm: 1, loop: true, at: R, offset: 0 },
    ]);
    expect(long[1]?.parts).toEqual([{ pcm: 1, loop: true, at: 0, offset: R * 9 }]);
  });

  it('継ぎ目だけ：手前と始まりを続けて 1 回', () => {
    const plan = planAudition(
      { frames: R * 20, rate: R, channels: 2, loop: [R, R * 11] },
      { capBytes: 1_000 },
    );
    if (plan?.mode !== 'seam') throw new Error('継ぎ目の計画でない');
    const [w] = seamWindows(plan, [62, 62], R);
    expect(w).toEqual({
      parts: [
        { pcm: 0, loop: false, at: 0, offset: 0 },
        { pcm: 1, loop: false, at: 62, offset: 0 },
      ],
      frames: 124,
      seams: [62],
    });
  });

  it('期待の標本は PCM を計画の順につないだもの（ループは本体の始まりへ戻る）', () => {
    const w: SeamWindow = {
      parts: [
        { pcm: 0, loop: false, at: 0, offset: 0 },
        { pcm: 1, loop: true, at: 2, offset: 0 },
      ],
      frames: 7,
      seams: [2, 5],
    };
    const [left] = expectedSamples(w, [pcm(0, 2, 1), pcm(2, 5, 1)], 1);
    expect([...(left ?? [])].map((v) => Math.round(v * 1000))).toEqual([0, 1, 2, 3, 4, 2, 3]);
  });

  it('1 標本でも違えば数え、最初の違いを返す（足りないチャンネルも違い）', () => {
    const a = [Float32Array.of(0, 1, 2), Float32Array.of(5, 6, 7)];
    expect(compareSamples(a, [Float32Array.of(0, 1, 2), Float32Array.of(5, 6, 7)])).toEqual({
      frames: 3,
      mismatches: 0,
      first: null,
    });
    const b = compareSamples(a, [Float32Array.of(0, 1, 2.5), Float32Array.of(5, 6.5, 7)]);
    expect(b.mismatches).toBe(2);
    expect(b.first).toEqual({ frame: 1, channel: 1, expected: 6, got: 6.5 });
    expect(compareSamples(a, [Float32Array.of(0, 1, 2)]).mismatches).toBe(3);
    expect(
      compareSamples([Float32Array.of(Number.NaN)], [Float32Array.of(Number.NaN)]).mismatches,
    ).toBe(0);
  });

  it('試聴 1 回分を照らして報告にまとめる（ずれがあれば数える）', async () => {
    const plan = planAudition({ frames: R * 20, rate: R, channels: 2, loop: [R, R * 11] });
    if (!plan) throw new Error('計画がない');
    const scheduled = { hash: 'h', plan, rate: R, pcms: [pcm(R, R * 11)] };
    let t = 0;
    const now = () => (t += 5);
    const ok = await runSeamCheck(scheduled, idealRender(), now);
    expect(ok).toMatchObject({
      hash: 'h',
      mode: 'loop',
      rate: R,
      channels: 2,
      windows: 1,
      seams: 1,
      frames: R * 4,
      mismatches: 0,
      first: null,
      ms: 5,
    });
    const bad = await runSeamCheck(scheduled, idealRender([R * 3, R * 3 + 1]), now);
    expect(bad.mismatches).toBe(2);
    expect(bad.first?.frame).toBe(R * 3);
  });
});

/** src の .ts・.tsx（テストを除く）。 */
const sources = Object.entries(
  import.meta.glob<string>(['/src/**/*.ts', '/src/**/*.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
  }),
).filter(([path]) => !/\.test\.tsx?$/.test(path));

describe('自己点検は通常のビルドに入らない', () => {
  it('読み込むのは audioRuntime.ts の VITE_KURA_CHECK の枝の動的な import だけ', () => {
    const users = sources
      .filter(([, text]) => /(?:from|import\()\s*'[^']*dev\/seamCheck'/.test(text))
      .map(([path]) => path);
    expect(users).toEqual(['/src/features/asset/audioRuntime.ts']);
    const text =
      sources.find(([path]) => path === '/src/features/asset/audioRuntime.ts')?.[1] ?? '';
    expect(text).toMatch(
      /if \(import\.meta\.env\.VITE_KURA_CHECK === '1'\) \{\s*void import\('\.\.\/\.\.\/dev\/seamCheck'\)/,
    );
    expect(text).not.toMatch(/^import .*dev\/seamCheck/m);
  });
});
