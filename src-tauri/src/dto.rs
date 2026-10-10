//! フロントとやり取りする型。kura-core の型を TypeScript に出せる形（camelCase、specta）にする。

use serde::{Deserialize, Serialize};
use specta::Type;

use kura_core::folders::FolderNode;
use kura_core::import::{DuplicatePolicy, ImportOptions, ImportPreview, ScanRoot};
use kura_core::jobs::{JobEvent, JobFilter, JobKind, JobRecord, JobState};
use kura_core::library::{IntegrityReport, LibraryInfo, LocationCheck, LocationStatus};

/// ライブラリの概要。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryInfoDto {
    pub name: String,
    pub path: String,
    pub item_count: u64,
    pub integrity: Option<IntegrityDto>,
}

impl From<LibraryInfo> for LibraryInfoDto {
    fn from(info: LibraryInfo) -> Self {
        Self {
            name: info.name,
            path: info.path.to_string_lossy().into_owned(),
            item_count: info.item_count,
            integrity: info.integrity.map(Into::into),
        }
    }
}

/// 起動時の整合性チェックの結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityDto {
    pub staging_removed: u64,
    pub missing_files: u64,
    pub orphan_files: u64,
}

impl From<IntegrityReport> for IntegrityDto {
    fn from(r: IntegrityReport) -> Self {
        Self {
            staging_removed: r.staging_removed,
            missing_files: r.missing_files,
            orphan_files: r.orphan_files,
        }
    }
}

/// 前回のライブラリを開いた結果。`info` が空で `lastPath` があれば、前回の場所が見つからなかった。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRestoreDto {
    pub info: Option<LibraryInfoDto>,
    pub last_path: Option<String>,
}

/// 場所の状態。
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum LocationStatusDto {
    New,
    ExistingLibrary,
    Occupied,
}

/// ライブラリを置く場所の確認結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LocationCheckDto {
    pub path: String,
    pub status: LocationStatusDto,
    pub sync_service: Option<String>,
}

impl From<LocationCheck> for LocationCheckDto {
    fn from(c: LocationCheck) -> Self {
        Self {
            path: c.path.to_string_lossy().into_owned(),
            status: match c.status {
                LocationStatus::New => LocationStatusDto::New,
                LocationStatus::ExistingLibrary => LocationStatusDto::ExistingLibrary,
                LocationStatus::Occupied => LocationStatusDto::Occupied,
            },
            sync_service: c.sync_service,
        }
    }
}

/// ドロップされたもの。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScanRootDto {
    pub name: String,
    pub is_dir: bool,
}

impl From<ScanRoot> for ScanRootDto {
    fn from(r: ScanRoot) -> Self {
        Self {
            name: r.name,
            is_dir: r.is_dir,
        }
    }
}

/// 取り込み前の確認。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewDto {
    pub total: u64,
    pub images: u64,
    pub videos: u64,
    pub audios: u64,
    pub total_bytes: u64,
    pub duplicates: u64,
    pub skipped: u64,
    pub roots: Vec<ScanRootDto>,
    pub subfolders: u64,
    pub sample_dirs: Vec<String>,
    pub errors: u64,
}

impl From<ImportPreview> for ImportPreviewDto {
    fn from(p: ImportPreview) -> Self {
        Self {
            total: p.total,
            images: p.images,
            videos: p.videos,
            audios: p.audios,
            total_bytes: p.total_bytes,
            duplicates: p.duplicates,
            skipped: p.skipped,
            roots: p.roots.into_iter().map(Into::into).collect(),
            subfolders: p.subfolders,
            sample_dirs: p.sample_dirs,
            errors: p.errors,
        }
    }
}

/// 重複の扱い。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DuplicatePolicyDto {
    #[default]
    AddMemberships,
    Ignore,
}

/// 取り込みオプション。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportOptionsDto {
    pub dest_folder_id: Option<i64>,
    pub reproduce_folders: bool,
    pub folder_names_as_tags: bool,
    pub units_per_folder: bool,
    pub tags: Vec<String>,
    pub duplicates: DuplicatePolicyDto,
}

impl From<ImportOptionsDto> for ImportOptions {
    fn from(o: ImportOptionsDto) -> Self {
        Self {
            dest_folder_id: o.dest_folder_id,
            reproduce_folders: o.reproduce_folders,
            folder_names_as_tags: o.folder_names_as_tags,
            units_per_folder: o.units_per_folder,
            tags: o.tags,
            duplicates: match o.duplicates {
                DuplicatePolicyDto::AddMemberships => DuplicatePolicy::AddMemberships,
                DuplicatePolicyDto::Ignore => DuplicatePolicy::Ignore,
            },
        }
    }
}

/// 取り込みの結果（`JobProgress.resultJson` を読むときの型）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportResultDto {
    pub cursor: u64,
    pub total: u64,
    pub added: u64,
    pub duplicates: u64,
    pub skipped: u64,
    pub failed: u64,
    pub failures: Vec<ImportFailureDto>,
}

/// 取り込みに失敗したファイル。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ImportFailureDto {
    pub path: String,
    pub reason: String,
}

/// ジョブの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum JobKindDto {
    Import,
    Thumbnail,
    Proxy,
    Download,
    Capture,
}

impl From<JobKind> for JobKindDto {
    fn from(k: JobKind) -> Self {
        match k {
            JobKind::Import => Self::Import,
            JobKind::Thumbnail => Self::Thumbnail,
            JobKind::Proxy => Self::Proxy,
            JobKind::Download => Self::Download,
            JobKind::Capture => Self::Capture,
        }
    }
}

impl From<JobKindDto> for JobKind {
    fn from(k: JobKindDto) -> Self {
        match k {
            JobKindDto::Import => Self::Import,
            JobKindDto::Thumbnail => Self::Thumbnail,
            JobKindDto::Proxy => Self::Proxy,
            JobKindDto::Download => Self::Download,
            JobKindDto::Capture => Self::Capture,
        }
    }
}

/// ジョブの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum JobStateDto {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

impl From<JobState> for JobStateDto {
    fn from(s: JobState) -> Self {
        match s {
            JobState::Queued => Self::Queued,
            JobState::Running => Self::Running,
            JobState::Paused => Self::Paused,
            JobState::Done => Self::Done,
            JobState::Failed => Self::Failed,
            JobState::Canceled => Self::Canceled,
        }
    }
}

impl From<JobStateDto> for JobState {
    fn from(s: JobStateDto) -> Self {
        match s {
            JobStateDto::Queued => Self::Queued,
            JobStateDto::Running => Self::Running,
            JobStateDto::Paused => Self::Paused,
            JobStateDto::Done => Self::Done,
            JobStateDto::Failed => Self::Failed,
            JobStateDto::Canceled => Self::Canceled,
        }
    }
}

/// ジョブ 1 件。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobDto {
    pub id: i64,
    pub kind: JobKindDto,
    pub state: JobStateDto,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub error_code: Option<String>,
    pub result_json: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl From<JobRecord> for JobDto {
    fn from(r: JobRecord) -> Self {
        Self {
            id: r.id,
            kind: r.kind.into(),
            state: r.state.into(),
            progress: r.progress,
            message: r.message,
            error_code: r.error_code,
            result_json: r.result_json,
            created_at: r.created_at,
            started_at: r.started_at,
            finished_at: r.finished_at,
        }
    }
}

/// ジョブ一覧の絞り込み。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct JobFilterDto {
    pub kinds: Vec<JobKindDto>,
    pub states: Vec<JobStateDto>,
    pub limit: Option<u32>,
}

impl From<JobFilterDto> for JobFilter {
    fn from(f: JobFilterDto) -> Self {
        Self {
            kinds: f.kinds.into_iter().map(Into::into).collect(),
            states: f.states.into_iter().map(Into::into).collect(),
            limit: f.limit,
        }
    }
}

/// ジョブの進捗（イベント `job://progress`）。
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "job://progress")]
pub struct JobProgress {
    pub id: i64,
    pub kind: JobKindDto,
    pub state: JobStateDto,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub result_json: Option<String>,
}

impl From<JobEvent> for JobProgress {
    fn from(e: JobEvent) -> Self {
        Self {
            id: e.id,
            kind: e.kind.into(),
            state: e.state.into(),
            progress: e.progress,
            message: e.message,
            result_json: e.result_json,
        }
    }
}

/// 仮想フォルダ 1 つ。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FolderNodeDto {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub pos: f64,
}

impl From<FolderNode> for FolderNodeDto {
    fn from(n: FolderNode) -> Self {
        Self {
            id: n.id,
            parent_id: n.parent_id,
            name: n.name,
            color: n.color,
            pos: n.pos,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_options_accept_partial_json() {
        let dto: ImportOptionsDto = serde_json::from_str(
            r#"{"reproduceFolders":true,"duplicates":"ignore","tags":["旅行"]}"#,
        )
        .unwrap();
        let options: ImportOptions = dto.into();
        assert!(options.reproduce_folders);
        assert_eq!(options.duplicates, DuplicatePolicy::Ignore);
        assert_eq!(options.tags, vec!["旅行".to_string()]);
    }

    #[test]
    fn progress_serializes_in_camel_case() {
        let event = JobProgress::from(JobEvent {
            id: 3,
            kind: JobKind::Import,
            state: JobState::Running,
            progress: Some(0.5),
            message: Some("1 / 2 項目".into()),
            result_json: None,
        });
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["kind"], "import");
        assert_eq!(json["state"], "running");
        assert_eq!(json["resultJson"], serde_json::Value::Null);
    }
}
