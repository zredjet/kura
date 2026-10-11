import { commands, type ExportResultDto } from './bindings';
import { unwrap } from './result';

export type { ExportResultDto };

/** 選んだもの（一覧の ID）を `dest` に書き出すジョブを積む（ジョブの ID）。 */
export function startExport(dest: string, entries: number[]): Promise<number> {
  return unwrap(commands.exportStart(dest, entries));
}

/** 書き出しのジョブの結果（`resultJson`）を読む。 */
export function parseExportResult(json: string | null): ExportResultDto | null {
  if (!json) {
    return null;
  }
  try {
    return JSON.parse(json) as ExportResultDto;
  } catch {
    return null;
  }
}
