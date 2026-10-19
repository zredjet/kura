//! 解析（M11。docs/architecture.md §5）：インスペクタで選んだものを先に解析する・解析できなかったものを解き直す。
//! 解析そのものはジョブ（`analyze`）が行い、取り込み・サムネイルが動いている間はそれを待つ。

use tauri::State;

use super::blocking;
use crate::error::AppError;
use crate::state::AppState;

/// 選んだアイテムを先に解析する（まだ解析していないものがあれば解析のジョブを積む）。解析を待つものがあれば true。
#[tauri::command]
#[specta::specta]
pub async fn request_analysis(state: State<'_, AppState>, ids: Vec<i64>) -> Result<bool, AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.request_analysis(ids)?)).await
}

/// 解析できなかったアイテムを、もう一度解析する（先に解く）。解き直すことにした数を返す。
#[tauri::command]
#[specta::specta]
pub async fn retry_analysis(state: State<'_, AppState>, ids: Vec<i64>) -> Result<u32, AppError> {
    let session = state.session()?;
    blocking(move || {
        let n = session.retry_analysis(ids)?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}
