//! ユニット（管理単位）：要約・詳細・作成・編集・メンバーの変更・解除。

use kura_core::time::now_ms;
use kura_core::unit::{self, UnitKind, UnitPatch};
use tauri::State;

use super::blocking;
use crate::dto::{
    UnitDetailDto, UnitFolderDto, UnitKindDto, UnitPatchDto, UnitRefDto, UnitSummaryDto,
};
use crate::error::AppError;
use crate::state::AppState;

fn folder_refs(folders: &[UnitFolderDto]) -> Vec<(i64, bool)> {
    folders.iter().map(|f| (f.folder_id, f.recursive)).collect()
}

/// 見えている分のユニットの要約（最大 200 件、渡した順）。
#[tauri::command]
#[specta::specta]
pub async fn units_get(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> Result<Vec<UnitSummaryDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(|conn| unit::summaries(conn, &ids))?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// ユニットのインスペクタ用の詳細。
#[tauri::command]
#[specta::specta]
pub async fn unit_detail(state: State<'_, AppState>, id: i64) -> Result<UnitDetailDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(|conn| unit::detail(conn, id))?
            .into())
    })
    .await
}

/// ユニットを選ぶ一覧（名前に `filter` を含むもの、更新の新しい順）。
#[tauri::command]
#[specta::specta]
pub async fn units_list(
    state: State<'_, AppState>,
    filter: String,
    kind: Option<UnitKindDto>,
    limit: u32,
) -> Result<Vec<UnitRefDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let kind: Option<UnitKind> = kind.map(Into::into);
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        Ok(session
            .library()
            .db()
            .read(|conn| unit::list(conn, &filter, kind, limit))?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// 選んだアイテムを set ユニットにまとめる（渡した順が並び順）。作ったユニットの ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn unit_create_set(
    state: State<'_, AppState>,
    name: String,
    item_ids: Vec<i64>,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::create_set(tx, &name, &item_ids, now))?)
    })
    .await
}

/// フォルダを参照する folder ユニットを作る。作ったユニットの ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn unit_create_folder(
    state: State<'_, AppState>,
    name: String,
    folders: Vec<UnitFolderDto>,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        let refs = folder_refs(&folders);
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::create_folder(tx, &name, &refs, now))?)
    })
    .await
}

/// ユニットの設定を変える。
#[tauri::command]
#[specta::specta]
pub async fn unit_update(
    state: State<'_, AppState>,
    id: i64,
    patch: UnitPatchDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    let patch: UnitPatch = patch.into();
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::update(tx, id, &patch, now))?)
    })
    .await
}

/// set ユニットの最後にアイテムを加える。
#[tauri::command]
#[specta::specta]
pub async fn unit_add_items(
    state: State<'_, AppState>,
    id: i64,
    item_ids: Vec<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::add_items(tx, id, &item_ids, now))?)
    })
    .await
}

/// set ユニットからアイテムを外す。
#[tauri::command]
#[specta::specta]
pub async fn unit_remove_items(
    state: State<'_, AppState>,
    id: i64,
    item_ids: Vec<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::remove_items(tx, id, &item_ids, now))?)
    })
    .await
}

/// set ユニットのメンバーを渡した順に並べ直す（並び順は手動になる）。
#[tauri::command]
#[specta::specta]
pub async fn unit_reorder(
    state: State<'_, AppState>,
    id: i64,
    item_ids: Vec<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::reorder(tx, id, &item_ids, now))?)
    })
    .await
}

/// folder ユニットの参照フォルダを置き換える。
#[tauri::command]
#[specta::specta]
pub async fn unit_set_folders(
    state: State<'_, AppState>,
    id: i64,
    folders: Vec<UnitFolderDto>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        let refs = folder_refs(&folders);
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::set_folders(tx, id, &refs, now))?)
    })
    .await
}

/// ユニットを解除する（アイテムは残る）。
#[tauri::command]
#[specta::specta]
pub async fn unit_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| unit::delete(tx, id))?)
    })
    .await
}
