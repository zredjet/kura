//! 再生（docs/architecture.md §8、docs/playlists.md §6）：再生可否の表、キュー、再生の記録、プロキシ、外部プレイヤー。

use kura_core::playback::{PlaybackSupport, SupportEntry, record};
use kura_core::queue::{from_selection, total_ms};
use kura_core::serve::{Route, resolve};
use kura_core::time::now_ms;
use tauri::{AppHandle, Manager, State};

use super::blocking;
use crate::dto::{ExternalPlayerDto, PlayReportDto, PlayerPrefsDto, QueueDto, SupportEntryDto};
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 起動時に作った再生可否の表を受け取る（表が変わっていればプロキシの判定をやり直す）。
#[tauri::command]
#[specta::specta]
pub async fn playback_support_set(
    app: AppHandle,
    entries: Vec<SupportEntryDto>,
) -> Result<(), AppError> {
    let entries: Vec<SupportEntry> = entries.into_iter().map(Into::into).collect();
    let support = PlaybackSupport::from_entries(&entries);
    blocking(move || app.state::<AppState>().set_playback_support(support)).await
}

/// 選択（アイテムと、まとめて表示したユニット）を並び順のまま再生キューにする。
#[tauri::command]
#[specta::specta]
pub async fn player_queue(
    state: State<'_, AppState>,
    entries: Vec<i64>,
) -> Result<QueueDto, AppError> {
    let session = state.session()?;
    let image_ms = state.player_prefs().image_duration_ms;
    blocking(move || {
        Ok(session.library().db().read(|conn| {
            let slots = from_selection(conn, &entries, Some(image_ms))?;
            let total = total_ms(conn, &slots)?;
            Ok(QueueDto::new(slots, total))
        })?)
    })
    .await
}

/// スロットを離れたときの報告。再生済みなら記録して true。
#[tauri::command]
#[specta::specta]
pub async fn playback_report(
    state: State<'_, AppState>,
    report: PlayReportDto,
) -> Result<bool, AppError> {
    let session = state.session()?;
    blocking(move || {
        let report = report.into();
        Ok(session
            .library()
            .db()
            .write(move |tx| record(tx, &report, now_ms()))?)
    })
    .await
}

/// 再生キューの今と次のアイテムのプロキシを先に作る。`force` なら、元のまま再生できなかったものを作り直す。
#[tauri::command]
#[specta::specta]
pub async fn proxy_request(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    force: bool,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.request_proxies(ids, force)?)).await
}

/// 元のファイルを外部プレイヤー（mpv、なければ QuickTime Player）で開く。
#[tauri::command]
#[specta::specta]
pub async fn open_external(
    state: State<'_, AppState>,
    hash: String,
) -> Result<ExternalPlayerDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let path = resolve(session.library().layout(), &Route::File { hash })
            .ok_or_else(|| AppError::new(AppErrorCode::NotFound, "ファイルが見つかりません"))?;
        kura_mac::open_in_player(&path)
            .map(Into::into)
            .map_err(AppError::internal)
    })
    .await
}

/// 再生の設定。
#[tauri::command]
#[specta::specta]
pub fn player_prefs_get(state: State<'_, AppState>) -> PlayerPrefsDto {
    state.player_prefs().into()
}

/// 再生の設定を変えて保存する（範囲外の値は丸めて返す）。
#[tauri::command]
#[specta::specta]
pub async fn player_prefs_set(
    app: AppHandle,
    prefs: PlayerPrefsDto,
) -> Result<PlayerPrefsDto, AppError> {
    blocking(move || {
        app.state::<AppState>()
            .set_player_prefs(prefs.into())
            .map(Into::into)
    })
    .await
}
