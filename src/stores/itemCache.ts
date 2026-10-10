import { create } from 'zustand';

import { getItems, type ItemSummaryDto } from '../ipc/items';

/** 詳細を何件まで覚えておくか（見えている分の数十倍）。 */
const LIMIT = 4000;
/** 一度に取る件数（items_get の上限）。 */
const CHUNK = 200;

/** 詳細のキャッシュ（ID → 概要、古いものから捨てる）。 */
const cache = new Map<number, ItemSummaryDto>();
const inflight = new Set<number>();

/** キャッシュが変わるたびに増える番号（グリッドを描き直すため）。 */
export const useItemCacheVersion = create<{ version: number }>(() => ({ version: 0 }));

function bump() {
  useItemCacheVersion.setState((s) => ({ version: s.version + 1 }));
}

/** キャッシュにある概要（なければ undefined）。 */
export function getCached(id: number): ItemSummaryDto | undefined {
  return cache.get(id);
}

/** まだ持っていない ID の詳細を 200 件ずつ取る（見えている分だけ呼ぶ）。 */
export function requestItems(ids: readonly number[]): void {
  const missing = ids.filter((id) => !cache.has(id) && !inflight.has(id));
  for (let i = 0; i < missing.length; i += CHUNK) {
    const chunk = missing.slice(i, i + CHUNK);
    chunk.forEach((id) => inflight.add(id));
    getItems(chunk)
      .then((items) => {
        for (const item of items) {
          cache.delete(item.id);
          cache.set(item.id, item);
        }
        while (cache.size > LIMIT) {
          const oldest = cache.keys().next().value;
          if (oldest === undefined) {
            break;
          }
          cache.delete(oldest);
        }
        bump();
      })
      .catch((error: unknown) => console.warn('詳細を取得できませんでした', error))
      .finally(() => chunk.forEach((id) => inflight.delete(id)));
  }
}

/** サムネイルがまだのものを捨てて、見えていれば取り直させる（サムネイルのジョブが進んだとき）。 */
export function refreshPending(): void {
  let changed = false;
  for (const [id, item] of cache) {
    if (item.thumbState === 'pending') {
      cache.delete(id);
      changed = true;
    }
  }
  if (changed) {
    bump();
  }
}

/** すべて捨てる（ライブラリを開き直したとき）。 */
export function clearItems(): void {
  cache.clear();
  bump();
}
