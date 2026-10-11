//! 書き出し（FR-SYS-02。ADR-120）：選んだアイテムを元のファイル名でフォルダへコピーし、
//! タグなどを `kura-export.json` に添える。
//!
//! - 読むのはライブラリの `files/` だけ。同じボリュームならクローンでコピーする。
//! - 書き出し先の既存のファイル・フォルダは上書きしない（同じ名前があれば「名前 2.jpg」）。
//! - ユニットを選んだら、ユニット名のフォルダの中に並び順で書き出す。

mod job;
pub mod names;

pub use job::{ExportHandler, ExportInput, ExportResult, ExportedFile, start_export};

#[cfg(test)]
mod job_tests;
