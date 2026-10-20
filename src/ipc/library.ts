import {
  type AppError,
  type BackupDto,
  commands,
  events,
  type IntegrityDto,
  type LibraryCacheUsageDto,
  type LibraryUsageDto,
  type LibraryInfoDto,
  type LibraryRestoreDto,
  type LibraryUpgradeEvent,
  type LocationCheckDto,
} from './bindings';
import { unwrap } from './result';

export type {
  AppError,
  BackupDto,
  IntegrityDto,
  LibraryCacheUsageDto,
  LibraryInfoDto,
  LibraryRestoreDto,
  LibraryUpgradeEvent,
  LibraryUsageDto,
  LocationCheckDto,
};

/** ライブラリを置く場所を確かめる（拡張子の補完、既存のライブラリか、同期フォルダの中か）。 */
export function checkLocation(path: string): Promise<LocationCheckDto> {
  return unwrap(commands.libraryCheckLocation(path));
}

/** 新しいライブラリを作って開く。 */
export function createLibrary(path: string): Promise<LibraryInfoDto> {
  return unwrap(commands.libraryCreate(path));
}

/** 既存のライブラリを開く。 */
export function openLibrary(path: string): Promise<LibraryInfoDto> {
  return unwrap(commands.libraryOpen(path));
}

/**
 * ライブラリの DB を新しい版にする知らせ（`library://upgrade`）を受け取る。戻り値で受け取りをやめる。
 * 開く IPC（`restoreLibrary`・`openLibrary`）の中で来るので、それより先に受け取りを始めておく。
 */
export function onLibraryUpgrade(
  handler: (event: LibraryUpgradeEvent) => void,
): Promise<() => void> {
  return events.libraryUpgrade.listen((e) => handler(e.payload));
}

/** 起動時に前回のライブラリを開く。開けなかった理由は `error`（例外にしない）。 */
export function restoreLibrary(): Promise<LibraryRestoreDto> {
  return unwrap(commands.libraryRestore());
}

/** 開いているライブラリの概要。 */
export function libraryInfo(): Promise<LibraryInfoDto> {
  return unwrap(commands.libraryInfo());
}

/** DB のバックアップの一覧（新しい順）。 */
export function listBackups(): Promise<BackupDto[]> {
  return unwrap(commands.backupsList());
}

/** いまのバックアップを作る。 */
export function backupNow(): Promise<BackupDto> {
  return unwrap(commands.backupNow());
}

/** Finder で見せる。 */
export function revealPath(path: string): Promise<null> {
  return unwrap(commands.revealPath(path));
}

/** ライブラリの容量と、あるボリュームの容量・空き。 */
export function libraryUsage(): Promise<LibraryUsageDto> {
  return unwrap(commands.libraryUsage());
}

/**
 * 中身から作ったキャッシュ（サムネイルとジャケット・波形・アニメの帯）の大きさとファイルの数。ファイルを数えるので
 * 設定の画面を開いたときだけ読む（冷えたディスクでは秒単位になりうる）。
 */
export function libraryCacheUsage(): Promise<LibraryCacheUsageDto> {
  return unwrap(commands.libraryCacheUsage());
}

/** 整合性チェックをいま行う。 */
export function checkLibrary(): Promise<IntegrityDto> {
  return unwrap(commands.libraryCheck());
}
