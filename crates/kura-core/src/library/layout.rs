//! ライブラリのフォルダ構成（docs/data-model.md §1）。

use std::path::{Path, PathBuf};

/// DB のファイル名。
pub const DB_FILE: &str = "library.sqlite";

/// サムネイルの大きさ（s：長辺 512px、l：長辺 1600px）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbSize {
    Small,
    Large,
}

impl ThumbSize {
    /// 長辺のピクセル数。
    pub fn max_px(self) -> u32 {
        match self {
            Self::Small => 512,
            Self::Large => 1600,
        }
    }

    /// ファイル名とパスに使う記号（`s` / `l`）。
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Small => "s",
            Self::Large => "l",
        }
    }

    /// `s` / `l` から戻す。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "s" => Some(Self::Small),
            "l" => Some(Self::Large),
            _ => None,
        }
    }
}

/// ライブラリの中に作るフォルダ。
pub const SUBDIRS: [&str; 8] = [
    "files", "thumbs", "art", "proxies", "staging", "archives", "logs", "backups",
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

    /// DB のバックアップの置き場所（`backups/`。FR-SYS-03）。
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// ジョブのログファイル。
    pub fn job_log_path(&self, job_id: i64) -> PathBuf {
        self.logs_dir().join(format!("job-{job_id}.log"))
    }

    /// 内容アドレスで決まる実ファイルのパス（`files/ab/cd/<hash>.<ext>`）。
    pub fn content_path(&self, hash: &[u8; 32], ext: &str) -> PathBuf {
        self.files_dir().join(content_rel_path(&hex(hash), ext))
    }

    /// 実ファイルが置かれるフォルダ（`files/ab/cd`）。拡張子がわからないときに探すのに使う。
    pub fn content_dir(&self, hash_hex: &str) -> Option<PathBuf> {
        Some(
            self.files_dir()
                .join(hash_hex.get(0..2)?)
                .join(hash_hex.get(2..4)?),
        )
    }

    /// サムネイル（`thumbs/ab/<hash>_s.jpg`・`_l.jpg`）。
    pub fn thumb_path(&self, hash_hex: &str, size: ThumbSize) -> PathBuf {
        let a = hash_hex.get(0..2).unwrap_or("00");
        self.root
            .join("thumbs")
            .join(a)
            .join(format!("{hash_hex}_{}.jpg", size.suffix()))
    }

    /// 音楽のジャケット（`art/ab/<hash>.jpg`）。
    pub fn art_path(&self, hash_hex: &str) -> PathBuf {
        let a = hash_hex.get(0..2).unwrap_or("00");
        self.root
            .join("art")
            .join(a)
            .join(format!("{hash_hex}.jpg"))
    }

    /// 再生用プロキシ（`proxies/ab/<hash>.mp4`。作るのは M5）。
    pub fn proxy_path(&self, hash_hex: &str) -> PathBuf {
        let a = hash_hex.get(0..2).unwrap_or("00");
        self.root
            .join("proxies")
            .join(a)
            .join(format!("{hash_hex}.mp4"))
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
    fn thumb_and_art_paths() {
        let layout = Layout::new("/lib/L.kuralib");
        let hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        assert_eq!(
            layout.thumb_path(hex, ThumbSize::Small),
            PathBuf::from(format!("/lib/L.kuralib/thumbs/ab/{hex}_s.jpg"))
        );
        assert_eq!(
            layout.thumb_path(hex, ThumbSize::Large),
            PathBuf::from(format!("/lib/L.kuralib/thumbs/ab/{hex}_l.jpg"))
        );
        assert_eq!(
            layout.art_path(hex),
            PathBuf::from(format!("/lib/L.kuralib/art/ab/{hex}.jpg"))
        );
        assert_eq!(
            layout.content_dir(hex),
            Some(PathBuf::from("/lib/L.kuralib/files/ab/cd"))
        );
        assert_eq!(ThumbSize::parse("l"), Some(ThumbSize::Large));
        assert_eq!(ThumbSize::Small.max_px(), 512);
    }

    #[test]
    fn hex_round_trips() {
        let hash: [u8; 32] = std::array::from_fn(|i| i as u8 * 7);
        assert_eq!(parse_hex32(&hex(&hash)), Some(hash));
        assert_eq!(parse_hex32("zz"), None);
    }
}
