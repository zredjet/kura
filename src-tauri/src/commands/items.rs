//! 一覧（ID 先取り）と、見えている分の詳細、アイテムの編集とゴミ箱。

use std::sync::atomic::{AtomicBool, Ordering};

use kura_core::entries::{self, EntryPatch};
use kura_core::items::{
    self, ItemPatch, ItemQuery, ItemView, detail, query_ids, recent_since, summaries,
};
use kura_core::query::{QueryContext, parse};
use kura_core::time::now_ms;
use tauri::State;
use tauri::ipc::Response;

use super::blocking;
use crate::dto::{
    EntriesUpdatedDto, EntryPatchDto, ItemDetailDto, ItemPatchDto, ItemSortDto, ItemSummaryDto,
    ItemViewDto, LibraryCountsDto, SelectionInfoDto, TrashEmptiedDto,
};
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 一覧の種類にする（「最近の取り込み」の起点は今の時刻から決める）。
pub(crate) fn to_view(view: ItemViewDto, now: i64) -> ItemView {
    match view {
        ItemViewDto::All => ItemView::All,
        ItemViewDto::Recent => ItemView::Recent {
            since: recent_since(now),
        },
        ItemViewDto::Unsorted => ItemView::Unsorted,
        ItemViewDto::Untagged => ItemView::Untagged,
        ItemViewDto::Random { seed } => ItemView::Random { seed },
        ItemViewDto::Trash => ItemView::Trash,
        ItemViewDto::Folder { id } => ItemView::Folder { id },
        ItemViewDto::Unit { id } => ItemView::Unit { id },
        ItemViewDto::Smart { id } => ItemView::Smart { id },
    }
}

static FIRST_IDS: AtomicBool = AtomicBool::new(true);
static FIRST_GET: AtomicBool = AtomicBool::new(true);

/// 条件に合う ID・縦横比・日付のグループを生バイトで返す（形式は docs/architecture.md §3）。
/// 生バイトを返すため tauri-specta を通さずに登録している（フロントは `src/ipc/items.ts` で読む）。
#[tauri::command]
pub async fn items_query_ids(
    state: State<'_, AppState>,
    utc_offset_minutes: i32,
    view: Option<ItemViewDto>,
    collapse_units: Option<bool>,
    query: Option<String>,
    sort: Option<ItemSortDto>,
) -> Result<Response, AppError> {
    let session = state.session()?;
    let bytes = blocking(move || {
        let now = now_ms();
        let filter = match query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            Some(text) => Some(
                parse(text)
                    .map_err(|e| AppError::new(AppErrorCode::InvalidQuery, e.message))?
                    .expr,
            ),
            None => None,
        };
        let query = ItemQuery {
            view: to_view(view.unwrap_or(ItemViewDto::All), now),
            collapse_units: collapse_units.unwrap_or(false),
            filter,
            sort: sort.map(Into::into).unwrap_or_default(),
        };
        let ctx = QueryContext {
            now_ms: now,
            utc_offset_minutes,
        };
        let list = session
            .library()
            .db()
            .read(|conn| query_ids(conn, &query, &ctx))?;
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

/// サイドバーの「ライブラリ」の件数。
#[tauri::command]
#[specta::specta]
pub async fn library_counts(state: State<'_, AppState>) -> Result<LibraryCountsDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let since = recent_since(now_ms());
        Ok(session
            .library()
            .db()
            .read(|conn| items::counts(conn, since))?
            .into())
    })
    .await
}

/// タイトル（1 件のとき）・評価・メモを変える。
#[tauri::command]
#[specta::specta]
pub async fn items_update(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    patch: ItemPatchDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    let patch: ItemPatch = patch.into();
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| items::update(tx, &ids, &patch, now))?)
    })
    .await
}

/// アイテムとユニットに評価・メモ・タグをまとめて付け外しする（ユニットはメンバー全員にも）。
#[tauri::command]
#[specta::specta]
pub async fn entries_update(
    state: State<'_, AppState>,
    item_ids: Vec<i64>,
    unit_ids: Vec<i64>,
    patch: EntryPatchDto,
) -> Result<EntriesUpdatedDto, AppError> {
    let session = state.session()?;
    let patch: EntryPatch = patch.into();
    blocking(move || {
        let now = now_ms();
        let done = session
            .library()
            .db()
            .write(move |tx| entries::update(tx, &item_ids, &unit_ids, &patch, now))?;
        Ok(done.into())
    })
    .await
}

/// ゴミ箱に入れる。入れた件数を返す。
#[tauri::command]
#[specta::specta]
pub async fn items_trash(state: State<'_, AppState>, ids: Vec<i64>) -> Result<u32, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        let n = session
            .library()
            .db()
            .write(move |tx| items::trash(tx, &ids, now))?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}

/// ゴミ箱から戻す。戻した件数を返す。
#[tauri::command]
#[specta::specta]
pub async fn items_restore(state: State<'_, AppState>, ids: Vec<i64>) -> Result<u32, AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        let n = session
            .library()
            .db()
            .write(move |tx| items::restore(tx, &ids, now))?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    })
    .await
}

/// ゴミ箱を空にする（実ファイルも消す。確認はフロントで行う）。
#[tauri::command]
#[specta::specta]
pub async fn trash_empty(state: State<'_, AppState>) -> Result<TrashEmptiedDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let result = session.library().empty_trash()?;
        tracing::info!(
            items = result.items,
            bytes = result.bytes,
            failed = result.failed,
            "ゴミ箱を空にしました"
        );
        Ok(result.into())
    })
    .await
}

/// 複数選択のインスペクタに出す集計（評価・メモ・タグ・フォルダ）。
#[tauri::command]
#[specta::specta]
pub async fn items_selection_info(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> Result<SelectionInfoDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(|conn| items::selection_info(conn, &ids))?
            .into())
    })
    .await
}
