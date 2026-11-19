//! レシピ（M14 段 3。docs/editing.md §4.3、docs/data-model.md §4「一括加工（M14）」、ADR-190・192）：一覧・1 件・エディタの文書から
//! 作る・中身を差し替える・名前・説明・並べ替え・複製・消す・`.kurarecipe` の書き出しと読み込み。ロジックは
//! `kura_core::edit::recipes`。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

/// このライブラリの場所（パスの確かめに使う。シンボリックリンクをたどったもの）。
fn library_root(session: &LibrarySession) -> Result<PathBuf, AppError> {
    session
        .library()
        .layout()
        .root()
        .canonicalize()
        .map_err(|e| AppError::internal(e.to_string()))
}

/// 書き出し・読み込みのパス：絶対パスで、ライブラリ（`root`。たどったもの）の外。親のフォルダはたどって確かめ、もうある名前は
/// リンクをたどった先も確かめる（ライブラリの中を指すシンボリックリンクを選んでも、ライブラリのファイルに触れない）。
fn outside_library(root: &Path, path: &str) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(path);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(AppError::new(
            AppErrorCode::Invalid,
            "レシピのファイルの場所が正しくありません",
        ));
    }
    let inside = || {
        AppError::new(
            AppErrorCode::Invalid,
            "ライブラリの中のファイルは使えません。ほかのフォルダを選んでください",
        )
    };
    let dir = path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| {
            AppError::new(
                AppErrorCode::Invalid,
                "レシピのファイルのフォルダが見つかりません",
            )
        })?;
    if dir.starts_with(root) {
        return Err(inside());
    }
    if let Ok(target) = path.canonicalize()
        && target.starts_with(root)
    {
        return Err(inside());
    }
    Ok(path)
}

/// `bytes` を `path` に書く：同じフォルダの一時ファイルに書いて同期し、名前を置き換える（`path` がシンボリックリンクなら
/// リンクそのものを置き換え、リンクの先には触れない。書けなかったら前のファイルは残る）。
fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("ファイル名がありません"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".kura-{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// 通常のファイル `path` を `max` バイトまで読む（FIFO・デバイスは開く前に断る。大きければ読み切らずに TooLarge）。
fn read_limited(path: &Path, max: usize) -> Result<Vec<u8>, AppError> {
    let io = |e: std::io::Error| {
        AppError::new(
            AppErrorCode::Io,
            format!("レシピのファイルを読めません: {e}"),
        )
    };
    if !std::fs::metadata(path).map_err(io)?.is_file() {
        return Err(AppError::new(
            AppErrorCode::Invalid,
            "レシピのファイルではありません（通常のファイルを選んでください）",
        ));
    }
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .map_err(io)?
        .take(max as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(io)?;
    if raw.len() > max {
        return Err(AppError::new(
            AppErrorCode::TooLarge,
            format!("レシピのファイルが大きすぎます（上限 {max} バイト）"),
        ));
    }
    Ok(raw)
}

/// `.kurarecipe` を `path`（保存のダイアログで選んだ絶対パス。ライブラリの外）に書き出す（一時ファイルから名前を置き換える）。
#[tauri::command]
#[specta::specta]
pub async fn recipe_export(
    state: State<'_, AppState>,
    id: i64,
    path: String,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let path = outside_library(&library_root(&session)?, &path)?;
        let runtime = runtime_of(&session)?;
        let bytes = recipes::export(session.library().db(), &runtime, id)?;
        write_replacing(&path, &bytes)
            .map_err(|e| AppError::new(AppErrorCode::Io, format!("レシピを書き出せません: {e}")))
    })
    .await
}

/// `.kurarecipe`（開くダイアログで選んだ絶対パス）を読み込んで足す。通常のファイルだけを、4 MiB まで読む。
#[tauri::command]
#[specta::specta]
pub async fn recipe_import(
    state: State<'_, AppState>,
    path: String,
) -> Result<RecipeImportedDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let path = outside_library(&library_root(&session)?, &path)?;
        let raw = read_limited(&path, MAX_RECIPE_BYTES)?;
        let runtime = runtime_of(&session)?;
        let (id, notes) = recipes::import(session.library().db(), &runtime, &raw)?;
        Ok(RecipeImportedDto { id, notes })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("L.kuralib");
        let out = d.path().join("out");
        std::fs::create_dir_all(root.join("files")).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        let (root, out) = (root.canonicalize().unwrap(), out.canonicalize().unwrap());
        (d, root, out)
    }

    /// ライブラリの外の絶対パスだけを受け付ける（相対・ライブラリの中・ライブラリの中を指すリンク・フォルダのないものは断る）。
    #[test]
    fn paths_must_be_outside_the_library() {
        let (_d, root, out) = dirs();
        let ok = out.join("a.kurarecipe");
        assert_eq!(outside_library(&root, ok.to_str().unwrap()).unwrap(), ok);
        let link = out.join("link.kurarecipe");
        std::fs::write(root.join("files/x.png"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("files/x.png"), &link).unwrap();
        for bad in [
            "a.kurarecipe".to_string(),
            root.join("a.kurarecipe").display().to_string(),
            root.join("files/x.png").display().to_string(),
            link.display().to_string(),
            out.join("none/a.kurarecipe").display().to_string(),
        ] {
            let e = outside_library(&root, &bad).unwrap_err();
            assert_eq!(e.code, AppErrorCode::Invalid, "{bad}");
        }
    }

    /// 書き出しは名前を置き換える：シンボリックリンクはリンクそのものが置き換わり、リンクの先は変わらない。一時ファイルは残らない。
    #[test]
    fn writing_replaces_the_name_not_the_link_target() {
        let (_d, _root, out) = dirs();
        let target = out.join("target.txt");
        std::fs::write(&target, b"keep").unwrap();
        let link = out.join("r.kurarecipe");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_replacing(&link, b"recipe").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read(&link).unwrap(), b"recipe");
        let names: Vec<_> = std::fs::read_dir(&out).unwrap().collect();
        assert_eq!(names.len(), 2, "一時ファイルが残っている");
    }

    /// 読み込みは通常のファイルだけを上限まで読む（大きいもの・フォルダ・FIFO は読み切らずに断り、FIFO で止まらない）。
    #[test]
    fn reading_is_limited_to_regular_files() {
        let (_d, _root, out) = dirs();
        let small = out.join("s.kurarecipe");
        std::fs::write(&small, b"abc").unwrap();
        assert_eq!(read_limited(&small, 3).unwrap(), b"abc");
        assert_eq!(
            read_limited(&small, 2).unwrap_err().code,
            AppErrorCode::TooLarge
        );
        assert_eq!(
            read_limited(&out, 10).unwrap_err().code,
            AppErrorCode::Invalid
        );
        let fifo = out.join("f.kurarecipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(
            read_limited(&fifo, 10).unwrap_err().code,
            AppErrorCode::Invalid
        );
    }
}
