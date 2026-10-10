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
