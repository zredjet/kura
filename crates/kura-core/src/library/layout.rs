//! ライブラリのフォルダ構成（docs/data-model.md §1）。

use std::path::{Path, PathBuf};

/// DB のファイル名。
pub const DB_FILE: &str = "library.sqlite";

/// ライブラリの中に作るフォルダ。
pub const SUBDIRS: [&str; 7] = [
    "files", "thumbs", "art", "proxies", "staging", "archives", "logs",
];

/// ライブラリ内の各パスを組み立てる。
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// `root`（`*.kuralib` フォルダ）を基準にする。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// ライブラリのフォルダ。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// DB ファイル。
    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILE)
    }

    /// 実ファイルの置き場所（`files/`）。
    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    /// 一時置き場（`staging/`）。
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// ジョブのログの置き場所（`logs/`）。
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// ジョブのログファイル。
    pub fn job_log_path(&self, job_id: i64) -> PathBuf {
        self.logs_dir().join(format!("job-{job_id}.log"))
    }

    /// 内容アドレスで決まる実ファイルのパス（`files/ab/cd/<hash>.<ext>`）。
    pub fn content_path(&self, hash: &[u8; 32], ext: &str) -> PathBuf {
        self.files_dir().join(content_rel_path(&hex(hash), ext))
    }
}

/// `files/` からの相対パス（`ab/cd/<hash>.<ext>`）。
pub fn content_rel_path(hash_hex: &str, ext: &str) -> String {
    let a = hash_hex.get(0..2).unwrap_or("00");
    let b = hash_hex.get(2..4).unwrap_or("00");
    format!("{a}/{b}/{hash_hex}.{ext}")
}

/// ハッシュを小文字の 16 進にする。
pub fn hex(hash: &[u8]) -> String {
    use std::fmt::Write;
    hash.iter()
        .fold(String::with_capacity(hash.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// 16 進の文字列を 32 バイトのハッシュに戻す（形式が違えば None）。
pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_path_splits_by_hash_prefix() {
        let mut hash = [0u8; 32];
        hash[0] = 0xab;
        hash[1] = 0xcd;
        let layout = Layout::new("/lib/Kura Library.kuralib");
        let path = layout.content_path(&hash, "jpg");
        let hex = hex(&hash);
        assert!(hex.starts_with("abcd"));
        assert_eq!(
            path,
            PathBuf::from(format!("/lib/Kura Library.kuralib/files/ab/cd/{hex}.jpg"))
        );
    }

    #[test]
    fn hex_round_trips() {
        let hash: [u8; 32] = std::array::from_fn(|i| i as u8 * 7);
        assert_eq!(parse_hex32(&hex(&hash)), Some(hash));
        assert_eq!(parse_hex32("zz"), None);
    }
}
