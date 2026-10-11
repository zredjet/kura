//! ダウンロード（docs/architecture.md §9）。URL の判定・引数・進捗・メタデータの読み取りは純粋関数に分け、
//! ジョブがそれらを順に使う。

pub mod args;
pub mod detect;
pub mod job;
pub mod meta;
pub mod progress;
pub mod register;

pub use args::{CookieSource, DownloadOptions, Quality};
pub use detect::{Detection, DownloadRule, Tool, detect, validate_rule};
pub use job::{DownloadHandler, DownloadInput, DownloadResult, start_download, work_dir_name};
