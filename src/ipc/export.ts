import {
  commands,
  type ExportFormatsDto,
  type ExportOptionsDto,
  type ExportPresetDto,
  type ExportPreviewDto,
  type ExportResultDto,
  type FormatTargetDto,
  type NameRuleDto,
  type NameStyleDto,
  type PreviewNameDto,
} from './bindings';
import { unwrap } from './result';

export type {
  ExportFormatsDto,
  ExportOptionsDto,
  ExportPresetDto,
  ExportPreviewDto,
  ExportResultDto,
  FormatTargetDto,
  NameRuleDto,
  NameStyleDto,
  PreviewNameDto,
};

/**
 * 選んだもの（一覧の ID）を `dest` に書き出すジョブを積む（ジョブの ID）。`options` を省けば M9 と同じ
 * （元の名前・形式はそのまま・`kura-export.json` を添える）。
 */
export function startExport(
  dest: string,
  entries: number[],
  options: ExportOptionsDto | null = null,
): Promise<number> {
  return unwrap(commands.exportStart(dest, entries, options));
}

/** 書き出しで選べる形式（画像・音声。今の環境で書けるもの）。 */
export function exportFormats(): Promise<ExportFormatsDto> {
  return unwrap(commands.exportFormats());
}

/**
 * 選んだもの（一覧の ID。ユニットは印付き）の書き出す名前と形式の見本（書き出しのジョブと同じ並び・番号・ユニットの
 * フォルダ。先頭の 50 件と、書き出す件数）。
 */
export function previewExportNames(
  entries: number[],
  options: ExportOptionsDto,
): Promise<ExportPreviewDto> {
  return unwrap(commands.exportPreview(entries, options));
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
