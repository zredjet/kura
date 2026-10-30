import { describe, expect, it } from 'vitest';

import type { EditAudioViewport } from '../../../ipc/edit';
import { editPcmUrl, editPeaksUrl } from '../../../ipc/media';
import { CHUNK } from './pcmChunks';
import { audioView, fakeClock, fakeFetch, pcmBytes, settle, tileBytes } from './testAudioView';
import {
  EDIT_DECODER_PREFIX,
  WAVE_IDLE_MS,
  WaveController,
  type WaveScene,
} from './waveController';

const S = 'ab01';
const TOTAL = 48_000 * 60;

function setup(width = 1000) {
  const clock = fakeClock();
  const io = fakeFetch();
  const scenes: WaveScene[] = [];
  const idle: [EditAudioViewport, boolean][] = [];
  const c = new WaveController({
    draw: (_canvas, scene) => scenes.push(scene),
    fetch: io.fetch,
    scheduler: clock.scheduler,
    onIdle: (vp, force) => idle.push([vp, force]),
  });
  c.attach(document.createElement('canvas'));
  c.resize(width, 300, 1);
  c.setSession(S);
  c.setShape({ rate: 48_000, channels: 2, frames: TOTAL });
  return { c, clock, io, scenes, idle };
}

const peaksUrl = (rev: number, bin: number, tile: number) => editPeaksUrl(S, rev, bin, tile);

describe('波形の段のタイルを取る', () => {
  it('全体図の最も粗い段 → 見ている区間の段の順に、今の peaks.rev の URL で、鍵ごとに 1 本', () => {
    const { c, io } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    c.applyView(audioView({ peaks: { rev: 3, changed: { type: 'spans', spans: [] } } }));
    // 60 分を 1000 列：1 列 2,880 フレーム → 1,024 の段（1 枚 1,048,576 フレームで 3 枚）
    expect(io.calls).toEqual([
      peaksUrl(3, 65536, 0),
      peaksUrl(3, 1024, 0),
      peaksUrl(3, 1024, 1),
      peaksUrl(3, 1024, 2),
    ]);
  });

  it('足りない列は粗い段で埋めて描き、細かい段が届いたら替える', async () => {
    const { c, io, clock } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    io.resolve(peaksUrl(3, 65536, 0), tileBytes(65536, 0, TOTAL, 2, 100));
    await settle();
    const coarse = c.scene();
    expect(coarse.mode).toBe('peaks');
    expect(Array.from(coarse.has.subarray(0, coarse.columns)).every((h) => h === 1)).toBe(true);
    expect(coarse.lanes[1]).toBeCloseTo(100 / 32768, 9);
    // 全体図も最も粗い段で
    expect(coarse.overviewHas[0]).toBe(1);
    for (const t of [0, 1, 2]) io.resolve(peaksUrl(3, 1024, t), tileBytes(1024, t, TOTAL, 2, 2000));
    await settle();
    clock.flushFrames();
    expect(c.scene().lanes[1]).toBeCloseTo(2000 / 32768, 9);
  });

  it('壊れたタイル・404 は同じ rev では取り直さない', async () => {
    const { c, io } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    io.reject(peaksUrl(3, 65536, 0), new Error('404'));
    io.resolve(peaksUrl(3, 1024, 0), new ArrayBuffer(8));
    await settle();
    const before = io.calls.length;
    c.applyView(audioView({ peaks: { rev: 3, changed: { type: 'spans', spans: [] } } }));
    expect(io.calls.length).toBe(before);
  });
});

describe('変わった区間を捨てる', () => {
  it('peaks.changed に掛かるタイルを捨て、新しい rev で取り直す（掛からないものは古い rev のまま使う）', async () => {
    const { c, io } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    io.resolve(peaksUrl(3, 65536, 0), tileBytes(65536, 0, TOTAL, 2));
    for (const t of [0, 1, 2]) io.resolve(peaksUrl(3, 1024, t), tileBytes(1024, t, TOTAL, 2));
    await settle();
    io.calls.length = 0;
    c.applyView(
      audioView({
        rev: 4,
        peaks: {
          rev: 4,
          changed: { type: 'spans', spans: [{ start: 1_100_000, end: 1_200_000 }] },
        },
      }),
    );
    // 1,024 の段のタイル 1 と、全体を受け持つ最も粗い段のタイル 0 だけ
    expect(io.calls.sort()).toEqual([peaksUrl(4, 1024, 1), peaksUrl(4, 65536, 0)].sort());
  });

  it('読んでいる間に捨てられたタイルは使わない', async () => {
    const { c, io } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    c.applyView(audioView({ rev: 4, peaks: { rev: 4, changed: { type: 'all' } } }));
    io.resolve(peaksUrl(3, 65536, 0), tileBytes(65536, 0, TOTAL, 2));
    await settle();
    expect(c.tiles.peek(65536, 0)).toBeUndefined();
  });

  it('changed に掛かる PCM のチャンクを捨てる', () => {
    const { c } = setup();
    c.applyView(audioView());
    for (const i of [0, 1, 2]) {
      c.pcm.put(i, { rev: 1, start: i * CHUNK, frames: CHUNK, planes: [new Float32Array(1)] });
    }
    c.applyView(
      audioView({
        rev: 2,
        changed: { type: 'spans', spans: [{ start: CHUNK + 1, end: CHUNK + 2 }] },
      }),
    );
    expect([0, 1, 2].map((i) => c.pcm.has(i))).toEqual([true, false, true]);
  });
});

describe('PCM で描く拡大', () => {
  it('1 列が 256 フレーム未満になったら PCM：見えている区間をチャンクにそろえて頼み、valid のチャンクだけ取る', async () => {
    const { c, io, clock, idle } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    io.calls.length = 0;
    // ちょうど 256 フレーム／列は段で描く
    c.setWindow({ start: 0, span: 256_000 }, true);
    expect(c.viewport().spans).toEqual([]);
    c.setWindow({ start: 0, span: 200_000 }, true);
    expect(c.viewport().spans).toEqual([{ start: 0, end: CHUNK * 4 }]);
    clock.advance(WAVE_IDLE_MS);
    expect(idle.at(-1)?.[0].spans).toEqual([{ start: 0, end: CHUNK * 4 }]);
    // valid に入ったチャンクだけ（0 と 1）
    const view = audioView({
      peaks: { rev: 3, changed: { type: 'spans', spans: [] } },
      valid: [{ start: 0, end: CHUNK * 2 }],
    });
    c.applyView(view);
    const pcmCalls = io.calls.filter((u) => u.includes('/pcm/'));
    expect(pcmCalls).toEqual([editPcmUrl(S, 1, 0, CHUNK), editPcmUrl(S, 1, CHUNK, CHUNK * 2)]);
    const decoder = EDIT_DECODER_PREFIX + view.samplesId;
    io.resolve(
      pcmCalls[0] ?? '',
      pcmBytes(0, TOTAL, 2, decoder, () => 0.5),
    );
    // デコーダが合わないものは使わない
    io.resolve(
      pcmCalls[1] ?? '',
      pcmBytes(1, TOTAL, 2, 'kura-edit/1:ffff', () => 0.5),
    );
    await settle();
    expect(c.pcm.has(0)).toBe(true);
    expect(c.pcm.has(1)).toBe(false);
    clock.flushFrames();
    const scene = c.scene();
    expect(scene.mode).toBe('pcm');
    expect(scene.lanes[1]).toBeCloseTo(0.5, 6);
  });
});

describe('止まってから頼み直す', () => {
  it('ズーム・パンが止まって 150 ms で今の区間を頼む（途中の操作で数え直す）', () => {
    const { c, clock, idle } = setup();
    c.applyView(audioView({ peaks: { rev: 3 } }));
    idle.length = 0;
    c.zoomAt(2, 500);
    clock.advance(WAVE_IDLE_MS - 1);
    c.panBy(10);
    clock.advance(WAVE_IDLE_MS - 1);
    expect(idle).toEqual([]);
    clock.advance(1);
    expect(idle).toHaveLength(1);
    const [vp, force] = idle[0] ?? [];
    expect(force).toBe(false);
    expect(vp?.work).toBe(false);
    expect(vp?.visible?.end).toBeGreaterThan(vp?.visible?.start ?? 0);
  });

  it('裏の仕事が残っていれば、手が止まって 150 ms で work: true を同じビューポートでも送る', () => {
    const { c, clock, idle } = setup();
    c.applyView(audioView({ work: true, peaks: { rev: 3, pending: true } }));
    clock.advance(WAVE_IDLE_MS);
    expect(idle).toHaveLength(1);
    expect(idle[0]?.[0].work).toBe(true);
    expect(idle[0]?.[1]).toBe(true);
    // 手の間（ドラッグの途中）は頼まない
    c.setInteracting(true);
    c.applyView(audioView({ work: true, peaks: { rev: 3, pending: true } }));
    clock.advance(WAVE_IDLE_MS * 3);
    expect(idle).toHaveLength(1);
    expect(c.viewport().interacting).toBe(true);
    // 離したら 150 ms で
    c.setInteracting(false);
    clock.advance(WAVE_IDLE_MS);
    expect(idle).toHaveLength(2);
    expect(idle[1]?.[0].work).toBe(true);
    // 仕事が終われば頼まない
    c.applyView(audioView({ work: false, peaks: { rev: 4, pending: false } }));
    clock.advance(WAVE_IDLE_MS);
    expect(idle).toHaveLength(2);
  });
});

describe('ズームと再生位置', () => {
  it('全体を表示・拡大・縮小・再生位置が外へ出たら 1 画面ぶん飛ぶ', () => {
    const { c } = setup();
    c.applyView(audioView());
    expect(c.fitting).toBe(true);
    c.zoomStep(1);
    expect(c.window.span).toBe(TOTAL / 2);
    expect(c.fitting).toBe(false);
    // 見ている区間は [T/4, 3T/4)。外へ出たらそこを頭に（尾では収める）
    expect(c.follow(TOTAL / 2)).toBe(false);
    expect(c.follow((TOTAL * 3) / 4 + 10)).toBe(true);
    expect(c.window.start).toBe(TOTAL / 2);
    expect(c.follow((TOTAL * 3) / 4 + 20)).toBe(false);
    c.fit();
    expect(c.window).toEqual({ start: 0, span: TOTAL });
  });

  it('長さが変わったら（全体を表示しているときは）全体を表示し直す', () => {
    const { c } = setup();
    c.applyView(audioView());
    c.applyView(audioView({ frames: 1000, peaks: { frames: 1000 } }));
    expect(c.window).toEqual({ start: 0, span: 1000 });
  });

  it('x とフレームの写し（余白の内側）', () => {
    const { c } = setup(1000);
    c.setInsets({ top: 0, right: 0, bottom: 0, left: 100 });
    c.applyView(audioView({ frames: 900, peaks: { frames: 900 } }));
    expect(c.xOf(0)).toBe(100);
    expect(c.frameAt(550)).toBe(450);
    expect(c.frameAt(-50)).toBe(0);
  });
});
