import type { MenuSubmenu } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { type AssetChoiceDto, setAssetSettings } from '../../ipc/asset';
import { isUnitEntry } from '../../ipc/items';
import { getCached, invalidateItems, invalidateUnits } from '../../stores/itemCache';
import { bumpRevision } from '../../stores/revisionStore';
import { useViewStore } from '../../stores/viewStore';
import { attempt } from './actions';

/** 選択肢の並び（design.md §3.1：自動・はい・いいえ）。右クリックとメニューバーで同じ。 */
export const PIXEL_ART_CHOICES: readonly AssetChoiceDto[] = ['auto', 'yes', 'no'];

export const PIXEL_ART_LABELS: Record<AssetChoiceDto, string> = {
  auto: ja.mediaView.pixelArtAuto,
  yes: ja.mediaView.pixelArtYes,
  no: ja.mediaView.pixelArtNo,
};

/** 設定（null は自動）を選択肢にする。 */
function choiceOf(setting: boolean | null): AssetChoiceDto {
  return setting === null ? 'auto' : setting ? 'yes' : 'no';
}

/** 「ドット絵として表示 ▸」を当てるもの。 */
interface PixelArtTarget {
  /** 送るアイテム：キャッシュにある画像と、キャッシュにない（見えていない）もの。画像でないものは Rust が飛ばす。 */
  ids: number[];
  /** チェックを付ける値（画像の設定がそろっていて、キャッシュにないものがないときだけ）。 */
  current: AssetChoiceDto | undefined;
}

/**
 * 選んだもののうち、設定を当てるもの（右クリックとメニューバーで同じ）。ユニットのタイル・キャッシュにある画像でない
 * ものは飛ばす。⌘A で選んだときはキャッシュにないものもあるので、それも送る（item_asset_settings_set が画像だけに
 * 付ける）。キャッシュにある画像が 1 つもなければ null（使えない）。
 */
function pixelArtTarget(entries: readonly number[]): PixelArtTarget | null {
  const ids: number[] = [];
  const settings = new Set<AssetChoiceDto>();
  let unknown = false;
  for (const id of entries) {
    if (isUnitEntry(id)) {
      continue;
    }
    const item = getCached(id);
    if (item === undefined) {
      unknown = true;
      ids.push(id);
    } else if (item.kind === 'image') {
      settings.add(choiceOf(item.pixelArtSetting));
      ids.push(id);
    }
  }
  if (settings.size === 0) {
    return null;
  }
  return { ids, current: settings.size === 1 && !unknown ? [...settings][0] : undefined };
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
  const target = pixelArtTarget(entries);
  if (target === null) {
    return null;
  }
  return {
    label: ja.mediaView.pixelArt,
    submenu: PIXEL_ART_CHOICES.map((choice) => ({
      label: PIXEL_ART_LABELS[choice],
      checked: choice === target.current,
      onSelect: () => void apply(target.ids, choice),
    })),
  };
}

/** メニューバーの対象：一覧で選んでいるもの（ゴミ箱では使わない。右クリックのメニューにも出ないので）。 */
function selectionTarget(): PixelArtTarget | null {
  const { view, selection } = useViewStore.getState();
  return view.kind === 'trash' ? null : pixelArtTarget(selection.ids);
}

/** メニューバーの「項目 › ドット絵として表示 ▸」の状態：使えるか（選んでいる画像があるか）と、チェックを付ける値。 */
export function pixelArtMenuState(): { enabled: boolean; current: AssetChoiceDto | null } {
  const target = selectionTarget();
  return { enabled: target !== null, current: target?.current ?? null };
}

/** メニューバーの「項目 › ドット絵として表示 ▸」：一覧で選んでいるものに、右クリックと同じく当てる。 */
export function applyPixelArtToSelection(choice: AssetChoiceDto): void {
  const target = selectionTarget();
  if (target !== null) {
    void apply(target.ids, choice);
  }
}
