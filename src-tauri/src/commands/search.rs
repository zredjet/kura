//! 検索（⌘K パレット）とスマートフォルダ。

use kura_core::items::{ItemQuery, ItemView, count_kinds};
use kura_core::query::{QueryContext, check, suggest::suggest};
use kura_core::smart;
use kura_core::time::now_ms;
use tauri::State;

use super::blocking;
use super::items::to_view;
use crate::dto::{ItemViewDto, QueryCheckDto, SmartFolderDto, SuggestionDto};
use crate::error::AppError;
use crate::state::AppState;

fn context(utc_offset_minutes: i32) -> QueryContext {
    QueryContext {
        now_ms: now_ms(),
        utc_offset_minutes,
    }
}

/// 入力を解析し、開いている一覧の中で一致する件数を数える（パレットの「一致 186 項目」）。
#[tauri::command]
#[specta::specta]
pub async fn query_parse(
    state: State<'_, AppState>,
    text: String,
    view: Option<ItemViewDto>,
    utc_offset_minutes: i32,
) -> Result<QueryCheckDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let ctx = context(utc_offset_minutes);
        let view = to_view(view.unwrap_or(ItemViewDto::All), ctx.now_ms);
        Ok(session
            .library()
            .db()
            .read(|conn| check(conn, &text, view, &ctx))?
            .into())
    })
    .await
}

/// 入力中の語の候補（タグ → フィールド名・値 → 保存済みの検索）。
#[tauri::command]
#[specta::specta]
pub async fn query_suggest(
    state: State<'_, AppState>,
    word: String,
    limit: u32,
) -> Result<Vec<SuggestionDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        Ok(session
            .library()
            .db()
            .read(|conn| suggest(conn, &word, limit))?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// すべてのスマートフォルダ。`withCounts` なら一致する件数も数える。
#[tauri::command]
#[specta::specta]
pub async fn smart_folders_list(
    state: State<'_, AppState>,
    with_counts: Option<bool>,
    utc_offset_minutes: i32,
) -> Result<Vec<SmartFolderDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let ctx = context(utc_offset_minutes);
        Ok(session.library().db().read(|conn| {
            let folders = smart::list(conn)?;
            let mut out = Vec::with_capacity(folders.len());
            for folder in folders {
                let count = if with_counts.unwrap_or(false) {
                    let query = ItemQuery {
                        view: ItemView::Smart { id: folder.id },
                        ..ItemQuery::default()
                    };
                    // 条件が壊れていても一覧は出す（件数なし）
                    count_kinds(conn, &query, &ctx).ok().map(|c| c.total())
                } else {
                    None
                };
                out.push(SmartFolderDto {
                    id: folder.id,
                    name: folder.name,
                    query: folder.query,
                    count,
                });
            }
            Ok(out)
        })?)
    })
    .await
}

/// 検索をスマートフォルダとして保存する。作ったスマートフォルダの ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn smart_folder_save(
    state: State<'_, AppState>,
    name: String,
    query: String,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| smart::save(tx, &name, &query, now))?)
    })
    .await
}

/// 名前・条件を変える（省いたものは変えない）。
#[tauri::command]
#[specta::specta]
pub async fn smart_folder_update(
    state: State<'_, AppState>,
    id: i64,
    name: Option<String>,
    query: Option<String>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| smart::update(tx, id, name.as_deref(), query.as_deref()))?)
    })
    .await
}

/// スマートフォルダを消す（アイテムはそのまま）。
#[tauri::command]
#[specta::specta]
pub async fn smart_folder_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| smart::delete(tx, id))?)
    })
    .await
}
