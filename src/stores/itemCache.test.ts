import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ItemSummaryDto } from '../ipc/items';
import type { UnitSummaryDto } from '../ipc/units';

const db = vi.hoisted(() => ({
  items: new Map<number, Record<string, unknown>>(),
  units: new Map<number, Record<string, unknown>>(),
  itemCalls: [] as number[][],
  unitCalls: [] as number[][],
}));

vi.mock('../ipc/items', () => ({
  getItems: vi.fn(async (ids: number[]) => {
    db.itemCalls.push([...ids]);
    return ids.flatMap((id) => {
      const row = db.items.get(id);
      return row ? [{ ...row }] : [];
    });
  }),
}));
vi.mock('../ipc/units', () => ({
  getUnits: vi.fn(async (ids: number[]) => {
    db.unitCalls.push([...ids]);
    return ids.flatMap((id) => {
      const row = db.units.get(id);
      return row ? [JSON.parse(JSON.stringify(row)) as Record<string, unknown>] : [];
    });
  }),
}));

const {
  clearItems,
  getCached,
  getCachedUnit,
  refreshAnalyzing,
  refreshThumbnails,
  requestItems,
  requestUnits,
  useItemCacheVersion,
} = await import('./itemCache');

function item(id: number, over: Partial<ItemSummaryDto> = {}): ItemSummaryDto {
  return {
    id,
    hash: `h${id}`,
    ext: 'png',
    kind: 'image',
    width: 16,
    height: 16,
    durationMs: null,
    sizeBytes: 1,
    title: `t${id}`,
    rating: 0,
    sourceSite: null,
    importedAt: 0,
    capturedAt: null,
    thumbState: 'ready',
    proxyState: 'not_needed',
    lastPlayedAt: null,
    thumbRev: 0,
    analysisState: 'ready',
    alpha: null,
    pixelArt: false,
    pixelArtSetting: null,
    animated: false,
    frameCount: null,
    hasLoop: false,
    ...over,
  };
}

function unit(id: number, cover: Partial<ItemSummaryDto>): UnitSummaryDto {
  const c = item(100 + id, cover);
  return {
    id,
    kind: 'set',
    name: `u${id}`,
    cover: {
      itemId: c.id,
      hash: c.hash,
      kind: c.kind,
      thumbState: c.thumbState,
      width: c.width,
      height: c.height,
      thumbRev: c.thumbRev,
      analysisState: c.analysisState,
      alpha: c.alpha,
      pixelArt: c.pixelArt,
    },
    count: 1,
    durationMs: 0,
    kinds: ['image'],
    rating: 0,
    ownRating: 0,
    origin: 'user',
    createdAt: 0,
    lastPlayedAt: null,
  } as UnitSummaryDto;
}

/** 取りに行った分が届くまで待つ。 */
async function settle() {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(async () => {
  clearItems();
  db.items.clear();
  db.units.clear();
  await settle();
  db.itemCalls.length = 0;
  db.unitCalls.length = 0;
});

describe('itemCache', () => {
  it('サムネイルのジョブが進んだら、まだのもの（pending）と作り直し中のもの（stale）を取り直す', async () => {
    db.items.set(1, item(1, { thumbState: 'pending' }));
    db.items.set(2, item(2, { thumbState: 'stale', thumbRev: 3 }));
    db.items.set(3, item(3));
    db.units.set(1, unit(1, { thumbState: 'stale' }));
    db.units.set(2, unit(2, {}));
    requestItems([1, 2, 3]);
    requestUnits([1, 2]);
    await settle();
    db.itemCalls.length = 0;
    db.unitCalls.length = 0;

    db.items.set(2, item(2, { thumbState: 'ready', thumbRev: 4 }));
    refreshThumbnails();
    await settle();
    expect(db.itemCalls.flat().sort()).toEqual([1, 2]);
    expect(db.unitCalls.flat()).toEqual([1]);
    expect(getCached(2)?.thumbRev).toBe(4);
  });

  it('解析のジョブが進んだら、解析待ちのものだけ取り直す（アイテムとユニットの表紙）', async () => {
    db.items.set(1, item(1, { analysisState: 'pending' }));
    db.items.set(2, item(2, { analysisState: 'ready' }));
    db.items.set(3, item(3, { kind: 'video', analysisState: 'not_needed' }));
    db.units.set(1, unit(1, { analysisState: 'pending' }));
    db.units.set(2, unit(2, { analysisState: 'failed' }));
    requestItems([1, 2, 3]);
    requestUnits([1, 2]);
    await settle();
    db.itemCalls.length = 0;
    db.unitCalls.length = 0;

    db.items.set(1, item(1, { analysisState: 'ready', alpha: 'binary', pixelArt: true }));
    db.units.set(1, unit(1, { analysisState: 'ready', alpha: 'partial' }));
    refreshAnalyzing();
    await settle();
    expect(db.itemCalls.flat()).toEqual([1]);
    expect(db.unitCalls.flat()).toEqual([1]);
    expect(getCached(1)).toMatchObject({ alpha: 'binary', pixelArt: true });
    expect(getCachedUnit(1)?.cover?.alpha).toBe('partial');
  });

  it('取り直しても変わらなければ、前の行のまま描き直さない', async () => {
    db.items.set(1, item(1, { analysisState: 'pending' }));
    requestItems([1]);
    await settle();
    const before = getCached(1);
    const version = useItemCacheVersion.getState().version;

    refreshAnalyzing();
    await settle();
    expect(db.itemCalls.flat()).toEqual([1, 1]);
    expect(getCached(1)).toBe(before);
    expect(useItemCacheVersion.getState().version).toBe(version);

    db.items.set(1, item(1, { analysisState: 'ready' }));
    refreshAnalyzing();
    await settle();
    expect(getCached(1)?.analysisState).toBe('ready');
    expect(useItemCacheVersion.getState().version).toBe(version + 1);
  });
});
