import { open, save } from '@tauri-apps/plugin-dialog';

/**
 * 取り込める拡張子（docs/architecture.md §6。kura-engine の形式の表 `FORMATS` と同じ。dialogs.test.ts で照らす）。
 * `.ase` は選べるが、Aseprite でない中身（Adobe の色見本）は取り込みで対象外になる。
 */
export const MEDIA_EXTENSIONS = [
  // 画像
  'png',
  'apng',
  'jpg',
  'jpeg',
  'gif',
  'webp',
  'aseprite',
  'ase',
  'psd',
  'psb',
  'svg',
  'heic',
  'heif',
  'avif',
  'tif',
  'tiff',
  'bmp',
  // 動画
  'mp4',
  'm4v',
  'mov',
  'webm',
  'mkv',
  'avi',
  // 音楽
  'wav',
  'aif',
  'aiff',
  'flac',
  'mp3',
  'm4a',
  'aac',
  'ogg',
  'opus',
  // フォント
  'ttf',
  'otf',
  'ttc',
  'woff',
  'woff2',
  // 3D
  'glb',
];

/** 新しいライブラリの場所を選ぶ（保存のダイアログ）。キャンセルなら null。 */
export function pickNewLibraryLocation(title: string, defaultPath: string): Promise<string | null> {
  return save({ title, defaultPath, canCreateDirectories: true });
}

/** 既存のライブラリ（`*.kuralib` フォルダ）を選ぶ。キャンセルなら null。 */
export function pickExistingLibrary(title: string): Promise<string | null> {
  return open({ title, directory: true, multiple: false });
}

/** 取り込むファイルを選ぶ。キャンセルなら空。 */
export async function pickImportFiles(title: string, filterName: string): Promise<string[]> {
  const picked = await open({
    title,
    multiple: true,
    directory: false,
    filters: [{ name: filterName, extensions: MEDIA_EXTENSIONS }],
  });
  return picked ?? [];
}

/** 外部ツールの実行ファイルを選ぶ。キャンセルなら null。 */
export function pickToolFile(title: string): Promise<string | null> {
  return open({ title, directory: false, multiple: false, defaultPath: '/opt/homebrew/bin' });
}

/** 書き出し先のフォルダを選ぶ（新しいフォルダも作れる）。キャンセルなら null。 */
export function pickExportFolder(title: string): Promise<string | null> {
  return open({ title, directory: true, multiple: false, canCreateDirectories: true });
}
