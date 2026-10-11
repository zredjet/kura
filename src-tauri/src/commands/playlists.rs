//! プレイリスト（docs/playlists.md §3〜§6）：一覧・作成・設定・エントリの操作・再開の状態。

use kura_core::playlist::Entry;
use kura_core::playlist::add::{AddOptions, AddSource, add};
use kura_core::playlist::play::{PlayRequest, playlist_queue};
use kura_core::playlist::store::{self, NewPlaylist, PlayState};
use kura_core::query::QueryContext;
use kura_core::time::now_ms;
use tauri::State;

use super::blocking;
use crate::dto::{
    AddSourceDto, AddedDto, EntryDto, GranularityDto, PlayStateDto, PlaylistContentsDto,
    PlaylistDto, PlaylistKindDto, PlaylistPatchDto, PlaylistSummaryDto, seed_from_string,
};
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// すべてのプレイリスト（サイドバー。件数は展開したアイテムの数）。
#[tauri::command]
#[specta::specta]
pub async fn playlists_list(
    state: State<'_, AppState>,
) -> Result<Vec<PlaylistSummaryDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session.library().db().read(|conn| {
            let mut out = Vec::new();
            for p in store::list(conn)? {
                let count = store::item_count(conn, p.id)?;
                out.push(PlaylistSummaryDto {
                    id: p.id,
                    name: p.name,
                    kind: p.kind.into(),
                    granularity: p.granularity.into(),
                    item_count: u32::try_from(count).unwrap_or(u32::MAX),
                });
            }
            Ok(out)
        })?)
    })
    .await
}

/// 1 つのプレイリストの設定と、続きから再生できる位置。
#[tauri::command]
#[specta::specta]
pub async fn playlist_get(state: State<'_, AppState>, id: i64) -> Result<PlaylistDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session.library().db().read(|conn| {
            let playlist = store::get(conn, id)?;
            let resume = store::load_state(conn, id)?;
            Ok(PlaylistDto::new(playlist, resume))
        })?)
    })
    .await
}

/// プレイリストを作る。ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn playlist_create(
    state: State<'_, AppState>,
    name: String,
    kind: PlaylistKindDto,
    granularity: GranularityDto,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let new = NewPlaylist {
            name,
            kind: kind.into(),
            granularity: granularity.into(),
        };
        Ok(session
            .library()
            .db()
            .write(move |tx| store::create(tx, &new, now_ms()))?)
    })
    .await
}

/// 設定を変える。
#[tauri::command]
#[specta::specta]
pub async fn playlist_update(
    state: State<'_, AppState>,
    id: i64,
    patch: PlaylistPatchDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let patch = patch.into();
        Ok(session
            .library()
            .db()
            .write(move |tx| store::update(tx, id, &patch, now_ms()))?)
    })
    .await
}

/// 削除する。
#[tauri::command]
#[specta::specta]
pub async fn playlist_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| store::delete(tx, id))?)
    })
    .await
}

/// 単位を切り替える（エントリは変えない）。
#[tauri::command]
#[specta::specta]
pub async fn playlist_set_granularity(
    state: State<'_, AppState>,
    id: i64,
    granularity: GranularityDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| store::set_granularity(tx, id, granularity.into(), now_ms()))?)
    })
    .await
}

/// 中身：エントリごとの行と、展開したアイテムの列。
#[tauri::command]
#[specta::specta]
pub async fn playlist_contents(
    state: State<'_, AppState>,
    id: i64,
) -> Result<PlaylistContentsDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let rows = session
            .library()
            .db()
            .read(|conn| store::contents(conn, id))?;
        Ok(PlaylistContentsDto::new(rows))
    })
    .await
}

/// 追加する（docs/playlists.md §3.5。`invert` は ⌥、`groupName` はユニットにまとめるときの名前）。
#[tauri::command]
#[specta::specta]
pub async fn playlist_add(
    state: State<'_, AppState>,
    id: i64,
    sources: Vec<AddSourceDto>,
    invert: bool,
    group_name: Option<String>,
    before: Option<i64>,
) -> Result<AddedDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let sources: Vec<AddSource> = sources.into_iter().map(Into::into).collect();
        let options = AddOptions { invert, group_name };
        let added = session
            .library()
            .db()
            .write(move |tx| add(tx, id, &sources, &options, before, now_ms()))?;
        Ok(AddedDto {
            entry_ids: added.entry_ids,
            unit_id: added.unit_id,
        })
    })
    .await
}

/// エントリを `before` の前（null なら末尾）へ移す。
#[tauri::command]
#[specta::specta]
pub async fn playlist_move(
    state: State<'_, AppState>,
    id: i64,
    entry_ids: Vec<i64>,
    before: Option<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| store::move_entries(tx, id, &entry_ids, before, now_ms()))?)
    })
    .await
}

/// エントリを外す。
#[tauri::command]
#[specta::specta]
pub async fn playlist_remove(
    state: State<'_, AppState>,
    id: i64,
    entry_ids: Vec<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| store::remove_entries(tx, id, &entry_ids, now_ms()))?)
    })
    .await
}

/// 「ファイルに固定」（`entryIds` を省けば全部）。取り消し用に、置き換える前のエントリを返す。
#[tauri::command]
#[specta::specta]
pub async fn playlist_snapshot_to_files(
    state: State<'_, AppState>,
    id: i64,
    entry_ids: Option<Vec<i64>>,
) -> Result<Vec<EntryDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let previous = session
            .library()
            .db()
            .write(move |tx| store::snapshot_to_files(tx, id, entry_ids.as_deref(), now_ms()))?;
        Ok(previous.into_iter().map(Into::into).collect())
    })
    .await
}

/// エントリを前の状態に戻す（「ファイルに固定」の取り消し）。
#[tauri::command]
#[specta::specta]
pub async fn playlist_restore_entries(
    state: State<'_, AppState>,
    id: i64,
    entries: Vec<EntryDto>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let entries: Vec<Entry> = entries.into_iter().map(Into::into).collect();
        Ok(session
            .library()
            .db()
            .write(move |tx| store::restore_entries(tx, id, &entries, now_ms()))?)
    })
    .await
}

/// スマートの並びを作り直す（新しいシード。「作り直す」）。
#[tauri::command]
#[specta::specta]
pub async fn playlist_regenerate(
    state: State<'_, AppState>,
    id: i64,
    utc_offset_minutes: i32,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let ctx = QueryContext {
            now_ms: now_ms(),
            utc_offset_minutes,
        };
        let request = PlayRequest {
            reseed: true,
            ..PlayRequest::default()
        };
        session
            .library()
            .db()
            .write(move |tx| playlist_queue(tx, id, &request, &ctx).map(|_| ()))?;
        Ok(())
    })
    .await
}

/// 再開の位置を保存する（§6.5）。`state` が null なら消す（最後まで再生した）。
#[tauri::command]
#[specta::specta]
pub async fn playlist_save_state(
    state: State<'_, AppState>,
    id: i64,
    play_state: Option<PlayStateDto>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let Some(s) = play_state else {
            return Ok(session
                .library()
                .db()
                .write(move |tx| store::clear_state(tx, id))?);
        };
        let seed = seed_from_string(&s.seed)
            .ok_or_else(|| AppError::new(AppErrorCode::Invalid, "シードが読めません"))?;
        let saved = PlayState {
            seed,
            slot_index: s.slot_index,
            item_id: s.item_id,
            position_ms: s.position_ms,
            updated_at: now_ms(),
        };
        Ok(session
            .library()
            .db()
            .write(move |tx| store::save_state(tx, id, &saved))?)
    })
    .await
}
