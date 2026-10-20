//! ライブラリの作成・オープン・概要。

use std::path::{Path, PathBuf};

use kura_core::library::{LibrarySession, backup, check_files, check_location, is_library};
use tauri::{AppHandle, Manager, State};

use super::blocking;
use crate::dto::{
    BackupDto, IntegrityDto, LibraryCacheUsageDto, LibraryInfoDto, LibraryRestoreDto,
    LibraryUsageDto, LocationCheckDto,
};
use crate::error::{AppError, AppErrorCode};
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
        let state = app.state::<AppState>();
        let services = state.services(&app);
        let session = state.replace_session(
            || LibrarySession::create(Path::new(&path), sink, services),
            true,
        )?;
        Ok(session.info()?.into())
    })
    .await
}

/// 既存のライブラリを開く。新しい版にするときは、始まり・終わり・失敗をイベント `library://upgrade` で知らせる。開けなければ
/// 理由の種類（`LibraryTooNew`・`NoSpace`・`BackupFailed`・`UpgradeFailed` など）と説明を返す。
#[tauri::command]
#[specta::specta]
pub async fn library_open(app: AppHandle, path: String) -> Result<LibraryInfoDto, AppError> {
    blocking(move || {
        let sink = event_sink(app.clone());
        let state = app.state::<AppState>();
        let services = state.services(&app);
        let session = state.replace_session(
            || LibrarySession::open(Path::new(&path), sink, services),
            true,
        )?;
        log_opened(&session, "ライブラリを開きました");
        Ok(session.info()?.into())
    })
    .await
}

/// 起動時に前回のライブラリ（`KURA_LIBRARY` があればそれ）を開く。前回の場所が見つからなければ `info` も `error` も空で、
/// 前回の場所だけ返す。開けなければ `error` に理由（`library_open` と同じ種類）を入れて返す（失敗にはしない）。
/// 新しい版にするときは、始まり・終わり・失敗をイベント `library://upgrade` で知らせる。
#[tauri::command]
#[specta::specta]
pub async fn library_restore(app: AppHandle) -> Result<LibraryRestoreDto, AppError> {
    blocking(move || {
        let state = app.state::<AppState>();
        // 開いているかを確かめてから開くまでを 1 つの鍵で（StrictMode などで並んで呼ばれても、開くのは 1 回）
        let _opening = state.opening();
        // 計測用：環境変数 KURA_LIBRARY のライブラリを、設定を書き換えずに開く
        if let Some(path) = std::env::var_os(LIBRARY_ENV).filter(|_| state.current().is_none()) {
            let path = PathBuf::from(path);
            return restore_from(&app, &state, path, false);
        }
        let last = state.last_library();
        let last_path = last.as_ref().map(|p| p.to_string_lossy().into_owned());
        if let Some(session) = state.current() {
            return Ok(LibraryRestoreDto {
                info: Some(session.info()?.into()),
                last_path,
                error: None,
            });
        }
        let Some(last) = last.filter(|p| is_library(p)) else {
            return Ok(LibraryRestoreDto {
                info: None,
                last_path,
                error: None,
            });
        };
        restore_from(&app, &state, last, true)
    })
    .await
}

/// 起動時に `path` のライブラリを開く（呼び手が [`AppState::opening`] の鍵を持っている）。開けなければ理由を `error` に
/// 入れて返す。
fn restore_from(
    app: &AppHandle,
    state: &AppState,
    path: PathBuf,
    remember: bool,
) -> Result<LibraryRestoreDto, AppError> {
    let last_path = Some(path.to_string_lossy().into_owned());
    let sink = event_sink(app.clone());
    let services = state.services(app);
    match state.replace_session_locked(|| LibrarySession::open(&path, sink, services), remember) {
        Ok(session) => {
            log_opened(&session, "起動から前回のライブラリを開くまで");
            Ok(LibraryRestoreDto {
                info: Some(session.info()?.into()),
                last_path,
                error: None,
            })
        }
        Err(err) => {
            tracing::warn!(code = ?err.code, message = %err.message, path = %path.display(), "前回のライブラリを開けませんでした");
            Ok(LibraryRestoreDto {
                info: None,
                last_path,
                error: Some(err),
            })
        }
    }
}

/// 開いたことをログに残す。DB を新しい版にした回（更新後の初回）は `upgraded` と時間を付ける（NFR-01 は 2 回目以降で
/// 測り、初回はその外として分けて記録する。docs/requirements.md）。
fn log_opened(session: &LibrarySession, what: &str) {
    let report = session.library().migration();
    tracing::info!(
        since_start_ms = crate::since_start_ms(),
        upgraded = report.upgraded(),
        from = report.from,
        to = report.to,
        upgrade_ms = report.total_ms(),
        "{what}"
    );
}

/// 開いているライブラリの概要。
#[tauri::command]
#[specta::specta]
pub async fn library_info(state: State<'_, AppState>) -> Result<LibraryInfoDto, AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.info()?.into())).await
}

/// DB のバックアップの一覧（新しい順。FR-SYS-03）。
#[tauri::command]
#[specta::specta]
pub async fn backups_list(state: State<'_, AppState>) -> Result<Vec<BackupDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(backup::list(session.library().layout())?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// いまのバックアップを作る（古いものは新しい 7 個を残して消す）。
#[tauri::command]
#[specta::specta]
pub async fn backup_now(state: State<'_, AppState>) -> Result<BackupDto, AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.backup_now()?.into())).await
}

/// Finder で見せる（書き出したフォルダ・バックアップ・ライブラリ）。
#[tauri::command]
#[specta::specta]
pub async fn reveal_path(path: String) -> Result<(), AppError> {
    blocking(move || {
        if kura_mac::reveal_in_finder(Path::new(&path)) {
            Ok(())
        } else {
            Err(AppError::new(
                AppErrorCode::NotFound,
                "見つかりません（移動・削除されたかもしれません）",
            ))
        }
    })
    .await
}

/// ライブラリの容量と、あるボリュームの容量・空き。
#[tauri::command]
#[specta::specta]
pub async fn library_usage(state: State<'_, AppState>) -> Result<LibraryUsageDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let library_bytes = session
            .library()
            .db()
            .read(kura_core::library::ledger::total_size)?;
        let volume = kura_mac::volume_capacity(session.library().layout().root());
        Ok(LibraryUsageDto {
            library_bytes,
            volume_total_bytes: volume.map(|v| v.total_bytes),
            volume_available_bytes: volume.map(|v| v.available_bytes),
        })
    })
    .await
}

/// サムネイル・波形・アニメのキャッシュの大きさ（設定の「容量」の行。ファイルを数えるので、設定の画面を開いたときだけ呼ぶ）。
#[tauri::command]
#[specta::specta]
pub async fn library_cache_usage(
    state: State<'_, AppState>,
) -> Result<LibraryCacheUsageDto, AppError> {
    let session = state.session()?;
    blocking(move || Ok(kura_core::media::usage::cache_usage(session.library().layout())?.into()))
        .await
}

/// 整合性チェックをいま行う（DB にあってファイルがないもの・ファイルがあって DB にないものを数える）。
#[tauri::command]
#[specta::specta]
pub async fn library_check(state: State<'_, AppState>) -> Result<IntegrityDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let library = session.library();
        let (missing_files, orphan_files) = check_files(library.db(), library.layout())?;
        let unreferenced_blobs = library
            .db()
            .read(kura_core::library::ledger::unreferenced)?;
        Ok(IntegrityDto {
            staging_removed: 0,
            missing_files,
            orphan_files,
            unreferenced_blobs,
        })
    })
    .await
}
