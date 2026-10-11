//! 再生（docs/architecture.md §8、docs/playlists.md §6）：再生可否の表、キュー、再生の記録、プロキシ、外部プレイヤー。

use kura_core::items::{ItemQuery, ItemSort, query_ids};
use kura_core::playback::{PlaybackSupport, SupportEntry, record};
use kura_core::playlist::QueueOptions;
use kura_core::playlist::play::{
    PlayRequest, describe, entries_queue, playlist_queue, selection_entries,
};
use kura_core::playlist::store as playlist_store;
use kura_core::query::{QueryContext, parse};
use kura_core::serve::{Route, resolve};
use kura_core::time::now_ms;
use kura_core::unit::LIST_FLAG;
use tauri::{AppHandle, Manager, State};

use super::blocking;
use super::items::to_view;
use crate::dto::{
    ExternalPlayerDto, PlayQueueDto, PlayReportDto, PlayRequestDto, PlaySourceDto, PlayerPrefsDto,
    SupportEntryDto, seed_from_string,
};
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

/// 再生するキューを作る（プレイリスト・ライブラリの選択・開いている一覧）。
///
/// - プレイリスト：設定どおり（シャッフル・シード・スマートの作り直し・再開）。シードなどを保存する。
/// - 選択：並び順のまま。ユニットを含めばユニット単位。`request.shuffle` でシャッフル。
/// - 一覧：⌘K の ⇧↵（結果をランダム再生）。既定でシャッフル。
#[tauri::command]
#[specta::specta]
pub async fn play_queue(
    state: State<'_, AppState>,
    source: PlaySourceDto,
    request: PlayRequestDto,
    utc_offset_minutes: i32,
) -> Result<PlayQueueDto, AppError> {
    let session = state.session()?;
    let image_ms = state.player_prefs().image_duration_ms;
    blocking(move || {
        let now = now_ms();
        let ctx = QueryContext {
            now_ms: now,
            utc_offset_minutes,
        };
        let play = PlayRequest::from(&request);
        let seed = request.seed.as_deref().and_then(seed_from_string);
        let adhoc = |entries: Vec<i64>, shuffle_default: bool| {
            let flag = i64::from(LIST_FLAG);
            let pairs: Vec<(bool, i64)> =
                entries.iter().map(|e| (e & flag != 0, e & !flag)).collect();
            let (entries, granularity) = selection_entries(&pairs);
            let options = QueueOptions {
                granularity,
                shuffle: play.shuffle.unwrap_or(shuffle_default),
                weight_by_rating: true,
                max_same_kind_run: 2,
                dedup_items: true,
                image_duration_ms: image_ms,
                now_ms: now,
            };
            session.library().db().read(|conn| {
                let queue = entries_queue(conn, entries, &options, &play, seed)?;
                let info = describe(conn, &queue)?;
                Ok(PlayQueueDto::new(queue, info, None))
            })
        };
        Ok(match source {
            PlaySourceDto::Playlist { id } => session.library().db().write(move |tx| {
                let queue = playlist_queue(tx, id, &play, &ctx)?;
                let playlist = playlist_store::get(tx, id)?;
                let info = describe(tx, &queue)?;
                Ok(PlayQueueDto::new(queue, info, Some(playlist)))
            })?,
            PlaySourceDto::Selection { entries } => adhoc(entries, false)?,
            PlaySourceDto::View {
                view,
                query,
                collapse_units,
                utc_offset_minutes,
            } => {
                let filter = match query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
                    Some(text) => Some(
                        parse(text)
                            .map_err(|e| AppError::new(AppErrorCode::InvalidQuery, e.message))?
                            .expr,
                    ),
                    None => None,
                };
                let item_query = ItemQuery {
                    view: to_view(view, now),
                    collapse_units,
                    filter,
                    sort: ItemSort::default(),
                };
                let view_ctx = QueryContext {
                    now_ms: now,
                    utc_offset_minutes,
                };
                let list = session
                    .library()
                    .db()
                    .read(|conn| query_ids(conn, &item_query, &view_ctx))?;
                adhoc(list.ids.iter().map(|id| i64::from(*id)).collect(), true)?
            }
        })
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
