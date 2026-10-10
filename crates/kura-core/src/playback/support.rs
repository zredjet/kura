//! 再生可否の表と、プロキシが要るかの判定（docs/architecture.md §8）。DB にも ffmpeg にも触らない純粋関数。
//!
//! フロントが起動時に `canPlayType` で「入れ物 × コーデック」の表を作って渡す。ffprobe で読んだ
//! 映像・音声のコーデックをこの表と照らし、元のまま再生できるか、mp4 への入れ替え（remux）で足りるか、
//! 変換（映像は VideoToolbox、音声は AAC）が要るかを決める。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 入れ物（拡張子から決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    /// mp4・m4v・mov・m4a（WebKit がそのまま読める QuickTime 系）。
    Mp4,
    Webm,
    Ogg,
    Mp3,
    Flac,
    Wav,
    Aiff,
    Aac,
    /// mkv・avi・wmv など、WebKit が読めない入れ物。
    Other,
}

impl Container {
    /// 拡張子から決める。
    pub fn from_ext(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "mp4" | "m4v" | "mov" | "m4a" => Self::Mp4,
            "webm" => Self::Webm,
            "ogg" | "oga" | "ogv" | "opus" => Self::Ogg,
            "mp3" => Self::Mp3,
            "flac" => Self::Flac,
            "wav" => Self::Wav,
            "aif" | "aiff" => Self::Aiff,
            "aac" => Self::Aac,
            _ => Self::Other,
        }
    }
}

/// 表の 1 行（フロントの `canPlayType` の結果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportEntry {
    pub container: Container,
    /// ffprobe のコーデック名（`h264`、`hevc`、`vp9`、`aac`、`opus`、`pcm` など）。
    pub codec: String,
    pub playable: bool,
}

/// 再生できる「入れ物 × コーデック」の集合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaybackSupport {
    playable: BTreeSet<(Container, String)>,
}

impl PlaybackSupport {
    /// フロントの表から作る。
    pub fn from_entries(entries: &[SupportEntry]) -> Self {
        Self {
            playable: entries
                .iter()
                .filter(|e| e.playable)
                .map(|e| (e.container, normalize_codec(&e.codec)))
                .collect(),
        }
    }

    /// `container` に入った `codec` を再生できるか。
    pub fn can(&self, container: Container, codec: &str) -> bool {
        self.playable.contains(&(container, normalize_codec(codec)))
    }

    /// 表の指紋（表が変わったら判定をやり直すため、ライブラリに記録する）。
    pub fn fingerprint(&self) -> String {
        let text = self
            .playable
            .iter()
            .map(|(c, codec)| format!("{c:?}/{codec}"))
            .collect::<Vec<_>>()
            .join(",");
        blake3::hash(text.as_bytes()).to_hex()[..16].to_string()
    }
}

/// コーデック名をそろえる（PCM は形式によらず `pcm`）。
pub fn normalize_codec(codec: &str) -> String {
    let lower = codec.to_ascii_lowercase();
    if lower.starts_with("pcm_") {
        "pcm".to_string()
    } else {
        lower
    }
}

/// 映像・音声のコーデック（ffprobe で読む。ジャケット画像は映像に数えない）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Streams {
    pub video: Option<String>,
    pub audio: Option<String>,
    /// ffmpeg の `-map 0:<index>` に使う番号。
    pub video_index: Option<i64>,
    pub audio_index: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// ffprobe の JSON（`-show_format -show_streams`）から最初の映像・音声を取り出す。
pub fn parse_streams(json: &str) -> Option<Streams> {
    let root: Value = serde_json::from_str(json).ok()?;
    let streams = root["streams"].as_array()?;
    let is_picture = |s: &Value| s["disposition"]["attached_pic"].as_i64() == Some(1);
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && !is_picture(s));
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    let codec = |s: Option<&Value>| {
        s.and_then(|s| s["codec_name"].as_str())
            .map(normalize_codec)
    };
    Some(Streams {
        video: codec(video),
        audio: codec(audio),
        video_index: video.and_then(|v| v["index"].as_i64()),
        audio_index: audio.and_then(|a| a["index"].as_i64()),
        width: video.and_then(|v| v["width"].as_i64()),
        height: video.and_then(|v| v["height"].as_i64()),
        duration_ms: root["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse::<f64>().ok())
            .map(|d| (d * 1000.0).round() as i64),
    })
}

/// 1 本のストリームの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAction {
    /// ストリームがない。
    None,
    /// そのまま mp4 に入れる。
    Copy,
    /// 変換する（映像は h264、音声は AAC）。
    Encode,
}

/// プロキシの要否。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyPlan {
    /// 元のまま再生できる。
    NotNeeded,
    /// mp4 のプロキシを作る。両方 Copy なら入れ替え（remux）だけ。
    Make {
        video: StreamAction,
        audio: StreamAction,
    },
    /// 映像も音声もない（読めないファイル）。
    Unplayable,
}

impl ProxyPlan {
    /// 入れ替えだけで済むか。
    pub fn is_remux(self) -> bool {
        matches!(
            self,
            Self::Make { video, audio }
                if video != StreamAction::Encode && audio != StreamAction::Encode
        )
    }
}

/// プロキシが要るかを決める。
pub fn plan(container: Container, streams: &Streams, support: &PlaybackSupport) -> ProxyPlan {
    if streams.video.is_none() && streams.audio.is_none() {
        return ProxyPlan::Unplayable;
    }
    let native =
        |codec: &Option<String>| codec.as_deref().is_none_or(|c| support.can(container, c));
    if container != Container::Other && native(&streams.video) && native(&streams.audio) {
        return ProxyPlan::NotNeeded;
    }
    let action = |codec: &Option<String>| match codec.as_deref() {
        None => StreamAction::None,
        Some(c) if support.can(Container::Mp4, c) => StreamAction::Copy,
        Some(_) => StreamAction::Encode,
    };
    ProxyPlan::Make {
        video: action(&streams.video),
        audio: action(&streams.audio),
    }
}

/// 元のまま再生できるはずなのに再生できなかったもの（`proxy_state` を pending に戻したもの）の計画：
/// 表が当てにならないので、ある方はすべて変換する。
pub fn forced_plan(streams: &Streams) -> ProxyPlan {
    let encode = |codec: &Option<String>| {
        if codec.is_some() {
            StreamAction::Encode
        } else {
            StreamAction::None
        }
    };
    if streams.video.is_none() && streams.audio.is_none() {
        return ProxyPlan::Unplayable;
    }
    ProxyPlan::Make {
        video: encode(&streams.video),
        audio: encode(&streams.audio),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS 26 の WebKit に近い表（mkv は読めない、ogg の vorbis は読めない、とする）。
    fn webkit() -> PlaybackSupport {
        let rows: &[(Container, &str)] = &[
            (Container::Mp4, "h264"),
            (Container::Mp4, "hevc"),
            (Container::Mp4, "av1"),
            (Container::Mp4, "aac"),
            (Container::Mp4, "mp3"),
            (Container::Mp4, "alac"),
            (Container::Mp4, "flac"),
            (Container::Mp4, "opus"),
            (Container::Webm, "vp8"),
            (Container::Webm, "vp9"),
            (Container::Webm, "opus"),
            (Container::Webm, "vorbis"),
            (Container::Ogg, "opus"),
            (Container::Mp3, "mp3"),
            (Container::Flac, "flac"),
            (Container::Wav, "pcm"),
        ];
        PlaybackSupport::from_entries(
            &rows
                .iter()
                .map(|(container, codec)| SupportEntry {
                    container: *container,
                    codec: (*codec).to_string(),
                    playable: true,
                })
                .collect::<Vec<_>>(),
        )
    }

    fn streams(video: Option<&str>, audio: Option<&str>) -> Streams {
        Streams {
            video: video.map(str::to_string),
            audio: audio.map(str::to_string),
            ..Streams::default()
        }
    }

    #[test]
    fn decides_by_container_and_codecs() {
        let s = webkit();
        let make = |video, audio| ProxyPlan::Make { video, audio };
        use StreamAction::{Copy, Encode};
        const NONE: StreamAction = StreamAction::None;
        let cases = [
            // 元のまま
            (
                "mp4",
                streams(Some("h264"), Some("aac")),
                ProxyPlan::NotNeeded,
            ),
            (
                "mov",
                streams(Some("hevc"), Some("aac")),
                ProxyPlan::NotNeeded,
            ),
            (
                "webm",
                streams(Some("vp9"), Some("opus")),
                ProxyPlan::NotNeeded,
            ),
            ("ogg", streams(None, Some("opus")), ProxyPlan::NotNeeded),
            (
                "wav",
                streams(None, Some("pcm_s16le")),
                ProxyPlan::NotNeeded,
            ),
            ("flac", streams(None, Some("flac")), ProxyPlan::NotNeeded),
            // mkv は入れ物だけ替える（remux）
            ("mkv", streams(Some("h264"), Some("aac")), make(Copy, Copy)),
            ("mkv", streams(Some("hevc"), Some("opus")), make(Copy, Copy)),
            // 読めないコーデックだけ変換
            (
                "mkv",
                streams(Some("h264"), Some("vorbis")),
                make(Copy, Encode),
            ),
            ("ogg", streams(None, Some("vorbis")), make(NONE, Encode)),
            (
                "webm",
                streams(Some("vp9"), Some("ac3")),
                make(Encode, Encode),
            ),
            (
                "avi",
                streams(Some("mpeg4"), Some("mp3")),
                make(Encode, Copy),
            ),
            (
                "wmv",
                streams(Some("wmv3"), Some("wmav2")),
                make(Encode, Encode),
            ),
            ("mkv", streams(None, None), ProxyPlan::Unplayable),
        ];
        for (ext, streams, expected) in cases {
            assert_eq!(
                plan(Container::from_ext(ext), &streams, &s),
                expected,
                "{ext} {streams:?}"
            );
        }
        assert!(make(Copy, Copy).is_remux());
        assert!(make(NONE, Copy).is_remux());
        assert!(!make(Copy, Encode).is_remux());
        assert!(!ProxyPlan::NotNeeded.is_remux());
    }

    #[test]
    fn forced_plan_encodes_everything() {
        assert_eq!(
            forced_plan(&streams(Some("h264"), Some("aac"))),
            ProxyPlan::Make {
                video: StreamAction::Encode,
                audio: StreamAction::Encode
            }
        );
        assert_eq!(
            forced_plan(&streams(None, Some("flac"))),
            ProxyPlan::Make {
                video: StreamAction::None,
                audio: StreamAction::Encode
            }
        );
        assert_eq!(forced_plan(&streams(None, None)), ProxyPlan::Unplayable);
    }

    #[test]
    fn empty_table_needs_proxies_for_everything() {
        let s = PlaybackSupport::default();
        assert_eq!(
            plan(Container::Mp4, &streams(Some("h264"), Some("aac")), &s),
            ProxyPlan::Make {
                video: StreamAction::Encode,
                audio: StreamAction::Encode
            }
        );
    }

    #[test]
    fn fingerprint_follows_the_playable_set() {
        let a = webkit();
        let mut entries = vec![SupportEntry {
            container: Container::Ogg,
            codec: "vorbis".into(),
            playable: false,
        }];
        assert_eq!(
            PlaybackSupport::from_entries(&entries).fingerprint(),
            PlaybackSupport::default().fingerprint()
        );
        entries[0].playable = true;
        let b = PlaybackSupport::from_entries(&entries);
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), webkit().fingerprint());
        assert_eq!(a.fingerprint().len(), 16);
    }

    #[test]
    fn parses_streams_and_skips_cover_art() {
        let json = r#"{"streams": [
            {"index": 0, "codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}},
            {"index": 1, "codec_type": "audio", "codec_name": "vorbis"},
            {"index": 2, "codec_type": "audio", "codec_name": "aac"}
        ], "format": {"duration": "61.5"}}"#;
        assert_eq!(
            parse_streams(json),
            Some(Streams {
                video: None,
                audio: Some("vorbis".into()),
                video_index: None,
                audio_index: Some(1),
                width: None,
                height: None,
                duration_ms: Some(61_500),
            })
        );
        let json = r#"{"streams": [
            {"codec_type": "video", "codec_name": "H264", "width": 1920, "height": 1080},
            {"codec_type": "subtitle", "codec_name": "ass"},
            {"codec_type": "audio", "codec_name": "pcm_s24le"}
        ], "format": {}}"#;
        let s = parse_streams(json).unwrap();
        assert_eq!(
            (s.video.as_deref(), s.audio.as_deref(), s.width),
            (Some("h264"), Some("pcm"), Some(1920))
        );
        assert_eq!(parse_streams("{ broken"), None);
    }
}
