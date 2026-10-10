//! Kura の OS 非依存なドメインロジック（DB、取り込み、検索、ユニット、プレイリスト、ジョブ）。
//!
//! macOS 固有の処理は `kura-mac` に置き、このクレートには `cfg(target_os)` を書かない。

pub mod error;

pub use error::{CoreError, Result};
