import { useEffect, useState } from 'react';

import { type AssetDto, getAsset } from '../../ipc/asset';
import { auditionLoop } from '../../lib/asset/audition';
import { useRevisionStore } from '../../stores/revisionStore';

/** ループの区間 [始まり, 終わり)（フレーム）。 */
export type LoopRange = readonly [number, number];

/** ファイルのループ点（解析の事実。エディタ・検索の `ループ:あり` が見るもの）。なければ null。 */
export function fileLoopOf(asset: AssetDto | null): LoopRange | null {
  const range = asset?.facts?.audio.loopRange;
  return range ? [range.start, range.end] : null;
}

/** 試聴の設定の区間（素材の設定 `loopRange`。M13。ファイルは変えない）。なければ null。 */
export function settingLoopOf(asset: AssetDto | null): LoopRange | null {
  const range = asset?.settings.loopRange;
  return range ? [range.start, range.end] : null;
}

/** 試聴の設定が解析の長さ（`facts.audio.frames`）を超えているか（上書きで縮んだなど。長さが分からなければ偽）。 */
export function settingLoopTooLong(asset: AssetDto | null): boolean {
  const setting = settingLoopOf(asset);
  const frames = asset?.facts?.audio.frames ?? null;
  return setting !== null && frames !== null && setting[1] > frames;
}

/**
 * 波形に出すループの区間（素材の欄・クイックルック・シアターで同じ）：試聴の設定が解析の長さに収まればそれ、なければ
 * ファイルのループ点（`auditionLoop`。M13 の決定 5）。どちらもなければ null。
 */
export function loopOf(asset: AssetDto | null): LoopRange | null {
  return auditionLoop(settingLoopOf(asset), asset?.facts?.audio.frames ?? null, fileLoopOf(asset));
}

/**
 * シアターの波形のループの区間。素材の欄（item_asset_get。DB だけを読む）を読んで `loopOf` で決める（試聴の設定はファイルに
 * ループ点がなくても使うので、概要の `hasLoop`（ファイルのループ点）では絞らない）。読むまで・アイテムが替わった直後は null。
 */
export function useLoopRange(itemId: number | null): LoopRange | null {
  const revision = useRevisionStore((s) => s.revision);
  const [loaded, setLoaded] = useState<{ id: number; loop: LoopRange | null } | null>(null);

  useEffect(() => {
    if (itemId === null) {
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
  }, [itemId, revision]);

  return loaded !== null && loaded.id === itemId ? loaded.loop : null;
}
