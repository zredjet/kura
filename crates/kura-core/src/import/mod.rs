//! 取り込み（docs/architecture.md §6）。走査・所属の決定・配置は純粋関数またはそれに近い形に分け、ジョブがそれらを順に呼ぶ。

mod job;
mod kind;
pub(crate) mod place;
mod plan;
mod preview;
mod scan;
pub(crate) mod store;

pub use job::{
    ImportFailure, ImportHandler, ImportInput, ImportResult, MAX_IMPORT_THREADS, start_import,
};
pub use kind::{
    KindCounts, MediaKind, Rejected, check_head, classify, classify_file, format_id, mime_for,
};
pub use place::{hash_file, partial_hash};
pub use plan::{DuplicatePolicy, ImportOptions, TagName, parse_tag};
pub use preview::{ImportPreview, preview};
pub use scan::{ScanResult, ScanRoot, ScannedFile, scan};
