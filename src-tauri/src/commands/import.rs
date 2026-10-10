//! 取り込み。

use std::path::PathBuf;

use kura_core::import::{ImportInput, preview, start_import};
use tauri::State;

use super::blocking;
use crate::dto::{ImportOptionsDto, ImportPreviewDto};
use crate::error::AppError;
use crate::state::AppState;

/// 取り込み前の確認（種類別件数・合計サイズ・重複の見込み・フォルダの概要）。
#[tauri::command]
#[specta::specta]
pub async fn import_preview(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<ImportPreviewDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        Ok(preview(session.library(), &paths)?.into())
    })
    .await
}

/// 取り込みジョブを積み、ジョブの ID を返す。進捗は `job://progress` で届く。
#[tauri::command]
#[specta::specta]
pub async fn import_start(
    state: State<'_, AppState>,
    paths: Vec<String>,
    options: ImportOptionsDto,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let input = ImportInput {
            paths: paths.into_iter().map(PathBuf::from).collect(),
            options: options.into(),
        };
        Ok(start_import(session.jobs(), &input)?)
    })
    .await
}
