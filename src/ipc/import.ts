import {
  commands,
  type ImportOptionsDto,
  type ImportPreviewDto,
  type ImportResultDto,
} from './bindings';
import { unwrap } from './result';

export type { ImportOptionsDto, ImportPreviewDto, ImportResultDto };

/** 取り込み前の確認。 */
export function previewImport(paths: string[]): Promise<ImportPreviewDto> {
  return unwrap(commands.importPreview(paths));
}

/** 取り込みジョブを積み、ジョブの ID を返す。 */
export function startImport(paths: string[], options: ImportOptionsDto): Promise<number> {
  return unwrap(commands.importStart(paths, options));
}

/** ジョブの resultJson を取り込みの結果として読む（読めなければ null）。 */
export function parseImportResult(json: string | null): ImportResultDto | null {
  if (!json) {
    return null;
  }
  try {
    return JSON.parse(json) as ImportResultDto;
  } catch {
    return null;
  }
}
