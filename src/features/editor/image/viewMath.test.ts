import { describe, expect, it } from 'vitest';

import {
  clampTransform,
  fitArea,
  fitTransform,
  fitZoom,
  formatCanBeTransparent,
  imagePixel,
  nextZoom,
  roundRect,
  smoothingFor,
  snapZoom,
  tileImageRect,
  tileRange,
  tileSize,
  tilesIn,
  visibleImageRect,
  wheelPixels,
  zoomAround,
  zoomPercent,
} from './viewMath';

describe('倍率の吸着', () => {
  it('ドット絵は装置の画素の整数倍に吸着し、縮めるときは吸着しない', () => {
    expect(snapZoom(2.3, 1, true)).toBe(2);
    expect(snapZoom(2.3, 2, true)).toBe(2.5); // 装置の画素で 4.6 → 5
    expect(snapZoom(0.7, 2, true)).toBe(0.5); // 1.4 → 1（装置の画素 1 倍）
    expect(snapZoom(0.3, 2, true)).toBe(0.3); // 0.6 < 1 は補間で縮める
    expect(snapZoom(2.3, 2, false)).toBe(2.3);
  });

  it('範囲の外・おかしな値は収める', () => {
    expect(snapZoom(1000, 1, false)).toBe(64);
    expect(snapZoom(0, 1, false)).toBe(1);
    expect(snapZoom(Number.NaN, 1, true)).toBe(1);
    expect(snapZoom(0.0001, 1, false)).toBe(0.01);
  });

  it('⌘+・⌘− は次の段へ。ドット絵は吸着して同じになる段を飛ばす', () => {
    expect(nextZoom(1, 1, 1, false)).toBe(1.5);
    expect(nextZoom(1, -1, 1, false)).toBeCloseTo(2 / 3);
    expect(nextZoom(64, 1, 1, false)).toBe(64);
    // DPR 2 のドット絵：1 → 1.5（装置 3 倍）→ 2 …、下は 0.5（装置 1 倍）の次は補間の 1/3
    expect(nextZoom(1, 1, 2, true)).toBe(1.5);
    expect(nextZoom(0.5, -1, 2, true)).toBeCloseTo(1 / 3);
    expect(nextZoom(1, -1, 2, true)).toBe(0.5);
  });
});

describe('全体を表示', () => {
  it('写真は枠に収めるが広げない（1 まで）', () => {
    expect(fitZoom(4000, 2000, { w: 1000, h: 800 }, 2, false)).toBe(0.25);
    expect(fitZoom(100, 100, { w: 1000, h: 800 }, 2, false)).toBe(1);
  });

  it('ドット絵は装置の画素の整数倍に広げ、収まらなければ縮める', () => {
    // 64×64 を 1000×800 に：最大 12.5 倍、DPR 2 で装置 25 倍 → 12.5
    expect(fitZoom(64, 64, { w: 1000, h: 800 }, 2, true)).toBe(12.5);
    expect(fitZoom(64, 64, { w: 1000, h: 800 }, 1, true)).toBe(12);
    expect(fitZoom(4000, 4000, { w: 1000, h: 800 }, 1, true)).toBe(0.2);
  });

  it('上部と下のカプセルを除いた枠の中央に置き、位置は装置の画素にそろえる', () => {
    const area = fitArea({ w: 1000, h: 800 }, { top: 60, right: 20, bottom: 90, left: 20 });
    expect(area).toEqual({ x: 20, y: 60, w: 960, h: 650 });
    const t = fitTransform(64, 32, area, 2, true);
    expect(t.zoom).toBe(15); // min(960/64, 650/32) = 15
    expect(t.ox).toBe(20); // (960 − 960) / 2 + 20
    expect(t.oy).toBe(60 + (650 - 480) / 2);
    const odd = fitTransform(3, 3, { x: 0, y: 0, w: 10.3, h: 10.3 }, 2, false);
    expect((odd.ox * 2) % 1).toBe(0);
  });
});

describe('変換', () => {
  it('点を動かさずに倍率を替える', () => {
    const t = zoomAround({ zoom: 2, ox: 10, oy: 20 }, 4, 110, 220);
    // 画面 (110, 220) の下は画像 (50, 100)。倍率 4 でも同じ画面の点に来る
    expect(110 - t.ox).toBe(50 * 4);
    expect(220 - t.oy).toBe(100 * 4);
  });

  it('画面の点の下の画素（床）', () => {
    expect(imagePixel({ zoom: 4, ox: 10, oy: 10 }, 13.9, 17)).toEqual([0, 1]);
    expect(imagePixel({ zoom: 4, ox: 10, oy: 10 }, 9, 9)).toEqual([-1, -1]);
  });

  it('画像の一部が面に残るように抑える', () => {
    const stage = { w: 800, h: 600 };
    const image = { w: 100, h: 100 };
    expect(clampTransform({ zoom: 1, ox: -500, oy: 900 }, stage, image)).toEqual({
      zoom: 1,
      ox: 48 - 100,
      oy: 600 - 48,
    });
    // 画像が残す量より小さければ、画像の大きさだけ残す
    expect(clampTransform({ zoom: 0.1, ox: -500, oy: 0 }, stage, image).ox).toBe(0);
  });
});

describe('見えている範囲とタイル', () => {
  it('見えている画像の範囲（等倍の整数。面の外へ広げて画像に切る）', () => {
    const t = { zoom: 2, ox: -100, oy: 50 };
    expect(visibleImageRect(t, { w: 400, h: 300 }, { w: 1000, h: 1000 })).toEqual({
      x: 50,
      y: 0,
      w: 200,
      h: 125,
    });
    expect(visibleImageRect(t, { w: 400, h: 300 }, { w: 1000, h: 1000 }, 20)).toEqual({
      x: 40,
      y: 0,
      w: 220,
      h: 135,
    });
    expect(
      visibleImageRect({ zoom: 1, ox: 900, oy: 0 }, { w: 400, h: 300 }, { w: 100, h: 100 }),
    ).toBeNull();
  });

  it('段のタイルの範囲（始まりは床・終わりは天井。フレームの位置を引く）', () => {
    const frame = { x: 0, y: 0, w: 600, h: 300 };
    expect(tileRange({ x: 0, y: 0, w: 600, h: 300 }, 0, frame, 256)).toEqual({
      tx0: 0,
      ty0: 0,
      tx1: 3,
      ty1: 2,
    });
    expect(tileRange({ x: 300, y: 10, w: 10, h: 10 }, 0, frame, 256)).toEqual({
      tx0: 1,
      ty0: 0,
      tx1: 2,
      ty1: 1,
    });
    // 段 1：等倍 1100 は段の 550 → タイル 2
    const half = { x: 0, y: 0, w: 1000, h: 500 };
    expect(tileRange({ x: 1100, y: 0, w: 2, h: 2 }, 1, half, 256)).toEqual({
      tx0: 2,
      ty0: 0,
      tx1: 3,
      ty1: 1,
    });
    // トリムしたフレーム（キャンバスの 40, 40 から）
    const trimmed = { x: 40, y: 40, w: 20, h: 20 };
    expect(tileRange({ x: 0, y: 0, w: 30, h: 30 }, 0, trimmed, 256)).toBeNull();
    expect(tilesIn(tileRange({ x: 45, y: 45, w: 5, h: 5 }, 0, trimmed, 256))).toEqual([[0, 0]]);
  });

  it('タイルの大きさと等倍の矩形（右端・下端は小さい）', () => {
    const frame = { x: 0, y: 0, w: 600, h: 300 };
    expect(tileSize(frame, 2, 1, 256)).toEqual({ w: 88, h: 44 });
    expect(tileImageRect(0, frame, 2, 1, 256)).toEqual({ x: 512, y: 256, w: 88, h: 44 });
    expect(tileImageRect(1, frame, 1, 0, 256)).toEqual({ x: 512, y: 0, w: 512, h: 512 });
  });

  it('拡大は最近傍、装置の画素で 1 倍未満の縮小だけ補間', () => {
    expect(smoothingFor(1, 2, 0)).toBe(false);
    expect(smoothingFor(0.5, 2, 0)).toBe(false);
    expect(smoothingFor(0.4, 2, 0)).toBe(true);
    expect(smoothingFor(0.2, 2, 2)).toBe(false); // 段 2 の画素は等倍 4 つ分
  });
});

describe('ほか', () => {
  it('倍率の文字', () => {
    expect(zoomPercent(2)).toBe('200%');
    expect(zoomPercent(0.125)).toBe('13%');
    expect(zoomPercent(0.0625)).toBe('6.3%');
  });

  it('透明を持てる形式', () => {
    for (const f of ['rgba8', 'grayA8', 'rgba16', 'grayA16', 'indexed8']) {
      expect(formatCanBeTransparent(f)).toBe(true);
    }
    for (const f of ['rgb8', 'gray8', 'rgb16', 'gray16', null]) {
      expect(formatCanBeTransparent(f)).toBe(false);
    }
  });

  it('端を整数にそろえると、隣り合う矩形のあいだに隙間ができない', () => {
    const a = roundRect({ x: 0.4, y: 0, w: 10.3, h: 1 });
    const b = roundRect({ x: 10.7, y: 0, w: 10.3, h: 1 });
    expect(a.x + a.w).toBe(b.x);
  });

  it('ホイールの量（行・ページ単位を直す）', () => {
    expect(wheelPixels(3, 0, 500)).toBe(3);
    expect(wheelPixels(3, 1, 500)).toBe(48);
    expect(wheelPixels(1, 2, 500)).toBe(500);
  });
});
