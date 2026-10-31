import { describe, expect, it, vi } from 'vitest';

import { editInputPcmUrl, editPcmUrl } from '../../../ipc/media';
import { ContextPool } from '../../../lib/assetAudio';
import { fakeFactory, type FakeContext, type FakeGain } from '../../../test/fakeAudio';
import {
  CROSSFADE_SECONDS,
  EditorPlayer,
  LEAD_SECONDS,
  MAX_PLACE_PER_FILL,
  neededSpans,
  PcmFeed,
  type PlaySource,
  planSegments,
  SWITCH_AHEAD_SECONDS,
  SWITCH_MIN_MS,
} from './playback';
import { CHUNK, type PcmChunk, PcmChunkCache } from './pcmChunks';
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
  opts: { total?: number; rate?: number; channels?: number; missing?: number[] } = {},
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
  it('レートが同じなら交差フェードで元に替え、違えば止めて始め直す', () => {
    const t = setup();
    for (let i = 0; i < 8; i++) t.chunks.source.set(i, makeChunk(i, CHUNK * 8));
    t.player.play({ from: 0, source: 'edited', info: t.info });
    const ctx = t.ctx();
    const n = ctx.sources.length;
    expect(t.player.switchTo({ from: 500, source: 'source', info: t.info })).toBe('playing');
    expect(t.player.source).toBe('source');
    expect(t.factory.contexts).toHaveLength(1);
    const fresh = ctx.sources[n];
    const T = Math.ceil((ctx.currentTime + SWITCH_AHEAD_SECONDS) * R);
    expect(Math.round((fresh?.started?.[0] ?? 0) * R)).toBe(T);
    expect(fresh?.buffer?.length).toBe(CHUNK - 500);
    // 元のレートが違う：止めて、そのレートの文脈で始め直す
    for (let i = 0; i < 8; i++) t.chunks.source.set(i, makeChunk(i, CHUNK * 8));
    t.player.switchTo({ from: 0, source: 'source', info: { ...t.info, rate: 44_100 } });
    expect(t.factory.contexts.map((c) => c.requested)).toEqual([R, 44_100]);
    expect(ctx.sources.every((s) => s.stopped)).toBe(true);
    expect(t.pool.isHeld(R)).toBe(false);
    expect(t.pool.isHeld(44_100)).toBe(true);
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
});
