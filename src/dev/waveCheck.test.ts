import { describe, expect, it, vi } from 'vitest';

import { type AudioRuntime, setAudioRuntimeForTest } from '../features/asset/audioRuntime';
import type { EditorPlayer, PlacedRecord } from '../features/editor/audio/playback';
import { ContextPool } from '../lib/assetAudio';
import { fakeFactory } from '../test/fakeAudio';
import { runEditorSeamCheck } from './seamCheck';
import { benchSteps, contextCheck, createAudioCheck, RoundTrips, stepKind } from './waveCheck';

vi.mock('./seamCheck', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./seamCheck')>()),
  runEditorSeamCheck: vi.fn(async () => null),
  runCrossfadeCheck: vi.fn(async () => null),
}));
vi.mock('../ipc/bindings', async (importOriginal) => {
  const real = await importOriginal<typeof import('../ipc/bindings')>();
  return {
    ...real,
    commands: { ...real.commands, benchReport: vi.fn(async () => ({ status: 'ok', data: null })) },
  };
});

describe('文脈の自己点検', () => {
  it('ok はエディタの文脈がプールに残っているかで決める（close の直後に state を読まない）', () => {
    for (const held of [true, false]) {
      const factory = fakeFactory();
      const pool = new ContextPool(factory.create, 3);
      setAudioRuntimeForTest({ pool } as unknown as AudioRuntime);
      const ctx = pool.get(48_000);
      const release = held ? pool.hold(48_000) : () => undefined;
      const player = { context: ctx, playing: true } as unknown as EditorPlayer;
      const report = contextCheck(player) as { ok: boolean; held: boolean };
      expect(report.held).toBe(held);
      // 印がなければ、ほかのレートの文脈を 3 つ作ると追い出される
      expect(report.ok).toBe(held);
      release();
      setAudioRuntimeForTest(null);
    }
  });
});

describe('往復の時間', () => {
  it('手を送ってから表示が届くまでを 20 回ためて、手の種類ごとにまとめる', () => {
    const trips = new RoundTrips();
    expect(trips.shown(99, 5)).toBeNull();
    let done = null;
    for (let i = 0; i < 20; i++) {
      trips.sent(i, i % 2 ? 'handle:fadeIn' : 'commit:setParam', i * 100);
      done = trips.shown(i, i * 100 + 10 + (i % 2));
    }
    expect(done?.count).toBe(20);
    expect(done?.all.max).toBe(11);
    expect(Object.keys(done?.kinds ?? {}).sort()).toEqual(['commit:setParam', 'handle:fadeIn']);
    expect(done?.kinds['handle:fadeIn']?.p50).toBe(11);
  });

  it('手の種類の名前', () => {
    expect(
      stepKind([
        { type: 'seal' },
        { type: 'handle', handle: { type: 'loop', range: null }, node: null, mode: 'step' },
      ]),
    ).toBe('handle:loop');
    expect(stepKind([{ type: 'undo' }])).toBe('undo');
    expect(stepKind([])).toBe('view');
  });
});

describe('描画を測る操作', () => {
  it('パン・拡大・パン・縮小を 30 フレームずつ', () => {
    const steps = benchSteps(120);
    expect(steps[0]).toEqual({ kind: 'pan', dx: 40 });
    expect(steps[30]).toEqual({ kind: 'zoom', factor: 1.08 });
    expect(steps[60]).toEqual({ kind: 'pan', dx: -40 });
    expect(steps[90]?.kind).toBe('zoom');
  });
});

describe('置いた区間を窓にまとめる', () => {
  function rec(abs: number, len: number, fadeIn = false): PlacedRecord {
    return {
      source: 'edited',
      rate: 8000,
      abs,
      start: 0,
      end: len,
      cycle: len,
      planes: [new Float32Array(len)],
      fadeIn,
      level: 1,
    };
  }

  it('続けて置いた区間をためて照らし、間が空いた・交差フェードで入れた所で区切る', () => {
    const check = createAudioCheck();
    check.placed?.(rec(0, 10));
    check.placed?.(rec(10, 10));
    check.placed?.(rec(50, 10));
    expect(runEditorSeamCheck).toHaveBeenCalledTimes(1);
    expect(vi.mocked(runEditorSeamCheck).mock.calls[0]?.[0].map((r) => r.abs)).toEqual([0, 10]);
    check.placed?.(rec(60, 10));
    check.placed?.(rec(70, 10, true));
    expect(runEditorSeamCheck).toHaveBeenCalledTimes(2);
    expect(vi.mocked(runEditorSeamCheck).mock.calls[1]?.[0].map((r) => r.abs)).toEqual([50, 60]);
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

describe('音声エディタの自己点検は通常のビルドに入らない', () => {
  it('読み込むのは audioStore.ts の VITE_KURA_CHECK の枝の動的な import だけ', () => {
    const users = sources
      .filter(([, text]) => /(?:from|import\()\s*'[^']*dev\/waveCheck'/.test(text))
      .map(([path]) => path);
    expect(users).toEqual(['/src/features/editor/audio/audioStore.ts']);
    const text =
      sources.find(([path]) => path === '/src/features/editor/audio/audioStore.ts')?.[1] ?? '';
    expect(text).toMatch(
      /if \(import\.meta\.env\.VITE_KURA_CHECK === '1'\) \{\s*void import\('\.\.\/\.\.\/\.\.\/dev\/waveCheck'\)/,
    );
    expect(text).not.toMatch(/^import .*dev\/waveCheck/m);
  });
});
