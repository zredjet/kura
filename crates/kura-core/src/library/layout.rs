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

/// サムネイルの入れ物（透明のないものは JPEG、透明のあるものは PNG。docs/data-model.md §1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbFormat {
    Jpeg,
    Png,
}

impl ThumbFormat {
    /// すべての入れ物（ゴミ箱を空にするときに、どちらも消す）。
    pub const ALL: [Self; 2] = [Self::Jpeg, Self::Png];

    /// 拡張子（`jpg` / `png`）。
    pub fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

/// アニメの帯の形式の版（`anim/ab/<hash>-<版>.png`。形を変えたら上げる）。
pub const ANIM_V: u32 = 1;

/// ライブラリの中に作るフォルダ。
pub const SUBDIRS: [&str; 10] = [
    "files", "thumbs", "art", "proxies", "peaks", "anim", "staging", "archives", "logs", "backups",
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

    /// 取得済みの記録の置き場所（`archives/`。ADR-95）。
    pub fn archives_dir(&self) -> PathBuf {
        self.root.join("archives")
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

    /// JPEG のサムネイル（`thumbs/ab/<hash>_s.jpg`・`_l.jpg`）。
    pub fn thumb_path(&self, hash_hex: &str, size: ThumbSize) -> PathBuf {
        self.thumb_path_as(hash_hex, size, ThumbFormat::Jpeg)
    }

    /// `format` のサムネイル（`thumbs/ab/<hash>_{s,l}.{jpg,png}`）。
    pub fn thumb_path_as(&self, hash_hex: &str, size: ThumbSize, format: ThumbFormat) -> PathBuf {
        let a = hash_hex.get(0..2).unwrap_or("00");
        self.root.join("thumbs").join(a).join(format!(
            "{hash_hex}_{}.{}",
            size.suffix(),
            format.ext()
        ))
    }

    /// 音声の波形の段（`peaks/ab/<hash>.peaks`。解析のジョブが書く）。
    pub fn peaks_path(&self, hash_hex: &str) -> PathBuf {
        let a = hash_hex.get(0..2).unwrap_or("00");
        self.root
            .join("peaks")
            .join(a)
            .join(format!("{hash_hex}.peaks"))
    }

    /// アニメの帯（`anim/ab/<hash>-1.png`。見たときにその場で作る）。
    pub fn anim_path(&self, hash_hex: &str) -> PathBuf {
        let a = hash_hex.get(0..2).unwrap_or("00");
        self.root
            .join("anim")
            .join(a)
            .join(format!("{hash_hex}-{ANIM_V}.png"))
    }

    /// 中身から作ったファイルのすべて（サムネイルの jpg・png、ジャケット、プロキシ、波形、アニメの帯）。
    /// 台帳の行を解放したら、実ファイルと一緒に消す。派生のファイルを足したら、ここにも足す。
    pub fn derived_paths(&self, hash_hex: &str) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(8);
        for size in [ThumbSize::Small, ThumbSize::Large] {
            for format in ThumbFormat::ALL {
                out.push(self.thumb_path_as(hash_hex, size, format));
            }
        }
        out.push(self.art_path(hash_hex));
        out.push(self.proxy_path(hash_hex));
        out.push(self.peaks_path(hash_hex));
        out.push(self.anim_path(hash_hex));
        out
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
    fn derived_paths_cover_every_file_made_from_the_content() {
        let layout = Layout::new("/lib/L.kuralib");
        let hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let rel: Vec<String> = layout
            .derived_paths(hex)
            .iter()
            .map(|p| {
                p.strip_prefix("/lib/L.kuralib")
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            rel,
            vec![
                format!("thumbs/ab/{hex}_s.jpg"),
                format!("thumbs/ab/{hex}_s.png"),
                format!("thumbs/ab/{hex}_l.jpg"),
                format!("thumbs/ab/{hex}_l.png"),
                format!("art/ab/{hex}.jpg"),
                format!("proxies/ab/{hex}.mp4"),
                format!("peaks/ab/{hex}.peaks"),
                format!("anim/ab/{hex}-1.png"),
            ]
        );
        // どれもライブラリの中に作るフォルダの下にある
        for p in &rel {
            let top = p.split('/').next().unwrap();
            assert!(SUBDIRS.contains(&top), "{p}");
        }
    }

    #[test]
    fn hex_round_trips() {
        let hash: [u8; 32] = std::array::from_fn(|i| i as u8 * 7);
        assert_eq!(parse_hex32(&hex(&hash)), Some(hash));
        assert_eq!(parse_hex32("zz"), None);
    }
}
