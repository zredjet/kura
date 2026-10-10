//! Kura の OS 非依存なドメインロジック（DB、取り込み、検索、ユニット、プレイリスト、ジョブ）。
//!
//! macOS 固有の処理は `kura-mac` に置き、このクレートには `cfg(target_os)` を書かない。

pub mod db;
pub mod error;
pub mod folders;
pub mod import;
pub mod items;
pub mod jobs;
pub mod library;
pub mod media;
pub mod random;
pub mod serve;
pub mod tags;
pub mod text;
pub mod time;
pub mod tools;
pub mod unit;

pub use error::{CoreError, Result};
