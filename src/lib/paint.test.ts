import { describe, expect, it } from 'vitest';

import overText from '../../crates/kura-engine/tests/vectors/paint/over.json?raw';
import invalidText from '../../crates/kura-engine/tests/vectors/paint/points-invalid.json?raw';
import validText from '../../crates/kura-engine/tests/vectors/paint/points-valid.json?raw';
import strokesText from '../../crates/kura-engine/tests/vectors/paint/strokes.json?raw';

import { type BrushShape, over8, packPoints, path, strokePixels, unpackPoints } from './paint';

interface StrokeCase {
  name: string;
  width: number;
  height: number;
  brush: { shape: BrushShape; size: number };
  pixelPerfect: boolean;
  points: [number, number][];
  packed: string;
  path: [number, number][];
  pixels: [number, number][];
}

interface OverCase {
  fmt: string;
  dst: number[];
  src: number[];
  out: number[];
}

const strokes = (JSON.parse(strokesText) as { cases: StrokeCase[] }).cases;
const overs = (JSON.parse(overText) as { cases: OverCase[] }).cases;
const invalid = (JSON.parse(invalidText) as { cases: { name: string; packed: string }[] }).cases;
const valid = (
  JSON.parse(validText) as {
    cases: { name: string; points: [number, number][]; packed: string }[];
  }
).cases;

// エンジンが凍結したベクタと同じ結果になること（docs/editing.md §10、E21）
describe('描画の規則（エンジンと共有のベクタ）', () => {
  it('ベクタがある', () => {
    expect(strokes.length).toBeGreaterThan(10);
    expect(overs.length).toBeGreaterThan(5);
  });

  for (const c of strokes) {
    it(`ストローク：${c.name}`, () => {
      expect(packPoints(c.points)).toBe(c.packed);
      expect(unpackPoints(c.packed)).toEqual(c.points);
      expect(path(c.points, c.pixelPerfect)).toEqual(c.path);
      expect(strokePixels(c.points, c.brush, c.pixelPerfect, c.width, c.height)).toEqual(c.pixels);
    });
  }

  it('重ねる式', () => {
    for (const c of overs) {
      expect(over8(c.dst, c.src)).toEqual(c.out);
    }
  });

  for (const c of valid) {
    it(`読める点の列の境界：${c.name}`, () => {
      expect(packPoints(c.points)).toBe(c.packed);
      expect(unpackPoints(c.packed)).toEqual(c.points);
    });
  }

  for (const c of invalid) {
    it(`読めない点の列：${c.name}`, () => {
      expect(unpackPoints(c.packed)).toBeNull();
    });
  }

  it('正規でない点の列は読まない', () => {
    expect(unpackPoints('A')).toBeNull();
    expect(unpackPoints('AAB')).toBeNull();
    expect(unpackPoints('gA')).toBeNull();
    expect(unpackPoints('A*A')).toBeNull();
    expect(unpackPoints('')).toEqual([]);
  });
});
