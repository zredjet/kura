//! 書き出し（FR-SYS-02。ADR-120）。

use std::path::PathBuf;

use kura_core::export::{ExportInput, start_export};
use tauri::State;

use super::blocking;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 選んだもの（一覧の ID。ユニットは印付き）を `dest` に書き出すジョブを積む。
/// 書き出し先はあるフォルダで、ライブラリの中は受け付けない。
#[tauri::command]
#[specta::specta]
pub async fn export_start(
    state: State<'_, AppState>,
    dest: String,
    entries: Vec<i64>,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let dest = PathBuf::from(dest);
        let missing = || {
            AppError::new(
                AppErrorCode::Invalid,
                "書き出し先のフォルダが見つかりません",
            )
        };
        if !dest.is_absolute() || !dest.is_dir() {
            return Err(missing());
        }
        let dest = dest.canonicalize().map_err(|_| missing())?;
        let root = session
            .library()
            .layout()
            .root()
            .canonicalize()
            .map_err(|e| AppError::internal(e.to_string()))?;
        if dest.starts_with(&root) {
            return Err(AppError::new(
                AppErrorCode::Invalid,
                "ライブラリの中には書き出せません。ほかのフォルダを選んでください",
            ));
        }
        Ok(start_export(
            session.jobs(),
            &ExportInput { dest, entries },
        )?)
    })
    .await
}
