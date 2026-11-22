//! 形式の表（docs/editing.md §9.2）。拡張子 → items.kind → 値の型 → 保存できるメタデータを 1 か所で決める。
//!
//! 取り込みの判定（kura-core の `import::kind` の `classify`・`mime_for`・`format_id`）と kura:// の MIME もこの表から
//! 作る（M11）。行を足すと取り込める形式が増える。照合のテストは kura-core の
//! `classify_and_mime_follow_the_engine_format_table`。

use serde::Serialize;

use crate::shape::ValueKind;

/// ライブラリのアイテムの種類（items.kind）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Image,
    Video,
    Audio,
    Font,
    Model,
}

/// 透明をどこまで保存できるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AlphaCaps {
    /// 透明を持てない。
    None,
    /// 透明か不透明かの 2 値だけ（GIF）。
    Binary,
    /// 半透明まで。
    Full,
}

/// 保存できるもの（書き出しで失うものを先に知らせる。docs/editing.md §6）。形式の表の値は形式の能力、
/// エンコーダの `caps` の値は、そのエンコーダがその設定で実際に書けるもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaCaps {
    /// ほかの項目（透明・色の数・深さ）の範囲の中の値なら、そのまま戻るか。
    pub lossless: bool,
    pub alpha: AlphaCaps,
    pub palette: bool,
    /// 色の数の上限（GIF は 256。超えると減色する）。
    pub max_colors: Option<u32>,
    pub high_bit_depth: bool,
    pub icc: bool,
    pub exif: bool,
    pub dpi: bool,
    pub text: bool,
    pub animation: bool,
    pub loop_points: bool,
    pub markers: bool,
    pub tags: bool,
}

/// 何も保存できない（形式の表にない形式）。
pub const NONE: MetaCaps = MetaCaps {
    lossless: false,
    alpha: AlphaCaps::None,
    palette: false,
    max_colors: None,
    high_bit_depth: false,
    icc: false,
    exif: false,
    dpi: false,
    text: false,
    animation: false,
    loop_points: false,
    markers: false,
    tags: false,
};

/// 形式 1 つの記述。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatDesc {
    /// 形式の ID（エンコーダ・デコーダが受け付ける名前）。
    pub id: &'static str,
    /// 拡張子（小文字。先頭が代表）。
    pub exts: &'static [&'static str],
    pub mime: &'static str,
    pub item_kind: ItemKind,
    /// 読んだときの値の型（空なら編集しない：フォント・3D・動画）。
    pub decodes_to: &'static [ValueKind],
    pub caps: MetaCaps,
}

const IMG: &[ValueKind] = &[ValueKind::Image];
const IMG_ANIM: &[ValueKind] = &[ValueKind::Image, ValueKind::Frames];
const AUD: &[ValueKind] = &[ValueKind::Audio];

/// すべての形式。
pub const FORMATS: &[FormatDesc] = &[
    FormatDesc {
        id: "png",
        exts: &["png", "apng"],
        mime: "image/png",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: MetaCaps {
            lossless: true,
            alpha: AlphaCaps::Full,
            palette: true,
            high_bit_depth: true,
            icc: true,
            exif: true,
            dpi: true,
            text: true,
            animation: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "jpeg",
        exts: &["jpg", "jpeg"],
        mime: "image/jpeg",
        item_kind: ItemKind::Image,
        decodes_to: IMG,
        caps: MetaCaps {
            icc: true,
            exif: true,
            dpi: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "gif",
        exts: &["gif"],
        mime: "image/gif",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: MetaCaps {
            lossless: true,
            alpha: AlphaCaps::Binary,
            max_colors: Some(256),
            palette: true,
            animation: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "webp",
        exts: &["webp"],
        mime: "image/webp",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: MetaCaps {
            lossless: true,
            alpha: AlphaCaps::Full,
            icc: true,
            exif: true,
            animation: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "aseprite",
        exts: &["aseprite", "ase"],
        mime: "image/x-aseprite",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: NONE,
    },
    FormatDesc {
        id: "psd",
        exts: &["psd", "psb"],
        mime: "image/vnd.adobe.photoshop",
        item_kind: ItemKind::Image,
        decodes_to: IMG,
        caps: NONE,
    },
    FormatDesc {
        id: "svg",
        exts: &["svg"],
        mime: "image/svg+xml",
        item_kind: ItemKind::Image,
        decodes_to: IMG,
        caps: NONE,
    },
    // HEIC・HEIF・AVIF は ImageIO（Platform）で読む。静止画とシーケンス（HEICS・AVIS。キャンバスいっぱいのフレームの列）。
    // 拡張子 heics・avis は取り込みの判定（kura-core）にないので足していない
    FormatDesc {
        id: "heic",
        exts: &["heic"],
        mime: "image/heic",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: NONE,
    },
    FormatDesc {
        id: "heif",
        exts: &["heif"],
        mime: "image/heif",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: NONE,
    },
    FormatDesc {
        id: "avif",
        exts: &["avif"],
        mime: "image/avif",
        item_kind: ItemKind::Image,
        decodes_to: IMG_ANIM,
        caps: NONE,
    },
    FormatDesc {
        id: "tiff",
        exts: &["tif", "tiff"],
        mime: "image/tiff",
        item_kind: ItemKind::Image,
        decodes_to: IMG,
        caps: MetaCaps {
            lossless: true,
            alpha: AlphaCaps::Full,
            high_bit_depth: true,
            icc: true,
            dpi: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "bmp",
        exts: &["bmp"],
        mime: "image/bmp",
        item_kind: ItemKind::Image,
        decodes_to: IMG,
        caps: MetaCaps {
            lossless: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "mp4",
        exts: &["mp4"],
        mime: "video/mp4",
        item_kind: ItemKind::Video,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "m4v",
        exts: &["m4v"],
        mime: "video/x-m4v",
        item_kind: ItemKind::Video,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "mov",
        exts: &["mov"],
        mime: "video/quicktime",
        item_kind: ItemKind::Video,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "webm",
        exts: &["webm"],
        mime: "video/webm",
        item_kind: ItemKind::Video,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "mkv",
        exts: &["mkv"],
        mime: "video/x-matroska",
        item_kind: ItemKind::Video,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "avi",
        exts: &["avi"],
        mime: "video/x-msvideo",
        item_kind: ItemKind::Video,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "wav",
        exts: &["wav"],
        mime: "audio/wav",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps {
            lossless: true,
            high_bit_depth: true,
            loop_points: true,
            markers: true,
            tags: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "aiff",
        exts: &["aif", "aiff"],
        mime: "audio/aiff",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps {
            lossless: true,
            high_bit_depth: true,
            loop_points: true,
            markers: true,
            tags: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "flac",
        exts: &["flac"],
        mime: "audio/flac",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps {
            lossless: true,
            high_bit_depth: true,
            loop_points: true,
            tags: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "mp3",
        exts: &["mp3"],
        mime: "audio/mpeg",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps { tags: true, ..NONE },
    },
    FormatDesc {
        id: "m4a",
        exts: &["m4a"],
        mime: "audio/mp4",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps { tags: true, ..NONE },
    },
    FormatDesc {
        id: "aac",
        exts: &["aac"],
        mime: "audio/aac",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: NONE,
    },
    FormatDesc {
        id: "ogg",
        exts: &["ogg"],
        mime: "audio/ogg",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps {
            loop_points: true,
            tags: true,
            ..NONE
        },
    },
    // Ogg に入った Opus（RFC 7845）。WebKit は audio/opus では再生しない（ADR-73）
    FormatDesc {
        id: "opus",
        exts: &["opus"],
        mime: "audio/ogg",
        item_kind: ItemKind::Audio,
        decodes_to: AUD,
        caps: MetaCaps {
            loop_points: true,
            tags: true,
            ..NONE
        },
    },
    FormatDesc {
        id: "ttf",
        exts: &["ttf"],
        mime: "font/ttf",
        item_kind: ItemKind::Font,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "otf",
        exts: &["otf"],
        mime: "font/otf",
        item_kind: ItemKind::Font,
        decodes_to: &[],
        caps: NONE,
    },
    // TrueType・OpenType のコレクション（複数の書体を 1 つのファイルに持つ）
    FormatDesc {
        id: "ttc",
        exts: &["ttc"],
        mime: "font/collection",
        item_kind: ItemKind::Font,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "woff",
        exts: &["woff"],
        mime: "font/woff",
        item_kind: ItemKind::Font,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "woff2",
        exts: &["woff2"],
        mime: "font/woff2",
        item_kind: ItemKind::Font,
        decodes_to: &[],
        caps: NONE,
    },
    FormatDesc {
        id: "glb",
        exts: &["glb"],
        mime: "model/gltf-binary",
        item_kind: ItemKind::Model,
        decodes_to: &[],
        caps: NONE,
    },
];

/// 拡張子（大文字・小文字は問わない）から形式を引く。
pub fn format_by_ext(ext: &str) -> Option<&'static FormatDesc> {
    let ext = ext.to_ascii_lowercase();
    FORMATS.iter().find(|f| f.exts.contains(&ext.as_str()))
}

/// 形式の ID から引く。
pub fn format_by_id(id: &str) -> Option<&'static FormatDesc> {
    FORMATS.iter().find(|f| f.id == id)
}

/// 形式の保存できるメタデータ（表にない形式は何も保存できないとする）。
pub fn caps_of(id: &str) -> MetaCaps {
    format_by_id(id).map(|f| f.caps).unwrap_or(NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_extensions_are_unique() {
        let mut ids: Vec<&str> = FORMATS.iter().map(|f| f.id).collect();
        let mut exts: Vec<&str> = FORMATS
            .iter()
            .flat_map(|f| f.exts.iter().copied())
            .collect();
        let (n_ids, n_exts) = (ids.len(), exts.len());
        ids.sort();
        ids.dedup();
        exts.sort();
        exts.dedup();
        assert_eq!((ids.len(), exts.len()), (n_ids, n_exts));
        assert!(exts.iter().all(|e| {
            e.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        }));
    }

    /// 段 9 の Platform・External のコーデックが受ける行がある：ImageIO の画像（HEIC・HEIF・AVIF は静止画とシーケンス、
    /// 固定のデコーダのない TIFF・BMP、固定の段が断った変種を回す JPEG・PSD）、AudioToolbox・ffmpeg の音声
    /// （m4a・aac・ogg・opus・wav・aiff・mp3）、AAC（m4a）と MP3 の書き出し。RAW は段 9 では扱わない。
    #[test]
    fn rows_for_platform_and_external_codecs() {
        use ValueKind::{Audio, Frames, Image};
        for (id, kinds) in [
            ("heic", &[Image, Frames][..]),
            ("heif", &[Image, Frames]),
            ("avif", &[Image, Frames]),
            ("tiff", &[Image]),
            ("bmp", &[Image]),
            ("jpeg", &[Image]),
            ("psd", &[Image]),
            ("m4a", &[Audio]),
            ("aac", &[Audio]),
            ("ogg", &[Audio]),
            ("opus", &[Audio]),
            ("wav", &[Audio]),
            ("aiff", &[Audio]),
            ("mp3", &[Audio]),
        ] {
            let f = format_by_id(id).unwrap_or_else(|| panic!("{id} がない"));
            assert_eq!(f.decodes_to, kinds, "{id}");
        }
        assert!(
            [
                "raw", "dng", "cr2", "nef", "arw", "jxl", "heics", "avis", "caf", "wma"
            ]
            .iter()
            .all(|e| format_by_ext(e).is_none())
        );
    }

    /// 拡張子は大文字・小文字を問わずに引く（取り込みの表との照合は kura-core の
    /// `classify_and_mime_follow_the_engine_format_table`）。
    #[test]
    fn looks_up_extensions_case_insensitively() {
        assert_eq!(format_by_ext("PNG").map(|f| f.id), Some("png"));
        assert_eq!(format_by_ext("ase").map(|f| f.id), Some("aseprite"));
        assert_eq!(format_by_ext("TTC").map(|f| f.id), Some("ttc"));
        assert!(format_by_ext("txt").is_none());
    }

    /// フォント・3D の行は取り込みと配信のためだけにあり、値に読まない（編集しない。デコーダを付けない）。
    #[test]
    fn font_and_model_rows_decode_to_nothing() {
        let rows: Vec<(&str, ItemKind)> = FORMATS
            .iter()
            .filter(|f| matches!(f.item_kind, ItemKind::Font | ItemKind::Model))
            .map(|f| (f.id, f.item_kind))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("ttf", ItemKind::Font),
                ("otf", ItemKind::Font),
                ("ttc", ItemKind::Font),
                ("woff", ItemKind::Font),
                ("woff2", ItemKind::Font),
                ("glb", ItemKind::Model),
            ]
        );
        for (id, _) in rows {
            assert!(
                format_by_id(id).is_some_and(|f| f.decodes_to.is_empty()),
                "{id}"
            );
        }
    }
}
