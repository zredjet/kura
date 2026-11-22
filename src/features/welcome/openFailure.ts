import { ja } from '../../i18n/ja';
import type { AppError } from '../../ipc/library';
import { isIpcError } from '../../ipc/result';

const t = ja.libraryOpen.failure;

/** ライブラリを開けなかった理由の表示（見出しと説明）。 */
export interface OpenFailure {
  title: string;
  /**
   * 説明（Rust の `message`。何をすればよいかと、ライブラリがどの版にあるか（変えていない・前の版のまま・途中の版
   * （スキーマ N）まで当てた・新しいライブラリを作る途中で止まった（開き直すと続きを当てる））まで書いてある）。
   * ほかのアプリが開いているとき（`LibraryInUse`）は、決まった説明（`ja.libraryOpen.inUseDetail`）。
   */
  detail: string;
}

/**
 * 開けなかった理由を、code ごとの見出しと説明にする（docs/architecture.md §3：フロントは code で文言を選び、
 * message を説明に出す）。新しい版の Kura のライブラリ・空き容量・バックアップの失敗・当てる失敗・ほかのアプリが
 * 開いている（ライブラリのロック。M12）を見分ける。
 */
export function openFailure(error: Pick<AppError, 'code' | 'message'>): OpenFailure {
  switch (error.code) {
    case 'LibraryTooNew':
    case 'NoSpace':
    case 'BackupFailed':
    case 'UpgradeFailed':
      return { title: t[error.code], detail: error.message };
    // Rust の message は見出しと同じ言葉で始まるので、説明は決まった文にする
    case 'LibraryInUse':
      return { title: t.LibraryInUse, detail: ja.libraryOpen.inUseDetail };
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
