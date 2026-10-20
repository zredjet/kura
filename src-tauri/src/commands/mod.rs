//! フロントから呼ぶコマンド。薄いラッパーに保ち、ロジックは kura-core / kura-mac に置く。

pub mod analysis;
pub mod appearance;
pub mod asset;
pub mod bench;
pub mod capture;
pub mod downloads;
pub mod export;
pub mod folders;
pub mod import;
pub mod items;
pub mod jobs;
pub mod library;
pub mod player;
pub mod playlists;
pub mod search;
pub mod tags;
pub mod units;

use crate::error::AppError;

/// SQLite・ファイル I/O などのブロックする処理を専用スレッドで実行する。
pub(crate) async fn blocking<T, F>(f: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|err| AppError::internal(format!("処理が中断されました: {err}")))?
}
