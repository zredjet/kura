import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { hasDuration, kindLabel } from '../../ipc/kinds';
import { formatBytes, formatDuration } from '../../lib/format';

/** インスペクタの「画像 · JPEG · 6000×4000 · 8.4 MB」の行。 */
export function itemMeta(item: ItemSummaryDto): string {
  const parts: string[] = [kindLabel(item.kind) || item.kind, item.ext.toUpperCase()];
  if (item.width && item.height) {
    parts.push(`${item.width}×${item.height}`);
  }
  if (item.durationMs !== null) {
    parts.push(formatDuration(item.durationMs));
  }
  parts.push(formatBytes(item.sizeBytes));
  return parts.join(' · ');
}

/** 読み上げる名前（タイル・リストの行）：「IMG_0001、動画、1:20、評価 4」。評価がなければ言わない。 */
export function spokenLabel(item: ItemSummaryDto): string {
  const parts = [item.title, kindLabel(item.kind) || item.kind];
  if (item.durationMs !== null && hasDuration(item.kind)) {
    parts.push(formatDuration(item.durationMs));
  }
  if (item.rating > 0) {
    parts.push(ja.inspector.rating(item.rating));
  }
  return parts.join('、');
}
