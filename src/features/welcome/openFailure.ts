import { ja } from '../../i18n/ja';
import type { AppError } from '../../ipc/library';
import { isIpcError } from '../../ipc/result';

const t = ja.libraryOpen.failure;

/** ライブラリを開けなかった理由の表示（見出しと説明）。 */
export interface OpenFailure {
  title: string;
  /** 説明（Rust の `message`。何をすればよいか・ライブラリは変えていないことまで書いてある）。 */
  detail: string;
}

/**
 * 開けなかった理由を、code ごとの見出しと説明にする（docs/architecture.md §3：フロントは code で文言を選び、
 * message を説明に出す）。新しい版の Kura のライブラリ・空き容量・バックアップの失敗・当てる失敗を見分ける。
 */
export function openFailure(error: Pick<AppError, 'code' | 'message'>): OpenFailure {
  switch (error.code) {
    case 'LibraryTooNew':
    case 'NoSpace':
    case 'BackupFailed':
    case 'UpgradeFailed':
      return { title: t[error.code], detail: error.message };
    default:
      return { title: t.other, detail: error.message };
  }
}

/** 開く操作の例外から（IpcError でなければ汎用の見出し）。 */
export function openFailureOf(error: unknown): OpenFailure {
  if (isIpcError(error)) {
    return openFailure(error);
  }
  return { title: t.other, detail: error instanceof Error ? error.message : String(error) };
}
