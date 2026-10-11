//! タグ：候補・付け外し・編集・統合・削除。

use kura_core::tags::{self, TagPatch};
use kura_core::time::now_ms;
use tauri::State;

use super::blocking;
use crate::dto::{TagDetailDto, TagListEntryDto, TagPatchDto, TagSuggestionDto};
use crate::error::AppError;
use crate::state::AppState;

/// 入力に合うタグの候補（前方一致 → 部分一致、使われている数の多い順）。
#[tauri::command]
#[specta::specta]
pub async fn tags_suggest(
    state: State<'_, AppState>,
    prefix: String,
    limit: u32,
) -> Result<Vec<TagSuggestionDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        Ok(session
            .library()
            .db()
            .read(|conn| tags::suggest(conn, &prefix, limit))?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// すべてのタグと、付いている数（タグの一覧の画面）。
#[tauri::command]
#[specta::specta]
pub async fn tags_list(state: State<'_, AppState>) -> Result<Vec<TagListEntryDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(tags::list_all)?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// アイテムに `add`（名前。なければ作る）を付け、`remove`（ID）を外す。付けたタグの ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn tags_apply(
    state: State<'_, AppState>,
    item_ids: Vec<i64>,
    add: Vec<String>,
    remove: Vec<i64>,
) -> Result<Vec<i64>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| tags::apply(tx, &item_ids, &add, &remove, now))?)
    })
    .await
}

/// タグの編集画面の内容（別名・親・子を含む）。
#[tauri::command]
#[specta::specta]
pub async fn tag_detail(state: State<'_, AppState>, id: i64) -> Result<TagDetailDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(|conn| tags::detail(conn, id))?
            .into())
    })
    .await
}

/// 名前空間・名前・色・別名・親タグを変える。
#[tauri::command]
#[specta::specta]
pub async fn tag_update(
    state: State<'_, AppState>,
    id: i64,
    patch: TagPatchDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    let patch: TagPatch = patch.into();
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| tags::update(tx, id, &patch))?)
    })
    .await
}

/// `from` を `into` に統合する（`from` の名前は別名として残る）。
#[tauri::command]
#[specta::specta]
pub async fn tag_merge(state: State<'_, AppState>, from: i64, into: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| tags::merge(tx, from, into))?)
    })
    .await
}

/// タグを消す（付いていたアイテムからも外れる）。
#[tauri::command]
#[specta::specta]
pub async fn tag_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| tags::delete(tx, id))?)
    })
    .await
}
