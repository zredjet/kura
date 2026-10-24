import { describe, expect, it } from 'vitest';

import strokesText from '../../../crates/kura-engine/tests/vectors/paint/strokes.json?raw';

import { type BrushShape, type Point, strokePixels, unpackPoints } from '../../lib/paint';
import {
  fillAction,
  OverlayTracker,
  overlayMode,
  StrokeBuilder,
  StrokeOverlay,
  type StrokeSpec,
  strokeAction,
} from './stroke';

interface StrokeCase {
  name: string;
  width: number;
  height: number;
  brush: { shape: BrushShape; size: number };
  pixelPerfect: boolean;
  points: [number, number][];
}

const vectors = (JSON.parse(strokesText) as { cases: StrokeCase[] }).cases;

function spec(over: Partial<StrokeSpec> = {}): StrokeSpec {
  return {
    tool: 'pen',
    color: { rgba: [255, 0, 0, 255], index: null },
    blend: 'over',
    brush: { shape: 'square', size: 1 },
    pixelPerfect: false,
    frame: 0,
    bounds: { x: 0, y: 0, w: 64, h: 64 },
    indexed: false,
    ...over,
  };
}

/** 点を 1 つずつ足して置いた画素（範囲の中だけ。行の順）。 */
function incremental(s: StrokeSpec, points: readonly Point[]): Point[] {
  const overlay = new StrokeOverlay(s.frame, s.bounds, 'over', [0, 0, 0, 255]);
  const placed = new Set<string>();
  const b = new StrokeBuilder(s, (x, y) => {
    if (overlay.plot(x, y)) placed.add(`${x},${y}`);
  });
  for (const p of points) b.add(p);
  b.finish();
  return [...placed]
    .map((k) => k.split(',').map(Number) as unknown as Point)
    .sort((a, c) => a[1] - c[1] || a[0] - c[0]);
}

/** 決まった種の擬似乱数（テストを毎回同じにする）。 */
function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

describe('仮の筆（StrokeBuilder）', () => {
  it.each(vectors)('共有のベクタ「$name」：点を 1 つずつ足して置いた画素がエンジンと同じ', (c) => {
    const s = spec({
      brush: c.brush,
      pixelPerfect: c.pixelPerfect,
      bounds: { x: 0, y: 0, w: c.width, h: c.height },
    });
    expect(incremental(s, c.points)).toEqual(
      strokePixels(c.points, c.brush, c.pixelPerfect, c.width, c.height),
    );
  });

  it('乱数の点の列でも、足しながら置いた画素は全体で求めたものと同じ', () => {
    const next = rng(7);
    for (let trial = 0; trial < 60; trial++) {
      const n = 2 + Math.floor(next() * 12);
      const points: Point[] = [];
      for (let i = 0; i < n; i++) {
        points.push([Math.floor(next() * 40) - 4, Math.floor(next() * 40) - 4]);
      }
      const brush = {
        shape: next() < 0.5 ? 'square' : 'circle',
        size: 1 + Math.floor(next() * 5),
      } as const;
      const pixelPerfect = next() < 0.5;
      const s = spec({ brush, pixelPerfect, bounds: { x: 0, y: 0, w: 32, h: 32 } });
      expect(incremental(s, points)).toEqual(strokePixels(points, brush, pixelPerfect, 32, 32));
    }
  });

  it('ピクセルパーフェクトでは最後の点は次の点か描き終えるまで置かない（L 字の角は置かない）', () => {
    const placed: string[] = [];
    const b = new StrokeBuilder(spec({ pixelPerfect: true }), (x, y) => placed.push(`${x},${y}`));
    b.add([0, 0]);
    expect(placed).toEqual([]);
    b.add([1, 0]);
    expect(placed).toEqual(['0,0']);
    b.add([1, 1]); // (1,0) は L 字の角なので除かれる
    expect(placed).toEqual(['0,0']);
    b.finish();
    expect(placed).toEqual(['0,0', '1,1']);
  });

  it('同じ点が続いても数えない', () => {
    const b = new StrokeBuilder(spec(), () => undefined);
    b.add([3, 3]);
    expect(b.add([3, 3])).toBe(0);
    expect(b.points).toEqual([[3, 3]]);
  });
});

describe('仮の画素（StrokeOverlay）', () => {
  it('範囲の外と置いた画素は置かない。区画ごとに色と覆いを持つ', () => {
    const o = new StrokeOverlay(0, { x: 0, y: 0, w: 300, h: 10 }, 'replace', [1, 2, 3, 128]);
    expect(o.plot(-1, 0)).toBe(false);
    expect(o.plot(300, 0)).toBe(false);
    expect(o.plot(5, 5)).toBe(true);
    expect(o.plot(5, 5)).toBe(false);
    expect(o.plot(260, 0)).toBe(true);
    expect([...o.chunks.keys()].sort()).toEqual(['0,0', '1,0']);
    const c = o.chunks.get('0,0');
    const i = (5 * 256 + 5) * 4;
    expect([...(c?.color.slice(i, i + 4) ?? [])]).toEqual([1, 2, 3, 128]);
    expect(c?.mask[i + 3]).toBe(255);
    expect(o.pixelCount).toBe(2);
  });

  it('描き方：消しゴムは抜く、置き換え（とパレットの画像）は抜いてから色、重ねるは色だけ', () => {
    expect(overlayMode({ tool: 'eraser', blend: 'over', indexed: false })).toBe('erase');
    expect(overlayMode({ tool: 'pen', blend: 'replace', indexed: false })).toBe('replace');
    expect(overlayMode({ tool: 'pen', blend: 'over', indexed: true })).toBe('replace');
    expect(overlayMode({ tool: 'pen', blend: 'over', indexed: false })).toBe('over');
  });
});

describe('仮の筆の寿命（OverlayTracker）', () => {
  const overlay = () => new StrokeOverlay(0, { x: 0, y: 0, w: 8, h: 8 }, 'over', [0, 0, 0, 255]);

  it('送った要求以降の表示が届き、見えている変わったタイルを解き終えたら外す', () => {
    const tr = new OverlayTracker();
    tr.begin(overlay());
    const p = tr.commit();
    expect(p).not.toBeNull();
    if (!p) return;
    expect(tr.overlays()).toHaveLength(1);
    tr.sent(p, 4, 0);
    // 前の要求の表示では外さない
    tr.shown(3, new Set());
    expect(tr.pending).toHaveLength(1);
    tr.shown(4, new Set(['0/0/0/0', '0/0/1/0']));
    expect(tr.pending).toHaveLength(1);
    tr.tileReady('0/0/0/0');
    expect(tr.pending).toHaveLength(1);
    expect(tr.tileReady('0/0/1/0')).toBe(true);
    expect(tr.overlays()).toHaveLength(0);
  });

  it('待つタイルがなければすぐ外す。新しい要求に取って代わられたら次の表示を待つ', () => {
    const tr = new OverlayTracker();
    tr.begin(overlay());
    const p = tr.commit();
    if (!p) throw new Error('筆がない');
    tr.sent(p, 2, 0);
    // superseded：何も呼ばない（次の表示を待つ）
    expect(tr.pending).toHaveLength(1);
    expect(tr.shown(3, new Set())).toBe(true);
    expect(tr.pending).toHaveLength(0);
  });

  it('断られた手より後の筆と、失敗した要求の筆はすぐ外す。まだ送っていない筆は残す', () => {
    const tr = new OverlayTracker();
    const ps = [overlay(), overlay(), overlay(), overlay()].map((o) => {
      tr.begin(o);
      const p = tr.commit();
      if (!p) throw new Error('筆がない');
      return p;
    });
    const [a, b, c] = ps;
    if (!a || !b || !c) throw new Error('筆がない');
    tr.sent(a, 5, 0);
    tr.sent(b, 5, 1);
    tr.sent(c, 6, 0);
    tr.rejected(5, 1);
    expect(tr.pending).toEqual([a, c, ps[3]]);
    tr.failed(6);
    expect(tr.pending).toEqual([ps[3]]);
  });

  it('描いている筆はやめられる（送らない）', () => {
    const tr = new OverlayTracker();
    tr.begin(overlay());
    expect(tr.cancel()).toBe(true);
    expect(tr.commit()).toBeNull();
    expect(tr.overlays()).toEqual([]);
  });
});

describe('描画の動作の JSON（docs/editing.md §10）', () => {
  it('ペン：色・重ね方・筆・ピクセルパーフェクト・フレーム・詰めた点', () => {
    const a = strokeAction(spec({ blend: 'replace', frame: 2 }), [
      [1, 1],
      [3, 2],
    ]);
    expect(a).toEqual({
      type: 'stroke',
      tool: 'pen',
      color: { rgba: [255, 0, 0, 255] },
      blend: 'replace',
      brush: { shape: 'square', size: 1 },
      pixelPerfect: false,
      frame: 2,
      points: a.points,
    });
    expect(unpackPoints(a.points)).toEqual([
      [1, 1],
      [3, 2],
    ]);
  });

  it('消しゴムは色なし・置き換え。パレットの画像はスポイトの番号で置き換え', () => {
    expect(strokeAction(spec({ tool: 'eraser', color: null }), [[0, 0]])).toMatchObject({
      tool: 'eraser',
      color: null,
      blend: 'replace',
    });
    const indexed = strokeAction(
      spec({ indexed: true, color: { rgba: [9, 9, 9, 255], index: 4 } }),
      [[0, 0]],
    );
    expect(indexed).toMatchObject({ color: { index: 4 }, blend: 'replace' });
  });

  it('塗りつぶし：起点・色・許容差（パレットは 0）・つながり方・つながった範囲だけ', () => {
    const fill = {
      color: { rgba: [0, 0, 255, 255] as [number, number, number, number], index: null },
      tolerance: 12,
      connectivity: 'eight' as const,
      contiguous: false,
      frame: 0,
      indexed: false,
    };
    expect(fillAction(fill, [4, 5])).toEqual({
      type: 'fill',
      at: { x: 4, y: 5 },
      color: { rgba: [0, 0, 255, 255] },
      tolerance: 12,
      connectivity: 'eight',
      contiguous: false,
      frame: 0,
    });
    expect(
      fillAction({ ...fill, indexed: true, color: { ...fill.color, index: 2 } }, [0, 0]),
    ).toMatchObject({ color: { index: 2 }, tolerance: 0 });
  });
});
