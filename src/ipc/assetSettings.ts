/** 素材の設定（M11。docs/architecture.md §3 の `item_asset_settings_set`）：右クリックの「ドット絵として表示 ▸」から使う。 */

import { type AssetChoiceDto, commands } from './bindings';
import { unwrap } from './result';

export type { AssetChoiceDto };

/** ドット絵として表示（自動・はい・いいえ）を変える。画像だけに付く（ほかの種類は飛ばす）。変えた数を返す。 */
export function setPixelArt(ids: number[], choice: AssetChoiceDto): Promise<number> {
  return unwrap(commands.itemAssetSettingsSet(ids, { pixelArt: choice }));
}
