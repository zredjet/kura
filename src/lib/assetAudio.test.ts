import { describe, expect, it } from 'vitest';

import { fakeFactory } from '../test/fakeAudio';
import type { AuditionFacts } from './asset/audition';
import { type PcmInfo, writePcmWav } from './asset/wav';
import { Audition, ContextPool, SfxPlayer } from './assetAudio';

/** 元の音声のまね：要求された区間の WAV を、同じ読み取りのループ点・フレーム数とともに返す。 */
function fakeSource(truth: {
  rate: number;
  channels: number;
  total: number;
  loop: readonly [number, number] | null;
}) {
  const fetched: [number, number][] = [];
  const log: string[] = [];
  let info: Partial<PcmInfo> = {};
  const fetchPcm = async (_hash: string, from: number, to: number): Promise<ArrayBuffer> => {
    log.push('fetch');
    fetched.push([from, to]);
    const frames = Math.max(0, Math.min(to, truth.total) - from);
    return writePcmWav({
      rate: truth.rate,
      format: 'i16',
      planes: Array.from({ length: truth.channels }, () => new Float32Array(frames)),
      info: {
        start: from,
        total: truth.total,
        loop: truth.loop,
        decoder: 'wav-kura-dec/1',
        ...info,
      },
    });
  };
  return {
    fetchPcm,
    fetched,
    log,
    override(next: Partial<PcmInfo>) {
      info = next;
    },
  };
}

const R = 8_000;
const facts: AuditionFacts = { frames: R * 20, rate: R, channels: 2, loop: [R, R * 11] };

function setup(opts: { honor?: boolean; truth?: Partial<typeof facts & { total: number }> } = {}) {
  const log: string[] = [];
  const factory = fakeFactory(log, opts.honor ?? true);
  const pool = new ContextPool(factory.create, 3);
  const src = fakeSource({
    rate: R,
    channels: 2,
    total: facts.frames,
    loop: facts.loop ? [facts.loop[0], facts.loop[1]] : null,
    ...opts.truth,
  });
  src.log.length = 0;
  let stops = 0;
  const audition = new Audition({
    pool,
    fetchPcm: async (h, a, b) => {
      log.push('fetch');
      return src.fetchPcm(h, a, b);
    },
    volume: () => 0.8,
    onStop: () => {
      stops++;
    },
  });
  return { audition, pool, factory, src, log, stops: () => stops };
}

/** フレームの境にそろっているか。 */
function onFrame(when: number, rate: number): boolean {
  return Math.abs(when * rate - Math.round(when * rate)) < 1e-6;
}

describe('ループの試聴', () => {
  it('本体だけを取り、元のレートの文脈で loop だけを立て、継ぎ目の 3 秒前から鳴らす', async () => {
    const { audition, factory, src } = setup();
    const out = await audition.start('h', facts);
    expect(out.status).toBe('playing');
    expect(src.fetched).toEqual([[R, R * 11]]);
    const ctx = factory.contexts[0];
    expect(ctx?.requested).toBe(R);
    const [body] = ctx?.sources ?? [];
    expect(body?.loop).toBe(true);
    expect(body?.buffer?.length).toBe(R * 10);
    expect(body?.buffer?.sampleRate).toBe(R);
    expect(body?.started?.[1]).toBeCloseTo((R * 10 - R * 3) / R, 12);
    expect(onFrame(body?.started?.[0] ?? 0.5, R)).toBe(true);
    expect(body?.started?.[0]).toBeGreaterThan(ctx?.currentTime ?? 0);
    expect(ctx?.touched).toEqual([]);
    expect(ctx?.gains[0]?.gain.value).toBe(0.8);
    expect(audition.playing).toBe(true);
  });

  it('はじめからは前奏のあとに、前奏の長さ ÷ レートだけ後ろへ本体を置く（どちらもフレームの境）', async () => {
    const { audition, factory, src } = setup();
    await audition.start('h', facts, { fromStart: true });
    expect(src.fetched).toEqual([
      [0, R],
      [R, R * 11],
    ]);
    const [intro, body] = factory.contexts[0]?.sources ?? [];
    expect(intro?.loop).toBe(false);
    expect(body?.loop).toBe(true);
    const t0 = intro?.started?.[0] ?? 0;
    const t1 = body?.started?.[0] ?? 0;
    expect(onFrame(t0, R)).toBe(true);
    expect(Math.round((t1 - t0) * R)).toBe(R);
    expect(Math.round(t1 * R) - Math.round(t0 * R)).toBe(R);
    expect(body?.started?.[1] ?? 0).toBe(0);
    expect(factory.contexts[0]?.touched).toEqual([]);
  });

  it('ループ点がなければ全体を回す', async () => {
    const { audition, factory, src } = setup({ truth: { loop: null } });
    const out = await audition.start('h', { ...facts, loop: null });
    expect(out.status).toBe('playing');
    expect(src.fetched).toEqual([[0, R * 20]]);
    expect(factory.contexts[0]?.sources[0]?.loop).toBe(true);
  });

  it('文脈の resume は取得を待つ前に呼ぶ（キー・クリックの処理の中で）', async () => {
    const { audition, log } = setup();
    await audition.start('h', facts);
    expect(log.slice(0, 3)).toEqual([`create:${R}`, 'resume', 'fetch']);
  });

  it('元のレートの文脈を作れない環境では試聴しない（補間になるので）', async () => {
    const { audition, src } = setup({ honor: false });
    expect((await audition.start('h', facts)).status).toBe('unsupported');
    expect(src.fetched).toEqual([]);
  });

  it('PCM のループ点・長さが DB と違えば、PCM の値で計画し直して取り直す', async () => {
    const truth = { total: R * 20 + 7, loop: [R + 4, R * 11] as [number, number] };
    const { audition, factory, src } = setup({ truth });
    const out = await audition.start('h', facts);
    expect(out.status).toBe('playing');
    expect(src.fetched).toEqual([
      [R, R * 11],
      [R + 4, R * 11],
    ]);
    if (out.status === 'playing') {
      expect(out.facts).toEqual({ ...facts, frames: R * 20 + 7, loop: [R + 4, R * 11] });
    }
    const sources = factory.contexts[0]?.sources ?? [];
    expect(sources).toHaveLength(1);
    expect(sources[0]?.buffer?.length).toBe(R * 11 - R - 4);
  });

  it('取り直しても合わなければ試聴しない', async () => {
    const { audition, src } = setup();
    let n = 0;
    src.override({});
    const flaky = new Audition({
      pool: new ContextPool(fakeFactory().create),
      fetchPcm: (h, a, b) => {
        n++;
        src.override({ loop: [R + n, R * 11] });
        return src.fetchPcm(h, a, b);
      },
      volume: () => 1,
    });
    expect((await flaky.start('h', facts)).status).toBe('unsupported');
    expect(n).toBe(2);
    audition.dispose();
  });

  it('応答のレート・チャンネル・開始フレーム・長さが頼んだものと違えば試聴しない', async () => {
    const wrongStart = setup();
    wrongStart.src.override({ start: 3 });
    expect((await wrongStart.audition.start('h', facts)).status).toBe('unsupported');

    const wrongRate = setup({ truth: { rate: R * 2 } });
    expect((await wrongRate.audition.start('h', facts)).status).toBe('unsupported');

    const wrongChannels = setup({ truth: { channels: 1 } });
    expect((await wrongChannels.audition.start('h', facts)).status).toBe('unsupported');

    const short = setup({ truth: { total: R * 5, loop: null } });
    const out = await short.audition.start('h', facts);
    expect(out.status).toBe('unsupported');
  });

  it('取得に失敗したら failed（理由を返す）', async () => {
    const pool = new ContextPool(fakeFactory().create);
    const a = new Audition({
      pool,
      fetchPcm: () => Promise.reject(new Error('503')),
      volume: () => 1,
    });
    const out = await a.start('h', facts);
    expect(out.status).toBe('failed');
    if (out.status === 'failed') expect(String(out.error)).toContain('503');
    expect(a.playing).toBe(false);
  });

  it('上限を超えれば継ぎ目の前後を続けて 1 回鳴らし、終わったら止まる', async () => {
    const { audition, factory, src, stops } = setup();
    const out = await audition.start('h', facts, { capBytes: 1_000 });
    expect(out.status).toBe('playing');
    expect(src.fetched).toEqual([
      [R * 11 - 62, R * 11],
      [R, R + 62],
    ]);
    const [tail, head] = factory.contexts[0]?.sources ?? [];
    expect(tail?.loop).toBe(false);
    expect(head?.loop).toBe(false);
    expect(
      Math.round((head?.started?.[0] ?? 0) * R) - Math.round((tail?.started?.[0] ?? 0) * R),
    ).toBe(62);
    head?.onended?.(new Event('ended'));
    expect(audition.playing).toBe(false);
    expect(stops()).toBe(1);
  });

  it('止める・次を始めると前を止める', async () => {
    const { audition, factory, stops } = setup();
    await audition.start('h', facts);
    await audition.start('h', facts);
    const sources = factory.contexts[0]?.sources ?? [];
    expect(sources[0]?.stopped).toBe(true);
    expect(sources[1]?.stopped).toBe(false);
    audition.stop();
    expect(sources[1]?.stopped).toBe(true);
    expect(audition.playing).toBe(false);
    expect(stops()).toBe(2);
    // 同じレートは同じ文脈を使い回す
    expect(factory.contexts).toHaveLength(1);
  });

  it('取得を待つ間に止めた・次を始めたら、前のものは鳴らさない', async () => {
    const factory = fakeFactory();
    const pool = new ContextPool(factory.create);
    const src = fakeSource({ rate: R, channels: 2, total: facts.frames, loop: [R, R * 11] });
    let release: () => void = () => undefined;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const a = new Audition({
      pool,
      fetchPcm: async (h, x, y) => {
        await gate;
        return src.fetchPcm(h, x, y);
      },
      volume: () => 1,
    });
    const first = a.start('h', facts);
    a.stop();
    release();
    expect((await first).status).toBe('superseded');
    expect(factory.contexts[0]?.sources ?? []).toEqual([]);
  });
});

// 鳴らす区間は PCM と同じ読み取りの長さで計画するので、/pcm の応答で知ってから計画する（素材の欄・L のキー。解析の長さはデコーダが変わるとずれる）
describe('/pcm の応答で長さを知ってから始める試聴', () => {
  function probing(truth: Parameters<typeof fakeSource>[0], honor = true) {
    const log: string[] = [];
    const factory = fakeFactory(log, honor);
    const src = fakeSource(truth);
    const scheduled: { hash: string; mode: string; rate: number; frames: number[] }[] = [];
    const audition = new Audition({
      pool: new ContextPool(factory.create),
      fetchPcm: async (h, a, b) => {
        log.push(`fetch:${a}-${b}`);
        return src.fetchPcm(h, a, b);
      },
      volume: () => 1,
      onScheduled: (s) =>
        scheduled.push({
          hash: s.hash,
          mode: s.plan.mode,
          rate: s.rate,
          frames: s.pcms.map((p) => p.frames),
        }),
    });
    return { audition, factory, log, scheduled };
  }

  it('先頭の 1 フレームを取り、応答の長さ・ループ点で計画する（resume は取る前）', async () => {
    const { audition, log, scheduled } = probing({
      rate: R,
      channels: 2,
      total: R * 20,
      loop: [R, R * 11],
    });
    const out = await audition.startProbed('h', { rate: R, channels: 2 });
    expect(out.status).toBe('playing');
    if (out.status === 'playing') {
      expect(out.facts).toEqual({ frames: R * 20, rate: R, channels: 2, loop: [R, R * 11] });
    }
    expect(log).toEqual([`create:${R}`, 'resume', 'fetch:0-1', `fetch:${R}-${R * 11}`]);
    expect(scheduled).toEqual([{ hash: 'h', mode: 'loop', rate: R, frames: [R * 10] }]);
  });

  it('ループ点がなければ全体を回す。応答のレート・チャンネルが違えば試聴しない', async () => {
    const whole = probing({ rate: R, channels: 1, total: R * 2, loop: null });
    const out = await whole.audition.startProbed('h', { rate: R, channels: 1 });
    expect(out.status).toBe('playing');
    expect(whole.log.slice(-1)).toEqual([`fetch:0-${R * 2}`]);

    const rate = probing({ rate: R * 2, channels: 1, total: R * 2, loop: null });
    expect((await rate.audition.startProbed('h', { rate: R, channels: 1 })).status).toBe(
      'unsupported',
    );
    const ch = probing({ rate: R, channels: 2, total: R * 2, loop: null });
    expect((await ch.audition.startProbed('h', { rate: R, channels: 1 })).status).toBe(
      'unsupported',
    );
    expect(ch.scheduled).toEqual([]);
  });

  it('元のレートの文脈を作れない環境では試聴しない（取りもしない。装置のレートでは補間になるので）', async () => {
    const { audition, log, scheduled } = probing(
      { rate: R, channels: 2, total: R * 20, loop: [R, R * 11] },
      false,
    );
    expect((await audition.startProbed('h', { rate: R, channels: 2 })).status).toBe('unsupported');
    expect(log).toEqual([`create:${R}`]);
    expect(scheduled).toEqual([]);
  });

  it('試聴の設定の区間があれば、ファイルのループ点の代わりに回す（応答のループ点とは比べ直さない）', async () => {
    const { audition, log, scheduled } = probing({
      rate: R,
      channels: 2,
      total: R * 20,
      loop: [R, R * 11],
    });
    const out = await audition.startProbed('h', { rate: R, channels: 2, loop: [R * 2, R * 5] });
    expect(out.status).toBe('playing');
    if (out.status === 'playing') {
      expect(out.plan).toMatchObject({ mode: 'loop', body: [R * 2, R * 5] });
      expect(out.facts.loop).toEqual([R * 2, R * 5]);
    }
    expect(log).toEqual([`create:${R}`, 'resume', 'fetch:0-1', `fetch:${R * 2}-${R * 5}`]);
    expect(scheduled).toEqual([{ hash: 'h', mode: 'loop', rate: R, frames: [R * 3] }]);
  });

  it('はじめからは前奏 [0, 設定の始め) のあとに設定の区間を回す', async () => {
    const { audition, log } = probing({ rate: R, channels: 1, total: R * 20, loop: null });
    const out = await audition.startProbed(
      'h',
      { rate: R, channels: 1, loop: [R * 2, R * 5] },
      { fromStart: true },
    );
    expect(out.status === 'playing' && out.plan).toMatchObject({
      intro: [0, R * 2],
      body: [R * 2, R * 5],
    });
    expect(log.slice(-2)).toEqual([`fetch:0-${R * 2}`, `fetch:${R * 2}-${R * 5}`]);
  });

  it('設定が応答の長さを超えれば（上書きで縮んだ）ファイルのループ点、なければ全体を回す', async () => {
    const looped = probing({ rate: R, channels: 2, total: R * 20, loop: [R, R * 11] });
    const a = await looped.audition.startProbed('h', {
      rate: R,
      channels: 2,
      loop: [R * 2, R * 20 + 1],
    });
    expect(a.status === 'playing' && a.plan).toMatchObject({ body: [R, R * 11] });
    expect(looped.log.slice(-1)).toEqual([`fetch:${R}-${R * 11}`]);

    const plain = probing({ rate: R, channels: 2, total: R * 4, loop: null });
    const b = await plain.audition.startProbed('h', { rate: R, channels: 2, loop: [R, R * 5] });
    expect(b.status === 'playing' && b.plan).toMatchObject({ body: [0, R * 4] });
  });

  it('鳴っている位置を元のファイルのフレームで返す（止めたら null）', async () => {
    const { audition, factory } = probing({
      rate: R,
      channels: 2,
      total: R * 20,
      loop: [R, R * 11],
    });
    expect(audition.position()).toBeNull();
    await audition.startProbed('h', { rate: R, channels: 2 });
    const ctx = factory.contexts[0] as { currentTime: number } | undefined;
    const body = factory.contexts[0]?.sources[0];
    if (!ctx || !body?.started) throw new Error('鳴らしていない');
    const t0 = body.started[0];
    // 継ぎ目の 3 秒前から：1 秒後は継ぎ目の 2 秒前、4 秒後は本体の始まりから 1 秒
    ctx.currentTime = t0 + 1;
    expect(audition.position()).toBe(R * 11 - R * 2);
    ctx.currentTime = t0 + 4;
    expect(audition.position()).toBe(R + R);
    audition.stop();
    expect(audition.position()).toBeNull();
  });
});

describe('文脈の使い回し（元のレートごとに 1 つ）', () => {
  it('同じレートは使い回し、別のレートは別に作る。上限を超えたら古いものを閉じる', () => {
    const { create, contexts } = fakeFactory();
    const pool = new ContextPool(create, 2);
    const a = pool.get(44_100);
    expect(pool.get(44_100)).toBe(a);
    const b = pool.get(48_000);
    expect(pool.size).toBe(2);
    pool.get(44_100); // 44.1k を新しくする
    pool.get(96_000); // 48k が最も古い → 閉じる
    expect(contexts.map((c) => [c.requested, c.closed])).toEqual([
      [44_100, false],
      [48_000, true],
      [96_000, false],
    ]);
    expect(pool.get(48_000)).not.toBe(b);
    expect(pool.size).toBe(2);
  });

  it('装置のレートの文脈は別の鍵', () => {
    const { create, contexts } = fakeFactory();
    const pool = new ContextPool(create);
    const device = pool.get();
    expect(pool.get()).toBe(device);
    pool.get(48_000);
    expect(contexts.map((c) => c.requested)).toEqual([undefined, 48_000]);
  });

  it('closeAll で全部を閉じ、次は作り直す', () => {
    const { create, contexts } = fakeFactory();
    const pool = new ContextPool(create);
    pool.get(44_100);
    pool.get();
    pool.closeAll();
    expect(contexts.every((c) => c.closed)).toBe(true);
    expect(pool.size).toBe(0);
    pool.get(44_100);
    expect(contexts).toHaveLength(3);
  });

  it('文脈を作れない（そのレートで NotSupportedError）なら試聴しない', async () => {
    const pool = new ContextPool(() => {
      throw new DOMException('作れない', 'NotSupportedError');
    });
    const a = new Audition({
      pool,
      fetchPcm: () => Promise.reject(new Error('呼ばない')),
      volume: () => 1,
    });
    expect((await a.start('h', facts)).status).toBe('unsupported');
    const sfx = new SfxPlayer({
      pool,
      fetchAudio: () => Promise.reject(new Error('呼ばない')),
      volume: () => 1,
    });
    expect(await sfx.play('a')).toBe('failed');
  });

  it('上限は 1 以上', () => {
    expect(() => new ContextPool(fakeFactory().create, 0)).toThrow(RangeError);
  });

  // M13：エディタが鳴らしている文脈は、効果音・試聴が別のレートで文脈を作っても閉じない
  it('使用中の印のある文脈は追い出さず、印のないものから閉じる（なければ上限を超えて作る）', () => {
    const { create, contexts } = fakeFactory();
    const pool = new ContextPool(create, 2);
    const release44 = pool.hold(44_100);
    const release48 = pool.hold(48_000);
    pool.get(44_100);
    pool.get(48_000);
    pool.get(); // 効果音（装置のレート）：どちらも使用中なので上限を超えて作る
    expect(contexts.map((c) => c.closed)).toEqual([false, false, false]);
    expect(pool.size).toBe(3);
    release44();
    release44(); // 2 回呼んでも 1 回だけ外す
    expect(pool.isHeld(44_100)).toBe(false);
    expect(pool.isHeld(48_000)).toBe(true);
    pool.get(96_000); // 印のない最も古いもの（44.1k）から閉じる
    expect(contexts.map((c) => [c.requested, c.closed])).toEqual([
      [44_100, true],
      [48_000, false],
      [undefined, true],
      [96_000, false],
    ]);
    release48();
    expect(pool.isHeld(48_000)).toBe(false);
  });

  it('印を重ねたら、すべて外すまで使用中', () => {
    const pool = new ContextPool(fakeFactory().create);
    const a = pool.hold(48_000);
    const b = pool.hold(48_000);
    a();
    expect(pool.isHeld(48_000)).toBe(true);
    b();
    expect(pool.isHeld(48_000)).toBe(false);
  });
});

describe('効果音の即再生', () => {
  function sfxSetup(maxCacheBytes = 1 << 20) {
    const log: string[] = [];
    const factory = fakeFactory(log);
    const pool = new ContextPool(factory.create);
    let volume = 0.5;
    const sizes: Record<string, number> = { a: 1_000, b: 2_000, c: 3_000, bad: 0 };
    const fetched: string[] = [];
    const sfx = new SfxPlayer({
      pool,
      fetchAudio: async (hash) => {
        fetched.push(hash);
        return new ArrayBuffer(sizes[hash] ?? 10);
      },
      volume: () => volume,
      maxCacheBytes,
    });
    return {
      sfx,
      factory,
      fetched,
      log,
      setVolume: (v: number) => {
        volume = v;
      },
    };
  }

  it('装置のレートの文脈で、音量を付けて鳴らす。同じ音は取り直さない', async () => {
    const { sfx, factory, fetched, setVolume } = sfxSetup();
    expect(await sfx.play('a')).toBe('playing');
    const ctx = factory.contexts[0];
    expect(ctx?.requested).toBeUndefined();
    expect(ctx?.sources[0]?.started?.[0]).toBe(0);
    expect(ctx?.gains[0]?.gain.value).toBe(0.5);
    setVolume(0.25);
    expect(await sfx.play('a')).toBe('playing');
    expect(fetched).toEqual(['a']);
    expect(ctx?.gains[1]?.gain.value).toBe(0.25);
    expect(ctx?.touched).toEqual([]);
  });

  it('次の音は前の音を 10ms で下げて止める', async () => {
    const { sfx, factory } = sfxSetup();
    await sfx.play('a');
    await sfx.play('b');
    const ctx = factory.contexts[0];
    const [g0] = ctx?.gains ?? [];
    expect(g0?.ramps).toEqual([[0, (ctx?.currentTime ?? 0) + 0.01]]);
    expect(ctx?.sources[0]?.stopped).toBe(true);
    // 下げ終わる時刻に止める（すぐ止めるとプツッと鳴る）
    expect(ctx?.sources[0]?.stoppedAt).toBe((ctx?.currentTime ?? 0) + 0.01);
    expect(ctx?.sources[1]?.stopped).toBe(false);
  });

  it('解いた音は上限のバイト数まで持ち、超えたら古いものから捨てる', async () => {
    // 偽のデコードは長さ = バイト数・2 チャンネルなので、a は 8,000、b は 16,000、c は 24,000 バイト
    const { sfx, fetched } = sfxSetup(40_000);
    await sfx.play('a');
    await sfx.play('b');
    await sfx.play('c'); // 48,000 > 40,000 → a を捨てる
    await sfx.play('b');
    await sfx.play('a');
    expect(fetched).toEqual(['a', 'b', 'c', 'a']);
  });

  it('読めなければ failed、待つ間に次を始めたら前は鳴らさない', async () => {
    const { sfx, factory } = sfxSetup();
    expect(await sfx.play('bad')).toBe('failed');
    const first = sfx.play('a');
    const second = sfx.play('b');
    expect(await first).toBe('superseded');
    expect(await second).toBe('playing');
    expect(factory.contexts[0]?.sources).toHaveLength(1);
  });
});
