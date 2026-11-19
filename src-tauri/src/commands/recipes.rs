//! レシピ（M14 段 3。docs/editing.md §4.3、docs/data-model.md §4「一括加工（M14）」、ADR-190・192）：一覧・1 件・エディタの文書から
//! 作る・中身を差し替える・名前・説明・並べ替え・複製・消す・`.kurarecipe` の書き出しと読み込み。ロジックは
//! `kura_core::edit::recipes`。

use std::path::PathBuf;
use std::sync::Arc;

use kura_core::edit::EngineRuntime;
use kura_core::edit::editor::{self, EditorEnv};
use kura_core::edit::recipes::{self, EditorPick, RecipeSummary, StoredRecipe};
use kura_core::library::LibrarySession;
use kura_core::time::now_ms;
use kura_engine::Graph;
use kura_engine::doc::NodeId;
use kura_engine::recipe::{Binding, MAX_RECIPE_BYTES, RecipeKind, RecipeNote};
use serde::Serialize;
use specta::Type;
use tauri::State;

use super::blocking;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 一覧の 1 行（サイドバーの節・レシピの画面の見出し）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecipeSummaryDto {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub kind: RecipeKind,
    /// 操作（ノード）の数。
    pub nodes: i64,
    /// 結び付けの数。
    pub bindings: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<RecipeSummary> for RecipeSummaryDto {
    fn from(r: RecipeSummary) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            kind: r.kind,
            nodes: r.nodes,
            bindings: r.bindings,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// 1 件のレシピ（文書・結び付け・旧版の印）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecipeDto {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub kind: RecipeKind,
    /// 見本の値の文書（今のスキーマ）。
    pub graph: Graph,
    pub bindings: Vec<Binding>,
    /// 今の版でない操作のノード（旧版・移す版・退役した版）。
    pub notes: Vec<RecipeNote>,
    /// 今の Kura では当てられない理由（あれば当てる画面は断る）。
    pub problem: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<StoredRecipe> for RecipeDto {
    fn from(s: StoredRecipe) -> Self {
        Self {
            id: s.id,
            name: s.recipe.name,
            description: s.recipe.description,
            kind: s.recipe.kind,
            graph: s.recipe.graph,
            bindings: s.recipe.bindings,
            notes: s.notes,
            problem: s.problem,
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

/// 読み込んだレシピ（足した ID と、旧版の操作の印。印があれば画面が「古い版の操作を含みます」を出す）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RecipeImportedDto {
    pub id: i64,
    pub notes: Vec<RecipeNote>,
}

/// このライブラリのエンジンの実行の場（エンジンを使わないライブラリなら断る）。
fn runtime_of(session: &LibrarySession) -> Result<Arc<EngineRuntime>, AppError> {
    let engine = session.engine().ok_or_else(|| {
        AppError::new(
            AppErrorCode::Invalid,
            "このライブラリではエンジンを使えません",
        )
    })?;
    Ok(engine.runtime()?.clone())
}

/// エディタの頭の文書を読んで、選んだものを作る（`f` に渡す）。
fn with_editor_pick<T>(
    session: &LibrarySession,
    editor_session: &str,
    keep: &[NodeId],
    with_format: bool,
    bindings: Vec<Binding>,
    f: impl FnOnce(EditorPick<'_>) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let env = EditorEnv::of(session)?;
    let graph = editor::head_graph(&env, editor_session)?;
    f(EditorPick {
        graph: &graph,
        keep,
        with_format,
        bindings,
    })
}

/// レシピの一覧（並びの順）。
#[tauri::command]
#[specta::specta]
pub async fn recipes_list(state: State<'_, AppState>) -> Result<Vec<RecipeSummaryDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(recipes::list)?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// 1 件のレシピ。
#[tauri::command]
#[specta::specta]
pub async fn recipe_get(state: State<'_, AppState>, id: i64) -> Result<RecipeDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let runtime = runtime_of(&session)?;
        Ok(recipes::get(session.library().db(), &runtime, id)?.into())
    })
    .await
}

/// エディタ（`session`）の頭の文書からレシピを作る（`keep` のノードだけを含める。`withFormat` が偽なら出力は「元と同じ」。
/// 結び付けはエディタの文書のノードで指す）。ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn recipe_create(
    state: State<'_, AppState>,
    session: String,
    name: String,
    description: String,
    keep: Vec<NodeId>,
    with_format: bool,
    bindings: Vec<Binding>,
) -> Result<i64, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let runtime = runtime_of(&lib)?;
        with_editor_pick(&lib, &session, &keep, with_format, bindings, |pick| {
            Ok(recipes::create_from_editor(
                lib.library().db(),
                &runtime,
                &name,
                &description,
                pick,
            )?)
        })
    })
    .await
}

/// レシピ `id` の中身（文書と結び付け）を、エディタ（`session`）の頭の文書から作り直して差し替える（ID・名前・説明・並びは
/// そのまま。見本で編集の保存）。
#[tauri::command]
#[specta::specta]
pub async fn recipe_replace(
    state: State<'_, AppState>,
    id: i64,
    session: String,
    keep: Vec<NodeId>,
    with_format: bool,
    bindings: Vec<Binding>,
) -> Result<(), AppError> {
    let lib = state.session()?;
    blocking(move || {
        let runtime = runtime_of(&lib)?;
        with_editor_pick(&lib, &session, &keep, with_format, bindings, |pick| {
            Ok(recipes::replace_from_editor(
                lib.library().db(),
                &runtime,
                id,
                pick,
            )?)
        })
    })
    .await
}

/// 名前を変える（前後の空白を除き NFC にする。1〜256 バイト、制御文字なし。重なってよい）。
#[tauri::command]
#[specta::specta]
pub async fn recipe_rename(
    state: State<'_, AppState>,
    id: i64,
    name: String,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(recipes::rename(session.library().db(), id, &name)?)).await
}

/// 説明を変える（空でよい。4,096 バイトまで、改行・タブのほかの制御文字なし）。
#[tauri::command]
#[specta::specta]
pub async fn recipe_describe(
    state: State<'_, AppState>,
    id: i64,
    description: String,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || Ok(recipes::describe(session.library().db(), id, &description)?)).await
}

/// 並びを変える（`ids` の順。含まれないものは後ろに今の順で）。
#[tauri::command]
#[specta::specta]
pub async fn recipes_reorder(state: State<'_, AppState>, ids: Vec<i64>) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| recipes::reorder(tx, &ids))?)
    })
    .await
}

/// 複製する（「{名前} のコピー」を元のすぐ後ろに）。新しい ID を返す。
#[tauri::command]
#[specta::specta]
pub async fn recipe_duplicate(state: State<'_, AppState>, id: i64) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| recipes::duplicate(tx, id, now_ms()))?)
    })
    .await
}

/// すぐ消す（画面は確かめてから。動いている一括はジョブの入力の写しで続く）。
#[tauri::command]
#[specta::specta]
pub async fn recipe_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| recipes::delete(tx, id))?)
    })
    .await
}

/// 書き出し・読み込みのパス（絶対パスで、ライブラリの外）。
fn outside_library(session: &LibrarySession, path: &str) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(AppError::new(
            AppErrorCode::Invalid,
            "レシピのファイルの場所が正しくありません",
        ));
    }
    let root = session
        .library()
        .layout()
        .root()
        .canonicalize()
        .map_err(|e| AppError::internal(e.to_string()))?;
    let dir = path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| {
            AppError::new(
                AppErrorCode::Invalid,
                "レシピのファイルのフォルダが見つかりません",
            )
        })?;
    if dir.starts_with(&root) {
        return Err(AppError::new(
            AppErrorCode::Invalid,
            "ライブラリの中のファイルは使えません。ほかのフォルダを選んでください",
        ));
    }
    Ok(path)
}

/// `.kurarecipe` を `path`（保存のダイアログで選んだ絶対パス。ライブラリの外）に書き出す。
#[tauri::command]
#[specta::specta]
pub async fn recipe_export(
    state: State<'_, AppState>,
    id: i64,
    path: String,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let path = outside_library(&session, &path)?;
        let runtime = runtime_of(&session)?;
        let bytes = recipes::export(session.library().db(), &runtime, id)?;
        std::fs::write(&path, bytes)
            .map_err(|e| AppError::new(AppErrorCode::Io, format!("レシピを書き出せません: {e}")))
    })
    .await
}

/// `.kurarecipe`（開くダイアログで選んだ絶対パス）を読み込んで足す。4 MiB を超えるファイルは読まずに断る。
#[tauri::command]
#[specta::specta]
pub async fn recipe_import(
    state: State<'_, AppState>,
    path: String,
) -> Result<RecipeImportedDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let path = outside_library(&session, &path)?;
        let io = |e: std::io::Error| {
            AppError::new(
                AppErrorCode::Io,
                format!("レシピのファイルを読めません: {e}"),
            )
        };
        let len = std::fs::metadata(&path).map_err(io)?.len();
        if len > MAX_RECIPE_BYTES as u64 {
            return Err(AppError::new(
                AppErrorCode::TooLarge,
                format!("レシピのファイルが大きすぎます（{len} バイト。上限 {MAX_RECIPE_BYTES}）"),
            ));
        }
        let raw = std::fs::read(&path).map_err(io)?;
        let runtime = runtime_of(&session)?;
        let (id, notes) = recipes::import(session.library().db(), &runtime, &raw)?;
        Ok(RecipeImportedDto { id, notes })
    })
    .await
}
