import { ja } from '../../i18n/ja';
import type { PreviousDownloadDto } from '../../ipc/downloads';
import { relativeDateTime } from '../../lib/dates';
import { formatCount } from '../../lib/format';

const t = ja.downloads.previous;

/** ダウンロード済みの URL を追加するときのメッセージ（いつ取得したか・今ライブラリにあるか・選択肢の説明）。 */
export function previousMessage(previous: PreviousDownloadDto, now: number = Date.now()): string {
  const { title, items, inLibrary } = previous;
  if (items === 0) {
    // 取得済みとして飛ばしただけのジョブ（いつ取得したかはわからない）
    const head = title ? t.skippedOnly(title) : t.skippedOnlyUntitled;
    return `${head}\n${t.explain}`;
  }
  const when = relativeDateTime(previous.at, now);
  const head = title ? t.fetched(title, when) : t.fetchedUntitled(when);
  const library =
    inLibrary === 0
      ? t.noneInLibrary
      : inLibrary >= items
        ? t.allInLibrary
        : t.someInLibrary(formatCount(items), formatCount(inLibrary));
  return `${head}${library}\n${t.explain}`;
}
