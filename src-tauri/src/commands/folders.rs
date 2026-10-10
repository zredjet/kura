//! 仮想フォルダ：木・作成・改名・色・移動・削除・アイテムの所属。

use kura_core::folders::{self, FolderPatch};
use kura_core::time::now_ms;
use tauri::State;

use super::blocking;
use crate::dto::{FolderNodeDto, FolderPatchDto};
use crate::error::AppError;
use crate::state::AppState;

/// すべての仮想フォルダ（木はフロントで組み立てる）。`with_counts` なら件数も数える。
#[tauri::command]
#[specta::specta]
pub async fn folders_tree(
    state: State<'_, AppState>,
    with_counts: Option<bool>,
) -> Result<Vec<FolderNodeDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let (nodes, counts) = session.library().db().read(|conn| {
            let nodes = folders::tree(conn)?;
            let counts = if with_counts.unwrap_or(false) {
                Some(folders::item_counts(conn)?)
            } else {
                None
            };
            Ok((nodes, counts))
        })?;
        Ok(nodes
            .into_iter()
            .map(|node| {
                let id = node.id;
                let mut dto = FolderNodeDto::from(node);
                dto.count = counts.as_ref().map(|c| c.get(&id).copied().unwrap_or(0));
                dto
            })
            .collect())
    })
    .await
}

/// フォルダを作る（`parent_id` がなければいちばん上）。作ったフォルダの ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn folder_create(
    state: State<'_, AppState>,
    parent_id: Option<i64>,
    name: String,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| folders::create(tx, parent_id, &name, now))?)
    })
    .await
}

/// 名前・色を変える。
#[tauri::command]
#[specta::specta]
pub async fn folder_update(
    state: State<'_, AppState>,
    id: i64,
    patch: FolderPatchDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    let patch: FolderPatch = patch.into();
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| folders::update(tx, id, &patch))?)
    })
    .await
}

/// フォルダを `parent_id` の下の `before_id` の前（なければ最後）へ移す。
#[tauri::command]
#[specta::specta]
pub async fn folder_move(
    state: State<'_, AppState>,
    id: i64,
    parent_id: Option<i64>,
    before_id: Option<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| folders::move_to(tx, id, parent_id, before_id))?)
    })
    .await
}

/// フォルダと中のフォルダを消す（アイテムは残る）。消したフォルダの数を返す。
#[tauri::command]
#[specta::specta]
pub async fn folder_delete(state: State<'_, AppState>, id: i64) -> Result<u32, AppError> {
    let session = state.session()?;
    blocking(move || {
        let n = session
            .library()
            .db()
            .write(move |tx| folders::delete(tx, id))?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}

/// アイテムを `add` のフォルダに入れ、`remove` のフォルダから外す。
#[tauri::command]
#[specta::specta]
pub async fn items_set_folders(
    state: State<'_, AppState>,
    item_ids: Vec<i64>,
    add: Vec<i64>,
    remove: Vec<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| folders::set_items(tx, &item_ids, &add, &remove))?)
    })
    .await
}
