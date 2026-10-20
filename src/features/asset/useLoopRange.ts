import { useEffect, useState } from 'react';

import { type AssetDto, getAsset } from '../../ipc/asset';
import { useRevisionStore } from '../../stores/revisionStore';

/** ループの区間 [始まり, 終わり)（フレーム）。 */
export type LoopRange = readonly [number, number];

/** 波形に出すループの区間：解析の事実のループ点（素材の欄・クイックルック・シアターで同じ）。なければ null。 */
export function loopOf(asset: AssetDto | null): LoopRange | null {
  const range = asset?.facts?.audio.loopRange;
  return range ? [range.start, range.end] : null;
}

/**
 * シアターの波形のループの区間。概要（`ItemSummaryDto.hasLoop`）でループありのものだけ素材の欄（item_asset_get。
 * DB だけを読む）を読む。読むまで・アイテムが替わった直後は null。
 */
export function useLoopRange(itemId: number | null, hasLoop: boolean): LoopRange | null {
  const revision = useRevisionStore((s) => s.revision);
  const [loaded, setLoaded] = useState<{ id: number; loop: LoopRange | null } | null>(null);

  useEffect(() => {
    if (itemId === null || !hasLoop) {
      return;
    }
    let alive = true;
    getAsset(itemId).then(
      (asset) => {
        if (alive) setLoaded({ id: itemId, loop: loopOf(asset) });
      },
      (error: unknown) => console.warn('ループ点を読めませんでした', error),
    );
    return () => {
      alive = false;
    };
  }, [itemId, hasLoop, revision]);

  return hasLoop && loaded !== null && loaded.id === itemId ? loaded.loop : null;
}
