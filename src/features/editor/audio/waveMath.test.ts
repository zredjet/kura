import { describe, expect, it } from 'vitest';

import {
  clampWindow,
  fitView,
  followPlayhead,
  frameToX,
  hitTest,
  type HitScene,
  laneLayout,
  MIN_LANE,
  minSpan,
  msText,
  overviewStart,
  panBy,
  pickBin,
  rulerTicks,
  TICK_MIN_PX,
  tickStep,
  xToFrame,
  zoomAround,
} from './waveMath';

const BINS = [256, 1024, 4096, 16384, 65536];

describe('フレームと x', () => {
  it('見ている区間を幅に写し、戻すと同じ', () => {
    const w = { start: 1000, span: 4000 };
    expect(frameToX(1000, w, 800)).toBe(0);
    expect(frameToX(5000, w, 800)).toBe(800);
    expect(frameToX(3000, w, 800)).toBe(400);
    expect(xToFrame(400, w, 800)).toBe(3000);
    expect(xToFrame(frameToX(1234.5, w, 800), w, 800)).toBeCloseTo(1234.5, 9);
  });
});

describe('ズームとパン', () => {
  const total = 48_000 * 60;

  it('ポインタの下のフレームを動かさずに拡大・縮小する', () => {
    const w = { start: 0, span: total };
    const anchor = 48_000 * 15;
    const z = zoomAround(w, 4, anchor, total, 10);
    expect(z.span).toBe(total / 4);
    // 拡大の前後で、アンカーの x の割合が同じ
    expect((anchor - z.start) / z.span).toBeCloseTo((anchor - w.start) / w.span, 12);
  });

  it('全体より広げない・最小より狭めない・端で止まる', () => {
    const w = { start: 1000, span: 48_000 };
    expect(zoomAround(w, 0.001, 2000, total, 10)).toEqual({ start: 0, span: total });
    expect(zoomAround(w, 1e9, 2000, total, 10).span).toBe(10);
    // 頭の手前に出ない
    expect(zoomAround({ start: 0, span: 1000 }, 0.5, 0, total, 10).start).toBe(0);
    // 不正な倍率は区間をそのまま（収めるだけ）
    expect(zoomAround(w, Number.NaN, 0, total, 10)).toEqual(w);
  });

  it('最小の長さは 1 フレーム 64px（全体が短ければ全体）', () => {
    expect(minSpan(640, 1_000_000)).toBe(10);
    expect(minSpan(640, 5)).toBe(5);
    expect(minSpan(64, 1_000_000)).toBe(2);
  });

  it('パンは端で止まる', () => {
    const w = { start: 100, span: 1000 };
    expect(panBy(w, -500, 10_000)).toEqual({ start: 0, span: 1000 });
    expect(panBy(w, 20_000, 10_000)).toEqual({ start: 9000, span: 1000 });
    expect(panBy(w, 50, 10_000)).toEqual({ start: 150, span: 1000 });
  });

  it('全体を表示・収める（長さ 0 の音声も 1 フレームの区間）', () => {
    expect(fitView(480)).toEqual({ start: 0, span: 480 });
    expect(fitView(0)).toEqual({ start: 0, span: 1 });
    expect(clampWindow({ start: -5, span: 99_999 }, 480)).toEqual({ start: 0, span: 480 });
  });
});

describe('再生位置を追う', () => {
  it('中なら動かさず、外へ出たらそこを頭に 1 画面ぶん飛ぶ（尾では収める）', () => {
    const w = { start: 0, span: 1000 };
    expect(followPlayhead(w, 999, 10_000)).toBe(w);
    expect(followPlayhead(w, 1000, 10_000)).toEqual({ start: 1000, span: 1000 });
    expect(followPlayhead({ start: 5000, span: 1000 }, 200, 10_000)).toEqual({
      start: 200,
      span: 1000,
    });
    expect(followPlayhead(w, 9500, 10_000)).toEqual({ start: 9000, span: 1000 });
  });
});

describe('目盛り', () => {
  it('間隔が 72px 以上になる最も細かいきりのよい刻み', () => {
    const rate = 48_000;
    // 10 秒を 1000px：1 秒 = 100px → 1 秒の刻み
    expect(tickStep({ start: 0, span: rate * 10 }, 1000, rate)).toBe(1);
    // 0.1 秒を 1000px：1 ms = 10px → 10 ms
    expect(tickStep({ start: 0, span: rate / 10 }, 1000, rate)).toBe(0.01);
    // 60 分を 600px：最も粗い 10 分
    expect(tickStep({ start: 0, span: rate * 3600 }, 600, rate)).toBe(600);
    // どれも 1 ms 以上
    expect(tickStep({ start: 0, span: 10 }, 1000, rate)).toBe(0.001);
  });

  it('区間の中の刻みと文字（1 秒以上の刻みは秒まで、細かければミリ秒まで）', () => {
    const rate = 1000;
    const ticks = rulerTicks({ start: 1500, span: 4000 }, 400, rate);
    expect(ticks.map((t) => t.label)).toEqual(['0:02', '0:03', '0:04', '0:05']);
    expect(ticks[0]?.x).toBe(50);
    for (const [i, t] of ticks.entries()) {
      if (i > 0) expect(t.x - (ticks[i - 1]?.x ?? 0)).toBeGreaterThanOrEqual(TICK_MIN_PX);
    }
    const fine = rulerTicks({ start: 0, span: 100 }, 1000, rate);
    expect(fine[0]?.label).toBe('0:00.000');
    expect(fine[1]?.label).toBe('0:00.010');
    expect(rulerTicks({ start: 0, span: 0 }, 100, rate)).toEqual([]);
  });

  it('時刻の文字（1 時間を超えれば時も）', () => {
    expect(msText(4200, true)).toBe('0:04.200');
    expect(msText(62_000, false)).toBe('1:02');
    expect(msText(3_723_456, true)).toBe('1:02:03.456');
    expect(msText(-5, true)).toBe('0:00.000');
  });
});

describe('段を選ぶ', () => {
  it('1 列に 1〜4 ビンの最も粗い段（M11 の pickLevel と同じ規則）', () => {
    // 1 列 1000 フレーム → 256 のビンなら 3.9 ビン、1024 なら 0.98（足りない）→ 256
    expect(pickBin(BINS, 1_000_000, 1000)).toBe(256);
    // 1 列 5000 フレーム → 4096
    expect(pickBin(BINS, 5_000_000, 1000)).toBe(4096);
    // 60 分の 48kHz を 1000 列 → 1 列 172,800 → 65,536
    expect(pickBin(BINS, 172_800_000, 1000)).toBe(65536);
  });

  it('1 列が 256 フレームより細かければ PCM で描く（null）', () => {
    expect(pickBin(BINS, 255_000, 1000)).toBeNull();
    expect(pickBin(BINS, 256_000, 1000)).toBe(256);
    expect(pickBin([], 1e6, 100)).toBeNull();
    // 細かい段がない（長い音声をライブラリの段で仮に出している）：持っている最も細かい段で描き、PCM は 256 未満だけ
    expect(pickBin([4096, 16384, 65536], 1_000_000, 1000)).toBe(4096);
    expect(pickBin([4096, 16384, 65536], 255_000, 1000)).toBeNull();
    expect(pickBin(BINS, 1e6, 0)).toBeNull();
  });
});

describe('縦の割り付け', () => {
  it('上から目盛り 20・全体図 24・チャンネルの段を等分', () => {
    const l = laneLayout(244, 2, 20, 24);
    expect(l.ruler).toEqual({ y: 0, h: 20 });
    expect(l.overview).toEqual({ y: 20, h: 24 });
    expect(l.lanes).toEqual([
      { y: 44, h: 100 },
      { y: 144, h: 100 },
    ]);
  });

  it('多チャンネルは 1 段 24px 以上（入らなければはみ出す）', () => {
    const l = laneLayout(100, 6, 20, 24);
    expect(l.lanes.every((b) => b.h === MIN_LANE)).toBe(true);
    expect(l.lanes).toHaveLength(6);
    expect(laneLayout(100, 0, 20, 24).lanes).toHaveLength(1);
  });
});

describe('当たり判定', () => {
  const layout = laneLayout(244, 2, 20, 24);
  const base: HitScene = {
    layout,
    window: { start: 0, span: 1000 },
    width: 1000,
    loop: null,
    fade: null,
    trim: null,
    selection: null,
  };

  it('ループの旗（目盛りの段）> フェードのハンドル > 切り詰めの端 > 選択の端', () => {
    const all: HitScene = {
      ...base,
      loop: { start: 100, end: 600 },
      fade: { inEnd: 100, outStart: 900 },
      trim: { start: 100, end: 800 },
      selection: { start: 102, end: 500 },
    };
    expect(hitTest(101, 10, all)).toEqual({ kind: 'loop', edge: 'start' });
    expect(hitTest(105, 50, all)).toEqual({ kind: 'fade', edge: 'in' });
    expect(hitTest(895, 50, all)).toEqual({ kind: 'fade', edge: 'out' });
    expect(hitTest(100, 120, all)).toEqual({ kind: 'trim', edge: 'start' });
    expect(hitTest(500, 120, all)).toEqual({ kind: 'selection', edge: 'end' });
    expect(hitTest(300, 120, all)).toEqual({ kind: 'lanes' });
  });

  it('全体図・目盛り・段・外', () => {
    expect(hitTest(10, 30, base)).toEqual({ kind: 'overview' });
    expect(hitTest(10, 5, base)).toEqual({ kind: 'ruler' });
    expect(hitTest(10, 100, base)).toEqual({ kind: 'lanes' });
    expect(hitTest(10, 300, base)).toEqual({ kind: 'empty' });
  });

  it('端の近いほう（ずっと狭い選択は終わりを先に）', () => {
    const s = { ...base, selection: { start: 500, end: 503 } };
    expect(hitTest(503, 100, s)).toEqual({ kind: 'selection', edge: 'end' });
    expect(hitTest(498, 100, s)).toEqual({ kind: 'selection', edge: 'start' });
  });
});

describe('全体図', () => {
  it('押した所が見ている区間の中央になる（端で止まる）', () => {
    const w = { start: 0, span: 100 };
    expect(overviewStart(500, 1000, w, 1000)).toBe(450);
    expect(overviewStart(0, 1000, w, 1000)).toBe(0);
    expect(overviewStart(1000, 1000, w, 1000)).toBe(900);
  });
});
