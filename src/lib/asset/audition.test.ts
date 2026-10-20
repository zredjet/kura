import { describe, expect, it } from 'vitest';

import {
  type AuditionFacts,
  auditionPosition,
  DEFAULT_AUDITION,
  SFX_MAX_MS,
  planAudition,
  rangesOf,
  shouldPlaySfx,
} from './audition';

const R = 44_100;
const facts: AuditionFacts = { frames: R * 70, rate: R, channels: 2, loop: [R, R * 61] };

// docs/design.md §3.1「ループの試聴」：本体だけを 1 つのバッファにして回す（loopStart・loopEnd を使わない）
describe('ループの試聴の計画', () => {
  it('上限に収まれば本体を 1 つのバッファにし、継ぎ目の 3 秒前から鳴らす', () => {
    const plan = planAudition(facts);
    expect(plan).toEqual({
      mode: 'loop',
      intro: null,
      body: [R, R * 61],
      offset: R * 57,
    });
    expect(plan && rangesOf(plan)).toEqual([[R, R * 61]]);
  });

  it('はじめからは前奏のあとに本体を続ける', () => {
    const plan = planAudition(facts, { ...DEFAULT_AUDITION, fromStart: true });
    expect(plan).toEqual({ mode: 'loop', intro: [0, R], body: [R, R * 61], offset: 0 });
    expect(plan && rangesOf(plan)).toEqual([
      [0, R],
      [R, R * 61],
    ]);
  });

  it('ループ点がなければ全体をループで試聴する（前奏はない）', () => {
    const whole = { ...facts, loop: null };
    expect(planAudition(whole)).toEqual({
      mode: 'loop',
      intro: null,
      body: [0, R * 70],
      offset: R * 67,
    });
    expect(planAudition(whole, { ...DEFAULT_AUDITION, fromStart: true })).toEqual({
      mode: 'loop',
      intro: null,
      body: [0, R * 70],
      offset: 0,
    });
  });

  it('本体が 3 秒より短ければ本体の頭から', () => {
    expect(planAudition({ ...facts, loop: [0, 1000] })).toMatchObject({
      offset: 0,
      body: [0, 1000],
    });
  });

  it('上限（64MiB）を超えれば継ぎ目の前後 4 秒だけを 1 回', () => {
    // 44.1kHz ステレオの f32 は 1 秒 352,800 バイト。64MiB は約 190 秒
    const long = { ...facts, frames: R * 400, loop: [R, R * 400] as [number, number] };
    const plan = planAudition(long);
    expect(plan).toEqual({ mode: 'seam', tail: [R * 396, R * 400], head: [R, R * 5] });
    expect(plan && rangesOf(plan)).toEqual([
      [R * 396, R * 400],
      [R, R * 5],
    ]);
    // ちょうど上限なら丸ごと
    const cap = 1000 * 2 * 4;
    expect(
      planAudition({ ...facts, loop: [0, 1000] }, { ...DEFAULT_AUDITION, capBytes: cap })?.mode,
    ).toBe('loop');
    expect(
      planAudition({ ...facts, loop: [0, 1001] }, { ...DEFAULT_AUDITION, capBytes: cap })?.mode,
    ).toBe('seam');
    // はじめからは前奏も数える
    expect(
      planAudition(
        { ...facts, loop: [1, 1000] },
        { ...DEFAULT_AUDITION, capBytes: cap, fromStart: true },
      )?.mode,
    ).toBe('loop');
    expect(
      planAudition(
        { ...facts, loop: [2, 1001] },
        { ...DEFAULT_AUDITION, capBytes: cap, fromStart: true },
      )?.mode,
    ).toBe('seam');
  });

  it('継ぎ目だけでも上限を超えるなら、上限に収まる長さに縮める', () => {
    const many = { frames: 384_000 * 100, rate: 384_000, channels: 32, loop: null };
    const plan = planAudition(many);
    expect(plan?.mode).toBe('seam');
    if (plan?.mode !== 'seam') return;
    const len = plan.tail[1] - plan.tail[0];
    expect(len).toBe(Math.floor(DEFAULT_AUDITION.capBytes / (2 * 32 * 4)));
    expect(plan.head).toEqual([0, len]);
  });

  it('壊れた区間・試聴できないレートは null', () => {
    expect(planAudition({ ...facts, loop: [10, 10] })).toBeNull();
    expect(planAudition({ ...facts, loop: [10, 5] })).toBeNull();
    expect(planAudition({ ...facts, loop: [-1, 5] })).toBeNull();
    expect(planAudition({ ...facts, loop: [0, facts.frames + 1] })).toBeNull();
    expect(planAudition({ ...facts, loop: [0.5, 5] })).toBeNull();
    expect(planAudition({ ...facts, frames: 0, loop: null })).toBeNull();
    expect(planAudition({ ...facts, channels: 0 })).toBeNull();
    expect(planAudition({ ...facts, rate: 2_999 })).toBeNull();
    expect(planAudition({ ...facts, rate: 384_001 })).toBeNull();
    expect(planAudition({ ...facts, rate: 768_000 })).toBeNull();
  });
});

describe('効果音の即再生', () => {
  const cx = { enabled: true, playerPlaying: false, quickLook: false, selectedCount: 1 };
  const short = { kind: 'audio', durationMs: 800 };

  it('10 秒以下の音声を 1 件だけ選んだときだけ鳴らす', () => {
    expect(SFX_MAX_MS).toBe(10_000);
    expect(shouldPlaySfx(short, cx)).toBe(true);
    expect(shouldPlaySfx({ kind: 'audio', durationMs: 10_000 }, cx)).toBe(true);
    expect(shouldPlaySfx({ kind: 'audio', durationMs: 10_001 }, cx)).toBe(false);
    expect(shouldPlaySfx({ kind: 'audio', durationMs: null }, cx)).toBe(false);
    expect(shouldPlaySfx({ kind: 'audio', durationMs: 0 }, cx)).toBe(false);
    expect(shouldPlaySfx({ kind: 'image', durationMs: null }, cx)).toBe(false);
    expect(shouldPlaySfx({ kind: 'video', durationMs: 800 }, cx)).toBe(false);
    expect(shouldPlaySfx(undefined, cx)).toBe(false);
  });

  it('設定・再生中・クイックルック中・複数選択では鳴らさない', () => {
    expect(shouldPlaySfx(short, { ...cx, enabled: false })).toBe(false);
    expect(shouldPlaySfx(short, { ...cx, playerPlaying: true })).toBe(false);
    expect(shouldPlaySfx(short, { ...cx, quickLook: true })).toBe(false);
    expect(shouldPlaySfx(short, { ...cx, selectedCount: 2 })).toBe(false);
    expect(shouldPlaySfx(short, { ...cx, selectedCount: 0 })).toBe(false);
  });
});

// docs/design.md §3.1「波形」：試聴中の位置は 1.5px の線（元のファイルのフレームで返す）
describe('試聴中の位置', () => {
  it('本体だけなら、始めた位置から本体の中を回る', () => {
    const plan = planAudition(facts);
    if (!plan) throw new Error('計画がない');
    // 継ぎ目の 3 秒前（本体の中の R * 57）から
    expect(auditionPosition(plan, 0)).toBe(R + R * 57);
    expect(auditionPosition(plan, R * 3 - 1)).toBe(R * 61 - 1);
    // 継ぎ目を越えたら本体の始まりへ
    expect(auditionPosition(plan, R * 3)).toBe(R);
    expect(auditionPosition(plan, R * 3 + R * 60)).toBe(R);
    // 始まる前は始めた位置
    expect(auditionPosition(plan, -5)).toBe(R + R * 57);
  });

  it('はじめからは前奏のあとに本体を回る', () => {
    const plan = planAudition(facts, { fromStart: true });
    if (!plan) throw new Error('計画がない');
    expect(auditionPosition(plan, 0)).toBe(0);
    expect(auditionPosition(plan, R - 1)).toBe(R - 1);
    expect(auditionPosition(plan, R)).toBe(R);
    expect(auditionPosition(plan, R + R * 60)).toBe(R);
    expect(auditionPosition(plan, R + R * 60 + 7)).toBe(R + 7);
  });

  it('継ぎ目だけは手前から始まりへ 1 回だけ進み、終われば null', () => {
    const plan = planAudition(facts, { capBytes: 1_000 });
    if (plan?.mode !== 'seam') throw new Error('継ぎ目の計画でない');
    const [t0, t1] = plan.tail;
    const [h0, h1] = plan.head;
    expect(auditionPosition(plan, 0)).toBe(t0);
    expect(auditionPosition(plan, t1 - t0 - 1)).toBe(t1 - 1);
    expect(auditionPosition(plan, t1 - t0)).toBe(h0);
    expect(auditionPosition(plan, t1 - t0 + (h1 - h0) - 1)).toBe(h1 - 1);
    expect(auditionPosition(plan, t1 - t0 + (h1 - h0))).toBeNull();
  });
});
