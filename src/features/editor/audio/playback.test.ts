import { describe, expect, it, vi } from 'vitest';

import { editAuditionPcmUrl, editInputPcmUrl, editPcmUrl } from '../../../ipc/media';
import { ContextPool } from '../../../lib/assetAudio';
import { fakeFactory, type FakeContext, type FakeGain } from '../../../test/fakeAudio';
import {
  CROSSFADE_SECONDS,
  EditorPlayer,
  LEAD_SECONDS,
  loopCut,
  MAX_CHUNK_FAILURES,
  MAX_PLACE_PER_FILL,
  neededSpans,
  PcmFeed,
  type PlaySource,
  planSegments,
  playbackChange,
  playsAudition,
  SWITCH_AHEAD_SECONDS,
  SWITCH_MIN_MS,
} from './playback';
import { CHUNK, FAIL_RETRY_MS, type PcmChunk, PcmChunkCache } from './pcmChunks';
import { fakeFetch, pcmBytes, settle } from './testAudioView';

const R = 48_000;

describe('鳴らす区間の計画', () => {
  it('チャンクの境で切り、累積の位置を付ける', () => {
    const segs = planSegments({ from: 1000, frames: CHUNK * 3, horizon: CHUNK * 2 });
    expect(segs).toEqual([
      { start: 1000, end: CHUNK, at: 0 },
      { start: CHUNK, end: CHUNK * 2, at: CHUNK - 1000 },
      { start: CHUNK * 2, end: CHUNK * 3, at: CHUNK * 2 - 1000 },
    ]);
  });

  it('選択の終わり・音の終わりで止まる', () => {
    expect(planSegments({ from: 10, frames: 1000, end: 500, horizon: 1e9 })).toEqual([
      { start: 10, end: 500, at: 0 },
    ]);
    expect(planSegments({ from: 10, frames: 100, horizon: 1e9 })).toEqual([
      { start: 10, end: 100, at: 0 },
    ]);
    expect(planSegments({ from: 100, frames: 100, horizon: 1e9 })).toEqual([]);
  });

  it('ループを試聴：終わりのあとに始めから続け、地平で打ち切る（外から始めたら始めから）', () => {
    const segs = planSegments({
      from: 300,
      frames: 1000,
      loop: { start: 200, end: 400 },
      horizon: 450,
    });
    expect(segs).toEqual([
      { start: 300, end: 400, at: 0 },
      { start: 200, end: 400, at: 100 },
      { start: 200, end: 400, at: 300 },
    ]);
    expect(
      planSegments({ from: 900, frames: 1000, loop: { start: 200, end: 400 }, horizon: 10 })[0],
    ).toEqual({
      start: 200,
      end: 400,
      at: 0,
    });
    // 長さ 0 のループは無いものとして扱う
    expect(planSegments({ from: 0, frames: 10, loop: { start: 5, end: 5 }, horizon: 100 })).toEqual(
      [{ start: 0, end: 10, at: 0 }],
    );
  });

  it('ループを替えたとき鳴っている区間を止める所（新しい終わり・1 周の境・今）', () => {
    // つないだバッファ（1 周 64 フレーム）：今の位置は 1,036
    const block = { start: 1000, end: 1064, abs: 0, len: 64 * 1024 };
    expect(loopCut(block, 100, { start: 1000, end: 1032 })).toEqual({ abs: 100, pos: 1000 });
    expect(loopCut(block, 100, { start: 1000, end: 1050 })).toEqual({ abs: 114, pos: 1000 });
    expect(loopCut(block, 100, { start: 1000, end: 2000 })).toEqual({ abs: 128, pos: 1064 });
    // 1 周の区間：終わりが新しいループの中なら止めない
    const seg = { start: 10_000, end: 65_536, abs: 0, len: 55_536 };
    expect(loopCut(seg, 4800, { start: 0, end: 200_000 })).toBeNull();
    expect(loopCut(seg, 4800, { start: 0, end: 30_000 })).toEqual({ abs: 20_000, pos: 0 });
  });

  it('先読みに要るチャンク（位置から 2 チャンク・ループの始め）', () => {
    expect(neededSpans(CHUNK + 5, CHUNK * 10, null)).toEqual([{ start: CHUNK, end: CHUNK * 3 }]);
    expect(neededSpans(CHUNK * 9 + 5, CHUNK * 9 + 100, null)).toEqual([
      { start: CHUNK * 9, end: CHUNK * 9 + 100 },
    ]);
    expect(neededSpans(CHUNK * 5, CHUNK * 10, { start: 10, end: 20 })).toEqual([
      { start: CHUNK * 5, end: CHUNK * 7 },
      { start: 0, end: CHUNK },
    ]);
  });
});

/** チャンクの標本（位置 f の値は f の小さな関数。継ぎ目で値がずれれば分かる）。 */
function makeChunk(index: number, total: number, channels = 2, shift = 0): PcmChunk {
  const a = index * CHUNK;
  const b = Math.min(total, a + CHUNK);
  return {
    rev: 1,
    start: a,
    frames: b - a,
    planes: Array.from({ length: channels }, (_, ch) => {
      const p = new Float32Array(b - a);
      for (let k = 0; k < p.length; k++) p[k] = (((a + k) % 1000) / 1000) * (ch + 1) + shift;
      return p;
    }),
  };
}

function setup(
  opts: {
    total?: number;
    rate?: number;
    channels?: number;
    missing?: number[];
    level?: (source: PlaySource) => number;
  } = {},
) {
  const total = opts.total ?? CHUNK * 8;
  const rate = opts.rate ?? R;
  const channels = opts.channels ?? 2;
  const factory = fakeFactory();
  const pool = new ContextPool(factory.create, 3);
  const chunks: Record<PlaySource, Map<number, PcmChunk>> = {
    edited: new Map(),
    source: new Map(),
  };
  const n = Math.ceil(total / CHUNK);
  for (let i = 0; i < n; i++) {
    if (!(opts.missing ?? []).includes(i)) chunks.edited.set(i, makeChunk(i, total, channels));
  }
  const wanted: [PlaySource, number][] = [];
  const needs: [PlaySource, unknown][] = [];
  const ended: number[] = [];
  let ms = 0;
  const released = vi.fn();
  const hold = vi.fn(() => released);
  const player = new EditorPlayer({
    pool,
    peek: (source, i) => chunks[source].get(i),
    want: (source, i) => void wanted.push([source, i]),
    need: (source, spans) => void needs.push([source, spans]),
    volume: () => 0.7,
    level: opts.level,
    hold,
    timer: { every: () => 1, clear: () => undefined },
    now: () => ms,
    onEnded: (pos) => void ended.push(pos),
  });
  const info = { rate, channels, frames: total };
  return {
    player,
    pool,
    factory,
    chunks,
    wanted,
    needs,
    ended,
    info,
    hold,
    released,
    advanceMs: (d: number) => {
      ms += d;
    },
    ctx: () => factory.contexts[0] as FakeContext,
  };
}

/** 置いた区間の [鳴り始めるフレーム, 長さ]（停止したものも含む、置いた順）。 */
function starts(ctx: FakeContext, rate: number): [number, number][] {
  return ctx.sources.map((s) => [Math.round((s.started?.[0] ?? 0) * rate), s.buffer?.length ?? 0]);
}

describe('編集中の試聴の置き方', () => {
  it('始めはフレームの境、区間はその標本だけのバッファを続けて置き、loop・loopStart・loopEnd に触れない', () => {
    const t = setup();
    expect(t.player.play({ from: 1000, source: 'edited', info: t.info })).toBe('playing');
    const ctx = t.ctx();
    expect(ctx.requested).toBe(R);
    const first = Math.ceil((ctx.currentTime + LEAD_SECONDS) * R);
    const placed = starts(ctx, R);
    expect(placed[0]).toEqual([first, CHUNK - 1000]);
    // 続けて置く：次の区間はちょうど前の区間の終わり
    for (let i = 1; i < placed.length; i++) {
      const [prevAt, prevLen] = placed[i - 1] ?? [0, 0];
      expect(placed[i]?.[0]).toBe(prevAt + prevLen);
    }
    // 時刻はフレームの境
    for (const s of ctx.sources) {
      const when = s.started?.[0] ?? 0.5;
      expect(Math.abs(when * R - Math.round(when * R))).toBeLessThan(1e-6);
      expect(s.loop).toBe(false);
      expect(s.started?.[1]).toBeUndefined();
    }
    expect(ctx.touched).toEqual([]);
    // バッファの中身は元のチャンクの切り出し
    const buf = ctx.buffers[0];
    expect(buf?.channels[0]?.[0]).toBeCloseTo((1000 % 1000) / 1000, 6);
    expect(buf?.channels[1]?.[5]).toBeCloseTo(((1005 % 1000) / 1000) * 2, 6);
    // 先読みは 3 秒
    const covered = placed.reduce((n, [, len]) => n + len, 0);
    expect(covered).toBeGreaterThanOrEqual(3 * R - Math.round(LEAD_SECONDS * R));
    expect(covered).toBeLessThan(3 * R + CHUNK);
    // 全体の音量
    expect(ctx.gains[0]?.gain.value).toBe(0.7);
  });

  it('時間が進んだら先読みを足し、位置は鳴っている区間から求める', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const before = ctx.sources.length;
    const first = Math.ceil((ctx.currentTime + LEAD_SECONDS) * R);
    ctx.currentTime += 2;
    t.player.tick();
    expect(ctx.sources.length).toBeGreaterThan(before);
    expect(t.player.position()).toBe(Math.floor(ctx.currentTime * R) - first);
  });

  it('チャンクが手元になければ取りに行き、届いてから置く（時刻が過ぎていれば遅らせる）', () => {
    const t = setup({ missing: [1] });
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    expect(ctx.sources).toHaveLength(1);
    expect(t.wanted).toContainEqual(['edited', 1]);
    expect(t.needs[0]).toEqual(['edited', [{ start: 0, end: CHUNK * 2 }]]);
    ctx.currentTime += 3;
    t.chunks.edited.set(1, makeChunk(1, t.info.frames));
    t.player.tick();
    const placed = starts(ctx, R);
    expect(placed[1]?.[0]).toBe(Math.ceil((ctx.currentTime + LEAD_SECONDS) * R));
  });

  it('選択の終わりで止まり、止まった位置を知らせて文脈の印を外す', () => {
    const t = setup();
    t.player.play({ from: 1000, end: 2000, source: 'edited', info: t.info });
    expect(t.pool.isHeld(R)).toBe(true);
    expect(t.hold).toHaveBeenCalledTimes(1);
    const ctx = t.ctx();
    expect(starts(ctx, R)).toEqual([[Math.ceil((ctx.currentTime + LEAD_SECONDS) * R), 1000]]);
    ctx.currentTime += 0.01;
    t.player.tick();
    expect(t.player.playing).toBe(true);
    ctx.currentTime += 1;
    t.player.tick();
    expect(t.player.playing).toBe(false);
    expect(t.ended).toEqual([2000]);
    expect(t.pool.isHeld(R)).toBe(false);
    expect(t.released).toHaveBeenCalledTimes(1);
  });

  it('ループを試聴：区間を回し（先読みは 1 秒）、旗を動かしたらまだ鳴っていない区間から替える', () => {
    const t = setup();
    const loop = { start: 1000, end: 13_000 }; // 0.25 秒
    t.player.play({ from: 0, loop, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const placed = starts(ctx, R);
    expect(ctx.sources[0]?.buffer?.length).toBe(12_000);
    expect(placed.reduce((n, [, len]) => n + len, 0)).toBeLessThan(R + 12_000 + 1);
    expect(placed.every(([, len]) => len === 12_000)).toBe(true);
    const n = ctx.sources.length;
    // 短いループ（SHORT_LOOP_FRAMES 以下）はつないだバッファになるので、それより長い区間に替える
    t.player.setLoop({ start: 1000, end: 10_000 });
    const after = ctx.sources.slice(n).map((s) => s.buffer?.length);
    expect(after.length).toBeGreaterThan(0);
    expect(after.every((len) => len === 9000)).toBe(true);
    expect(ctx.sources.slice(1, n).every((s) => s.stopped)).toBe(true);
  });

  it('ループの終わりを鳴っている区間の中へ縮めたら、新しい終わりで止めて始めから続ける', () => {
    const t = setup();
    t.player.play({
      from: 10_000,
      loop: { start: 0, end: 200_000 },
      source: 'edited',
      info: t.info,
    });
    const ctx = t.ctx();
    const first = Math.round((ctx.sources[0]?.started?.[0] ?? 0) * R);
    // 位置が約 14,800 のときに終わりを 30,000 へ
    ctx.currentTime = (first + 4800) / R;
    const n = ctx.sources.length;
    t.player.setLoop({ start: 0, end: 30_000 });
    expect((ctx.sources[0]?.stoppedAt ?? 0) * R).toBeCloseTo(first + 20_000, 3);
    const next = starts(ctx, R).slice(n);
    expect(next[0]).toEqual([first + 20_000, 30_000]);
    expect(ctx.buffers[n]?.channels[0]?.[0]).toBeCloseTo(0, 6);
    // 0.5 秒後の位置はループの中
    ctx.currentTime += 0.5;
    t.player.tick();
    expect(t.player.position()).toBeLessThan(30_000);
    // もう新しい終わりを過ぎていれば、すぐ止めて始めから
    const m = ctx.sources.length;
    const now = Math.ceil(ctx.currentTime * R) + Math.round(0.01 * R);
    const playing = ctx.sources.find((s, i) => {
      const [at, len] = starts(ctx, R)[i] ?? [0, 0];
      return !s.stopped && at <= now && now < at + len;
    });
    expect(playing).toBeDefined();
    t.player.setLoop({ start: 0, end: 2000 });
    expect((playing?.stoppedAt ?? 0) * R).toBeCloseTo(now, 3);
    expect(starts(ctx, R)[m]?.[0]).toBe(now);
  });

  it('短いループ（1 フレーム・数十フレーム）は、ループをつないだチャンク程度のバッファを 1 区間として置く', () => {
    for (const len of [1, 64]) {
      const t = setup();
      const loop = { start: 1000, end: 1000 + len };
      t.player.play({ from: 1000, loop, source: 'edited', info: t.info });
      const ctx = t.ctx();
      // 先読みの 1 秒を数本で埋める（区間ごとのノードを作らない）
      expect(ctx.sources.length).toBeLessThanOrEqual(2);
      const buf = ctx.buffers[0];
      expect(buf?.length).toBeGreaterThanOrEqual(R);
      expect(buf?.length).toBeLessThanOrEqual(CHUNK);
      expect((buf?.length ?? 0) % len).toBe(0);
      // 中身はループの標本の繰り返し
      for (const k of [0, len - 1, len, 5 * len + 1, (buf?.length ?? 1) - 1]) {
        expect(buf?.channels[0]?.[k]).toBeCloseTo(((1000 + (k % len)) % 1000) / 1000, 6);
      }
      // 位置はループの中
      ctx.currentTime += 0.3;
      t.player.tick();
      const pos = t.player.position() ?? -1;
      expect(pos).toBeGreaterThanOrEqual(loop.start);
      expect(pos).toBeLessThan(loop.end);
      // 時間が進んでも、1 回の足しで作るノードは数本
      const n = ctx.sources.length;
      ctx.currentTime += 1;
      t.player.tick();
      expect(ctx.sources.length - n).toBeLessThanOrEqual(2);
      expect(ctx.gains.length).toBeLessThanOrEqual(ctx.sources.length + 1);
    }
  });

  it('1 回の足しで置く区間の数には上限がある（高いレートの細かいループ）', () => {
    const t = setup({ rate: 384_000, total: CHUNK * 8 });
    const loop = { start: 0, end: 9000 };
    t.player.play({ from: 0, loop, source: 'edited', info: t.info });
    const ctx = t.ctx();
    expect(ctx.sources.length).toBe(MAX_PLACE_PER_FILL);
    t.player.tick();
    expect(ctx.sources.length).toBeGreaterThan(MAX_PLACE_PER_FILL);
  });

  it('このレートは鳴らさない（3〜384 kHz の外）', () => {
    const t = setup({ rate: 2000 });
    expect(t.player.play({ from: 0, source: 'edited', info: t.info })).toBe('rateUnsupported');
    expect(t.factory.contexts).toHaveLength(0);
    expect(t.player.playing).toBe(false);
  });
});

describe('ソロ（M17 段 6）', () => {
  it('ソロにしたときだけバンドパスの道を作り、全体の音量のあとで素通しと 10 ms で替える。やめると戻す・帯に付いて行く', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    expect(ctx.biquads).toHaveLength(0);
    const master = ctx.gains[0];
    t.player.setSolo({ freq: 2_800, q: 6 });
    const f = ctx.biquads[0];
    expect(f?.type).toBe('bandpass');
    expect(f?.frequency.ramps.at(-1)).toEqual([2_800, ctx.currentTime + CROSSFADE_SECONDS]);
    expect(f?.Q.ramps.at(-1)?.[0]).toBe(6);
    // 全体の音量 → 素通し・バンドパス（素通しは 1 → 0、通した音は 0 → 1）
    const [dry, wet] = ctx.gains.slice(-2);
    expect(master?.dest).toBe(f);
    expect(f?.dest).toBe(wet);
    expect(dry?.ramps.at(-1)?.[0]).toBe(0);
    expect(wet?.ramps.at(-1)?.[0]).toBe(1);
    // ナイキストの 0.49 倍まで
    t.player.setSolo({ freq: 40_000, q: 2 });
    expect(f?.frequency.ramps.at(-1)?.[0]).toBeCloseTo(R * 0.49, 6);
    // やめる：素通しへ戻す（道は残す）
    t.player.setSolo(null);
    expect(dry?.ramps.at(-1)?.[0]).toBe(1);
    expect(wet?.ramps.at(-1)?.[0]).toBe(0);
    expect(ctx.biquads).toHaveLength(1);
  });

  it('鳴らす前に決めたソロは鳴らし始めに当て、止めても覚えている（鳴らし始めはしない）', () => {
    const t = setup();
    t.player.setSolo({ freq: 500, q: 1.414 });
    expect(t.player.playing).toBe(false);
    expect(t.player.soloing).toEqual({ freq: 500, q: 1.414 });
    t.player.play({ from: 0, source: 'edited', info: t.info });
    expect(t.ctx().biquads[0]?.frequency.value).toBe(500);
    t.player.stop();
    expect(t.player.soloing).toEqual({ freq: 500, q: 1.414 });
  });
});

describe('音ごとの利得（M17 段 6 の音量を合わせて聞く）', () => {
  it('区間はその音の利得で置き、利得が替わったら今 + 60 ms から前の利得 → 0・新しい利得へ 10 ms で替える', () => {
    let level = 0.5;
    const t = setup({ level: () => level });
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const placed = ctx.gains.slice(1);
    expect(placed.length).toBeGreaterThan(0);
    expect(placed.every((g) => g.gain.value === 0.5)).toBe(true);
    // 同じ利得なら置き直さない
    t.player.relevel();
    expect(ctx.gains.length).toBe(placed.length + 1);
    level = 0.25;
    t.player.relevel();
    const fading = placed.filter((g) => g.gain.sets.some(([v]) => v === 0.5));
    expect(fading.length).toBeGreaterThan(0);
    expect(fading.every((g) => g.ramps.at(-1)?.[0] === 0)).toBe(true);
    const fresh = ctx.gains.slice(placed.length + 1);
    expect(fresh[0]?.ramps.at(-1)?.[0]).toBe(0.25);
    expect(fresh.slice(1).every((g) => g.gain.value === 0.25)).toBe(true);
  });
});

describe('手のあとの交差フェード', () => {
  function rampsOf(g: FakeGain | undefined) {
    return { sets: g?.gain.sets ?? [], ramps: g?.gain.ramps ?? [] };
  }

  it('新しい PCM が届いてから、今 + 60 ms で新しい音を 0→1、古い音を 1→0 の 10 ms で替える', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    ctx.currentTime += 0.5;
    t.player.tick();
    const oldCount = ctx.sources.length;
    // 0 番のチャンクが変わった：新しい中身が届くまでは替えない
    t.chunks.edited.delete(0);
    t.player.refresh({ type: 'spans', spans: [{ start: 0, end: 10 }] });
    expect(ctx.sources.length).toBe(oldCount);
    t.chunks.edited.set(0, makeChunk(0, t.info.frames, 2, 0.5));
    t.player.tick();
    const T = Math.ceil((ctx.currentTime + SWITCH_AHEAD_SECONDS) * R);
    const when = T / R;
    // 古い音（鳴っている区間）：1→0
    const oldGain = ctx.gains[1];
    expect(rampsOf(oldGain).sets).toContainEqual([1, when]);
    expect(rampsOf(oldGain).ramps).toContainEqual([0, when + CROSSFADE_SECONDS]);
    expect(ctx.sources[0]?.stoppedAt).toBeCloseTo(when + CROSSFADE_SECONDS, 9);
    // まだ鳴っていない区間は捨てる
    expect(
      ctx.sources.slice(1, oldCount).every((s) => s.stopped && s.stoppedAt === undefined),
    ).toBe(true);
    // 新しい音：T から、0→1
    const fresh = ctx.sources[oldCount];
    expect(Math.round((fresh?.started?.[0] ?? 0) * R)).toBe(T);
    const freshGain = ctx.gains[oldCount + 1];
    expect(rampsOf(freshGain).sets).toContainEqual([0, when]);
    expect(rampsOf(freshGain).ramps).toContainEqual([1, when + CROSSFADE_SECONDS]);
    // 新しい音は替える位置からの標本（新しい中身）
    const at = T - Math.ceil((0.1234567 + LEAD_SECONDS) * R);
    expect(fresh?.buffer?.length).toBe(CHUNK - at);
    expect((fresh?.buffer as unknown as { channels: Float32Array[] }).channels[0]?.[0]).toBeCloseTo(
      (at % 1000) / 1000 + 0.5,
      5,
    );
    expect(ctx.touched).toEqual([]);
  });

  it('替える位置のチャンクのチャンネル数が今の形と違えば替えない（古い音を鳴らし続ける）', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const n = ctx.sources.length;
    t.chunks.edited.set(0, makeChunk(0, t.info.frames, 1));
    t.player.refresh({ type: 'all' });
    expect(ctx.sources.length).toBe(n);
    expect(ctx.sources.some((s) => s.stopped)).toBe(false);
  });

  it('替えるのは 150 ms に 1 回まで', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    t.player.refresh({ type: 'all' });
    const n1 = ctx.sources.length;
    t.player.refresh({ type: 'all' });
    expect(ctx.sources.length).toBe(n1);
    t.advanceMs(SWITCH_MIN_MS);
    t.player.tick();
    expect(ctx.sources.length).toBeGreaterThan(n1);
  });

  it('変わった区間に掛からなければ替えない・元を鳴らしているときは替えない', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const n = ctx.sources.length;
    t.player.refresh({ type: 'spans', spans: [{ start: CHUNK * 7, end: CHUNK * 7 + 1 }] });
    expect(ctx.sources.length).toBe(n);
    t.player.refresh({ type: 'spans', spans: [] });
    expect(ctx.sources.length).toBe(n);
  });
});

describe('聞き比べ', () => {
  /** 元の位置 = 出力の位置 + 500（元の頭 500 を切り詰めた出力のまね）。 */
  const shifted = (pos: number) => pos + 500;

  it('レートが同じなら、替える時刻 T の位置を写して交差フェードで替える（位置が続く）', () => {
    const t = setup();
    for (let i = 0; i < 8; i++) t.chunks.source.set(i, makeChunk(i, CHUNK * 8, 2, 0.25));
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const first = Math.round((ctx.sources[0]?.started?.[0] ?? 0) * R);
    ctx.currentTime += 0.3;
    t.player.tick();
    const n = ctx.sources.length;
    expect(t.player.switchTo({ source: 'source', info: t.info, map: shifted })).toBe('playing');
    expect(t.player.source).toBe('source');
    expect(t.factory.contexts).toHaveLength(1);
    const T = Math.ceil((ctx.currentTime + SWITCH_AHEAD_SECONDS) * R);
    // T で古い音が鳴らす位置は T − first。新しい音はそれを写した位置から（戻らない）
    const at = shifted(T - first);
    const fresh = ctx.sources[n];
    expect(Math.round((fresh?.started?.[0] ?? 0) * R)).toBe(T);
    expect(fresh?.buffer?.length).toBe(CHUNK - at);
    expect(ctx.buffers[n]?.channels[0]?.[0]).toBeCloseTo((at % 1000) / 1000 + 0.25, 5);
    expect(ctx.sources[0]?.stoppedAt).toBeCloseTo(T / R + CROSSFADE_SECONDS, 9);
  });

  it('替える先の T のチャンクが手元に来るまでは、古い音を鳴らし続けて待つ', () => {
    const t = setup();
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const first = Math.round((ctx.sources[0]?.started?.[0] ?? 0) * R);
    const n = ctx.sources.length;
    expect(t.player.switchTo({ source: 'source', info: t.info, map: shifted })).toBe('playing');
    // まだ替えない（元が届くまで編集後が鳴る）
    expect(t.player.source).toBe('edited');
    expect(ctx.sources.length).toBe(n);
    expect(ctx.sources.some((s) => s.stopped)).toBe(false);
    expect(t.wanted).toContainEqual(['source', 0]);
    // 0.2 秒後に届いた：その時の T で替える
    ctx.currentTime += 0.2;
    t.chunks.source.set(0, makeChunk(0, CHUNK * 8));
    t.player.tick();
    expect(t.player.source).toBe('source');
    const T = Math.ceil((ctx.currentTime + SWITCH_AHEAD_SECONDS) * R);
    const fresh = ctx.sources.find((s, i) => i >= n && Math.round((s.started?.[0] ?? 0) * R) === T);
    expect(fresh?.buffer?.length).toBe(CHUNK - shifted(T - first));
  });

  it('写せなければ替えない・レートが違えば今の位置を写して止めてから始め直す', () => {
    const t = setup();
    for (let i = 0; i < 8; i++) t.chunks.source.set(i, makeChunk(i, CHUNK * 8));
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const n = ctx.sources.length;
    expect(t.player.switchTo({ source: 'source', info: t.info, map: () => null })).toBe(
      'unmappable',
    );
    expect(t.player.source).toBe('edited');
    expect(ctx.sources.length).toBe(n);
    // 元のレートが違う：止めて、そのレートの文脈で始め直す
    t.player.switchTo({ source: 'source', info: { ...t.info, rate: 44_100 }, map: shifted });
    expect(t.factory.contexts.map((c) => c.requested)).toEqual([R, 44_100]);
    expect(ctx.sources.every((s) => s.stopped)).toBe(true);
    expect(t.pool.isHeld(R)).toBe(false);
    expect(t.pool.isHeld(44_100)).toBe(true);
    expect(t.factory.contexts[1]?.buffers[0]?.length).toBe(CHUNK - 500);
  });
});

describe('試聴の PCM を取る', () => {
  it('出力は valid に覆われるチャンクだけ、元はいつでも取る（デコーダ・頭・長さを確かめる）', async () => {
    const io = fakeFetch();
    const output = new PcmChunkCache();
    const loaded = vi.fn();
    const feed = new PcmFeed({ fetch: io.fetch, output, onLoaded: loaded });
    const total = CHUNK * 4;
    feed.setSession('ab');
    feed.setInput({ rate: R, channels: 2, frames: total });
    const view = {
      rev: 5,
      samplesId: '0011',
      valid: [{ start: 0, end: CHUNK }],
      frames: total,
      channels: 2,
      rate: R,
    };
    feed.setView(view, null);
    feed.want('edited', 0);
    feed.want('edited', 1);
    feed.want('edited', 0);
    feed.want('source', 3);
    expect(io.calls).toEqual([
      editPcmUrl('ab', 5, 0, CHUNK),
      editInputPcmUrl('ab', CHUNK * 3, total),
    ]);
    io.resolve(io.calls[0] ?? '', pcmBytes(0, total, 2, 'kura-edit/1:0011'));
    io.resolve(io.calls[1] ?? '', pcmBytes(3, total, 2, 'wav-kura-dec/1'));
    await settle();
    expect(feed.peek('edited', 0)?.frames).toBe(CHUNK);
    expect(feed.peek('source', 3)?.start).toBe(CHUNK * 3);
    expect(loaded).toHaveBeenCalledTimes(2);
    // デコーダが違う出力の PCM は使わない
    feed.setView({ ...view, valid: [{ start: 0, end: total }] }, null);
    feed.want('edited', 1);
    io.resolve(editPcmUrl('ab', 5, CHUNK, CHUNK * 2), pcmBytes(1, total, 2, 'kura-edit/1:ffff'));
    await settle();
    expect(feed.peek('edited', 1)).toBeUndefined();
    // 読んでいる間に出力が変わったら使わない
    feed.want('edited', 2);
    feed.setView({ ...view, rev: 6, valid: [{ start: 0, end: total }] }, { type: 'all' });
    io.resolve(
      editPcmUrl('ab', 5, CHUNK * 2, CHUNK * 3),
      pcmBytes(2, total, 2, 'kura-edit/1:0011'),
    );
    await settle();
    expect(feed.peek('edited', 2)).toBeUndefined();
  });

  it('外して聞いている間（auditionId が samplesId と違う）、編集後の音は試聴の PCM を audition.valid の区間だけ別の表に取る（M17 段 6）', async () => {
    const io = fakeFetch();
    const output = new PcmChunkCache();
    const feed = new PcmFeed({ fetch: io.fetch, output });
    const total = CHUNK * 4;
    feed.setSession('ab');
    const shown = {
      rev: 5,
      samplesId: '0011223344556677',
      valid: [{ start: 0, end: total }],
      frames: total,
      channels: 2,
      rate: R,
    };
    const id = '8899aabbccddeeff';
    const bypassed = {
      ...shown,
      auditionId: id,
      audition: { valid: [{ start: 0, end: CHUNK * 2 }] },
    };
    expect(playsAudition(shown)).toBe(false);
    expect(playsAudition({ ...shown, auditionId: shown.samplesId })).toBe(false);
    expect(playsAudition(bypassed)).toBe(true);
    // 表示の表にあるチャンクは、外して聞いている間は使わない
    output.put(0, { rev: 5, start: 0, frames: CHUNK, planes: [new Float32Array(CHUNK)] });
    feed.setView(bypassed, null);
    expect(feed.peek('edited', 0)).toBeUndefined();
    feed.want('edited', 0);
    feed.want('edited', 2);
    expect(io.calls).toEqual([editAuditionPcmUrl('ab', id, 0, CHUNK)]);
    io.resolve(io.calls[0] ?? '', pcmBytes(0, total, 2, `kura-edit/1:${id}`));
    await settle();
    expect(feed.peek('edited', 0)?.frames).toBe(CHUNK);
    expect(feed.auditionCache.peek(0)).toBeDefined();
    // デコーダが表示の id のもの（外す前の音）は使わない
    feed.want('edited', 1);
    io.resolve(
      editAuditionPcmUrl('ab', id, CHUNK, CHUNK * 2),
      pcmBytes(1, total, 2, 'kura-edit/1:0011223344556677'),
    );
    await settle();
    expect(feed.peek('edited', 1)).toBeUndefined();
    // やめたら表示の表に戻り、試聴の表は空になる
    feed.setView(shown, null);
    expect(feed.peek('edited', 0)?.rev).toBe(5);
    expect(feed.auditionCache.peek(0)).toBeUndefined();
  });

  it('鳴らす音の替わり：表示の音どうしなら changed、試聴の id が替われば全体、同じ試聴なら替えない（M17 段 6）', () => {
    const base = {
      rev: 1,
      samplesId: '0011223344556677',
      valid: [],
      frames: 10,
      channels: 1,
      rate: R,
    };
    const spans = { type: 'spans' as const, spans: [{ start: 0, end: 5 }] };
    const bypassed = { ...base, auditionId: 'ffeeddccbbaa9988', changed: spans };
    expect(playbackChange(null, { ...base, changed: spans })).toEqual(spans);
    expect(playbackChange(base, { ...base, rev: 2, changed: spans })).toEqual(spans);
    expect(playbackChange(base, bypassed)).toEqual({ type: 'all' });
    expect(playbackChange(bypassed, { ...bypassed, samplesId: '1111111111111111' })).toBeNull();
    expect(playbackChange(bypassed, { ...base, changed: { type: 'spans', spans: [] } })).toEqual({
      type: 'all',
    });
  });

  it('取れなかったチャンクは期限（1 秒から倍々）が過ぎたら取り直す（出力も元も）。失敗の回数を知らせる', async () => {
    const io = fakeFetch();
    let now = 0;
    const failed: [PlaySource, number, number][] = [];
    const feed = new PcmFeed({
      fetch: io.fetch,
      output: new PcmChunkCache(),
      now: () => now,
      onFailed: (source, index, count) => void failed.push([source, index, count]),
    });
    const total = CHUNK * 4;
    feed.setSession('ab');
    feed.setInput({ rate: R, channels: 2, frames: total });
    const view = {
      rev: 5,
      samplesId: '0011',
      valid: [{ start: 0, end: total }],
      frames: total,
      channels: 2,
      rate: R,
    };
    feed.setView(view, null);
    const out = editPcmUrl('ab', 5, CHUNK, CHUNK * 2);
    const input = editInputPcmUrl('ab', CHUNK, CHUNK * 2);
    const count = (url: string) => io.calls.filter((u) => u === url).length;
    for (const [source, url] of [
      ['edited', out],
      ['source', input],
    ] as const) {
      feed.want(source, 1);
      io.reject(url, new Error('HTTP 503'));
      await settle();
      // 同じ表示のまま頼み直しても、期限の前は取らない
      feed.setView(view, { type: 'spans', spans: [] });
      feed.want(source, 1);
      expect(count(url)).toBe(1);
      now += FAIL_RETRY_MS;
      feed.want(source, 1);
      expect(count(url)).toBe(2);
      io.reject(url, new Error('HTTP 503'));
      await settle();
      // 2 回目のあとは 2 秒
      now += FAIL_RETRY_MS;
      feed.want(source, 1);
      expect(count(url)).toBe(2);
      now += FAIL_RETRY_MS;
      feed.want(source, 1);
      expect(count(url)).toBe(3);
      io.resolve(
        url,
        pcmBytes(1, total, 2, source === 'edited' ? 'kura-edit/1:0011' : 'wav-kura-dec/1'),
      );
      await settle();
      expect(feed.peek(source, 1)?.start).toBe(CHUNK);
    }
    expect(failed).toEqual([
      ['edited', 1, 1],
      ['edited', 1, 2],
      ['source', 1, 1],
      ['source', 1, 2],
    ]);
  });
});

describe('PCM を取れないとき', () => {
  it('待っているチャンクが 3 回続けて取れなければ止めて知らせる（ほかのチャンク・2 回までは止めない）', () => {
    const failedPlays: number[] = [];
    const t = setup({ missing: [1] });
    const player = new EditorPlayer({
      pool: t.pool,
      peek: (source, i) => t.chunks[source].get(i),
      want: () => undefined,
      volume: () => 1,
      timer: { every: () => 1, clear: () => undefined },
      onFailed: () => void failedPlays.push(1),
    });
    player.play({ from: 0, source: 'edited', info: t.info });
    player.chunkFailed('edited', 2, MAX_CHUNK_FAILURES);
    player.chunkFailed('edited', 1, MAX_CHUNK_FAILURES - 1);
    expect(player.playing).toBe(true);
    player.chunkFailed('edited', 1, MAX_CHUNK_FAILURES);
    expect(player.playing).toBe(false);
    expect(failedPlays).toEqual([1]);
  });

  it('聞き比べで替える先のチャンクが取れなければ、替えをやめて今の音を鳴らし続ける', () => {
    const reasons: string[] = [];
    const t = setup();
    const player = new EditorPlayer({
      pool: t.pool,
      peek: (source, i) => t.chunks[source].get(i),
      want: () => undefined,
      volume: () => 1,
      timer: { every: () => 1, clear: () => undefined },
      onSwitchFailed: (reason) => void reasons.push(reason),
    });
    player.play({ from: 0, source: 'edited', info: t.info });
    expect(player.switchTo({ source: 'source', info: t.info, map: (p) => p })).toBe('playing');
    player.chunkFailed('source', 0, MAX_CHUNK_FAILURES);
    expect(player.playing).toBe(true);
    expect(player.source).toBe('edited');
    expect(reasons).toEqual(['unavailable']);
    // 替えをやめたので、元が届いても替えない
    t.chunks.source.set(0, makeChunk(0, t.info.frames));
    player.tick();
    expect(player.source).toBe('edited');
  });
});
