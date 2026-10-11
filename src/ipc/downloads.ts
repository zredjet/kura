import {
  type ArchiveStatusDto,
  commands,
  type CookieSourceDto,
  type DetectionDto,
  type DownloadJobDto,
  type DownloadOptionsDto,
  type DownloadPrefsDto,
  type DownloadRuleDto,
  type QualityDto,
  type ToolDto,
  type ToolInfoDto,
  type ToolsUpdateDto,
} from './bindings';
import { unwrap } from './result';

export type {
  ArchiveStatusDto,
  CookieSourceDto,
  DetectionDto,
  DownloadJobDto,
  DownloadOptionsDto,
  DownloadPrefsDto,
  DownloadRuleDto,
  QualityDto,
  ToolDto,
  ToolInfoDto,
  ToolsUpdateDto,
};

/** URL の取得方法を判定する（サイト・ツール・ツールがあるか）。 */
export function detectDownload(url: string): Promise<DetectionDto> {
  return unwrap(commands.downloadDetect(url));
}

/** 前回の選択（画質・保存先など）。 */
export function downloadDefaults(): Promise<DownloadOptionsDto> {
  return unwrap(commands.downloadDefaults());
}

/** ダウンロードを積む。ジョブの ID を返す。 */
export function enqueueDownload(url: string, options: DownloadOptionsDto): Promise<number> {
  return unwrap(commands.downloadEnqueue(url, options));
}

/** ダウンロードを新しい順に一覧する。 */
export function listDownloads(limit: number | null = null): Promise<DownloadJobDto[]> {
  return unwrap(commands.downloadsList(limit));
}

/** ダウンロードの設定（Cookie の読み込み元・規則）。 */
export function getDownloadPrefs(): Promise<DownloadPrefsDto> {
  return commands.downloadPrefsGet();
}

/** ダウンロードの設定を保存する（規則が正しくなければエラー）。 */
export function setDownloadPrefs(prefs: DownloadPrefsDto): Promise<DownloadPrefsDto> {
  return unwrap(commands.downloadPrefsSet(prefs));
}

/** 外部ツールの状態。 */
export function toolsStatus(): Promise<ToolInfoDto[]> {
  return unwrap(commands.toolsStatus());
}

/** 外部ツールの場所を指定する（null で自動に戻す）。 */
export function setToolPath(tool: string, path: string | null): Promise<ToolInfoDto> {
  return unwrap(commands.toolsSetPath(tool, path));
}

/** 「更新を確認」。 */
export function updateTools(): Promise<ToolsUpdateDto> {
  return unwrap(commands.toolsUpdate());
}

/** ツールのコマンド名（`yt-dlp` など）。 */
export function toolCommand(tool: ToolDto): string {
  return tool === 'ytDlp' ? 'yt-dlp' : 'gallery-dl';
}

/** 取得済みの記録の状態（yt-dlp・gallery-dl）。 */
export function downloadArchives(): Promise<ArchiveStatusDto[]> {
  return unwrap(commands.downloadArchives());
}

/** 取得済みの記録を消す。 */
export function clearDownloadArchive(tool: ToolDto): Promise<null> {
  return unwrap(commands.downloadArchiveClear(tool));
}
