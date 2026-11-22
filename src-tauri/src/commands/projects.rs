//! プロジェクトと使った記録（M15 段 2。docs/data-model.md §4「プロジェクトと使った記録（M15）」、ADR-202・204）：登録・一覧・
//! 名前・場所の選び直し・外す・照らし直しを積む・使っているアイテム・アイテムを使っているプロジェクト。ロジックは
//! `kura_core::project`。

use std::path::PathBuf;

use kura_core::project::{self, Engine, ItemUse, Project};

use super::export::ExportPresetDto;
use kura_core::time::now_ms;
use serde::Serialize;
use specta::Type;
use tauri::State;

use super::blocking;
use crate::error::AppError;
use crate::state::AppState;

/// プロジェクトのエンジン。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum EngineDto {
    Godot,
    Unity,
    /// どのエンジンでもないフォルダ。
    Folder,
}

impl From<Engine> for EngineDto {
    fn from(e: Engine) -> Self {
        match e {
            Engine::Godot => Self::Godot,
            Engine::Unity => Self::Unity,
            Engine::Folder => Self::Folder,
        }
    }
}

/// 登録したプロジェクト 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    pub id: i64,
    pub name: String,
    /// 正規化した絶対パス。
    pub root: String,
    pub engine: EngineDto,
    /// 最後の照らしでフォルダが見つからなかった（使った記録は残っている）。
    pub missing: bool,
    /// 最後に照らし終えた時刻（まだなら null）。
    pub scanned_at: Option<i64>,
    /// 照らしたファイルの数。
    pub files: i64,
    /// 使っているアイテムの数。
    pub items: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<Project> for ProjectDto {
    fn from(p: Project) -> Self {
        Self {
            id: p.id,
            name: p.name,
            root: p.root,
            engine: p.engine.into(),
            missing: p.missing,
            scanned_at: p.scanned_at,
            files: p.files,
            items: p.items,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// アイテムを使っているプロジェクト 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemProjectDto {
    pub project_id: i64,
    pub name: String,
    pub missing: bool,
    /// そのアイテムと一致したファイル（ルートからの相対パス）。
    pub paths: Vec<String>,
}

impl From<ItemUse> for ItemProjectDto {
    fn from(u: ItemUse) -> Self {
        Self {
            project_id: u.project_id,
            name: u.name,
            missing: u.missing,
            paths: u.paths,
        }
    }
}

/// 登録・場所の選び直しのあとに照らし直しを積む。積めなくても登録は残すので、誤りは記録だけにする（次に開いたときに積む）。
fn queue(session: &kura_core::library::LibrarySession) {
    if let Err(err) = project::queue_scan(session.jobs()) {
        tracing::warn!(%err, "プロジェクトの照らし直しを積めませんでした");
    }
}

/// 登録したプロジェクトの一覧（並びの順）。
#[tauri::command]
#[specta::specta]
pub async fn projects_list(state: State<'_, AppState>) -> Result<Vec<ProjectDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(project::list)?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// `path`（書き出し先などのフォルダ）を中に含むプロジェクト（なければ null。書き出しのシートが既定を選ぶ。M15 段 6）。
#[tauri::command]
#[specta::specta]
pub async fn project_for_dest(
    state: State<'_, AppState>,
    path: String,
) -> Result<Option<ProjectDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let projects = session.library().db().read(project::list)?;
        Ok(project::containing(&projects, &PathBuf::from(path)).map(|p| p.clone().into()))
    })
    .await
}

/// フォルダをプロジェクトとして登録し、照らし直しを積む（エンジンはフォルダの中身で見分ける。`name` が null ならフォルダの
/// 名前）。ない・フォルダでない・ライブラリの中か外側は Invalid／NotFound、ほかのプロジェクトと重なれば Conflict。
#[tauri::command]
#[specta::specta]
pub async fn project_add(
    state: State<'_, AppState>,
    path: String,
    name: Option<String>,
) -> Result<ProjectDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let library = session.library();
        let existing = library.db().read(project::list)?;
        let (root, engine) = project::prepare_root(
            &PathBuf::from(path),
            library.layout().root(),
            &existing,
            None,
        )?;
        let now = now_ms();
        let id = library
            .db()
            .write(move |tx| project::add(tx, &root, engine, name.as_deref(), now))?;
        queue(&session);
        Ok(library.db().read(move |c| project::get(c, id))?.into())
    })
    .await
}

/// 名前を変える。
#[tauri::command]
#[specta::specta]
pub async fn project_rename(
    state: State<'_, AppState>,
    id: i64,
    name: String,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| project::rename(tx, id, &name, now))?)
    })
    .await
}

/// 場所を選び直し（使った記録は残す）、照らし直しを積む。
#[tauri::command]
#[specta::specta]
pub async fn project_relocate(
    state: State<'_, AppState>,
    id: i64,
    path: String,
) -> Result<ProjectDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let library = session.library();
        let existing = library.db().read(project::list)?;
        let (root, engine) = project::prepare_root(
            &PathBuf::from(path),
            library.layout().root(),
            &existing,
            Some(id),
        )?;
        let now = now_ms();
        library
            .db()
            .write(move |tx| project::relocate(tx, id, &root, engine, now))?;
        queue(&session);
        Ok(library.db().read(move |c| project::get(c, id))?.into())
    })
    .await
}

/// 登録を外す（使った記録も消える。プロジェクトのフォルダには触れない）。
#[tauri::command]
#[specta::specta]
pub async fn project_remove(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| project::remove(tx, id))?)
    })
    .await
}

/// 登録したプロジェクト全部の照らし直しを積む（待っているものがあれば積まず null）。
#[tauri::command]
#[specta::specta]
pub async fn projects_scan(state: State<'_, AppState>) -> Result<Option<i64>, AppError> {
    let session = state.session()?;
    blocking(move || Ok(project::queue_scan(session.jobs())?)).await
}

/// プロジェクトで使っているアイテム（ID の順。クレジットの一覧の対象）。
#[tauri::command]
#[specta::specta]
pub async fn project_items(state: State<'_, AppState>, id: i64) -> Result<Vec<i64>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(move |c| project::used_items(c, id))?)
    })
    .await
}

/// アイテムを使っているプロジェクト（並びの順。一致したファイルのパスつき）。
#[tauri::command]
#[specta::specta]
pub async fn item_projects(
    state: State<'_, AppState>,
    item: i64,
) -> Result<Vec<ItemProjectDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(move |c| project::item_projects(c, item))?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// プロジェクトの持ち出しの既定（命名・形式・JSON。まだ決めていなければエンジンごとの既定）。
#[tauri::command]
#[specta::specta]
pub async fn project_settings(
    state: State<'_, AppState>,
    id: i64,
) -> Result<ExportPresetDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(move |c| project::settings(c, id))?
            .into())
    })
    .await
}

/// プロジェクトの持ち出しの既定を書く（名前のテンプレートと形式を確かめる）。
#[tauri::command]
#[specta::specta]
pub async fn project_settings_set(
    state: State<'_, AppState>,
    id: i64,
    preset: ExportPresetDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    let preset: kura_core::export::ExportPreset = preset.into();
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| project::set_settings(tx, id, &preset, now))?)
    })
    .await
}
