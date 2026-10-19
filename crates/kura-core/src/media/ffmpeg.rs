//! ffmpeg / ffprobe による実装（macOS で扱えない mkv・webm・ogg などの受け皿）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::{MediaBackend, MediaError, MediaInfo, ThumbFormat, ThumbTargets};
use crate::import::MediaKind;
use crate::time::parse_iso8601_ms;
use crate::tools::{find, run_with_timeout};

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const THUMB_TIMEOUT: Duration = Duration::from_secs(60);

/// ffmpeg / ffprobe を外部プロセスとして呼ぶ。
pub struct FfmpegBackend {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl FfmpegBackend {
    /// ffmpeg と ffprobe が見つかれば作る。
    pub fn detect() -> Option<Self> {
        Some(Self {
            ffmpeg: find("ffmpeg")?,
            ffprobe: find("ffprobe")?,
        })
    }

    /// `src` の 1 フレームを、長辺 `max_px` 以下の JPEG にする（`seek` 秒の地点、`map` で入力を選ぶ）。
    fn frame(
        &self,
        src: &Path,
        out: &Path,
        max_px: u32,
        seek: Option<f64>,
        map: Option<&str>,
    ) -> Result<(), MediaError> {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(["-v", "error", "-y"]);
        if let Some(seek) = seek {
            cmd.args(["-ss", &format!("{seek:.3}")]);
        }
        cmd.arg("-i").arg(src);
        if let Some(map) = map {
            cmd.args(["-map", map]);
        }
        let scale = format!(
            "scale='min({max_px},iw)':'min({max_px},ih)':force_original_aspect_ratio=decrease"
        );
        cmd.args(["-an", "-frames:v", "1", "-vf", &scale, "-q:v", "3"])
            .arg(out);
        let output = run_with_timeout(&mut cmd, THUMB_TIMEOUT)?;
        if output.status.success() && out.is_file() {
            Ok(())
        } else {
            Err(MediaError::Failed(stderr_line(&output.stderr)))
        }
    }

    /// 長辺 1600 の画像から 512 を作る（どちらも JPEG）。
    fn small_from_large(&self, out: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
        let (large, small) = out.files(ThumbFormat::Jpeg);
        self.frame(&large, &small, 512, None, None)?;
        Ok(ThumbFormat::Jpeg)
    }
}

fn stderr_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("ffmpeg が失敗しました")
        .to_string()
}

impl MediaBackend for FfmpegBackend {
    fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError> {
        // フォント・3D は ffprobe では読まない（段 3 の専用の道で読む）
        if !reads_with_ffprobe(kind) {
            return Err(MediaError::Unsupported);
        }
        let output = run_with_timeout(
            Command::new(&self.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-print_format",
                    "json",
                    "-show_format",
                    "-show_streams",
                ])
                .arg(path),
            PROBE_TIMEOUT,
        )?;
        if !output.status.success() {
            return Err(MediaError::Failed(stderr_line(&output.stderr)));
        }
        parse_ffprobe(&String::from_utf8_lossy(&output.stdout), kind)
    }

    fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
        // ffmpeg の道はいつも JPEG（透明は落ちる。macOS で読めない形式の受け皿）
        self.frame(path, &out.files(ThumbFormat::Jpeg).0, 1600, None, None)?;
        self.small_from_large(out)
    }

    fn video_thumbnails(
        &self,
        path: &Path,
        at_sec: f64,
        out: &ThumbTargets,
    ) -> Result<ThumbFormat, MediaError> {
        let large = out.files(ThumbFormat::Jpeg).0;
        // 指定の地点が取れなければ先頭で作り直す
        if self
            .frame(path, &large, 1600, Some(at_sec.max(0.0)), Some("0:v:0"))
            .is_err()
        {
            self.frame(path, &large, 1600, None, Some("0:v:0"))?;
        }
        self.small_from_large(out)
    }

    fn audio_artwork(
        &self,
        path: &Path,
        art: &Path,
        out: &ThumbTargets,
    ) -> Result<Option<ThumbFormat>, MediaError> {
        // 埋め込まれた画像（attached picture）は動画のストリームとして見える
        let info = self.probe(path, MediaKind::Audio);
        let has_picture = info.is_ok() && {
            let output = run_with_timeout(
                Command::new(&self.ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-select_streams",
                        "v",
                        "-show_entries",
                        "stream=index",
                        "-of",
                        "csv=p=0",
                    ])
                    .arg(path),
                PROBE_TIMEOUT,
            )?;
            !String::from_utf8_lossy(&output.stdout).trim().is_empty()
        };
        if !has_picture {
            return Ok(None);
        }
        self.frame(path, art, 1600, None, Some("0:v:0"))?;
        let large = out.files(ThumbFormat::Jpeg).0;
        if let Some(parent) = large.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(art, &large)?;
        self.small_from_large(out).map(Some)
    }

    fn font_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
        // フォントの見本は CoreText（kura-mac）だけ
        Err(MediaError::Unsupported)
    }
}

/// ffprobe で情報を読む種類か（画像・動画・音楽）。
fn reads_with_ffprobe(kind: MediaKind) -> bool {
    match kind {
        MediaKind::Image | MediaKind::Video | MediaKind::Audio => true,
        MediaKind::Font | MediaKind::Model => false,
    }
}

/// ffprobe の JSON（`-show_format -show_streams`）からメディア情報を取り出す。
/// 映像の大きさは画像・動画で、主なコーデックは画像・動画は映像、音楽は音声から、長さは動画・音楽で読む。
pub fn parse_ffprobe(json: &str, kind: MediaKind) -> Result<MediaInfo, MediaError> {
    let root: Value = serde_json::from_str(json).map_err(|e| MediaError::Failed(e.to_string()))?;
    let streams = root["streams"].as_array().cloned().unwrap_or_default();
    let is_picture = |s: &Value| s["disposition"]["attached_pic"].as_i64() == Some(1);
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && !is_picture(s));
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");

    // (映像の大きさ・コーデックを読む, 音声のコーデックを読む)
    let (picture, sound) = match kind {
        MediaKind::Image | MediaKind::Video => (true, false),
        MediaKind::Audio => (false, true),
        MediaKind::Font | MediaKind::Model => (false, false),
    };
    let mut info = MediaInfo::default();
    if let Some(v) = video.filter(|_| picture) {
        let (w, h) = (v["width"].as_i64(), v["height"].as_i64());
        let rotation = v["side_data_list"]
            .as_array()
            .and_then(|list| list.iter().find_map(|d| d["rotation"].as_f64()))
            .or_else(|| v["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
            .unwrap_or(0.0);
        let quarter = (rotation.round() as i64).rem_euclid(180) == 90;
        (info.width, info.height) = if quarter { (h, w) } else { (w, h) };
        info.codec = v["codec_name"].as_str().map(str::to_string);
    }
    if sound {
        info.codec = audio
            .and_then(|a| a["codec_name"].as_str())
            .map(str::to_string);
    }
    if kind.has_duration() {
        info.duration_ms = root["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse::<f64>().ok())
            .map(|d| (d * 1000.0).round() as i64);
    }
    let tag = |name: &str| -> Option<String> {
        [
            &root["format"]["tags"],
            audio.map(|a| &a["tags"]).unwrap_or(&Value::Null),
        ]
        .into_iter()
        .filter_map(Value::as_object)
        .flat_map(|tags| tags.iter())
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    };
    info.artist = tag("artist");
    info.album = tag("album");
    info.title = tag("title");
    info.captured_at = tag("creation_time").and_then(|t| parse_iso8601_ms(&t));
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rotated_video() {
        let json = r#"{
          "streams": [
            {"codec_type": "video", "codec_name": "hevc", "width": 1920, "height": 1080,
             "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}]},
            {"codec_type": "audio", "codec_name": "aac"}
          ],
          "format": {"duration": "12.3456", "tags": {"creation_time": "2026-08-30T09:22:01.000000Z"}}
        }"#;
        let info = parse_ffprobe(json, MediaKind::Video).unwrap();
        assert_eq!((info.width, info.height), (Some(1080), Some(1920)));
        assert_eq!(info.duration_ms, Some(12_346));
        assert_eq!(info.codec.as_deref(), Some("hevc"));
        assert_eq!(info.captured_at, parse_iso8601_ms("2026-08-30T09:22:01Z"));
    }

    #[test]
    fn reads_audio_tags_and_ignores_cover_art() {
        let json = r#"{
          "streams": [
            {"codec_type": "audio", "codec_name": "flac"},
            {"codec_type": "video", "codec_name": "mjpeg", "width": 600, "height": 600, "disposition": {"attached_pic": 1}}
          ],
          "format": {"duration": "258.0", "tags": {"ARTIST": "灯野", "ALBUM": "夜の作業", "TITLE": "rain"}}
        }"#;
        let info = parse_ffprobe(json, MediaKind::Audio).unwrap();
        assert_eq!(info.width, None);
        assert_eq!(info.duration_ms, Some(258_000));
        assert_eq!(info.codec.as_deref(), Some("flac"));
        assert_eq!(info.artist.as_deref(), Some("灯野"));
        assert_eq!(info.album.as_deref(), Some("夜の作業"));
        assert_eq!(info.title.as_deref(), Some("rain"));
    }

    #[test]
    fn images_have_no_duration() {
        let json = r#"{"streams": [{"codec_type": "video", "codec_name": "webp", "width": 800, "height": 600}],
                       "format": {"duration": "0.04"}}"#;
        let info = parse_ffprobe(json, MediaKind::Image).unwrap();
        assert_eq!(
            (info.width, info.height, info.duration_ms),
            (Some(800), Some(600), None)
        );
        assert!(parse_ffprobe("not json", MediaKind::Image).is_err());
    }

    /// フォント・3D は ffprobe を起動せずに断る（次の実装か、段 3 の専用の道に回す）。
    #[test]
    fn fonts_and_models_are_not_probed_with_ffprobe() {
        let backend = FfmpegBackend {
            ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            ffprobe: PathBuf::from("/nonexistent/ffprobe"),
        };
        for kind in [MediaKind::Font, MediaKind::Model] {
            assert!(matches!(
                backend.probe(Path::new("x.ttf"), kind),
                Err(MediaError::Unsupported)
            ));
        }
        // 起動しようとした種類は、ないファイルの失敗になる（Unsupported ではない）
        assert!(!matches!(
            backend.probe(Path::new("x.jpg"), MediaKind::Image),
            Err(MediaError::Unsupported)
        ));
        let json = r#"{"streams": [{"codec_type": "video", "codec_name": "png", "width": 8, "height": 8}],
                       "format": {"duration": "1.0"}}"#;
        assert_eq!(
            parse_ffprobe(json, MediaKind::Font).unwrap(),
            MediaInfo::default()
        );
    }

    /// ffmpeg があれば実際に動かす（ない環境では何もしない）。
    #[test]
    fn real_ffmpeg_makes_thumbnails_when_available() {
        let Some(backend) = FfmpegBackend::detect() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mkv");
        let status = Command::new(&backend.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=640x360:rate=10:duration=2",
                "-c:v",
                "mpeg4",
            ])
            .arg(&video)
            .status()
            .unwrap();
        assert!(status.success());
        let info = backend.probe(&video, MediaKind::Video).unwrap();
        assert_eq!((info.width, info.height), (Some(640), Some(360)));
        assert!(info.duration_ms.unwrap() >= 1900);

        let out = ThumbTargets {
            large: dir.path().join("l"),
            small: dir.path().join("s"),
        };
        assert_eq!(
            backend.video_thumbnails(&video, 0.2, &out).unwrap(),
            ThumbFormat::Jpeg
        );
        let (large, small) = out.files(ThumbFormat::Jpeg);
        assert!(large.is_file() && small.is_file());

        let art = dir.path().join("art.jpg");
        assert!(backend.audio_artwork(&video, &art, &out).unwrap().is_none() || art.is_file());
        assert!(matches!(
            backend.font_thumbnails(&video, &out),
            Err(MediaError::Unsupported)
        ));
    }
}
