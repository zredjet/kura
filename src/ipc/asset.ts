/** 素材の欄・アニメの帯・解析の口（M11。docs/architecture.md §3）。 */

import {
  type AnimDto,
  type AnimTagDto,
  type AssetAudioFactsDto,
  type AssetChoiceDto,
  type AssetDto,
  type AssetFactsDto,
  type AssetFontDto,
  type AssetGridDto,
  type AssetImageFactsDto,
  type AssetModelDto,
  type AssetSettingsPatchDto,
  commands,
} from './bindings';
import { unwrap } from './result';

export type {
  AnimDto,
  AnimTagDto,
  AssetAudioFactsDto,
  AssetChoiceDto,
  AssetDto,
  AssetFactsDto,
  AssetFontDto,
  AssetGridDto,
  AssetImageFactsDto,
  AssetModelDto,
  AssetSettingsPatchDto,
};

/** 素材の欄の中身（解析の事実・設定・実効値・フォントと 3D の情報。DB だけを読む）。 */
export function getAsset(itemId: number): Promise<AssetDto> {
  return unwrap(commands.itemAssetGet(itemId));
}

/** 素材の設定を変える（画像だけに付く。変えた数を返す。格子が合わなければ `Invalid` で何も変えない）。 */
export function setAssetSettings(ids: number[], patch: AssetSettingsPatchDto): Promise<number> {
  return unwrap(commands.itemAssetSettingsSet(ids, patch));
}

/** アニメの帯の情報（その場で帯を作る。大きすぎれば `TooLarge`、混んでいれば `Busy`）。 */
export function getAnim(itemId: number): Promise<AnimDto> {
  return unwrap(commands.itemAnimGet(itemId));
}

/** まだ解析していないものを先に解析する（待つものがあれば true）。 */
export function requestAnalysis(ids: number[]): Promise<boolean> {
  return unwrap(commands.requestAnalysis(ids));
}

/** 解析できなかったものを、もう一度解析する（解き直すことにした数）。 */
export function retryAnalysis(ids: number[]): Promise<number> {
  return unwrap(commands.retryAnalysis(ids));
}
