//! Kura の macOS 専用機能。Swift ヘルパー（`swift/`）を C 互換関数として呼ぶ。
//!
//! - `reduce_transparency`：「透明度を下げる」の設定
//! - `MacBackend`：`MediaBackend` の macOS 実装（ImageIO / AVFoundation）
//! - `media_backend`：macOS の実装で扱えない形式を ffmpeg に回す組み合わせ
//! - `image_thumbnails_keep_alpha`・`font_thumbnails`・`probe_font`：M11 の ImageIO のアルファを保つサムネイル、
//!   CoreText のフォントの見本とフォントの情報（`thumbs`）
//! - `open_in_player`：外部プレイヤー（mpv / QuickTime Player）で開く
//! - `find_chrome`・`snapshot_webview`：キャプチャ用の Google Chrome の場所と、WKWebView の表示範囲の撮影
//! - `reveal_in_finder`・`volume_capacity`・`physical_memory`：Finder で見せる、ボリュームの容量、物理メモリ（編集のメモリの予算）
//! - `codec`：編集の Platform の段のコーデック（ImageIO・AudioToolbox の読み取り、AudioToolbox の AAC の書き出し）と
//!   `codec_set`（固定の段に足した組）・`full_codec_set`（さらに ffmpeg の段を足したアプリの組）・`codec_caps`（能力と環境）
//!
//! macOS 以外では同じ関数・型が「扱えない」を返すだけの空の実装になり、ワークスペース全体がビルドできる。

mod abi;
#[cfg(target_os = "macos")]
mod bridge;
mod browser;
pub mod codec;
mod external;
mod files;
mod thumbs;

pub use browser::{
    SnapshotDone, SnapshotInfo, find_chrome, frontmost_pid, play_capture_sound, snapshot_webview,
};
pub use codec::{
    MacCaps, codec_caps, codec_set, full_codec_set, platform_decoders, platform_encoders,
    platform_status,
};
pub use external::{ExternalPlayer, open_in_player};
pub use files::{
    VolumeCapacity, keep_webview_focus_on_fullscreen, physical_memory, reveal_in_finder,
    volume_capacity,
};
pub use thumbs::{
    FONT_SAMPLES, FontInfo, FontProbe, ThumbFile, ThumbStems, font_thumbnails,
    image_thumbnails_keep_alpha, probe_font,
};

use std::path::Path;
use std::sync::Arc;

use kura_core::import::MediaKind;
use kura_core::media::{
    ChainBackend, FfmpegBackend, MediaBackend, MediaError, MediaInfo, ThumbTargets,
};

/// サムネイルの長辺（docs/architecture.md §4：s = 512、l = 1600）。
const LARGE_PX: i32 = 1600;
const SMALL_PX: i32 = 512;

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::c_char;

    unsafe extern "C" {
        pub fn kura_reduce_transparency() -> bool;
        pub fn kura_video_thumbnails(
            path: *const c_char,
            at_sec: f64,
            out_large: *const c_char,
            out_small: *const c_char,
            large_px: i32,
            small_px: i32,
        ) -> i32;
        pub fn kura_audio_artwork(
            path: *const c_char,
            out_art: *const c_char,
            out_large: *const c_char,
            out_small: *const c_char,
            large_px: i32,
            small_px: i32,
        ) -> i32;
    }
}

/// 「透明度を下げる」がオンか。macOS 以外では常に false。
pub fn reduce_transparency() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: 引数を取らず bool を返すだけの Swift 関数（Appearance.swift）。
        unsafe { ffi::kura_reduce_transparency() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// ffmpeg を先に試す拡張子。FLAC・Ogg・Opus のタグとジャケットは AVFoundation では読めず、
/// mkv・webm・avi は AVFoundation では開けないため。
const FFMPEG_FIRST: [&str; 6] = ["flac", "ogg", "opus", "mkv", "webm", "avi"];

/// macOS の実装と ffmpeg（見つかれば）を、形式に合わせた順で試す。
pub fn media_backend() -> Arc<dyn MediaBackend> {
    let mac: Arc<dyn MediaBackend> = Arc::new(MacBackend);
    let ffmpeg: Option<Arc<dyn MediaBackend>> =
        FfmpegBackend::detect().map(|f| Arc::new(f) as Arc<dyn MediaBackend>);
    let chain = |first: Vec<Arc<dyn MediaBackend>>| ChainBackend::new(first);
    Arc::new(RoutedBackend {
        mac_first: chain(std::iter::once(mac.clone()).chain(ffmpeg.clone()).collect()),
        ffmpeg_first: chain(ffmpeg.into_iter().chain(std::iter::once(mac)).collect()),
    })
}

/// 拡張子で試す順を変える。
struct RoutedBackend {
    mac_first: ChainBackend,
    ffmpeg_first: ChainBackend,
}

impl RoutedBackend {
    fn pick(&self, path: &Path) -> &ChainBackend {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if FFMPEG_FIRST.contains(&ext.as_str()) {
            &self.ffmpeg_first
        } else {
            &self.mac_first
        }
    }
}

impl MediaBackend for RoutedBackend {
    fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError> {
        self.pick(path).probe(path, kind)
    }
    fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<(), MediaError> {
        self.pick(path).image_thumbnails(path, out)
    }
    fn video_thumbnails(
        &self,
        path: &Path,
        at_sec: f64,
        out: &ThumbTargets,
    ) -> Result<(), MediaError> {
        self.pick(path).video_thumbnails(path, at_sec, out)
    }
    fn audio_artwork(
        &self,
        path: &Path,
        art: &Path,
        out: &ThumbTargets,
    ) -> Result<bool, MediaError> {
        self.pick(path).audio_artwork(path, art, out)
    }
}

/// `MediaBackend` の macOS 実装（ImageIO / AVFoundation）。
pub struct MacBackend;

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use crate::thumbs::mac::{NO_ARTWORK, c_path, check, probe_json};
    use crate::thumbs::{ThumbFile, ThumbStems};

    /// 今の `ThumbTargets`（`.jpg` のパス）を拡張子のない書き先にする。`.jpg` でなければ断る。
    fn jpeg_stems(out: &ThumbTargets) -> Result<ThumbStems, MediaError> {
        let stem = |p: &Path| {
            if p.extension().is_some_and(|e| e == "jpg") {
                Ok(p.with_extension(""))
            } else {
                Err(MediaError::Failed(format!(
                    "サムネイルの書き先が .jpg ではありません: {}",
                    p.display()
                )))
            }
        };
        Ok(ThumbStems {
            large: stem(&out.large)?,
            small: stem(&out.small)?,
        })
    }

    fn kind_code(kind: MediaKind) -> i32 {
        match kind {
            MediaKind::Image => 0,
            MediaKind::Video => 1,
            MediaKind::Audio => 2,
        }
    }

    impl MediaBackend for MacBackend {
        fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError> {
            let json = probe_json(path, kind_code(kind))?;
            serde_json::from_str(&json).map_err(|e| MediaError::Failed(e.to_string()))
        }

        fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<(), MediaError> {
            // 今の道（ThumbTargets は .jpg）はアルファを保たずに JPEG だけを書く。PNG の道は image_thumbnails_keep_alpha
            match crate::thumbs::mac::image_thumbnails(path, &jpeg_stems(out)?, false)? {
                ThumbFile::Jpeg => Ok(()),
                ThumbFile::Png => Err(MediaError::Failed(
                    "JPEG を求めたのに PNG が書かれました".into(),
                )),
            }
        }

        fn video_thumbnails(
            &self,
            path: &Path,
            at_sec: f64,
            out: &ThumbTargets,
        ) -> Result<(), MediaError> {
            let (src, large, small) = (c_path(path)?, c_path(&out.large)?, c_path(&out.small)?);
            // SAFETY: すべて NUL 終端の文字列。Swift 側は読むだけ。
            check(unsafe {
                ffi::kura_video_thumbnails(
                    src.as_ptr(),
                    at_sec,
                    large.as_ptr(),
                    small.as_ptr(),
                    LARGE_PX,
                    SMALL_PX,
                )
            })
        }

        fn audio_artwork(
            &self,
            path: &Path,
            art: &Path,
            out: &ThumbTargets,
        ) -> Result<bool, MediaError> {
            let (src, art, large, small) = (
                c_path(path)?,
                c_path(art)?,
                c_path(&out.large)?,
                c_path(&out.small)?,
            );
            // SAFETY: すべて NUL 終端の文字列。Swift 側は読むだけ。
            let code = unsafe {
                ffi::kura_audio_artwork(
                    src.as_ptr(),
                    art.as_ptr(),
                    large.as_ptr(),
                    small.as_ptr(),
                    LARGE_PX,
                    SMALL_PX,
                )
            };
            if code == NO_ARTWORK {
                return Ok(false);
            }
            check(code).map(|()| true)
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl MediaBackend for MacBackend {
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

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn reduce_transparency_is_callable() {
        let _ = reduce_transparency();
    }

    /// ffmpeg と sips でテスト用のメディアを作る（ffmpeg がなければ None）。
    struct Media {
        _dir: tempfile::TempDir,
        jpeg: PathBuf,
        heic: PathBuf,
        mov: PathBuf,
        mkv: PathBuf,
        flac: PathBuf,
        flac_plain: PathBuf,
    }

    fn ffmpeg(args: &[&str]) {
        let status = Command::new(kura_core::tools::find("ffmpeg").unwrap())
            .args(["-v", "error", "-y"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg {args:?}");
    }

    fn media() -> Option<Media> {
        kura_core::tools::find("ffmpeg")?;
        let dir = tempfile::tempdir().unwrap();
        let p = |name: &str| dir.path().join(name);
        let s = |path: &PathBuf| path.to_string_lossy().into_owned();
        let (jpeg, heic, mov, mkv, flac, flac_plain) = (
            p("photo.jpg"),
            p("photo.heic"),
            p("clip.mov"),
            p("clip.mkv"),
            p("song.flac"),
            p("plain.flac"),
        );
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=1200x800",
            "-frames:v",
            "1",
            &s(&jpeg),
        ]);
        let status = Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(&jpeg)
            .arg("--out")
            .arg(&heic)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=24:duration=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            &s(&mov),
        ]);
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=10:duration=2",
            "-c:v",
            "mpeg4",
            &s(&mkv),
        ]);
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-i",
            &s(&jpeg),
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:a",
            "flac",
            "-c:v",
            "mjpeg",
            "-disposition:v",
            "attached_pic",
            "-metadata",
            "artist=灯野",
            "-metadata",
            "album=夜の作業",
            &s(&flac),
        ]);
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:duration=1",
            "-c:a",
            "flac",
            &s(&flac_plain),
        ]);
        Some(Media {
            _dir: dir,
            jpeg,
            heic,
            mov,
            mkv,
            flac,
            flac_plain,
        })
    }

    fn targets(dir: &Path) -> ThumbTargets {
        ThumbTargets {
            large: dir.join("t/l.jpg"),
            small: dir.join("t/s.jpg"),
        }
    }

    fn long_edge(path: &Path) -> u32 {
        let out = Command::new("sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split(':').nth(1)?.trim().parse::<u32>().ok())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn probes_images_videos_and_audio() {
        let Some(m) = media() else { return };
        let image = MacBackend.probe(&m.heic, MediaKind::Image).unwrap();
        assert_eq!((image.width, image.height), (Some(1200), Some(800)));
        assert_eq!(image.codec.as_deref(), Some("heic"));

        let video = MacBackend.probe(&m.mov, MediaKind::Video).unwrap();
        assert_eq!((video.width, video.height), (Some(640), Some(360)));
        assert!((2900..=3100).contains(&video.duration_ms.unwrap()));
        assert_eq!(video.codec.as_deref(), Some("h264"));

        // mkv は macOS では扱えないので ffmpeg に回る
        assert!(matches!(
            MacBackend.probe(&m.mkv, MediaKind::Video),
            Err(MediaError::Unsupported)
        ));
        let chained = media_backend().probe(&m.mkv, MediaKind::Video).unwrap();
        assert_eq!((chained.width, chained.height), (Some(320), Some(240)));

        let audio = media_backend().probe(&m.flac, MediaKind::Audio).unwrap();
        assert!((1900..=2100).contains(&audio.duration_ms.unwrap()));
        assert_eq!(audio.artist.as_deref(), Some("灯野"));
        assert_eq!(audio.album.as_deref(), Some("夜の作業"));
    }

    /// 完了条件：HEIC・MOV・FLAC のサムネイルが出る。
    #[test]
    fn makes_heic_mov_and_flac_thumbnails() {
        let Some(m) = media() else { return };
        let dir = tempfile::tempdir().unwrap();
        let backend = media_backend();

        let heic = targets(&dir.path().join("heic"));
        backend.image_thumbnails(&m.heic, &heic).unwrap();
        assert_eq!(long_edge(&heic.large), 1200); // 元より大きくはしない
        assert_eq!(long_edge(&heic.small), 512);

        let jpeg = targets(&dir.path().join("jpeg"));
        backend.image_thumbnails(&m.jpeg, &jpeg).unwrap();
        assert!(jpeg.small.is_file());

        let mov = targets(&dir.path().join("mov"));
        backend.video_thumbnails(&m.mov, 0.3, &mov).unwrap();
        assert_eq!(long_edge(&mov.large), 640);
        assert_eq!(long_edge(&mov.small), 512);

        let mkv = targets(&dir.path().join("mkv"));
        backend.video_thumbnails(&m.mkv, 0.2, &mkv).unwrap();
        assert!(mkv.small.is_file());

        let flac = targets(&dir.path().join("flac"));
        let art = dir.path().join("art.jpg");
        assert!(backend.audio_artwork(&m.flac, &art, &flac).unwrap());
        assert!(art.is_file() && flac.large.is_file() && flac.small.is_file());

        let plain = targets(&dir.path().join("plain"));
        assert!(
            !backend
                .audio_artwork(&m.flac_plain, &dir.path().join("none.jpg"), &plain)
                .unwrap()
        );
    }

    #[test]
    fn broken_files_fail_instead_of_crashing() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.jpg");
        std::fs::write(&bad, b"not an image").unwrap();
        let result = media_backend().image_thumbnails(&bad, &targets(dir.path()));
        assert!(result.is_err());
    }
}
