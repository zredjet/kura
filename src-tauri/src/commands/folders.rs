//! 仮想フォルダ。

use tauri::State;

use super::blocking;
use crate::dto::FolderNodeDto;
use crate::error::AppError;
use crate::state::AppState;

/// すべての仮想フォルダ（木はフロントで組み立てる）。
#[tauri::command]
#[specta::specta]
pub async fn folders_tree(state: State<'_, AppState>) -> Result<Vec<FolderNodeDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(kura_core::folders::tree(session.library().db())?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}
