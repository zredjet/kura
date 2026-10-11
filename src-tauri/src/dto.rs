//! フロントとやり取りする型。kura-core の型を TypeScript に出せる形（camelCase、specta）にする。

use serde::{Deserialize, Serialize};
use specta::Type;

use kura_core::entries::{EntryPatch, EntryUpdate};
use kura_core::folders::{FolderNode, FolderPatch};
use kura_core::import::{DuplicatePolicy, ImportOptions, ImportPreview, ScanRoot};
use kura_core::items::{
    FolderRef, ItemDetail, ItemPatch, ItemSort, ItemSummary, KindCounts, LibraryCounts,
    SelectionInfo, SortKey, TagRef, UnitRef,
};
use kura_core::jobs::{JobEvent, JobFilter, JobKind, JobRecord, JobState};
use kura_core::library::{
    IntegrityReport, LibraryInfo, LocationCheck, LocationStatus, TrashEmptied,
};
use kura_core::playback::{Container, PlayReport, SupportEntry};
use kura_core::playlist::add::AddSource;
use kura_core::playlist::play::{PlayQueue, PlayRequest, QueueInfo};
use kura_core::playlist::smart::{RuleMatch, SmartRule};
use kura_core::playlist::store::{EntryRow, PlayState, Playlist, PlaylistPatch};
use kura_core::playlist::{
    Entry, EntryTarget, Granularity, LimitKind, PlaylistKind, RepeatMode, UnitMatch,
};
use kura_core::query::QueryCheck;
use kura_core::queue::UnitKey;
use kura_mac::ExternalPlayer;

use crate::config::PlayerPrefs;
use kura_core::query::suggest::{Suggestion, SuggestionKind};
use kura_core::tags::{TagDetail, TagPatch, TagSuggestion};
use kura_core::unit::{Cover, OrderMode, UnitDetail, UnitKind, UnitPatch, UnitSummary};

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
    pub file_info_tags: bool,
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
            file_info_tags: o.file_info_tags,
            tags: o.tags,
            duplicates: match o.duplicates {
                DuplicatePolicyDto::AddMemberships => DuplicatePolicy::AddMemberships,
                DuplicatePolicyDto::Ignore => DuplicatePolicy::Ignore,
            },
        }
    }
}

/// 書き出しの結果（`JobProgress.resultJson` を読むときの型。ADR-120）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportResultDto {
    /// 書き出し先のフォルダ。
    pub dest: String,
    pub total: u64,
    pub exported: u64,
    pub failed: u64,
    /// 書いた JSON の名前（終わったとき）。
    pub manifest: Option<String>,
}

/// ライブラリの容量（サイドバーの下・設定画面）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryUsageDto {
    /// ライブラリのファイルの合計（ゴミ箱を含む）。
    pub library_bytes: u64,
    /// ライブラリのあるボリュームの容量と空き（読めなければ空）。
    pub volume_total_bytes: Option<u64>,
    pub volume_available_bytes: Option<u64>,
}

/// 取得済みの記録の状態（設定画面。ADR-95）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveStatusDto {
    pub tool: ToolDto,
    /// 記録の件数（読めなければ空）。
    pub entries: Option<u64>,
    pub size_bytes: u64,
}

impl From<kura_core::download::archive::ArchiveStatus> for ArchiveStatusDto {
    fn from(a: kura_core::download::archive::ArchiveStatus) -> Self {
        Self {
            tool: a.tool.into(),
            entries: a.entries,
            size_bytes: a.size_bytes,
        }
    }
}

/// DB のバックアップ 1 つ（FR-SYS-03。ADR-121）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BackupDto {
    pub name: String,
    pub path: String,
    pub created_at: i64,
    pub size_bytes: u64,
}

impl From<kura_core::library::backup::BackupInfo> for BackupDto {
    fn from(b: kura_core::library::backup::BackupInfo) -> Self {
        Self {
            name: b.name,
            path: b.path.display().to_string(),
            created_at: b.created_at,
            size_bytes: b.size_bytes,
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
    Export,
}

impl From<JobKind> for JobKindDto {
    fn from(k: JobKind) -> Self {
        match k {
            JobKind::Import => Self::Import,
            JobKind::Thumbnail => Self::Thumbnail,
            JobKind::Proxy => Self::Proxy,
            JobKind::Download => Self::Download,
            JobKind::Capture => Self::Capture,
            JobKind::Export => Self::Export,
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
            JobKindDto::Export => Self::Export,
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

/// 一覧の 1 件（グリッドのタイル）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummaryDto {
    pub id: i64,
    /// BLAKE3 の 16 進（`kura://localhost/thumb/{hash}/s` などに使う）。
    pub hash: String,
    pub ext: String,
    /// `image` / `video` / `audio`。
    pub kind: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub size_bytes: i64,
    pub title: String,
    pub rating: i64,
    pub source_site: Option<String>,
    pub imported_at: i64,
    pub captured_at: Option<i64>,
    /// `pending` / `ready` / `failed`。`ready` のときだけサムネイルがある。
    pub thumb_state: String,
    pub proxy_state: String,
    pub last_played_at: Option<i64>,
}

impl From<ItemSummary> for ItemSummaryDto {
    fn from(i: ItemSummary) -> Self {
        Self {
            id: i.id,
            hash: i.hash,
            ext: i.ext,
            kind: i.kind,
            width: i.width,
            height: i.height,
            duration_ms: i.duration_ms,
            size_bytes: i.size_bytes,
            title: i.title,
            rating: i.rating,
            source_site: i.source_site,
            imported_at: i.imported_at,
            captured_at: i.captured_at,
            thumb_state: i.thumb_state,
            proxy_state: i.proxy_state,
            last_played_at: i.last_played_at,
        }
    }
}

/// タグ。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TagRefDto {
    pub id: i64,
    pub namespace: String,
    pub name: String,
    /// 色のトークン名（`blue` / `violet` / `amber` / `teal`）。
    pub color: Option<String>,
}

impl From<TagRef> for TagRefDto {
    fn from(t: TagRef) -> Self {
        Self {
            id: t.id,
            namespace: t.namespace,
            name: t.name,
            color: t.color,
        }
    }
}

/// 所属する仮想フォルダ（いちばん上からのパス）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FolderRefDto {
    pub id: i64,
    pub path: Vec<String>,
}

impl From<FolderRef> for FolderRefDto {
    fn from(f: FolderRef) -> Self {
        Self {
            id: f.id,
            path: f.path,
        }
    }
}

/// 所属するユニット。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UnitRefDto {
    pub id: i64,
    pub name: String,
    pub kind: UnitKindDto,
}

impl From<UnitRef> for UnitRefDto {
    fn from(u: UnitRef) -> Self {
        Self {
            id: u.id,
            name: u.name,
            kind: u.kind.into(),
        }
    }
}

/// インスペクタ用の詳細。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemDetailDto {
    pub item: ItemSummaryDto,
    pub mime: Option<String>,
    pub codec_info: Option<String>,
    pub original_name: String,
    pub original_path: Option<String>,
    pub source_url: Option<String>,
    pub note: Option<String>,
    pub trashed_at: Option<i64>,
    pub tags: Vec<TagRefDto>,
    pub folders: Vec<FolderRefDto>,
    pub units: Vec<UnitRefDto>,
}

impl From<ItemDetail> for ItemDetailDto {
    fn from(d: ItemDetail) -> Self {
        Self {
            item: d.summary.into(),
            mime: d.mime,
            codec_info: d.codec_info,
            original_name: d.original_name,
            original_path: d.original_path,
            source_url: d.source_url,
            note: d.note,
            trashed_at: d.trashed_at,
            tags: d.tags.into_iter().map(Into::into).collect(),
            folders: d.folders.into_iter().map(Into::into).collect(),
            units: d.units.into_iter().map(Into::into).collect(),
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
    /// 件数（子孫フォルダの分を含む。`folders_tree(withCounts)` のときだけ）。
    pub count: Option<i64>,
    /// このフォルダを参照する folder ユニット。
    pub unit_id: Option<i64>,
}

impl From<FolderNode> for FolderNodeDto {
    fn from(n: FolderNode) -> Self {
        Self {
            id: n.id,
            parent_id: n.parent_id,
            name: n.name,
            color: n.color,
            pos: n.pos,
            count: None,
            unit_id: n.unit_id,
        }
    }
}

/// 空文字を「消す」として読む（色・メモなど）。
fn clearable(value: Option<String>) -> Option<Option<String>> {
    value.map(|v| (!v.trim().is_empty()).then_some(v))
}

/// フォルダの変更（省略した項目は変えない。`color` の空文字は色を外す）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderPatchDto {
    pub name: Option<String>,
    pub color: Option<String>,
}

impl From<FolderPatchDto> for FolderPatch {
    fn from(p: FolderPatchDto) -> Self {
        Self {
            name: p.name,
            color: clearable(p.color),
        }
    }
}

/// サイドバーの「ライブラリ」の件数。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCountsDto {
    pub all: i64,
    pub recent: i64,
    pub unsorted: i64,
    pub untagged: i64,
    pub trash: i64,
    /// キャプチャで取り込んだもの。
    pub captures: i64,
}

impl From<LibraryCounts> for LibraryCountsDto {
    fn from(c: LibraryCounts) -> Self {
        Self {
            all: c.all,
            recent: c.recent,
            unsorted: c.unsorted,
            untagged: c.untagged,
            trash: c.trash,
            captures: c.captures,
        }
    }
}

/// 一覧の種類（`items_query_ids` の引数。生バイトのコマンドなので TS の型は `src/ipc/items.ts` に書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ItemViewDto {
    All,
    Recent,
    Unsorted,
    Untagged,
    Random { seed: u64 },
    Trash,
    Folder { id: i64 },
    Unit { id: i64 },
    Smart { id: i64 },
}

/// 並び順（`items_query_ids` の引数、プレイリストの並び順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemSortDto {
    pub key: SortKeyDto,
    pub desc: bool,
}

/// 並べる基準。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SortKeyDto {
    Imported,
    Captured,
    Name,
    Rating,
    Duration,
    Size,
}

impl From<ItemSort> for ItemSortDto {
    fn from(s: ItemSort) -> Self {
        Self {
            key: match s.key {
                SortKey::Imported => SortKeyDto::Imported,
                SortKey::Captured => SortKeyDto::Captured,
                SortKey::Name => SortKeyDto::Name,
                SortKey::Rating => SortKeyDto::Rating,
                SortKey::Duration => SortKeyDto::Duration,
                SortKey::Size => SortKeyDto::Size,
            },
            desc: s.desc,
        }
    }
}

impl From<ItemSortDto> for ItemSort {
    fn from(s: ItemSortDto) -> Self {
        Self {
            key: match s.key {
                SortKeyDto::Imported => SortKey::Imported,
                SortKeyDto::Captured => SortKey::Captured,
                SortKeyDto::Name => SortKey::Name,
                SortKeyDto::Rating => SortKey::Rating,
                SortKeyDto::Duration => SortKey::Duration,
                SortKeyDto::Size => SortKey::Size,
            },
            desc: s.desc,
        }
    }
}

/// 解析のエラー（位置は入力の UTF-16 の文字位置）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ParseErrorDto {
    pub start: u32,
    pub end: u32,
    pub message: String,
}

/// チップに分けた条件。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ClauseDto {
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub negated: bool,
    /// フィールドの条件なら英語名（`type` など）。
    pub field: Option<String>,
}

/// 種類ごとの件数。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KindCountsDto {
    pub image: i64,
    pub video: i64,
    pub audio: i64,
    pub total: i64,
}

impl From<KindCounts> for KindCountsDto {
    fn from(c: KindCounts) -> Self {
        Self {
            image: c.image,
            video: c.video,
            audio: c.audio,
            total: c.total(),
        }
    }
}

/// パレットに出す解析の結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct QueryCheckDto {
    pub error: Option<ParseErrorDto>,
    pub clauses: Vec<ClauseDto>,
    pub counts: Option<KindCountsDto>,
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl From<QueryCheck> for QueryCheckDto {
    fn from(c: QueryCheck) -> Self {
        Self {
            error: c.error.map(|e| ParseErrorDto {
                start: to_u32(e.start),
                end: to_u32(e.end),
                message: e.message,
            }),
            clauses: c
                .clauses
                .into_iter()
                .map(|c| ClauseDto {
                    start: to_u32(c.start),
                    end: to_u32(c.end),
                    text: c.text,
                    negated: c.negated,
                    field: c.field.map(str::to_string),
                })
                .collect(),
            counts: c.counts.map(Into::into),
        }
    }
}

/// 候補の種類。
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionKindDto {
    Tag,
    Field,
    Value,
    Saved,
}

/// パレットの候補。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionDto {
    pub kind: SuggestionKindDto,
    /// 入力中の語と置き換える文字列（保存済みの検索は、その条件）。
    pub insert: String,
    /// 前に薄く出す部分（名前空間・フィールド名）。
    pub prefix: String,
    pub label: String,
    pub detail: Option<String>,
    pub count: Option<i64>,
    /// 保存済みの検索の ID。
    pub id: Option<i64>,
}

impl From<Suggestion> for SuggestionDto {
    fn from(s: Suggestion) -> Self {
        Self {
            kind: match s.kind {
                SuggestionKind::Tag => SuggestionKindDto::Tag,
                SuggestionKind::Field => SuggestionKindDto::Field,
                SuggestionKind::Value => SuggestionKindDto::Value,
                SuggestionKind::Saved => SuggestionKindDto::Saved,
            },
            insert: s.insert,
            prefix: s.prefix,
            label: s.label,
            detail: s.detail,
            count: s.count,
            id: s.id,
        }
    }
}

/// スマートフォルダ。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SmartFolderDto {
    pub id: i64,
    pub name: String,
    pub query: String,
    /// 一致する件数（`withCounts` のときだけ）。
    pub count: Option<i64>,
}

/// アイテムの変更（省略した項目は変えない。`note` の空文字はメモを消す）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct ItemPatchDto {
    pub title: Option<String>,
    pub rating: Option<i64>,
    pub note: Option<String>,
}

impl From<ItemPatchDto> for ItemPatch {
    fn from(p: ItemPatchDto) -> Self {
        Self {
            title: p.title,
            rating: p.rating,
            note: clearable(p.note),
        }
    }
}

/// 選択（アイテムとユニット）にまとめて付ける評価・メモ・タグ（`note` の空文字はメモを消す）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct EntryPatchDto {
    pub rating: Option<i64>,
    pub note: Option<String>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<i64>,
}

impl From<EntryPatchDto> for EntryPatch {
    fn from(p: EntryPatchDto) -> Self {
        Self {
            rating: p.rating,
            note: clearable(p.note),
            add_tags: p.add_tags,
            remove_tags: p.remove_tags,
        }
    }
}

/// まとめて変えた結果：変えたアイテムと、そのうちユニットのメンバーの数。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EntriesUpdatedDto {
    pub item_ids: Vec<i64>,
    pub members: u32,
}

impl From<EntryUpdate> for EntriesUpdatedDto {
    fn from(u: EntryUpdate) -> Self {
        Self {
            item_ids: u.item_ids,
            members: u32::try_from(u.members).unwrap_or(u32::MAX),
        }
    }
}

/// タグと、付いている件数。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TagCountDto {
    pub tag: TagRefDto,
    pub count: u32,
}

/// フォルダと、入っている件数。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FolderCountDto {
    pub folder: FolderRefDto,
    pub count: u32,
}

/// 複数選択のインスペクタに出す集計。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SelectionInfoDto {
    pub count: u32,
    /// 全件の評価が同じならその値。
    pub rating: Option<i64>,
    /// 全件のメモが同じか。
    pub same_note: bool,
    /// 全件が同じときのメモ。
    pub note: Option<String>,
    pub tags: Vec<TagCountDto>,
    pub folders: Vec<FolderCountDto>,
}

impl From<SelectionInfo> for SelectionInfoDto {
    fn from(s: SelectionInfo) -> Self {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Self {
            count: count(s.count),
            rating: s.rating,
            same_note: s.note.is_some(),
            note: s.note.flatten(),
            tags: s
                .tags
                .into_iter()
                .map(|(tag, n)| TagCountDto {
                    tag: tag.into(),
                    count: count(n),
                })
                .collect(),
            folders: s
                .folders
                .into_iter()
                .map(|(folder, n)| FolderCountDto {
                    folder: folder.into(),
                    count: count(n),
                })
                .collect(),
        }
    }
}

/// ゴミ箱を空にした結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashEmptiedDto {
    pub items: u64,
    pub bytes: u64,
    pub failed: u64,
}

impl From<TrashEmptied> for TrashEmptiedDto {
    fn from(t: TrashEmptied) -> Self {
        Self {
            items: t.items,
            bytes: t.bytes,
            failed: t.failed,
        }
    }
}

/// タグの候補。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TagSuggestionDto {
    pub tag: TagRefDto,
    pub count: i64,
    /// 別名で一致したときの別名。
    pub alias: Option<String>,
}

impl From<TagSuggestion> for TagSuggestionDto {
    fn from(s: TagSuggestion) -> Self {
        Self {
            tag: s.tag.into(),
            count: s.count,
            alias: s.alias,
        }
    }
}

/// タグの一覧の 1 行（タグの一覧の画面）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TagListEntryDto {
    pub tag: TagRefDto,
    /// 付いているアイテムの数（ゴミ箱を除く）。
    pub items: i64,
    /// そのうち、ダウンロード・キャプチャで自動で付いたもの。
    pub auto: i64,
    /// 付いているユニットの数。
    pub units: i64,
    /// このタグで絞り込む検索の条件（「作者:KR」など）。
    pub clause: String,
}

impl From<kura_core::tags::TagListEntry> for TagListEntryDto {
    fn from(e: kura_core::tags::TagListEntry) -> Self {
        let clause = kura_core::query::suggest::tag_clause(&e.tag.namespace, &e.tag.name);
        Self {
            tag: e.tag.into(),
            items: e.items,
            auto: e.auto,
            units: e.units,
            clause,
        }
    }
}

/// タグの編集画面の内容。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TagDetailDto {
    pub tag: TagRefDto,
    pub count: i64,
    pub aliases: Vec<String>,
    pub parents: Vec<TagRefDto>,
    pub children: Vec<TagRefDto>,
}

impl From<TagDetail> for TagDetailDto {
    fn from(d: TagDetail) -> Self {
        Self {
            tag: d.tag.into(),
            count: d.count,
            aliases: d.aliases,
            parents: d.parents.into_iter().map(Into::into).collect(),
            children: d.children.into_iter().map(Into::into).collect(),
        }
    }
}

/// タグの変更（省略した項目は変えない。`color` の空文字は色を外す。`aliases`・`parents` は置き換え）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct TagPatchDto {
    pub namespace: Option<String>,
    pub name: Option<String>,
    pub color: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub parents: Option<Vec<i64>>,
}

impl From<TagPatchDto> for TagPatch {
    fn from(p: TagPatchDto) -> Self {
        Self {
            namespace: p.namespace,
            name: p.name,
            color: clearable(p.color),
            aliases: p.aliases,
            parents: p.parents,
        }
    }
}

/// ユニットの種類（`set` = ファイル群、`folder` = フォルダ群）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum UnitKindDto {
    Set,
    Folder,
}

impl From<UnitKind> for UnitKindDto {
    fn from(k: UnitKind) -> Self {
        match k {
            UnitKind::Set => Self::Set,
            UnitKind::Folder => Self::Folder,
        }
    }
}

impl From<UnitKindDto> for UnitKind {
    fn from(k: UnitKindDto) -> Self {
        match k {
            UnitKindDto::Set => Self::Set,
            UnitKindDto::Folder => Self::Folder,
        }
    }
}

/// ユニットのメンバーの並び順。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum OrderModeDto {
    Manual,
    Name,
    Imported,
    Captured,
    Duration,
}

impl From<OrderMode> for OrderModeDto {
    fn from(m: OrderMode) -> Self {
        match m {
            OrderMode::Manual => Self::Manual,
            OrderMode::Name => Self::Name,
            OrderMode::Imported => Self::Imported,
            OrderMode::Captured => Self::Captured,
            OrderMode::Duration => Self::Duration,
        }
    }
}

impl From<OrderModeDto> for OrderMode {
    fn from(m: OrderModeDto) -> Self {
        match m {
            OrderModeDto::Manual => Self::Manual,
            OrderModeDto::Name => Self::Name,
            OrderModeDto::Imported => Self::Imported,
            OrderModeDto::Captured => Self::Captured,
            OrderModeDto::Duration => Self::Duration,
        }
    }
}

/// ユニットの表紙。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CoverDto {
    pub item_id: i64,
    pub hash: String,
    pub kind: String,
    pub thumb_state: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

impl From<Cover> for CoverDto {
    fn from(c: Cover) -> Self {
        Self {
            item_id: c.item_id,
            hash: c.hash,
            kind: c.kind,
            thumb_state: c.thumb_state,
            width: c.width,
            height: c.height,
        }
    }
}

/// グリッドのタイルに出すユニットの要約。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UnitSummaryDto {
    pub id: i64,
    pub kind: UnitKindDto,
    pub name: String,
    pub cover: Option<CoverDto>,
    pub count: i64,
    /// 音楽・動画は長さ、画像は表示時間の合計（ミリ秒）。
    pub duration_ms: i64,
    /// メンバーの種類（`image` / `video` / `audio`）。
    pub kinds: Vec<String>,
    /// 集計した評価（ユニットの評価、なければメンバーの平均）。
    pub rating: i64,
    /// ユニット自体の評価（0 は未評価）。
    pub own_rating: i64,
    /// `user` / `import` / `download`。
    pub origin: String,
    pub created_at: i64,
    pub last_played_at: Option<i64>,
}

impl From<UnitSummary> for UnitSummaryDto {
    fn from(u: UnitSummary) -> Self {
        Self {
            id: u.id,
            kind: u.kind.into(),
            name: u.name,
            cover: u.cover.map(Into::into),
            count: i64::try_from(u.stats.count).unwrap_or(i64::MAX),
            duration_ms: u.stats.duration_ms,
            kinds: u.stats.kinds.into_iter().map(str::to_string).collect(),
            rating: u.stats.rating,
            own_rating: u.own_rating,
            origin: u.origin,
            created_at: u.created_at,
            last_played_at: u.last_played_at,
        }
    }
}

/// folder ユニットの参照フォルダ。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UnitFolderDto {
    pub folder_id: i64,
    /// いちばん上からのパス（出力のときだけ）。
    #[serde(default)]
    pub path: Vec<String>,
    pub recursive: bool,
}

/// ユニットのインスペクタ用の詳細。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct UnitDetailDto {
    pub unit: UnitSummaryDto,
    pub order_mode: OrderModeDto,
    /// 並びを逆にしているか。
    pub order_reverse: bool,
    pub inner_shuffle: bool,
    /// 画像の表示時間（なしはプレイリストや既定の値）。
    pub image_duration_ms: Option<i64>,
    pub note: Option<String>,
    pub source_url: Option<String>,
    pub tags: Vec<TagRefDto>,
    pub folders: Vec<UnitFolderDto>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

impl From<UnitDetail> for UnitDetailDto {
    fn from(d: UnitDetail) -> Self {
        Self {
            unit: d.summary.into(),
            order_mode: d.order_mode.into(),
            order_reverse: d.order_reverse,
            inner_shuffle: d.inner_shuffle,
            image_duration_ms: d.image_duration_ms,
            note: d.note,
            source_url: d.source_url,
            tags: d.tags.into_iter().map(Into::into).collect(),
            folders: d
                .folders
                .into_iter()
                .map(|f| UnitFolderDto {
                    folder_id: f.folder_id,
                    path: f.path,
                    recursive: f.recursive,
                })
                .collect(),
            play_count: d.play_count,
            last_played_at: d.last_played_at,
        }
    }
}

/// ユニットの変更（省略した項目は変えない。`imageDurationMs` の 0 は既定に戻す）。
/// 評価・メモ・タグはメンバーにも付けるので `entries_update` で変える（ADR-129）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct UnitPatchDto {
    pub name: Option<String>,
    pub cover_item_id: Option<i64>,
    pub order_mode: Option<OrderModeDto>,
    pub order_reverse: Option<bool>,
    pub inner_shuffle: Option<bool>,
    pub image_duration_ms: Option<i64>,
}

impl From<UnitPatchDto> for UnitPatch {
    fn from(p: UnitPatchDto) -> Self {
        Self {
            name: p.name,
            cover_item_id: p.cover_item_id.map(Some),
            order_mode: p.order_mode.map(Into::into),
            order_reverse: p.order_reverse,
            inner_shuffle: p.inner_shuffle,
            image_duration_ms: p.image_duration_ms.map(|ms| (ms != 0).then_some(ms)),
            ..UnitPatch::default()
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

// ===== 再生（M5） =====

/// 入れ物（`kura_core::playback::Container`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum ContainerDto {
    Mp4,
    Webm,
    Ogg,
    Mp3,
    Flac,
    Wav,
    Aiff,
    Aac,
    Other,
}

impl From<ContainerDto> for Container {
    fn from(c: ContainerDto) -> Self {
        match c {
            ContainerDto::Mp4 => Self::Mp4,
            ContainerDto::Webm => Self::Webm,
            ContainerDto::Ogg => Self::Ogg,
            ContainerDto::Mp3 => Self::Mp3,
            ContainerDto::Flac => Self::Flac,
            ContainerDto::Wav => Self::Wav,
            ContainerDto::Aiff => Self::Aiff,
            ContainerDto::Aac => Self::Aac,
            ContainerDto::Other => Self::Other,
        }
    }
}

/// 再生可否の表の 1 行（フロントの `canPlayType` の結果）。
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SupportEntryDto {
    pub container: ContainerDto,
    /// ffprobe のコーデック名。
    pub codec: String,
    pub playable: bool,
}

impl From<SupportEntryDto> for SupportEntry {
    fn from(e: SupportEntryDto) -> Self {
        Self {
            container: e.container.into(),
            codec: e.codec,
            playable: e.playable,
        }
    }
}

/// 1 スロット分の再生の報告（docs/playlists.md §6.4）。
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlayReportDto {
    pub item_id: i64,
    pub unit_id: Option<i64>,
    pub unit_start: bool,
    pub played_ms: i64,
    pub completed: bool,
    pub duration_ms: Option<i64>,
    /// プレイリストから再生したとき。
    pub playlist_id: Option<i64>,
}

impl From<PlayReportDto> for PlayReport {
    fn from(r: PlayReportDto) -> Self {
        Self {
            item_id: r.item_id,
            unit_id: r.unit_id,
            unit_start: r.unit_start,
            playlist_id: r.playlist_id,
            played_ms: r.played_ms,
            completed: r.completed,
            duration_ms: r.duration_ms,
        }
    }
}

/// 再生の設定。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlayerPrefsDto {
    /// 音量（0〜100）。
    pub volume: u32,
    pub bgm_through_images: bool,
    pub bgm_loop: bool,
    pub image_duration_ms: i64,
}

impl From<PlayerPrefs> for PlayerPrefsDto {
    fn from(p: PlayerPrefs) -> Self {
        Self {
            volume: p.volume,
            bgm_through_images: p.bgm_through_images,
            bgm_loop: p.bgm_loop,
            image_duration_ms: p.image_duration_ms,
        }
    }
}

impl From<PlayerPrefsDto> for PlayerPrefs {
    fn from(p: PlayerPrefsDto) -> Self {
        Self {
            volume: p.volume,
            bgm_through_images: p.bgm_through_images,
            bgm_loop: p.bgm_loop,
            image_duration_ms: p.image_duration_ms,
        }
    }
}

/// 外部プレイヤー。
#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ExternalPlayerDto {
    Mpv,
    QuickTime,
}

impl From<ExternalPlayer> for ExternalPlayerDto {
    fn from(p: ExternalPlayer) -> Self {
        match p {
            ExternalPlayer::Mpv => Self::Mpv,
            ExternalPlayer::QuickTime => Self::QuickTime,
        }
    }
}

// ===== プレイリスト（M6） =====

/// シードを JS に渡す形（u64 は number では精度が落ちるので 16 進の文字列）。
pub fn seed_to_string(seed: u64) -> String {
    format!("{seed:016x}")
}

/// 16 進の文字列からシードへ（読めなければ None）。
pub fn seed_from_string(text: &str) -> Option<u64> {
    u64::from_str_radix(text, 16).ok()
}

macro_rules! simple_enum {
    ($dto:ident, $core:ty, { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
        #[serde(rename_all = "camelCase")]
        pub enum $dto {
            $($variant),+
        }

        impl From<$dto> for $core {
            fn from(v: $dto) -> Self {
                match v {
                    $($dto::$variant => Self::$variant),+
                }
            }
        }

        impl From<$core> for $dto {
            fn from(v: $core) -> Self {
                match v {
                    $(<$core>::$variant => Self::$variant),+
                }
            }
        }
    };
}

simple_enum!(PlaylistKindDto, PlaylistKind, { Manual, Smart });
simple_enum!(GranularityDto, Granularity, { File, Unit });
simple_enum!(RepeatModeDto, RepeatMode, { Off, All, One });
simple_enum!(UnitMatchDto, UnitMatch, { Any, All, UnitOnly });
simple_enum!(LimitKindDto, LimitKind, { Count, Duration });
simple_enum!(RuleMatchDto, RuleMatch, { All, Any });

/// スマートの条件（検索の文字列を条件ごとに。docs/playlists.md §3.1・ADR）。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SmartRuleDto {
    #[serde(rename = "match")]
    pub match_mode: RuleMatchDto,
    pub clauses: Vec<String>,
}

impl From<SmartRule> for SmartRuleDto {
    fn from(r: SmartRule) -> Self {
        Self {
            match_mode: r.match_mode.into(),
            clauses: r.clauses,
        }
    }
}

impl From<SmartRuleDto> for SmartRule {
    fn from(r: SmartRuleDto) -> Self {
        Self {
            match_mode: r.match_mode.into(),
            clauses: r.clauses,
        }
    }
}

/// 上限（`kind` が null なら上限なし）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LimitDto {
    pub kind: Option<LimitKindDto>,
    pub value: i64,
}

/// サイドバーの 1 行。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummaryDto {
    pub id: i64,
    pub name: String,
    pub kind: PlaylistKindDto,
    pub granularity: GranularityDto,
    /// 展開したアイテムの数。
    pub item_count: u32,
}

/// 再開できる位置。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlayStateDto {
    pub seed: String,
    pub slot_index: u32,
    pub item_id: Option<i64>,
    pub position_ms: i64,
}

impl From<PlayState> for PlayStateDto {
    fn from(s: PlayState) -> Self {
        Self {
            seed: seed_to_string(s.seed),
            slot_index: s.slot_index,
            item_id: s.item_id,
            position_ms: s.position_ms,
        }
    }
}

/// プレイリストの設定。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDto {
    pub id: i64,
    pub name: String,
    pub kind: PlaylistKindDto,
    pub granularity: GranularityDto,
    pub shuffle: bool,
    pub seed: Option<String>,
    pub seed_locked: bool,
    pub regenerate_on_play: bool,
    pub weight_by_rating: bool,
    pub max_same_kind_run: u32,
    pub dedup_items: bool,
    pub image_duration_ms: i64,
    pub bgm_through_images: bool,
    pub repeat_mode: RepeatModeDto,
    pub rule: Option<SmartRuleDto>,
    pub limit: LimitDto,
    pub unit_match: UnitMatchDto,
    pub include_loose_items: bool,
    pub sort: ItemSortDto,
    pub generated_at: Option<i64>,
    /// 続きから再生できる位置（なければ null）。
    pub resume: Option<PlayStateDto>,
}

impl PlaylistDto {
    /// プレイリストと再開の状態から作る。
    pub fn new(p: Playlist, resume: Option<PlayState>) -> Self {
        Self {
            id: p.id,
            name: p.name,
            kind: p.kind.into(),
            granularity: p.granularity.into(),
            shuffle: p.shuffle,
            seed: p.shuffle_seed.map(seed_to_string),
            seed_locked: p.seed_locked,
            regenerate_on_play: p.regenerate_on_play,
            weight_by_rating: p.weight_by_rating,
            max_same_kind_run: p.max_same_kind_run,
            dedup_items: p.dedup_items,
            image_duration_ms: p.image_duration_ms,
            bgm_through_images: p.bgm_through_images,
            repeat_mode: p.repeat_mode.into(),
            rule: p.rule.map(Into::into),
            limit: LimitDto {
                kind: p.limit.map(|(k, _)| k.into()),
                value: p.limit.map_or(0, |(_, v)| v),
            },
            unit_match: p.unit_match.into(),
            include_loose_items: p.include_loose_items,
            sort: p.sort.into(),
            generated_at: p.generated_at,
            resume: resume.map(Into::into),
        }
    }
}

/// 変える項目（省いた項目は変えない）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaylistPatchDto {
    pub name: Option<String>,
    pub shuffle: Option<bool>,
    pub seed_locked: Option<bool>,
    pub regenerate_on_play: Option<bool>,
    pub weight_by_rating: Option<bool>,
    pub max_same_kind_run: Option<u32>,
    pub dedup_items: Option<bool>,
    pub image_duration_ms: Option<i64>,
    pub bgm_through_images: Option<bool>,
    pub repeat_mode: Option<RepeatModeDto>,
    pub rule: Option<SmartRuleDto>,
    pub limit: Option<LimitDto>,
    pub unit_match: Option<UnitMatchDto>,
    pub include_loose_items: Option<bool>,
    pub sort: Option<ItemSortDto>,
}

impl From<PlaylistPatchDto> for PlaylistPatch {
    fn from(p: PlaylistPatchDto) -> Self {
        Self {
            name: p.name,
            shuffle: p.shuffle,
            seed_locked: p.seed_locked,
            regenerate_on_play: p.regenerate_on_play,
            weight_by_rating: p.weight_by_rating,
            max_same_kind_run: p.max_same_kind_run,
            dedup_items: p.dedup_items,
            image_duration_ms: p.image_duration_ms,
            bgm_through_images: p.bgm_through_images,
            repeat_mode: p.repeat_mode.map(Into::into),
            rule: p.rule.map(Into::into),
            limit: p
                .limit
                .map(|l| l.kind.map(|k| (LimitKind::from(k), l.value))),
            unit_match: p.unit_match.map(Into::into),
            include_loose_items: p.include_loose_items,
            sort: p.sort.map(Into::into),
        }
    }
}

/// エントリが参照するもの。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EntryTargetDto {
    Item { id: i64 },
    Unit { id: i64 },
    Folder { id: i64, recursive: bool },
}

impl From<EntryTarget> for EntryTargetDto {
    fn from(t: EntryTarget) -> Self {
        match t {
            EntryTarget::Item(id) => Self::Item { id },
            EntryTarget::Unit(id) => Self::Unit { id },
            EntryTarget::Folder {
                folder_id,
                recursive,
            } => Self::Folder {
                id: folder_id,
                recursive,
            },
        }
    }
}

impl From<EntryTargetDto> for EntryTarget {
    fn from(t: EntryTargetDto) -> Self {
        match t {
            EntryTargetDto::Item { id } => Self::Item(id),
            EntryTargetDto::Unit { id } => Self::Unit(id),
            EntryTargetDto::Folder { id, recursive } => Self::Folder {
                folder_id: id,
                recursive,
            },
        }
    }
}

/// エントリ（「ファイルに固定」の取り消しに使う）。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    pub id: i64,
    pub pos: f64,
    pub target: EntryTargetDto,
    pub image_duration_ms: Option<i64>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

impl From<Entry> for EntryDto {
    fn from(e: Entry) -> Self {
        Self {
            id: e.id,
            pos: e.pos,
            target: e.target.into(),
            image_duration_ms: e.image_duration_ms,
            start_ms: e.start_ms,
            end_ms: e.end_ms,
        }
    }
}

impl From<EntryDto> for Entry {
    fn from(e: EntryDto) -> Self {
        Self {
            id: e.id,
            pos: e.pos,
            target: e.target.into(),
            image_duration_ms: e.image_duration_ms,
            start_ms: e.start_ms,
            end_ms: e.end_ms,
        }
    }
}

/// 一覧の 1 行（エントリ）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EntryRowDto {
    pub entry_id: i64,
    pub target: EntryTargetDto,
    pub name: String,
    pub item_count: u32,
    pub duration_ms: i64,
    pub kinds: Vec<String>,
    /// 表紙にするアイテム（先頭のメンバー）。
    pub cover_item_id: Option<i64>,
    pub image_duration_ms: Option<i64>,
}

/// プレイリストの中身。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistContentsDto {
    pub rows: Vec<EntryRowDto>,
    /// ファイル単位の一覧：展開したアイテムとそのエントリ（並び順）。
    pub flat_entry_ids: Vec<i64>,
    pub flat_item_ids: Vec<i64>,
    pub item_count: u32,
    pub total_ms: i64,
    pub images: u32,
    pub videos: u32,
    pub audios: u32,
}

impl PlaylistContentsDto {
    /// エントリの行から作る。
    pub fn new(rows: Vec<EntryRow>) -> Self {
        let mut dto = Self {
            rows: Vec::with_capacity(rows.len()),
            flat_entry_ids: Vec::new(),
            flat_item_ids: Vec::new(),
            item_count: 0,
            total_ms: 0,
            images: 0,
            videos: 0,
            audios: 0,
        };
        for row in rows {
            for item in &row.items {
                dto.flat_entry_ids.push(row.entry.id);
                dto.flat_item_ids.push(*item);
            }
            dto.images += row.kind_counts[0];
            dto.videos += row.kind_counts[1];
            dto.audios += row.kind_counts[2];
            dto.total_ms += row.duration_ms;
            dto.rows.push(EntryRowDto {
                entry_id: row.entry.id,
                target: row.entry.target.into(),
                name: row.name,
                item_count: u32::try_from(row.items.len()).unwrap_or(u32::MAX),
                duration_ms: row.duration_ms,
                kinds: row.kinds.iter().map(|k| (*k).to_string()).collect(),
                cover_item_id: row.items.first().copied(),
                image_duration_ms: row.entry.image_duration_ms,
            });
        }
        dto.item_count = u32::try_from(dto.flat_item_ids.len()).unwrap_or(u32::MAX);
        dto
    }
}

/// 追加するもの。
#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AddSourceDto {
    Item { id: i64 },
    Unit { id: i64 },
    Folder { id: i64 },
}

impl From<AddSourceDto> for AddSource {
    fn from(s: AddSourceDto) -> Self {
        match s {
            AddSourceDto::Item { id } => Self::Item(id),
            AddSourceDto::Unit { id } => Self::Unit(id),
            AddSourceDto::Folder { id } => Self::Folder(id),
        }
    }
}

/// 追加した結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AddedDto {
    pub entry_ids: Vec<i64>,
    pub unit_id: Option<i64>,
}

/// 再生するもの。
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PlaySourceDto {
    /// 保存したプレイリスト。
    Playlist { id: i64 },
    /// ライブラリの選択（アイテムと `0x8000_0000 | unit_id`、並び順）。
    Selection { entries: Vec<i64> },
    /// 開いている一覧（⌘K の ⇧↵：結果をランダム再生）。
    View {
        view: ItemViewDto,
        query: Option<String>,
        #[serde(rename = "collapseUnits")]
        collapse_units: bool,
        #[serde(rename = "utcOffsetMinutes")]
        utc_offset_minutes: i32,
    },
}

/// 再生の依頼（省いた項目は既定値）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayRequestDto {
    pub resume: bool,
    pub shuffle: Option<bool>,
    pub reseed: bool,
    pub start_item: Option<i64>,
    /// 並びはそのままで、このアイテムから始める（一覧の「ここから再生」）。
    pub from_item: Option<i64>,
    /// その場のキューを同じシードで作り直すとき（16 進）。
    pub seed: Option<String>,
}

impl From<&PlayRequestDto> for PlayRequest {
    fn from(r: &PlayRequestDto) -> Self {
        Self {
            resume: r.resume,
            shuffle: r.shuffle,
            reseed: r.reseed,
            start_item: r.start_item,
            from_item: r.from_item,
        }
    }
}

/// キューのまとまり（ユニット単位で再生するときのユニット・フォルダ・単体）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct GroupDto {
    pub entry_id: i64,
    pub target: EntryTargetDto,
    pub name: String,
    /// キューの何番目から何件か。
    pub start: u32,
    pub len: u32,
}

/// 再生しているプレイリストの設定（シアターで使う・変えるもの）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistPlayDto {
    pub id: i64,
    pub name: String,
    pub kind: PlaylistKindDto,
    pub shuffle: bool,
    pub seed_locked: bool,
    pub repeat_mode: RepeatModeDto,
    pub bgm_through_images: bool,
    pub max_same_kind_run: u32,
}

/// 再生するキュー（docs/playlists.md §5.1 の Slot 列を列ごとの配列で）。
#[derive(Debug, Clone, Default, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlayQueueDto {
    pub item_ids: Vec<i64>,
    /// ユニットとして再生するスロットのユニット（ファイル単位・単体は null。再生回数に使う）。
    pub unit_ids: Vec<Option<i64>>,
    /// まとまりの中の位置（0 から）と件数。
    pub index_in_unit: Vec<u32>,
    pub unit_len: Vec<u32>,
    /// 画像の表示時間（音楽・動画は null）。
    pub image_duration_ms: Vec<Option<i64>>,
    /// トリム（item エントリの start_ms / end_ms）。
    pub start_ms: Vec<Option<i64>>,
    pub end_ms: Vec<Option<i64>>,
    /// ユニット単位のとき、スロットのまとまりの番号（`groups` の添字）。ファイル単位では空。
    pub group_index: Vec<u32>,
    pub groups: Vec<GroupDto>,
    pub total_ms: i64,
    pub seed: String,
    pub granularity: Option<GranularityDto>,
    pub start_index: u32,
    pub start_position_ms: i64,
    pub playlist: Option<PlaylistPlayDto>,
}

impl PlayQueueDto {
    /// キューと、そのまとまり・合計の長さから作る。
    pub fn new(queue: PlayQueue, info: QueueInfo, playlist: Option<Playlist>) -> Self {
        let by_unit = queue.granularity == Granularity::Unit;
        let mut dto = Self {
            total_ms: info.total_ms,
            seed: seed_to_string(queue.seed),
            granularity: Some(queue.granularity.into()),
            start_index: u32::try_from(queue.start_index).unwrap_or(0),
            start_position_ms: queue.start_ms,
            group_index: info
                .group_index
                .iter()
                .map(|g| u32::try_from(*g).unwrap_or(0))
                .collect(),
            groups: info
                .groups
                .into_iter()
                .map(|g| GroupDto {
                    entry_id: g.entry_id,
                    target: g.target.into(),
                    name: g.name,
                    start: u32::try_from(g.start).unwrap_or(0),
                    len: u32::try_from(g.len).unwrap_or(0),
                })
                .collect(),
            playlist: playlist.map(|p| PlaylistPlayDto {
                id: p.id,
                name: p.name,
                kind: p.kind.into(),
                shuffle: p.shuffle,
                seed_locked: p.seed_locked,
                repeat_mode: p.repeat_mode.into(),
                bgm_through_images: p.bgm_through_images,
                max_same_kind_run: p.max_same_kind_run,
            }),
            ..Self::default()
        };
        for slot in queue.slots {
            dto.item_ids.push(slot.item_id);
            // ユニットとして再生したスロットだけ、ユニットの再生回数に数える
            dto.unit_ids.push(match slot.unit_key {
                Some(UnitKey::Unit(id)) if by_unit => Some(id),
                _ => None,
            });
            dto.index_in_unit.push(slot.index_in_unit);
            dto.unit_len.push(slot.unit_len);
            dto.image_duration_ms.push(slot.image_duration_ms);
            dto.start_ms.push(slot.start_ms);
            dto.end_ms.push(slot.end_ms);
        }
        dto
    }
}

// ===== ダウンロード（docs/architecture.md §9） =====

simple_enum!(ToolDto, kura_core::download::Tool, { YtDlp, GalleryDl });
simple_enum!(CookieSourceDto, kura_core::download::CookieSource, { None, Firefox, Safari, Chrome });

/// 画質（`best` / `1080` / `720` / `480`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum QualityDto {
    #[serde(rename = "best")]
    Best,
    #[serde(rename = "1080")]
    P1080,
    #[serde(rename = "720")]
    P720,
    #[serde(rename = "480")]
    P480,
}

impl From<QualityDto> for kura_core::download::Quality {
    fn from(q: QualityDto) -> Self {
        match q {
            QualityDto::Best => Self::Best,
            QualityDto::P1080 => Self::P1080,
            QualityDto::P720 => Self::P720,
            QualityDto::P480 => Self::P480,
        }
    }
}

impl From<kura_core::download::Quality> for QualityDto {
    fn from(q: kura_core::download::Quality) -> Self {
        use kura_core::download::Quality;
        match q {
            Quality::Best => Self::Best,
            Quality::P1080 => Self::P1080,
            Quality::P720 => Self::P720,
            Quality::P480 => Self::P480,
        }
    }
}

/// 1 件のダウンロードの選択（画質・音声のみ・保存先・自動タグ）。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DownloadOptionsDto {
    pub quality: QualityDto,
    pub audio_only: bool,
    pub dest_folder_id: Option<i64>,
    pub tag_uploader: bool,
    pub tag_site_tags: bool,
    pub playlist_unit: bool,
}

impl From<DownloadOptionsDto> for kura_core::download::DownloadOptions {
    fn from(o: DownloadOptionsDto) -> Self {
        Self {
            quality: o.quality.into(),
            audio_only: o.audio_only,
            dest_folder_id: o.dest_folder_id,
            tag_uploader: o.tag_uploader,
            tag_site_tags: o.tag_site_tags,
            playlist_unit: o.playlist_unit,
        }
    }
}

impl From<kura_core::download::DownloadOptions> for DownloadOptionsDto {
    fn from(o: kura_core::download::DownloadOptions) -> Self {
        Self {
            quality: o.quality.into(),
            audio_only: o.audio_only,
            dest_folder_id: o.dest_folder_id,
            tag_uploader: o.tag_uploader,
            tag_site_tags: o.tag_site_tags,
            playlist_unit: o.playlist_unit,
        }
    }
}

/// URL の判定の結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DetectionDto {
    pub url: String,
    pub site: String,
    pub site_label: String,
    pub tool: ToolDto,
    /// 一致した規則の名前。
    pub rule: Option<String>,
    /// 再生リスト・まとめのページ。
    pub playlist: bool,
    /// ツールが見つかるか。
    pub tool_found: bool,
    /// キャプチャのルールに一致したら、そのルール（ダウンロードより先に撮る。ADR）。
    pub capture: Option<CaptureMatchDto>,
}

/// URL に一致したキャプチャのルール。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CaptureMatchDto {
    pub rule_id: i64,
    pub rule_name: String,
    pub mode: CaptureModeDto,
    /// Google Chrome が見つかるか。
    pub chrome_found: bool,
}

/// URL の規則。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRuleDto {
    pub name: String,
    pub pattern: String,
    pub tool: ToolDto,
    pub args: Vec<String>,
}

impl From<DownloadRuleDto> for kura_core::download::DownloadRule {
    fn from(r: DownloadRuleDto) -> Self {
        Self {
            name: r.name,
            pattern: r.pattern,
            tool: r.tool.into(),
            args: r.args,
        }
    }
}

impl From<kura_core::download::DownloadRule> for DownloadRuleDto {
    fn from(r: kura_core::download::DownloadRule) -> Self {
        Self {
            name: r.name,
            pattern: r.pattern,
            tool: r.tool.into(),
            args: r.args,
        }
    }
}

/// ダウンロードの設定（この Mac で共通）。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPrefsDto {
    pub cookies: CookieSourceDto,
    pub rules: Vec<DownloadRuleDto>,
}

impl From<crate::config::DownloadPrefs> for DownloadPrefsDto {
    fn from(p: crate::config::DownloadPrefs) -> Self {
        Self {
            cookies: p.cookies.into(),
            rules: p.rules.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DownloadPrefsDto> for crate::config::DownloadPrefs {
    fn from(p: DownloadPrefsDto) -> Self {
        Self {
            cookies: p.cookies.into(),
            rules: p.rules.into_iter().map(Into::into).collect(),
        }
    }
}

/// ダウンロードの一覧の 1 件（ジョブの入力と結果を読んだもの）。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DownloadJobDto {
    pub id: i64,
    /// `download` か `capture`。
    pub kind: JobKindDto,
    pub state: JobStateDto,
    pub progress: Option<f64>,
    pub message: Option<String>,
    pub error_code: Option<String>,
    pub url: String,
    pub site_label: String,
    /// 使うツールの表示（`yt-dlp`・`gallery-dl`・`Chrome`）。
    pub tool_label: String,
    /// キャプチャのルールの名前。
    pub rule_name: Option<String>,
    pub audio_only: bool,
    /// 取得したものの題名（わかるまでは None）。
    pub title: Option<String>,
    pub files: u64,
    pub added: u64,
    pub duplicates: u64,
    pub skipped: u64,
    pub tags: u64,
    pub item_ids: Vec<i64>,
    pub unit_ids: Vec<i64>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

impl DownloadJobDto {
    /// ジョブの行から作る（ダウンロード・キャプチャでなければ None）。
    pub fn from_record(r: JobRecord) -> Option<Self> {
        match r.kind {
            JobKind::Download => Self::from_download(r),
            JobKind::Capture => Self::from_capture(r),
            _ => None,
        }
    }

    fn from_download(r: JobRecord) -> Option<Self> {
        let input: kura_core::download::DownloadInput = serde_json::from_str(&r.input_json).ok()?;
        let result: kura_core::download::DownloadResult = r
            .result_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        Some(Self {
            id: r.id,
            kind: r.kind.into(),
            state: r.state.into(),
            progress: r.progress,
            message: r.message,
            error_code: r.error_code,
            url: input.detection.url,
            site_label: input.detection.site_label,
            tool_label: input.detection.tool.command().to_string(),
            rule_name: input.detection.rule,
            audio_only: input.options.audio_only,
            title: result.title,
            files: result.files,
            added: result.added,
            duplicates: result.duplicates,
            skipped: result.skipped,
            tags: result.tags,
            item_ids: result.item_ids,
            unit_ids: result.unit_ids,
            created_at: r.created_at,
            finished_at: r.finished_at,
        })
    }

    fn from_capture(r: JobRecord) -> Option<Self> {
        let input: kura_core::capture::CaptureInput = serde_json::from_str(&r.input_json).ok()?;
        let result: kura_core::capture::CaptureResult = r
            .result_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        let site = kura_core::capture::job::host_of(&input.url).unwrap_or_default();
        Some(Self {
            id: r.id,
            kind: r.kind.into(),
            state: r.state.into(),
            progress: r.progress,
            message: r.message,
            error_code: r.error_code,
            url: input.url,
            site_label: site,
            tool_label: "Chrome".into(),
            rule_name: Some(input.rule.name),
            audio_only: false,
            title: result.title,
            files: result.added + result.duplicates,
            added: result.added,
            duplicates: result.duplicates,
            skipped: 0,
            tags: result.tags,
            item_ids: result.item_ids,
            unit_ids: Vec::new(),
            created_at: r.created_at,
            finished_at: r.finished_at,
        })
    }
}

/// 外部ツールの状態。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfoDto {
    pub name: String,
    pub path: Option<String>,
    pub version: Option<String>,
    /// Homebrew で入れたもの（更新は `brew upgrade`）。
    pub homebrew: bool,
    /// 設定で場所を指定している。
    pub overridden: bool,
}

impl From<kura_core::tools::ToolInfo> for ToolInfoDto {
    fn from(t: kura_core::tools::ToolInfo) -> Self {
        Self {
            name: t.name,
            path: t.path.map(|p| p.to_string_lossy().into_owned()),
            version: t.version,
            homebrew: t.homebrew,
            overridden: t.overridden,
        }
    }
}

/// 「更新を確認」の結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ToolsUpdateDto {
    /// Homebrew で入れたものを更新するコマンド（ターミナルで実行してもらう）。
    pub brew_command: Option<String>,
    /// 自分で更新できるツール（単体版の yt-dlp など）を実行した結果。
    pub outputs: Vec<ToolUpdateOutputDto>,
}

/// 1 つのツールの更新の結果。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ToolUpdateOutputDto {
    pub tool: String,
    pub ok: bool,
    pub text: String,
}

// ===== キャプチャ（docs/architecture.md §10） =====

simple_enum!(CaptureModeDto, kura_core::capture::CaptureMode, { Element, FullPage, Viewport });

/// キャプチャのルール。
#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRuleDto {
    pub id: i64,
    pub name: String,
    pub url_pattern: String,
    pub mode: CaptureModeDto,
    pub target_selector: Option<String>,
    pub hide_selectors: Vec<String>,
    pub viewport_width: i64,
    pub scroll_times: i64,
    pub wait_ms: i64,
    pub dest_folder_id: Option<i64>,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    /// `ok` / `failed`。
    pub last_status: Option<String>,
}

impl From<kura_core::capture::CaptureRule> for CaptureRuleDto {
    fn from(r: kura_core::capture::CaptureRule) -> Self {
        Self {
            id: r.id,
            name: r.name,
            url_pattern: r.url_pattern,
            mode: r.mode.into(),
            target_selector: r.target_selector,
            hide_selectors: r.hide_selectors,
            viewport_width: r.viewport_width,
            scroll_times: r.scroll_times,
            wait_ms: r.wait_ms,
            dest_folder_id: r.dest_folder_id,
            enabled: r.enabled,
            last_run_at: r.last_run_at,
            last_status: r.last_status,
        }
    }
}

/// ルールを作る・変えるときの内容。
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRuleInputDto {
    pub name: String,
    pub url_pattern: String,
    pub mode: CaptureModeDto,
    pub target_selector: Option<String>,
    pub hide_selectors: Vec<String>,
    pub viewport_width: i64,
    pub scroll_times: i64,
    pub wait_ms: i64,
    pub dest_folder_id: Option<i64>,
    pub enabled: bool,
}

impl From<CaptureRuleInputDto> for kura_core::capture::RuleInput {
    fn from(r: CaptureRuleInputDto) -> Self {
        Self {
            name: r.name,
            url_pattern: r.url_pattern,
            mode: r.mode.into(),
            target_selector: r.target_selector,
            hide_selectors: r.hide_selectors,
            viewport_width: r.viewport_width,
            scroll_times: r.scroll_times,
            wait_ms: r.wait_ms,
            dest_folder_id: r.dest_folder_id,
            enabled: r.enabled,
        }
    }
}

/// 見たままキャプチャ・ログイン用の Chrome の撮影の結果。
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResultDto {
    pub title: String,
    pub url: String,
    pub item_ids: Vec<i64>,
    pub added: u64,
    /// 使ったキャプチャのルール（URL が一致したとき）。
    pub rule_name: Option<String>,
    /// 長いページのため途中まで。
    pub truncated: bool,
}

/// ⌃⌥⌘C（ログイン用の Chrome を見ながら撮る）の結果（イベント `capture://login-shot`。ADR-112）。
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "capture://login-shot")]
pub struct LoginShotEvent {
    pub ok: bool,
    /// 撮れなかったときの理由。
    pub message: String,
    pub result: Option<SnapshotResultDto>,
}
