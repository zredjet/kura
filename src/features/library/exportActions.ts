import { ja } from '../../i18n/ja';
import { pickExportFolder } from '../../ipc/dialogs';
import { type ExportResultDto, startExport } from '../../ipc/export';
import { revealPath } from '../../ipc/library';
import { notify } from '../../stores/noticeStore';

const t = ja.exporting;

/** 選んだものを書き出す（⌘⇧E・メニュー）：書き出し先のフォルダを選んでもらい、ジョブを積む。 */
export async function exportEntries(entries: readonly number[]): Promise<void> {
  if (entries.length === 0) {
    notify(t.nothing);
    return;
  }
  const dest = await pickExportFolder(t.pickTitle);
  if (!dest) {
    return;
  }
  try {
    await startExport(dest, [...entries]);
  } catch (error) {
    notify(error instanceof Error ? error.message : String(error));
  }
}

/** 書き出したものを Finder で見せる（JSON を選んだ状態で、なければフォルダ）。 */
export function revealExport(result: ExportResultDto): void {
  const target = result.manifest ? `${result.dest}/${result.manifest}` : result.dest;
  revealPath(target).catch((error: unknown) =>
    notify(error instanceof Error ? error.message : String(error)),
  );
}
