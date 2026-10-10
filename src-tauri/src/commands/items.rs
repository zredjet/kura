//! 一覧（ID 先取り）と、見えている分の詳細。

use std::sync::atomic::{AtomicBool, Ordering};

use kura_core::items::{ItemQuery, ItemSort, detail, query_ids, summaries};
use tauri::State;
use tauri::ipc::Response;

use super::blocking;
use crate::dto::{ItemDetailDto, ItemSummaryDto};
use crate::error::AppError;
use crate::state::AppState;

static FIRST_IDS: AtomicBool = AtomicBool::new(true);
static FIRST_GET: AtomicBool = AtomicBool::new(true);

/// 条件に合う ID・縦横比・日付のグループを生バイトで返す（形式は docs/architecture.md §3）。
/// 生バイトを返すため tauri-specta を通さずに登録している（フロントは `src/ipc/items.ts` で読む）。
#[tauri::command]
pub async fn items_query_ids(
    state: State<'_, AppState>,
    utc_offset_minutes: i32,
    trashed: Option<bool>,
) -> Result<Response, AppError> {
    let session = state.session()?;
    let bytes = blocking(move || {
        let query = ItemQuery {
            trashed: trashed.unwrap_or(false),
        };
        let list = session
            .library()
            .db()
            .read(|conn| query_ids(conn, &query, ItemSort::ImportedDesc, utc_offset_minutes))?;
        if FIRST_IDS.swap(false, Ordering::Relaxed) {
            tracing::info!(
                items = list.ids.len(),
                since_start_ms = crate::since_start_ms(),
                "起動から一覧の ID を返すまで"
            );
        }
        Ok(list.encode())
    })
    .await?;
    Ok(Response::new(bytes))
}

/// 見えている分の詳細（最大 200 件、渡した順）。
#[tauri::command]
#[specta::specta]
pub async fn items_get(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> Result<Vec<ItemSummaryDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let items = session.library().db().read(|conn| summaries(conn, &ids))?;
        if FIRST_GET.swap(false, Ordering::Relaxed) {
            tracing::info!(
                items = items.len(),
                since_start_ms = crate::since_start_ms(),
                "起動から最初のタイルの詳細を返すまで"
            );
        }
        Ok(items.into_iter().map(Into::into).collect())
    })
    .await
}

/// インスペクタ用の詳細（タグ・フォルダ・ユニットを含む）。
#[tauri::command]
#[specta::specta]
pub async fn item_detail(state: State<'_, AppState>, id: i64) -> Result<ItemDetailDto, AppError> {
    let session = state.session()?;
    blocking(move || Ok(session.library().db().read(|conn| detail(conn, id))?.into())).await
}
