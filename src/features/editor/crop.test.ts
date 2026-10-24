import { describe, expect, it } from 'vitest';

import {
  clampCrop,
  dragCrop,
  isFullCrop,
  newCropRect,
  nudgeCrop,
  readRect,
  sameRect,
} from './crop';

const B = { w: 100, h: 50 };
const R = { x: 10, y: 10, w: 40, h: 20 };

describe('切り抜きの枠', () => {
  it('画像の中の整数の矩形に収める（幅・高さは 1 以上）', () => {
    expect(clampCrop({ x: -5.4, y: 3.6, w: 200, h: 0 }, B)).toEqual({ x: 0, y: 4, w: 100, h: 1 });
    expect(clampCrop({ x: 90, y: 45, w: 20, h: 10 }, B)).toEqual({ x: 80, y: 40, w: 20, h: 10 });
  });

  it('枠の外から取り直す：押した点と今の点を含む整数の矩形（画像の外へはみ出しても押した辺は動かない）', () => {
    expect(newCropRect(10.5, 5.2, 30.1, 20.9, B)).toEqual({ x: 10, y: 5, w: 21, h: 16 });
    // 逆向きのドラッグ
    expect(newCropRect(30, 20, 10, 5, B)).toEqual({ x: 10, y: 5, w: 20, h: 15 });
    // 右下へはみ出す：左上（押した点）は 50・30 のまま、端で止める
    expect(newCropRect(50, 30, 170, 130, B)).toEqual({ x: 50, y: 30, w: 50, h: 20 });
    // 左上へはみ出す：右下（押した点）は動かない
    expect(newCropRect(60, 40, -10, -10, B)).toEqual({ x: 0, y: 0, w: 60, h: 40 });
    // 画像の外で押して外で離しても 1px は残す
    expect(newCropRect(120, 10, 130, 10, B)).toEqual({ x: 99, y: 10, w: 1, h: 1 });
  });

  it('中をドラッグすると動かす（画像の外へは出ない。小数は丸める）', () => {
    expect(dragCrop(R, 'move', 5.4, -2.6, B)).toEqual({ x: 15, y: 7, w: 40, h: 20 });
    expect(dragCrop(R, 'move', 500, 500, B)).toEqual({ x: 60, y: 30, w: 40, h: 20 });
  });

  it('辺・隅で広げる（向かいは動かない。裏返らず 1px は残す）', () => {
    expect(dragCrop(R, 'e', 7, 0, B)).toEqual({ x: 10, y: 10, w: 47, h: 20 });
    expect(dragCrop(R, 'nw', -3, -4, B)).toEqual({ x: 7, y: 6, w: 43, h: 24 });
    expect(dragCrop(R, 'w', 100, 0, B)).toEqual({ x: 49, y: 10, w: 1, h: 20 });
    expect(dragCrop(R, 's', 0, 1000, B)).toEqual({ x: 10, y: 10, w: 40, h: 40 });
  });

  it('⇧ で比を保つ：隅は向かいの隅、辺は向かいの辺の中央を止める', () => {
    // 2:1 の枠を右下へ広げる（大きく動いた軸に合わせる）
    expect(dragCrop(R, 'se', 20, 2, B, true)).toEqual({ x: 10, y: 10, w: 60, h: 30 });
    // 画像に収まるまで縮める
    expect(dragCrop(R, 'se', 200, 0, B, true)).toEqual({ x: 10, y: 10, w: 80, h: 40 });
    // 右の辺：高さは中央（y 20）を保つ
    expect(dragCrop(R, 'e', 20, 0, B, true)).toEqual({ x: 10, y: 5, w: 60, h: 30 });
    // 上の辺：幅は中央（x 30）を保つ
    expect(dragCrop(R, 'n', 0, -5, B, true)).toEqual({ x: 5, y: 5, w: 50, h: 25 });
  });

  it('矢印で 1px・⇧ で 10px 動かす（画像の外へは出ない）', () => {
    expect(nudgeCrop(R, 'ArrowLeft', false, B)).toEqual({ x: 9, y: 10, w: 40, h: 20 });
    expect(nudgeCrop(R, 'ArrowDown', true, B)).toEqual({ x: 10, y: 20, w: 40, h: 20 });
    expect(nudgeCrop(R, 'ArrowDown', true, { w: 100, h: 35 })).toEqual({
      x: 10,
      y: 15,
      w: 40,
      h: 20,
    });
    expect(nudgeCrop(R, 'Enter', false, B)).toBeNull();
  });

  it('全体か・同じか・矩形の値を読む', () => {
    expect(isFullCrop({ x: 0, y: 0, w: 100, h: 50 }, B)).toBe(true);
    expect(isFullCrop(R, B)).toBe(false);
    expect(sameRect(R, { ...R })).toBe(true);
    expect(sameRect(R, null)).toBe(false);
    expect(readRect({ x: 1, y: 2, w: 3, h: 4 })).toEqual({ x: 1, y: 2, w: 3, h: 4 });
    expect(readRect({ x: 1 })).toBeNull();
  });
});
