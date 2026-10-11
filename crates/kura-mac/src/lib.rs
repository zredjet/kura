//! Kura の macOS 専用機能。Swift ヘルパー（`swift/`）を C 互換関数として呼ぶ。
//!
//! - `reduce_transparency`：「透明度を下げる」の設定
//! - `MacBackend`：`MediaBackend` の macOS 実装（ImageIO / AVFoundation）
//! - `media_backend`：macOS の実装で扱えない形式を ffmpeg に回す組み合わせ
//! - `open_in_player`：外部プレイヤー（mpv / QuickTime Player）で開く
//! - `find_chrome`・`snapshot_webview`：キャプチャ用の Google Chrome の場所と、WKWebView の表示範囲の撮影
//!
//! macOS 以外では同じ関数・型が「扱えない」を返すだけの空の実装になり、ワークスペース全体がビルドできる。

mod browser;
mod external;

pub use browser::{
    SnapshotDone, SnapshotInfo, find_chrome, frontmost_pid, play_capture_sound, snapshot_webview,
};
pub use external::{ExternalPlayer, open_in_player};

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
        pub fn kura_image_thumbnails(
            path: *const c_char,
            out_large: *const c_char,
            out_small: *const c_char,
            large_px: i32,
            small_px: i32,
        ) -> i32;
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
        pub fn kura_probe(path: *const c_char, kind: i32) -> *mut c_char;
        pub fn kura_free_string(pointer: *mut c_char);
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
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    const OK: i32 = 0;
    const UNSUPPORTED: i32 = 1;
    const NO_ARTWORK: i32 = 3;

    fn c_path(path: &Path) -> Result<CString, MediaError> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            MediaError::Failed(format!("パスに NUL 文字があります: {}", path.display()))
        })
    }

    fn check(code: i32) -> Result<(), MediaError> {
        match code {
            OK => Ok(()),
            UNSUPPORTED => Err(MediaError::Unsupported),
            _ => Err(MediaError::Failed("macOS での処理に失敗しました".into())),
        }
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
            let path = c_path(path)?;
            // SAFETY: path は NUL 終端。戻り値は NULL か strdup した文字列で、kura_free_string で解放する。
            let pointer = unsafe { ffi::kura_probe(path.as_ptr(), kind_code(kind)) };
            if pointer.is_null() {
                return Err(MediaError::Unsupported);
            }
            // SAFETY: NULL でない戻り値は NUL 終端の UTF-8（JSON）。
            let json = unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: kura_probe が返したポインタを一度だけ解放する。
            unsafe { ffi::kura_free_string(pointer) };
            serde_json::from_str(&json).map_err(|e| MediaError::Failed(e.to_string()))
        }

        fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<(), MediaError> {
            let (src, large, small) = (c_path(path)?, c_path(&out.large)?, c_path(&out.small)?);
            // SAFETY: すべて NUL 終端の文字列。Swift 側は読むだけ。
            check(unsafe {
                ffi::kura_image_thumbnails(
                    src.as_ptr(),
                    large.as_ptr(),
                    small.as_ptr(),
                    LARGE_PX,
                    SMALL_PX,
                )
            })
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
