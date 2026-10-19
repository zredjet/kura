//! Kura の OS 非依存なドメインロジック（DB、取り込み、検索、ユニット、プレイリスト、ジョブ）。
//!
//! macOS 固有の処理は `kura-mac` に置き、このクレートには `cfg(target_os)` を書かない。

pub mod capture;
pub mod codec_ext;
pub mod db;
pub mod download;
pub mod edit;
pub mod entries;
pub mod error;
pub mod export;
pub mod folders;
pub mod import;
pub mod items;
pub mod jobs;
pub mod library;
pub mod media;
pub mod playback;
pub mod playlist;
pub mod query;
pub mod queue;
pub mod random;
pub mod serve;
pub mod settings;
pub mod smart;
pub mod tags;
pub mod text;
pub mod time;
pub mod tools;
pub mod unit;

pub use error::{CoreError, Result};
