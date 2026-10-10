//! 拡張子からメディアの種類と MIME を決める（docs/architecture.md §6 の対応拡張子）。

use serde::{Deserialize, Serialize};

/// メディアの種類（items.kind）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl MediaKind {
    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }
}

/// 拡張子（大文字・小文字は問わない）から種類を決める。対応していなければ None。
pub fn classify(ext: &str) -> Option<MediaKind> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "tif" | "tiff"
        | "bmp" => MediaKind::Image,
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" => MediaKind::Video,
        "mp3" | "m4a" | "aac" | "flac" | "wav" | "aif" | "aiff" | "ogg" | "opus" => {
            MediaKind::Audio
        }
        _ => return None,
    })
}

/// 拡張子から MIME を決める。
pub fn mime_for(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "aif" | "aiff" => "audio/aiff",
        "ogg" => "audio/ogg",
        // Ogg に入った Opus（RFC 7845）。WebKit は audio/opus では再生しない
        "opus" => "audio/ogg",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_supported_extensions_case_insensitively() {
        assert_eq!(classify("JPG"), Some(MediaKind::Image));
        assert_eq!(classify("heic"), Some(MediaKind::Image));
        assert_eq!(classify("MKV"), Some(MediaKind::Video));
        assert_eq!(classify("flac"), Some(MediaKind::Audio));
        assert_eq!(classify("txt"), None);
        assert_eq!(classify(""), None);
    }

    #[test]
    fn every_supported_extension_has_a_mime() {
        for ext in [
            "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "avif", "tif", "tiff", "bmp",
            "mp4", "m4v", "mov", "webm", "mkv", "avi", "mp3", "m4a", "aac", "flac", "wav", "aif",
            "aiff", "ogg", "opus",
        ] {
            assert!(classify(ext).is_some(), "{ext}");
            assert!(mime_for(ext).is_some(), "{ext}");
        }
        // WebKit が再生できる形で配る
        assert_eq!(mime_for("opus"), Some("audio/ogg"));
    }
}
