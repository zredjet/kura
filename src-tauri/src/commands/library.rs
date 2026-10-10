//! ライブラリの作成・オープン・概要。

use std::path::{Path, PathBuf};

use kura_core::library::{LibrarySession, check_location, is_library};
use tauri::{AppHandle, Manager, State};

use super::blocking;
use crate::dto::{LibraryInfoDto, LibraryRestoreDto, LocationCheckDto};
use crate::error::AppError;
use crate::state::{AppState, event_sink};

/// 計測などで、設定を書き換えずに開くライブラリを指定する環境変数。
const LIBRARY_ENV: &str = "KURA_LIBRARY";

/// ライブラリを置く場所を確かめる（拡張子の補完、既存のライブラリか、同期フォルダの中か）。
#[tauri::command]
#[specta::specta]
pub async fn library_check_location(
    app: AppHandle,
    path: String,
) -> Result<LocationCheckDto, AppError> {
    let home = app.path().home_dir().ok();
    blocking(move || Ok(check_location(Path::new(&path), home.as_deref()).into())).await
}

/// 新しいライブラリを作って開く。
#[tauri::command]
#[specta::specta]
pub async fn library_create(app: AppHandle, path: String) -> Result<LibraryInfoDto, AppError> {
    blocking(move || {
        let sink = event_sink(app.clone());
        let session = app.state::<AppState>().replace_session(
            || LibrarySession::create(Path::new(&path), sink, kura_mac::media_backend()),
            true,
        )?;
        Ok(session.info()?.into())
    })
    .await
}

/// 既存のライブラリを開く。
#[tauri::command]
#[specta::specta]
pub async fn library_open(app: AppHandle, path: String) -> Result<LibraryInfoDto, AppError> {
    blocking(move || {
        let sink = event_sink(app.clone());
        let session = app.state::<AppState>().replace_session(
            || LibrarySession::open(Path::new(&path), sink, kura_mac::media_backend()),
            true,
        )?;
        Ok(session.info()?.into())
    })
    .await
}

/// 起動時に前回のライブラリを開く。開けなければ `info` は空で、前回の場所だけ返す。
#[tauri::command]
#[specta::specta]
pub async fn library_restore(app: AppHandle) -> Result<LibraryRestoreDto, AppError> {
    blocking(move || {
        let state = app.state::<AppState>();
        // 計測用：環境変数 KURA_LIBRARY のライブラリを、設定を書き換えずに開く
        if let Some(path) = std::env::var_os(LIBRARY_ENV).filter(|_| state.current().is_none()) {
            let path = PathBuf::from(path);
            let sink = event_sink(app.clone());
            let session = state.replace_session(
                || LibrarySession::open(&path, sink, kura_mac::media_backend()),
                false,
            )?;
            return Ok(LibraryRestoreDto {
                info: Some(session.info()?.into()),
                last_path: Some(path.to_string_lossy().into_owned()),
            });
        }
        let last = state.last_library();
        let last_path = last.as_ref().map(|p| p.to_string_lossy().into_owned());
        if let Some(session) = state.current() {
            return Ok(LibraryRestoreDto {
                info: Some(session.info()?.into()),
                last_path,
            });
        }
        let Some(last) = last.filter(|p| is_library(p)) else {
            return Ok(LibraryRestoreDto {
                info: None,
                last_path,
            });
        };
        let sink = event_sink(app.clone());
        match state.replace_session(
            || LibrarySession::open(&last, sink, kura_mac::media_backend()),
            true,
        ) {
            Ok(session) => Ok(LibraryRestoreDto {
                info: Some(session.info()?.into()),
                last_path,
            }),
            Err(err) => {
                tracing::warn!(%err, path = %last.display(), "前回のライブラリを開けませんでした");
                Ok(LibraryRestoreDto {
                    info: None,
                    last_path,
                })
            }
        }
    })
    .await
}

/// 開いているライブラリの概要。
#[tauri::command]
#[specta::specta]
pub async fn library_info(state: State<'_, AppState>) -> Result<LibraryInfoDto, AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.info()?.into())).await
}
