import { useCallback, useEffect, useRef, useState } from 'react';

import { type AnimDto, type AssetDto, getAnim, getAsset, requestAnalysis } from '../../ipc/asset';
import { onJobProgress } from '../../ipc/jobs';
import { isIpcError } from '../../ipc/result';
import { useRevisionStore } from '../../stores/revisionStore';
import { rememberAsset } from './auditionStore';

/** アニメの帯の読み具合。 */
export type AnimState =
  | { status: 'none' }
  | { status: 'loading' }
  | { status: 'ready'; anim: AnimDto }
  /** 帯の 1 辺・画素の数・予算の全体を超える（先頭のコマで止める）。 */
  | { status: 'tooLarge' }
  | { status: 'failed' };

export interface AssetData {
  /** 素材の欄の中身（読むまで null）。 */
  asset: AssetDto | null;
  anim: AnimState;
  /** 読み直す（設定を変えた・解析をやり直した）。 */
  reload: () => void;
}

/** 解析の進みの知らせが続けて来ても、読み直すのはこの間隔に 1 回。 */
const ANALYSIS_RELOAD_MS = 500;
/** 混んでいて帯を作れなかったときに待ってやり直す間隔と回数。 */
const BUSY_RETRY_MS = 800;
const BUSY_RETRIES = 2;

/** 帯を作り直すきっかけ（中身・格子・コマの長さ）。 */
function animKey(asset: AssetDto): string | null {
  if (asset.kind !== 'image' || !asset.effective.animated) {
    return null;
  }
  return JSON.stringify([asset.hash, asset.settings.grid, asset.settings.frameMs]);
}

/**
 * 1 件の素材の欄を読む（item_asset_get。インスペクタ・クイックルック。null なら読まない）。アニメなら帯
 * （item_anim_get）も読む。
 * - 選ぶものが変わった・ライブラリのデータが変わったら読み直す（読み直す間は前の内容を出したまま）。
 * - 解析を待っているものは先に解析してもらい（request_analysis）、解析のジョブが進むたびに読み直す。
 */
export function useAssetData(itemId: number | null): AssetData {
  const revision = useRevisionStore((s) => s.revision);
  const [tick, setTick] = useState(0);
  const [loaded, setLoaded] = useState<{ id: number; asset: AssetDto } | null>(null);
  const [anim, setAnim] = useState<{ key: string; state: AnimState } | null>(null);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  const asset = loaded?.id === itemId ? loaded.asset : null;

  useEffect(() => {
    if (itemId === null) {
      return;
    }
    let alive = true;
    getAsset(itemId).then(
      (value) => {
        if (alive) {
          rememberAsset(value);
          setLoaded({ id: itemId, asset: value });
        }
      },
      (error: unknown) => console.warn('素材の欄を読めませんでした', error),
    );
    return () => {
      alive = false;
    };
  }, [itemId, revision, tick]);

  // 解析を待っているもの：先に解析してもらい、解析のジョブが進むたびに読み直す
  const pending = asset?.analysisState === 'pending';
  const asked = useRef<number | null>(null);
  useEffect(() => {
    if (!pending || itemId === null) {
      return;
    }
    if (asked.current !== itemId) {
      asked.current = itemId;
      requestAnalysis([itemId]).catch(() => undefined);
    }
    let timer: ReturnType<typeof setTimeout> | null = null;
    let stop: (() => void) | null = null;
    let alive = true;
    onJobProgress((event) => {
      if (event.kind === 'analyze' && timer === null) {
        timer = setTimeout(() => {
          timer = null;
          reload();
        }, ANALYSIS_RELOAD_MS);
      }
    }).then(
      (unlisten) => {
        if (alive) stop = unlisten;
        else unlisten();
      },
      () => undefined,
    );
    return () => {
      alive = false;
      stop?.();
      if (timer !== null) clearTimeout(timer);
    };
  }, [pending, itemId, reload]);

  // アニメの帯：中身・格子・コマの長さが変わったときだけ作り直す
  const key = asset ? animKey(asset) : null;
  useEffect(() => {
    if (key === null || itemId === null) {
      return;
    }
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    // 読んでいる間は前の鍵の結果を出さない（下の animState が loading にする）
    const attempt = (left: number) => {
      getAnim(itemId).then(
        (value) => {
          if (alive) setAnim({ key, state: { status: 'ready', anim: value } });
        },
        (error: unknown) => {
          if (!alive) return;
          if (isIpcError(error, 'Busy') && left > 0) {
            timer = setTimeout(() => attempt(left - 1), BUSY_RETRY_MS);
          } else {
            setAnim({
              key,
              state: { status: isIpcError(error, 'TooLarge') ? 'tooLarge' : 'failed' },
            });
          }
        },
      );
    };
    attempt(BUSY_RETRIES);
    return () => {
      alive = false;
      if (timer !== null) clearTimeout(timer);
    };
  }, [itemId, key]);

  const animState: AnimState =
    key === null ? { status: 'none' } : anim?.key === key ? anim.state : { status: 'loading' };
  return { asset, anim: animState, reload };
}
