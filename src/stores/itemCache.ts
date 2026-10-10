import { create } from 'zustand';

import { getItems, type ItemSummaryDto } from '../ipc/items';
import { getUnits, type UnitSummaryDto } from '../ipc/units';

/** 詳細を何件まで覚えておくか（見えている分の数十倍）。 */
const LIMIT = 4000;
/** 一度に取る件数（items_get・units_get の上限）。 */
const CHUNK = 200;

/** ID → 概要のキャッシュ（古いものから捨てる）。アイテムとユニットで 1 つずつ持つ。 */
class SummaryCache<T extends { id: number }> {
  private readonly cache = new Map<number, T>();
  private readonly inflight = new Set<number>();

  constructor(private readonly fetch: (ids: number[]) => Promise<T[]>) {}

  get(id: number): T | undefined {
    return this.cache.get(id);
  }

  request(ids: readonly number[]): void {
    this.fetchChunks(ids.filter((id) => !this.cache.has(id) && !this.inflight.has(id)));
  }

  /** 持っているものを取り直す（届くまでは前の内容を出したままにする）。 */
  refresh(test: (row: T) => boolean): boolean {
    const stale = [...this.cache.values()].filter(test).map((row) => row.id);
    this.fetchChunks(stale.filter((id) => !this.inflight.has(id)));
    return stale.length > 0;
  }

  private fetchChunks(missing: readonly number[]): void {
    for (let i = 0; i < missing.length; i += CHUNK) {
      const chunk = missing.slice(i, i + CHUNK);
      chunk.forEach((id) => this.inflight.add(id));
      this.fetch(chunk)
        .then((rows) => {
          for (const row of rows) {
            this.cache.delete(row.id);
            this.cache.set(row.id, row);
          }
          while (this.cache.size > LIMIT) {
            const oldest = this.cache.keys().next().value;
            if (oldest === undefined) {
              break;
            }
            this.cache.delete(oldest);
          }
          bump();
        })
        .catch((error: unknown) => console.warn('詳細を取得できませんでした', error))
        .finally(() => chunk.forEach((id) => this.inflight.delete(id)));
    }
  }

  clear(): void {
    this.cache.clear();
  }
}

const items = new SummaryCache<ItemSummaryDto>(getItems);
const units = new SummaryCache<UnitSummaryDto>(getUnits);

/** キャッシュが変わるたびに増える番号（グリッドを描き直すため）。 */
export const useItemCacheVersion = create<{ version: number }>(() => ({ version: 0 }));

function bump() {
  useItemCacheVersion.setState((s) => ({ version: s.version + 1 }));
}

/** キャッシュにあるアイテムの概要（なければ undefined）。 */
export function getCached(id: number): ItemSummaryDto | undefined {
  return items.get(id);
}

/** キャッシュにあるユニットの概要（なければ undefined）。 */
export function getCachedUnit(unitId: number): UnitSummaryDto | undefined {
  return units.get(unitId);
}

/** まだ持っていないアイテムの詳細を 200 件ずつ取る（見えている分だけ呼ぶ）。 */
export function requestItems(ids: readonly number[]): void {
  items.request(ids);
}

/** まだ持っていないユニットの概要を取る。 */
export function requestUnits(unitIds: readonly number[]): void {
  units.request(unitIds);
}

/** サムネイルがまだのものを取り直す（サムネイルのジョブが進んだとき）。 */
export function refreshPending(): void {
  items.refresh((item) => item.thumbState === 'pending');
  units.refresh((unit) => unit.cover?.thumbState === 'pending');
}

/** 編集したアイテムを取り直す。 */
export function invalidateItems(ids: readonly number[]): void {
  const set = new Set(ids);
  items.refresh((item) => set.has(item.id));
}

/** ユニットの概要を取り直す（ID を省くとすべて）。 */
export function invalidateUnits(unitIds?: readonly number[]): void {
  const set = unitIds ? new Set(unitIds) : null;
  units.refresh((unit) => set === null || set.has(unit.id));
}

/** すべて捨てる（ライブラリを開き直したとき）。 */
export function clearItems(): void {
  items.clear();
  units.clear();
  bump();
}
