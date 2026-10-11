import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { formatBytes, formatDuration } from '../../lib/format';

/** インスペクタの「画像 · JPEG · 6000×4000 · 8.4 MB」の行。 */
export function itemMeta(item: ItemSummaryDto): string {
  const kindLabel = ja.kinds[item.kind as keyof typeof ja.kinds] ?? item.kind;
  const parts: string[] = [kindLabel, item.ext.toUpperCase()];
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
  const parts = [item.title, ja.kinds[item.kind as keyof typeof ja.kinds] ?? item.kind];
  if (item.durationMs !== null && item.kind !== 'image') {
    parts.push(formatDuration(item.durationMs));
  }
  if (item.rating > 0) {
    parts.push(ja.inspector.rating(item.rating));
  }
  return parts.join('、');
}
