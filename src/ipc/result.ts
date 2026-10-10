import type { AppError, AppErrorCode } from './bindings';

/** tauri-specta のコマンドが返す結果の形。 */
export type CommandResult<T> = { status: 'ok'; data: T } | { status: 'error'; error: AppError };

/** コマンドが返した AppError を JS の例外にしたもの。`code` で分岐する。 */
export class IpcError extends Error {
  readonly code: AppErrorCode;

  constructor(error: AppError) {
    super(error.message);
    this.name = 'IpcError';
    this.code = error.code;
  }
}

/** コマンドの結果を開く。失敗なら IpcError を投げる。 */
export async function unwrap<T>(result: Promise<CommandResult<T>>): Promise<T> {
  const settled = await result;
  if (settled.status === 'error') {
    throw new IpcError(settled.error);
  }
  return settled.data;
}

/** 例外が IpcError か（`code` を指定したらその種類か）を調べる。 */
export function isIpcError(error: unknown, code?: AppErrorCode): error is IpcError {
  return error instanceof IpcError && (code === undefined || error.code === code);
}
