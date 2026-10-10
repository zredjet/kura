import { describe, expect, it } from 'vitest';

import { justify, ratioOf, rowOfItem, tileWidths, type TileRow } from './justify';

const base = { width: 1000, targetHeight: 200, gap: 6, headingHeight: 32 };

describe('justify', () => {
  it('幅を超えたところで行を区切り、ちょうど収まる高さにする', () => {
    // 比 1.5 が 4 つ：目標 200 だと 4 つで 1218 > 1000。4 つ入れると高さ 164、3 つで区切ると 220。
    // 目標の 200 に近い 3 つで区切る
    const layout = justify({ ...base, aspects: [1500, 1500, 1500, 1500], groupStarts: [0] });
    const rows = layout.rows;
    expect(rows[0]).toMatchObject({ kind: 'heading', count: 4, y: 0, height: 32 });
    const first = rows[1] as TileRow;
    expect(first).toMatchObject({ kind: 'tiles', start: 0, end: 3, y: 38, justified: true });
    // (1000 - 12) / 4.5
    expect(first.height).toBeCloseTo(219.56, 1);
    const last = rows[2] as TileRow;
    expect(last).toMatchObject({ start: 3, end: 4, height: 200, justified: false });
    expect(layout.totalHeight).toBeCloseTo(38 + first.height + 6 + 200, 5);
  });

  it('日付のグループごとに見出しを入れ、行をまたがない', () => {
    const layout = justify({
      ...base,
      aspects: [1000, 1000, 1000, 1000, 1000],
      groupStarts: [0, 2],
    });
    const kinds = layout.rows.map((r) =>
      r.kind === 'heading' ? `h${r.count}` : `${r.start}-${r.end}`,
    );
    expect(kinds).toEqual(['h2', '0-2', 'h3', '2-5']);
  });

  it('どの行の幅もコンテナ幅に揃う（最後の行を除く）', () => {
    const aspects = Array.from(
      { length: 500 },
      (_, i) => [563, 1000, 1333, 1500, 1778, 707][i % 6] ?? 1000,
    );
    const layout = justify({ ...base, width: 1234, aspects, groupStarts: [0, 120, 300] });
    for (const row of layout.rows) {
      if (row.kind !== 'tiles' || !row.justified) {
        continue;
      }
      const widths = tileWidths(row, aspects, 1234, 6);
      const total = widths.reduce((a, b) => a + b, 0) + 6 * (widths.length - 1);
      expect(total).toBe(1234);
      // 目標から大きく外れない
      expect(row.height).toBeGreaterThan(200 * 0.6);
      expect(row.height).toBeLessThan(200 * 1.6);
    }
  });

  it('10 万件でも速い', () => {
    const aspects = new Uint16Array(100_000).map((_, i) => [563, 1000, 1500, 1778][i % 4] ?? 1000);
    const groupStarts = Array.from({ length: 700 }, (_, i) => i * 142);
    const started = performance.now();
    const layout = justify({ ...base, aspects, groupStarts });
    expect(performance.now() - started).toBeLessThan(100);
    expect(layout.rows.length).toBeGreaterThan(10_000);
  });

  it('極端な縦横比と空の一覧', () => {
    expect(ratioOf(65535)).toBe(4);
    expect(ratioOf(1)).toBe(0.25);
    expect(ratioOf(0)).toBe(1);
    expect(justify({ ...base, aspects: [], groupStarts: [] })).toEqual({
      rows: [],
      totalHeight: 0,
    });
  });
});

describe('rowOfItem', () => {
  it('アイテムのある行を返す', () => {
    const layout = justify({
      ...base,
      aspects: [1000, 1000, 1000, 1000, 1000],
      groupStarts: [0, 2],
    });
    expect(rowOfItem(layout, 0)).toBe(1);
    expect(rowOfItem(layout, 1)).toBe(1);
    expect(rowOfItem(layout, 2)).toBe(3);
    expect(rowOfItem(layout, 4)).toBe(3);
    expect(rowOfItem(layout, 9)).toBe(-1);
  });

  it('lays out ungrouped lists without headings', () => {
    const layout = justify({ ...base, aspects: [1500, 1500, 1500], groupStarts: [] });
    expect(layout.rows.every((r) => r.kind === 'tiles')).toBe(true);
    expect(layout.rows[0]?.y).toBe(0);
  });
});
