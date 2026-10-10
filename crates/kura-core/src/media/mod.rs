//! メディア情報とサムネイル（docs/architecture.md §7）。
//!
//! kura-core は `MediaBackend` trait だけを知り、実装は起動時に注入する（macOS は kura-mac、
//! 扱えない形式は ffmpeg）。テストでは偽物の実装を使う。

mod ffmpeg;
mod job;

pub use ffmpeg::{FfmpegBackend, parse_ffprobe};
pub use job::{THUMB_BATCH, ThumbnailHandler};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::import::MediaKind;

/// メディア情報。幅・高さは表示上の向き（回転を反映したもの）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaInfo {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// 撮影・作成日時（UTC ミリ秒）。
    pub captured_at: Option<i64>,
    /// 主なコーデック（例：`h264`、`hevc`、`aac`、`flac`、`jpeg`）。
    pub codec: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
}

impl MediaInfo {
    /// items.codec_info に入れる要約（JSON）。
    pub fn codec_json(&self) -> Option<String> {
        self.codec
            .as_ref()
            .map(|codec| serde_json::json!({ "codec": codec }).to_string())
    }
}

/// サムネイルの書き出し先（長辺 1600 と 512）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbTargets {
    pub large: PathBuf,
    pub small: PathBuf,
}

/// メディアの処理の失敗。
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// この実装では扱えない形式（次の実装に回す）。
    #[error("この形式は扱えません")]
    Unsupported,
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// メディア情報とサムネイルを作る実装。
pub trait MediaBackend: Send + Sync {
    /// メディア情報を読む。
    fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError>;
    /// 画像のサムネイル（長辺 1600 と 512 の JPEG）を作る。
    fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<(), MediaError>;
    /// 動画の `at_sec` 秒の地点のサムネイルを作る。
    fn video_thumbnails(
        &self,
        path: &Path,
        at_sec: f64,
        out: &ThumbTargets,
    ) -> Result<(), MediaError>;
    /// 音楽のジャケットを `art` に書き、サムネイルも作る。ジャケットがなければ Ok(false)。
    fn audio_artwork(
        &self,
        path: &Path,
        art: &Path,
        out: &ThumbTargets,
    ) -> Result<bool, MediaError>;
}

/// 何も扱えない実装（テストや、ツールがないとき）。
pub struct NoBackend;

impl MediaBackend for NoBackend {
    fn probe(&self, _: &Path, _: MediaKind) -> Result<MediaInfo, MediaError> {
        Err(MediaError::Unsupported)
    }
    fn image_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<(), MediaError> {
        Err(MediaError::Unsupported)
    }
    fn video_thumbnails(&self, _: &Path, _: f64, _: &ThumbTargets) -> Result<(), MediaError> {
        Err(MediaError::Unsupported)
    }
    fn audio_artwork(&self, _: &Path, _: &Path, _: &ThumbTargets) -> Result<bool, MediaError> {
        Err(MediaError::Unsupported)
    }
}

/// 複数の実装を順に試す（`Unsupported` なら次へ。ジャケットが見つからなかったときも次を試す）。
pub struct ChainBackend {
    backends: Vec<Arc<dyn MediaBackend>>,
}

impl ChainBackend {
    /// 先頭から順に試す。
    pub fn new(backends: Vec<Arc<dyn MediaBackend>>) -> Self {
        Self { backends }
    }

    fn first<T>(
        &self,
        mut call: impl FnMut(&dyn MediaBackend) -> Result<T, MediaError>,
    ) -> Result<T, MediaError> {
        for backend in &self.backends {
            match call(backend.as_ref()) {
                Err(MediaError::Unsupported) => continue,
                other => return other,
            }
        }
        Err(MediaError::Unsupported)
    }
}

impl MediaBackend for ChainBackend {
    fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError> {
        self.first(|b| b.probe(path, kind))
    }
    fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<(), MediaError> {
        self.first(|b| b.image_thumbnails(path, out))
    }
    fn video_thumbnails(
        &self,
        path: &Path,
        at_sec: f64,
        out: &ThumbTargets,
    ) -> Result<(), MediaError> {
        self.first(|b| b.video_thumbnails(path, at_sec, out))
    }
    fn audio_artwork(
        &self,
        path: &Path,
        art: &Path,
        out: &ThumbTargets,
    ) -> Result<bool, MediaError> {
        let mut result = Err(MediaError::Unsupported);
        for backend in &self.backends {
            match backend.audio_artwork(path, art, out) {
                Ok(true) => return Ok(true),
                Ok(false) => result = Ok(false),
                Err(MediaError::Unsupported) => {}
                Err(err) => {
                    if result.is_err() {
                        result = Err(err);
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! テスト用の偽物：画像は 1600×900、動画は 1920×1080・10 秒、音楽は 3 分。サムネイルは小さなファイルを書く。
    use super::*;

    pub struct FakeBackend;

    fn touch(path: &Path, bytes: &[u8]) -> Result<(), MediaError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    impl MediaBackend for FakeBackend {
        fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError> {
            if path.extension().is_some_and(|e| e == "bad") {
                return Err(MediaError::Failed("壊れています".into()));
            }
            Ok(match kind {
                MediaKind::Image => MediaInfo {
                    width: Some(1600),
                    height: Some(900),
                    codec: Some("jpeg".into()),
                    ..Default::default()
                },
                MediaKind::Video => MediaInfo {
                    width: Some(1920),
                    height: Some(1080),
                    duration_ms: Some(10_000),
                    codec: Some("h264".into()),
                    ..Default::default()
                },
                MediaKind::Audio => MediaInfo {
                    duration_ms: Some(180_000),
                    artist: Some("灯野".into()),
                    album: Some("夜".into()),
                    ..Default::default()
                },
            })
        }
        fn image_thumbnails(&self, _: &Path, out: &ThumbTargets) -> Result<(), MediaError> {
            touch(&out.large, b"L")?;
            touch(&out.small, b"S")
        }
        fn video_thumbnails(&self, _: &Path, _: f64, out: &ThumbTargets) -> Result<(), MediaError> {
            touch(&out.large, b"VL")?;
            touch(&out.small, b"VS")
        }
        fn audio_artwork(
            &self,
            path: &Path,
            art: &Path,
            out: &ThumbTargets,
        ) -> Result<bool, MediaError> {
            if path.to_string_lossy().contains("noart") {
                return Ok(false);
            }
            touch(art, b"A")?;
            touch(&out.large, b"AL")?;
            touch(&out.small, b"AS")?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoArt;
    impl MediaBackend for NoArt {
        fn probe(&self, _: &Path, _: MediaKind) -> Result<MediaInfo, MediaError> {
            Ok(MediaInfo {
                width: Some(1),
                ..Default::default()
            })
        }
        fn image_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<(), MediaError> {
            Err(MediaError::Failed("壊れています".into()))
        }
        fn video_thumbnails(&self, _: &Path, _: f64, _: &ThumbTargets) -> Result<(), MediaError> {
            Err(MediaError::Unsupported)
        }
        fn audio_artwork(&self, _: &Path, _: &Path, _: &ThumbTargets) -> Result<bool, MediaError> {
            Ok(false)
        }
    }

    #[test]
    fn chain_falls_through_unsupported_only() {
        let chain = ChainBackend::new(vec![Arc::new(NoBackend), Arc::new(NoArt)]);
        assert_eq!(
            chain.probe(Path::new("x"), MediaKind::Image).unwrap().width,
            Some(1)
        );
        let targets = ThumbTargets {
            large: "l".into(),
            small: "s".into(),
        };
        // 失敗は次へ回さない
        assert!(matches!(
            chain.image_thumbnails(Path::new("x"), &targets),
            Err(MediaError::Failed(_))
        ));
        assert!(matches!(
            chain.video_thumbnails(Path::new("x"), 1.0, &targets),
            Err(MediaError::Unsupported)
        ));
    }

    #[test]
    fn chain_keeps_looking_for_artwork() {
        let dir = tempfile::tempdir().unwrap();
        let targets = ThumbTargets {
            large: dir.path().join("l.jpg"),
            small: dir.path().join("s.jpg"),
        };
        let art = dir.path().join("a.jpg");
        let chain = ChainBackend::new(vec![Arc::new(NoArt), Arc::new(fake::FakeBackend)]);
        assert!(
            chain
                .audio_artwork(Path::new("song.flac"), &art, &targets)
                .unwrap()
        );
        let none = ChainBackend::new(vec![Arc::new(NoArt), Arc::new(NoBackend)]);
        assert!(
            !none
                .audio_artwork(Path::new("song.flac"), &art, &targets)
                .unwrap()
        );
    }

    #[test]
    fn codec_summary_is_json() {
        let info = MediaInfo {
            codec: Some("hevc".into()),
            ..Default::default()
        };
        assert_eq!(info.codec_json().as_deref(), Some(r#"{"codec":"hevc"}"#));
        assert_eq!(MediaInfo::default().codec_json(), None);
    }
}
