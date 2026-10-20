import type { MenuSubmenu } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { type AssetChoiceDto, setAssetSettings } from '../../ipc/asset';
import { isUnitEntry } from '../../ipc/items';
import { getCached, invalidateItems, invalidateUnits } from '../../stores/itemCache';
import { bumpRevision } from '../../stores/revisionStore';
import { attempt } from './actions';

/** 選択肢の並び（design.md §3.1：自動・はい・いいえ）。 */
const CHOICES: readonly AssetChoiceDto[] = ['auto', 'yes', 'no'];

const LABELS: Record<AssetChoiceDto, string> = {
  auto: ja.mediaView.pixelArtAuto,
  yes: ja.mediaView.pixelArtYes,
  no: ja.mediaView.pixelArtNo,
};

/** 設定（null は自動）を選択肢にする。 */
function choiceOf(setting: boolean | null): AssetChoiceDto {
  return setting === null ? 'auto' : setting ? 'yes' : 'no';
}

/** 設定を変えて、タイル（とユニットの表紙・インスペクタ）を読み直す。 */
async function apply(ids: number[], choice: AssetChoiceDto): Promise<void> {
  await attempt(async () => {
    await setAssetSettings(ids, { pixelArt: choice });
    invalidateItems(ids);
    invalidateUnits();
    bumpRevision();
  });
}

/**
 * 右クリックの「ドット絵として表示 ▸ 自動・はい・いいえ」（design.md §3.1・§6）。選んだもののうち画像だけに付ける
 * （ユニットのタイル・ほかの種類は飛ばす）。画像の設定がそろっていればその値にチェック、ばらばらならチェックなし。
 * 画像がなければ出さない（null）。
 */
export function pixelArtMenu(entries: readonly number[]): MenuSubmenu | null {
  const images = entries
    .filter((id) => !isUnitEntry(id))
    .flatMap((id) => {
      const item = getCached(id);
      return item?.kind === 'image' ? [item] : [];
    });
  if (images.length === 0) {
    return null;
  }
  const settings = new Set(images.map((item) => choiceOf(item.pixelArtSetting)));
  const current = settings.size === 1 ? [...settings][0] : undefined;
  const ids = images.map((item) => item.id);
  return {
    label: ja.mediaView.pixelArt,
    submenu: CHOICES.map((choice) => ({
      label: LABELS[choice],
      checked: choice === current,
      onSelect: () => void apply(ids, choice),
    })),
  };
}
