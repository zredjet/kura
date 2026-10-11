//! ジョブの一覧と操作。

use tauri::State;

use super::blocking;
use crate::dto::{JobDto, JobFilterDto};
use crate::error::AppError;
use crate::state::AppState;

/// ジョブを新しい順に一覧する。
#[tauri::command]
#[specta::specta]
pub async fn jobs_list(
    state: State<'_, AppState>,
    filter: Option<JobFilterDto>,
) -> Result<Vec<JobDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let filter = filter.unwrap_or_default().into();
        Ok(session
            .jobs()
            .list(&filter)?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// 中止する（取り込み済みの分は残る）。
#[tauri::command]
#[specta::specta]
pub async fn job_cancel(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.jobs().cancel(id)?)).await
}

/// 一時停止する。
#[tauri::command]
#[specta::specta]
pub async fn job_pause(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.jobs().pause(id)?)).await
}

/// 一時停止したジョブを再開する。
#[tauri::command]
#[specta::specta]
pub async fn job_resume(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.jobs().resume(id)?)).await
}

/// 失敗・中止したジョブをもう一度動かす（途中経過から続ける）。
#[tauri::command]
#[specta::specta]
pub async fn job_retry(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.jobs().retry(id)?)).await
}

/// 待っているジョブを先に動かす。
#[tauri::command]
#[specta::specta]
pub async fn job_prioritize(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.jobs().prioritize(id)?)).await
}

/// ジョブのログ（末尾 256KB）。
#[tauri::command]
#[specta::specta]
pub async fn job_log(state: State<'_, AppState>, id: i64) -> Result<String, AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.jobs().log_text(id, 256 * 1024)?)).await
}
