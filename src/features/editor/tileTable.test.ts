import { describe, expect, it, type Mock, vi } from 'vitest';

import { type TileBitmap, TileTable, tileKey } from './tileTable';

function bitmap(w = 256, h = 256): TileBitmap & { close: Mock<() => void> } {
  return { width: w, height: h, close: vi.fn<() => void>() };
}

describe('タイルの rev の表', () => {
  it('all は段のタイルをすべてその rev に、tiles は挙がったタイルだけにする', () => {
    const t = new TileTable();
    expect(t.rev(0, 0, 1, 1)).toBeNull();
    t.markAll(0, 5);
    expect(t.rev(0, 0, 1, 1)).toBe(5);
    expect(t.rev(1, 0, 1, 1)).toBeNull();
    t.markTiles(0, 0, [[1, 1]], 7);
    expect(t.rev(0, 0, 1, 1)).toBe(7);
    expect(t.rev(0, 0, 0, 0)).toBe(5);
    // undo は前の rev を返す：変わったタイルだけがその番号になる
    t.markTiles(0, 0, [[1, 1]], 5);
    expect(t.rev(0, 0, 1, 1)).toBe(5);
    // もう一度 all なら、それまでの個別の番号は消える
    t.markTiles(0, 0, [[2, 0]], 8);
    t.markAll(0, 9);
    expect(t.rev(0, 0, 2, 0)).toBe(9);
  });
});

describe('解いた絵', () => {
  it('同じ rev を二重に取りに行かず、失敗した rev は取り直さない（失敗の印は消せる）', () => {
    const t = new TileTable();
    const key = tileKey(0, 0, 0, 0);
    expect(t.needsLoad(key, 3)).toBe(true);
    t.startLoad(key, 3);
    expect(t.needsLoad(key, 3)).toBe(false);
    expect(t.isLoading(key)).toBe(true);
    t.failLoad(key, 3);
    expect(t.isLoading(key)).toBe(false);
    expect(t.needsLoad(key, 3)).toBe(false);
    t.clearFailures();
    expect(t.needsLoad(key, 3)).toBe(true);
    const b = bitmap();
    t.startLoad(key, 3);
    t.finishLoad(key, 3, b);
    expect(t.needsLoad(key, 3)).toBe(false);
    expect(t.needsLoad(key, 4)).toBe(true);
    expect(t.get(key)).toEqual({ bitmap: b, rev: 3 });
  });

  it('新しい絵が来たら前の絵は閉じる（古い rev の絵は、新しいのが来るまで出せる）', () => {
    const t = new TileTable();
    const key = tileKey(0, 0, 0, 0);
    const a = bitmap();
    const b = bitmap();
    t.finishLoad(key, 1, a);
    t.startLoad(key, 2);
    expect(t.get(key)?.rev).toBe(1);
    t.finishLoad(key, 2, b);
    expect(a.close).toHaveBeenCalledOnce();
    expect(t.bytes).toBe(256 * 256 * 4);
  });

  it('上限を超えたら、見えていないものから使った順の古いものを閉じて捨てる', () => {
    const one = 256 * 256 * 4;
    const t = new TileTable(one * 2);
    const bs = [bitmap(), bitmap(), bitmap()];
    bs.forEach((b, i) => t.finishLoad(tileKey(0, 0, i, 0), 1, b));
    t.get(tileKey(0, 0, 0, 0)); // 0 を使った（いちばん新しい）
    const dropped = t.evict(new Set([tileKey(0, 0, 2, 0)]));
    expect(dropped).toEqual([tileKey(0, 0, 1, 0)]);
    expect(bs[1]?.close).toHaveBeenCalledOnce();
    expect(t.bytes).toBe(one * 2);
    expect(t.count).toBe(2);
  });

  it('段を替えたとき、その段の古い rev の絵は捨てる', () => {
    const t = new TileTable();
    const old = bitmap();
    const keep = bitmap();
    t.finishLoad(tileKey(1, 0, 0, 0), 3, old);
    t.finishLoad(tileKey(1, 0, 1, 0), 5, keep);
    t.finishLoad(tileKey(0, 0, 0, 0), 3, bitmap());
    t.dropStale(1, 5);
    expect(old.close).toHaveBeenCalledOnce();
    expect(t.get(tileKey(1, 0, 0, 0))).toBeNull();
    expect(t.get(tileKey(1, 0, 1, 0))?.rev).toBe(5);
    expect(t.get(tileKey(0, 0, 0, 0))?.rev).toBe(3);
  });

  it('閉じたら（clear）すべての絵を close する', () => {
    const t = new TileTable();
    const bs = [bitmap(), bitmap(10, 10)];
    bs.forEach((b, i) => t.finishLoad(tileKey(0, 0, i, 0), 1, b));
    t.clear();
    expect(bs.every((b) => b.close.mock.calls.length === 1)).toBe(true);
    expect(t.bytes).toBe(0);
    expect(t.rev(0, 0, 0, 0)).toBeNull();
  });
});
