import { ja } from '../../i18n/ja';
import type { ExportResultDto } from '../../ipc/export';
import { revealPath } from '../../ipc/library';
import { notify } from '../../stores/noticeStore';
import { openExportSheet } from '../export/exportSheetStore';

const t = ja.exporting;

/** 選んだものを書き出す（⌘⇧E・メニュー）：書き出しのシートを開く（書き出し先・名前・形式を選んでジョブを積む。M15 段 6）。 */
export async function exportEntries(entries: readonly number[]): Promise<void> {
  if (entries.length === 0) {
    notify(t.nothing);
    return;
  }
  openExportSheet(entries);
  return Promise.resolve();
}

/** 書き出したものを Finder で見せる（JSON を選んだ状態で、なければフォルダ）。 */
export function revealExport(result: ExportResultDto): void {
  const target = result.manifest ? `${result.dest}/${result.manifest}` : result.dest;
  revealPath(target).catch((error: unknown) =>
    notify(error instanceof Error ? error.message : String(error)),
  );
}
