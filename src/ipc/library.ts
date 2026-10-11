import {
  type BackupDto,
  commands,
  type IntegrityDto,
  type LibraryUsageDto,
  type LibraryInfoDto,
  type LibraryRestoreDto,
  type LocationCheckDto,
} from './bindings';
import { unwrap } from './result';

export type {
  BackupDto,
  IntegrityDto,
  LibraryInfoDto,
  LibraryRestoreDto,
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

/** 起動時に前回のライブラリを開く。 */
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

/** 整合性チェックをいま行う。 */
export function checkLibrary(): Promise<IntegrityDto> {
  return unwrap(commands.libraryCheck());
}
