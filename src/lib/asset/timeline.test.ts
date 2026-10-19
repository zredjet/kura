import { describe, expect, it } from 'vitest';

import gridText from '../../../crates/kura-engine/tests/vectors/sprite/grid-cells.json?raw';

import {
  type AnimTag,
  type Direction,
  MAX_FRAMES,
  cellsOnAxis,
  effectiveDuration,
  frameAt,
  gridCells,
  sequence,
  startOfStep,
  stripCell,
  track,
} from './timeline';

interface AxisCase {
  len: number;
  offset: number;
  cell: number;
  gap: number;
  count: number;
}

interface GridCase {
  name: string;
  width: number;
  height: number;
  grid: {
    cellW: number;
    cellH: number;
    offsetX: number;
    offsetY: number;
    gapX: number;
    gapY: number;
  };
  cells: [number, number, number, number][];
}

const vectors = JSON.parse(gridText) as { axis: AxisCase[]; grids: GridCase[] };

const tag = (direction: Direction, from = 1, to = 4): AnimTag => ({
  name: 't',
  from,
  to,
  direction,
  repeat: 0,
});

describe('アニメのコマの並び', () => {
  it('向きごとの 1 周（ピンポンは端のコマを重ねない）', () => {
    expect(sequence(6)).toEqual([0, 1, 2, 3, 4, 5]);
    expect(sequence(6, tag('forward'))).toEqual([1, 2, 3, 4]);
    expect(sequence(6, tag('reverse'))).toEqual([4, 3, 2, 1]);
    expect(sequence(6, tag('pingPong'))).toEqual([1, 2, 3, 4, 3, 2]);
    expect(sequence(6, tag('pingPongReverse'))).toEqual([4, 3, 2, 1, 2, 3]);
  });

  it('1 コマ・2 コマのピンポン', () => {
    expect(sequence(6, tag('pingPong', 2, 2))).toEqual([2]);
    expect(sequence(6, tag('pingPong', 2, 3))).toEqual([2, 3]);
    expect(sequence(6, tag('pingPongReverse', 2, 3))).toEqual([3, 2]);
  });

  it('タグの範囲はコマの数で切る・逆向きの範囲・範囲の外は先頭のコマ', () => {
    expect(sequence(3, tag('forward', 1, 9))).toEqual([1, 2]);
    expect(sequence(6, tag('forward', 4, 1))).toEqual([1, 2, 3, 4]);
    expect(sequence(3, tag('forward', 7, 9))).toEqual([0]);
    expect(sequence(0)).toEqual([]);
    expect(sequence(0, tag('pingPong'))).toEqual([]);
  });
});

describe('コマの長さと時間軸', () => {
  it('0ms（と読めない値）は 100ms、ほかはファイルのまま', () => {
    expect(effectiveDuration(0)).toBe(100);
    expect(effectiveDuration(-5)).toBe(100);
    expect(effectiveDuration(Number.NaN)).toBe(100);
    expect(effectiveDuration(1)).toBe(1);
    expect(effectiveDuration(16)).toBe(16);
    expect(effectiveDuration(65_535)).toBe(65_535);
  });

  it('経過時間からコマを決める（刻みに依らない・周期で回る）', () => {
    const tr = track([100, 50, 0, 200]);
    expect(tr.period).toBe(100 + 50 + 100 + 200);
    expect(frameAt(tr, 0)).toEqual({ frame: 0, step: 0 });
    expect(frameAt(tr, 99.9).frame).toBe(0);
    expect(frameAt(tr, 100).frame).toBe(1);
    expect(frameAt(tr, 150).frame).toBe(2);
    expect(frameAt(tr, 449).frame).toBe(3);
    expect(frameAt(tr, 450).frame).toBe(0);
    expect(frameAt(tr, 450 * 1000 + 160).frame).toBe(2);
    expect(frameAt(tr, -1).frame).toBe(3);
    expect(frameAt(track([]), 10)).toEqual({ frame: 0, step: 0 });
  });

  it('タグの向きで時刻の表を作る（ピンポンの戻りもコマの長さで）', () => {
    const tr = track([10, 20, 30, 40, 50], tag('pingPong', 1, 3));
    expect(tr.steps).toEqual([1, 2, 3, 2]);
    // コマ 1・2・3・2 の長さは 20・30・40・30
    expect(tr.starts).toEqual([0, 20, 50, 90]);
    expect(tr.period).toBe(120);
    expect(frameAt(tr, 95)).toEqual({ frame: 2, step: 3 });
  });

  it('1ms のコマが続いても全体の時間は正しい（16ms の刻みで飛ばすだけ）', () => {
    const tr = track(new Array<number>(100).fill(1));
    expect(tr.period).toBe(100);
    expect(frameAt(tr, 16).frame).toBe(16);
    expect(frameAt(tr, 116).frame).toBe(16);
  });

  it('コマ送り：手順の始まりの時刻（前へも回る）', () => {
    const tr = track([100, 50, 0, 200]);
    expect(startOfStep(tr, 0)).toBe(0);
    expect(startOfStep(tr, 2)).toBe(150);
    expect(startOfStep(tr, 4)).toBe(0);
    expect(startOfStep(tr, -1)).toBe(250);
    expect(startOfStep(track([]), 3)).toBe(0);
    expect(frameAt(tr, startOfStep(tr, 3)).step).toBe(3);
  });
});

describe('格子のセル（エンジンと共有のベクタ。sprite.split_grid と同じ数え方・並び）', () => {
  it('ベクタがある', () => {
    expect(vectors.axis.length).toBeGreaterThan(10);
    expect(vectors.grids.length).toBeGreaterThan(5);
  });

  it('1 つの軸に収まるセルの数', () => {
    for (const c of vectors.axis) {
      expect(cellsOnAxis(c.len, c.offset, c.cell, c.gap), JSON.stringify(c)).toBe(c.count);
    }
  });

  for (const c of vectors.grids) {
    it(`セルの矩形：${c.name}`, () => {
      expect(gridCells(c.width, c.height, c.grid).map((r) => [r.x, r.y, r.w, r.h])).toEqual(
        c.cells,
      );
    });
  }

  it('数を絞る・既定の上限はエンジンのコマの数の上限（4,096）', () => {
    const g = { cellW: 16, cellH: 8, offsetX: 2, offsetY: 1, gapX: 2, gapY: 2 };
    expect(gridCells(36, 20, g, 3)).toHaveLength(3);
    expect(gridCells(36, 20, g, 0)).toEqual([]);
    expect(MAX_FRAMES).toBe(4096);
    const one = { cellW: 1, cellH: 1, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0 };
    expect(gridCells(2 ** 32 - 1, 2 ** 32 - 1, one)).toHaveLength(MAX_FRAMES);
  });

  it('帯の中のコマの位置（行ごとに左から cols 個ずつ）', () => {
    expect(stripCell(0, 4, 32, 24)).toEqual({ x: 0, y: 0 });
    expect(stripCell(5, 4, 32, 24)).toEqual({ x: 32, y: 24 });
    expect(stripCell(3, 1, 32, 24)).toEqual({ x: 0, y: 72 });
    expect(stripCell(3, 0, 32, 24)).toEqual({ x: 0, y: 0 });
  });
});
