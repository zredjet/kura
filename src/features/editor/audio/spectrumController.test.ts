import { describe, expect, it, vi } from 'vitest';

import type {
  EditSpecAverageDto,
  EditSpecAverageRequestDto,
  EditSpecPortDto,
} from '../../../ipc/edit';
import { editSpecUrl } from '../../../ipc/media';
import type { SpectrumScene } from './drawSpectrum';
import { SpectrumController, toPortFrame } from './spectrumController';
import { specBands } from './spectrumMath';
import { audioView, fakeClock, fakeFetch, settle, specTileBytes } from './testAudioView';

const SESSION = 'ab12';
const RATE = 8_000;
const N = 512;
const HOP = 128;
const BANDS = specBands(RATE).length;
const FRAMES = 3 * 65_536;
const KEY_POST = '11'.repeat(16);
const KEY_PRE = '22'.repeat(16);

function port(over: Partial<EditSpecPortDto>): EditSpecPortDto {
  return {
    node: null,
    side: 'output',
    channel: { type: 'sum' },
    key: KEY_POST,
    rate: RATE,
    frames: FRAMES,
    n: N,
    hop: HOP,
    bands: BANDS,
    map: [{ outStart: 0, outEnd: FRAMES, inStart: 0, inRate: RATE, outRate: RATE }],
    maxChunks: 22,
    ready: [{ start: 0, end: 3 }],
    pending: false,
    ...over,
  };
}

/** u8 を値に（dB = q/2 − 120）。帯 0 だけ見る。 */
function tile(chunk: number, value: (frame: number) => number): ArrayBuffer {
  return specTileBytes({
    rate: RATE,
    n: N,
    hop: HOP,
    bands: BANDS,
    code: 0xffff,
    firstFrame: chunk * 512,
    frames: 512,
    value: (j, k) => (k === 0 ? value(j) : 0),
  });
}

function setup(
  options: {
    playhead?: () => number | null;
    average?: (s: string, r: EditSpecAverageRequestDto) => Promise<EditSpecAverageDto>;
  } = {},
) {
  const clock = fakeClock();
  const io = fakeFetch();
  const scenes: SpectrumScene[] = [];
  const onStale = vi.fn();
  const onWork = vi.fn();
  const c = new SpectrumController({
    fetch: io.fetch,
    scheduler: clock.scheduler,
    now: clock.now,
    playhead: options.playhead,
    average: options.average,
    draw: (_canvas, scene) => scenes.push(scene),
    onStale,
    onWork,
  });
  c.attach(document.createElement('canvas'));
  c.resize(800, 400, 1);
  c.setSession(SESSION);
  c.setSettings({ fall: 20, tilt: 0, channel: { type: 'sum' }, range: 90, eqScale: 12 });
  return { c, clock, io, scenes, onStale, onWork };
}

describe('スペクトルの面の制御', () => {
  it('頼みの spec は [前, 後]（ノードがなければ出力だけ）。区間は止まっていればカーソルと選択の始めのチャンク、再生中は再生の区間', () => {
    const { c } = setup();
    c.applyView(audioView({ rate: RATE, frames: FRAMES }));
    c.setStill({ cursor: 70_000, selection: { start: 140_000, end: 150_000 } });
    expect(c.specRequests()).toEqual([
      {
        node: null,
        side: 'output',
        channel: { type: 'sum' },
        spans: [
          { start: 65_536, end: 131_072 },
          { start: 131_072, end: 196_608 },
        ],
      },
    ]);
    c.setTarget(3);
    const reqs = c.specRequests();
    expect(reqs.map((r) => [r.node, r.side])).toEqual([
      [3, 'input'],
      [3, 'output'],
    ]);
    c.setPlaybackSpans([{ start: 0, end: 131_072 }]);
    c.setPlaying(true);
    expect(c.specRequests()[0]?.spans).toEqual([{ start: 0, end: 131_072 }]);
  });

  it('止まっているとき：置き場にあるカーソルのチャンクのタイルを取り、その列の値を出す。404 は頼み直す', async () => {
    const { c, io, clock, scenes, onStale } = setup();
    c.setStill({ cursor: 65_536 + 128 * 10, selection: null });
    c.applyView(
      audioView({
        rate: RATE,
        frames: FRAMES,
        spec: [port({ ready: [{ start: 1, end: 2 }] })],
      }),
    );
    const url = editSpecUrl(SESSION, KEY_POST, { type: 'sum' }, 1);
    expect(io.pending()).toEqual([url]);
    io.resolve(
      url,
      tile(1, (j) => (j === 512 + 10 ? 200 : 100)),
    );
    await settle();
    clock.flushFrames();
    const post = scenes[scenes.length - 1]?.post;
    expect(post?.[0]).toBe(200 / 2 - 120);
    // 置き場から消えていた
    c.setStill({ cursor: 10, selection: null });
    c.applyView(
      audioView({ rate: RATE, frames: FRAMES, spec: [port({ ready: [{ start: 0, end: 1 }] })] }),
    );
    io.reject(editSpecUrl(SESSION, KEY_POST, { type: 'sum' }, 0), { status: 404 });
    await settle();
    expect(onStale).toHaveBeenCalled();
  });

  it('再生中：鳴っている位置の列へ落ちる速さで下げ、最大の保持を溜め、遅れたフレームの間の最大を取る', async () => {
    let pos = 0;
    const { c, io, clock, scenes } = setup({ playhead: () => pos });
    c.applyView(audioView({ rate: RATE, frames: FRAMES, spec: [port({})] }));
    c.setPlaying(true);
    c.tickPlaying();
    const url0 = editSpecUrl(SESSION, KEY_POST, { type: 'sum' }, 0);
    // フレーム 3 だけ大きい（遅れて 0 → 5 へ飛んだら、間の 3 を拾う）
    io.resolve(
      url0,
      tile(0, (j) => (j === 3 ? 220 : 100)),
    );
    await settle();
    c.tickPlaying();
    const at0 = scenes[scenes.length - 1]?.post?.[0];
    expect(at0).toBe(-70);
    clock.advance(16);
    pos = 5 * HOP;
    c.tickPlaying();
    expect(scenes[scenes.length - 1]?.post?.[0]).toBe(-10);
    expect(scenes[scenes.length - 1]?.hold?.[0]).toBe(-10);
    // 0.5 秒で 20 dB/s × 0.5 = 10 dB だけ下がる（目標は −70）
    clock.advance(500);
    pos = 6 * HOP;
    c.tickPlaying();
    expect(scenes[scenes.length - 1]?.post?.[0]).toBeCloseTo(-20, 5);
    expect(scenes[scenes.length - 1]?.hold?.[0]).toBe(-10);
    // 再生を始め直すと保持は消える
    c.setPlaying(false);
    c.setPlaying(true);
    c.tickPlaying();
    expect(scenes[scenes.length - 1]?.hold?.[0]).toBe(-70);
  });

  it('選択の平均：前・後を 1 回ずつ頼み、ready の値を出す。同じ頼みは繰り返さず、鍵が替われば頼み直す。計っている間は仕事を起こす', async () => {
    const calls: EditSpecAverageRequestDto[] = [];
    const replies: ((r: EditSpecAverageDto) => void)[] = [];
    const { c, clock, scenes, onWork } = setup({
      average: (_s, r) =>
        new Promise((resolve) => {
          calls.push(r);
          replies.push(resolve);
        }),
    });
    c.setTarget(3);
    const view = audioView({
      rate: RATE,
      frames: FRAMES,
      spec: [port({ node: 3, side: 'input', key: KEY_PRE }), port({ node: 3, side: 'output' })],
    });
    c.applyView(view);
    c.setStill({ cursor: 0, selection: { start: 1_000, end: 9_000 } });
    expect(calls.map((r) => r.side)).toEqual(['input', 'output']);
    const ready = (v: number): EditSpecAverageDto => ({
      type: 'ready',
      frames: 60,
      mean: new Array<number>(BANDS).fill(v),
      max: new Array<number>(BANDS).fill(v + 3),
    });
    replies[0]?.(ready(-40));
    replies[1]?.({ type: 'measuring', progress: 0.1 });
    await settle();
    expect(onWork).toHaveBeenCalledWith(true);
    clock.flushFrames();
    expect(scenes[scenes.length - 1]?.pre?.[0]).toBe(-40);
    expect(scenes[scenes.length - 1]?.dimPost).toBe(true);
    // 同じ表示が来ても頼み直さない。表示の specAverage が ready になったら頼み直す
    c.applyView(view);
    expect(calls).toHaveLength(2);
    c.applyView({
      ...view,
      specAverage: [
        {
          request: calls[1] as EditSpecAverageRequestDto,
          key: KEY_POST,
          phase: { type: 'ready' },
        },
      ],
    });
    expect(calls).toHaveLength(3);
    replies[2]?.(ready(-30));
    await settle();
    clock.flushFrames();
    expect(scenes[scenes.length - 1]?.post?.[0]).toBe(-30);
    expect(scenes[scenes.length - 1]?.hold?.[0]).toBe(-27);
    expect(onWork).toHaveBeenLastCalledWith(false);
    // 後の鍵が替わった（引数を変えた）：後だけ頼み直す
    c.applyView({
      ...view,
      spec: [
        port({ node: 3, side: 'input', key: KEY_PRE }),
        port({ node: 3, side: 'output', key: '33'.repeat(16) }),
      ],
    });
    expect(calls).toHaveLength(4);
    expect(calls[3]?.side).toBe('output');
  });

  it('出力のフレームをポートのフレームへ写す（区分の比で。外は null）', () => {
    const p = port({
      map: [{ outStart: 100, outEnd: 1_100, inStart: 5_000, inRate: 8_000, outRate: 16_000 }],
    });
    expect(toPortFrame(p, 100)).toBe(5_000);
    expect(toPortFrame(p, 300)).toBe(5_100);
    expect(toPortFrame(p, 99)).toBeNull();
    expect(toPortFrame(p, 1_100)).toBeNull();
  });
});

describe('イコライザの重ね（M17 段 5）', () => {
  it('点を周波数と ±dB で置き、当たり（点・選んだ点の Q の印）を返す。ドラッグ中は「前 ＋ 曲線」で後を描く', async () => {
    const { c, clock, io, scenes } = setup();
    c.setInsets({ top: 0, left: 0, right: 0, bottom: 0 });
    c.setTarget(3);
    c.setStill({ cursor: 0, selection: null });
    c.applyView(
      audioView({
        rate: RATE,
        frames: FRAMES,
        spec: [port({ node: 3, side: 'input', key: KEY_PRE }), port({ node: 3, side: 'output' })],
      }),
    );
    io.resolve(
      editSpecUrl(SESSION, KEY_PRE, { type: 'sum' }, 0),
      tile(0, () => 160),
    );
    io.resolve(
      editSpecUrl(SESSION, KEY_POST, { type: 'sum' }, 0),
      tile(0, () => 100),
    );
    await settle();
    const bell = {
      type: 'bell' as const,
      freq: 1_000,
      gainDb: 6,
      q: 1,
      slope: 12,
      channel: 'all' as const,
      enabled: true,
    };
    c.setEqModel({
      bands: [bell],
      curve: { freqs: [20, 4_000], total: [6, 6], bands: [{ index: 0, db: [6, 6] }] },
      selected: 0,
      hovered: null,
    });
    clock.flushFrames();
    const scene = scenes[scenes.length - 1];
    const p = scene?.eqLayer?.points[0];
    expect(p?.x).toBeCloseTo(c.xOfFreq(1_000), 6);
    expect(p?.y).toBeCloseTo(c.yOfEqDb(6), 6);
    expect(p?.q).not.toBeNull();
    expect(c.hitBand(p?.x ?? 0, p?.y ?? 0)).toEqual({ band: 0, part: 'point' });
    expect(c.hitBand(p?.q?.right ?? 0, p?.y ?? 0)).toEqual({ band: 0, part: 'qHi' });
    expect(c.hitBand(5, 5)).toBeNull();
    // ふつうは後のタイル、ドラッグ中は前（−40）＋ 曲線（+6）
    expect(scene?.post?.[0]).toBe(-70);
    c.setPostMode('approx');
    clock.flushFrames();
    expect(scenes[scenes.length - 1]?.post?.[0]).toBe(-34);
    c.setPostMode('dim');
    clock.flushFrames();
    expect(scenes[scenes.length - 1]?.dimPost).toBe(true);
    expect(c.curveDbAt(1_000)).toBe(6);
  });
});
