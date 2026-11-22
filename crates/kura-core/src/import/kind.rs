//! メディアの種類と、拡張子からの種類・MIME の決め方（docs/architecture.md §6 の対応拡張子）。
//!
//! 拡張子・MIME・種類は kura-engine の形式の表（`kura_engine::codec::formats::FORMATS`）が正で、ここはそれを引くだけにする
//! （ADR-140）。種類の文字列（items.kind）は `MediaKind::parse`（DB からは `FromSql`）を通して読み、`_ =>` の既定の枝で
//! 黙って別の種類に落とさない。

use std::io::Read;
use std::path::Path;

use kura_engine::codec::formats::{FormatDesc, ItemKind, format_by_ext};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

/// メディアの種類（items.kind）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    /// フォント（ttf・otf・ttc・woff・woff2）。再生しない。
    Font,
    /// 3D モデル（glb）。再生しない。
    Model,
}

impl MediaKind {
    /// すべての種類（表示・集計の順）。
    pub const ALL: [Self; 5] = [
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Font,
        Self::Model,
    ];

    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Font => "font",
            Self::Model => "model",
        }
    }

    /// DB の名前から読む（知らない名前は None）。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// 再生のキュー・スマートの候補に入るか（画像・動画・音楽）。フォント・3D はユニット・プレイリストに
    /// 入れられるが、再生では飛ばす（M11 の決定 11）。
    pub fn is_playable(self) -> bool {
        match self {
            Self::Image | Self::Video | Self::Audio => true,
            Self::Font | Self::Model => false,
        }
    }

    /// 再生用プロキシの判定・作成の対象か（動画・音楽。DB の `items_proxy_av` と同じ）。
    pub fn needs_proxy(self) -> bool {
        match self {
            Self::Video | Self::Audio => true,
            Self::Image | Self::Font | Self::Model => false,
        }
    }

    /// 長さ（duration_ms）を持つか（動画・音楽）。
    pub fn has_duration(self) -> bool {
        match self {
            Self::Video | Self::Audio => true,
            Self::Image | Self::Font | Self::Model => false,
        }
    }
}

impl From<ItemKind> for MediaKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Image => Self::Image,
            ItemKind::Video => Self::Video,
            ItemKind::Audio => Self::Audio,
            ItemKind::Font => Self::Font,
            ItemKind::Model => Self::Model,
        }
    }
}

impl FromSql for MediaKind {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        Self::parse(s).ok_or_else(|| FromSqlError::Other(format!("不明な種類: {s}").into()))
    }
}

impl ToSql for MediaKind {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// 種類ごとの数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindCounts {
    pub image: u64,
    pub video: u64,
    pub audio: u64,
    pub font: u64,
    pub model: u64,
}

impl KindCounts {
    /// `kind` の数を `n` 増やす。
    pub fn add(&mut self, kind: MediaKind, n: u64) {
        let slot = match kind {
            MediaKind::Image => &mut self.image,
            MediaKind::Video => &mut self.video,
            MediaKind::Audio => &mut self.audio,
            MediaKind::Font => &mut self.font,
            MediaKind::Model => &mut self.model,
        };
        *slot = slot.saturating_add(n);
    }

    /// `kind` の数。
    pub fn get(&self, kind: MediaKind) -> u64 {
        match kind {
            MediaKind::Image => self.image,
            MediaKind::Video => self.video,
            MediaKind::Audio => self.audio,
            MediaKind::Font => self.font,
            MediaKind::Model => self.model,
        }
    }

    /// 合計。
    pub fn total(&self) -> u64 {
        MediaKind::ALL
            .into_iter()
            .fold(0u64, |sum, k| sum.saturating_add(self.get(k)))
    }

    /// 1 つ以上ある種類（`MediaKind::ALL` の順）。
    pub fn kinds(&self) -> Vec<MediaKind> {
        MediaKind::ALL
            .into_iter()
            .filter(|k| self.get(*k) > 0)
            .collect()
    }
}

/// 拡張子（大文字・小文字は問わない）の形式。
fn format_of(ext: &str) -> Option<&'static FormatDesc> {
    format_by_ext(ext)
}

/// 拡張子（大文字・小文字は問わない）から種類を決める。対応していなければ None。
/// 先頭のバイトで見分ける形式（`.ase`）もあるので、ファイルを取り込むときは `classify_file` を使う。
pub fn classify(ext: &str) -> Option<MediaKind> {
    format_of(ext).map(|f| MediaKind::from(f.item_kind))
}

/// 拡張子から MIME を決める。
pub fn mime_for(ext: &str) -> Option<&'static str> {
    format_of(ext).map(|f| f.mime)
}

/// 拡張子から形式の ID（エンジンのコーデックが受け付ける名前。`jpg` → `jpeg`、`ase` → `aseprite`）を決める。
pub fn format_id(ext: &str) -> Option<&'static str> {
    format_of(ext).map(|f| f.id)
}

/// 取り込まない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// 対応していない拡張子。
    Unsupported,
    /// 拡張子は対応しているが、中身がその形式でない（`.ase` が Adobe の色見本など）。
    NotThisFormat(&'static str),
    /// 先頭を読めなかった。
    Unreadable(String),
}

impl Rejected {
    /// 取り込みの結果・ログに出す理由。
    pub fn reason(&self) -> String {
        match self {
            Self::Unsupported => "対応していない形式です".to_string(),
            Self::NotThisFormat(why) => (*why).to_string(),
            Self::Unreadable(err) => format!("読めませんでした（{err}）"),
        }
    }
}

/// 先頭のバイトで見分けるのに読む長さ。
const HEAD_LEN: u64 = 6;

/// 先頭のバイトで見分ける形式なら、その形式かを確かめる（純粋関数。見分けない形式はいつも Ok）。
///
/// - aseprite（`.aseprite`・`.ase`）：先頭 4 バイトのファイルの長さのあとに、マジック 0xA5E0（リトルエンディアン）。
///   `.ase` は Adobe の色見本（Swatch Exchange。先頭が `ASEF`）と拡張子が重なるので、画像として並べない。
pub fn check_head(format_id: &str, head: &[u8]) -> Result<(), &'static str> {
    match format_id {
        "aseprite" => {
            if head.get(4..6) == Some(&[0xE0, 0xA5][..]) {
                Ok(())
            } else {
                Err("Aseprite のファイルではありません（Adobe の色見本などは取り込みません）")
            }
        }
        _ => Ok(()),
    }
}

/// 先頭のバイトで見分ける形式か（`check_head` が見る形式）。
fn needs_head(format_id: &str) -> bool {
    matches!(format_id, "aseprite")
}

/// 拡張子と先頭のバイトから種類を決める（取り込み・ダウンロード・キャプチャで共通）。
/// 先頭を読むのは、見分ける必要のある形式（`.ase`・`.aseprite`）だけ。
pub fn classify_file(path: &Path, ext: &str) -> Result<MediaKind, Rejected> {
    let format = format_of(ext).ok_or(Rejected::Unsupported)?;
    if needs_head(format.id) {
        let mut head = Vec::with_capacity(HEAD_LEN as usize);
        std::fs::File::open(path)
            .and_then(|f| f.take(HEAD_LEN).read_to_end(&mut head))
            .map_err(|err| Rejected::Unreadable(err.to_string()))?;
        check_head(format.id, &head).map_err(Rejected::NotThisFormat)?;
    }
    Ok(MediaKind::from(format.item_kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::codec::formats::FORMATS;

    #[test]
    fn kind_names_round_trip_and_properties() {
        for kind in MediaKind::ALL {
            assert_eq!(MediaKind::parse(kind.as_str()), Some(kind));
            // serde の名前も DB の名前と同じ
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::from(kind.as_str())
            );
        }
        assert_eq!(MediaKind::parse("Image"), None);
        assert_eq!(MediaKind::parse("sticker"), None);
        let names = |pred: fn(MediaKind) -> bool| {
            MediaKind::ALL
                .into_iter()
                .filter(|k| pred(*k))
                .map(MediaKind::as_str)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(MediaKind::is_playable), ["image", "video", "audio"]);
        assert_eq!(names(MediaKind::needs_proxy), ["video", "audio"]);
        assert_eq!(names(MediaKind::has_duration), ["video", "audio"]);
        // エンジンの種類はすべて写せる（網羅の From）
        for (item, name) in [
            (ItemKind::Image, "image"),
            (ItemKind::Video, "video"),
            (ItemKind::Audio, "audio"),
            (ItemKind::Font, "font"),
            (ItemKind::Model, "model"),
        ] {
            assert_eq!(MediaKind::from(item).as_str(), name);
        }
    }

    #[test]
    fn kinds_round_trip_through_sqlite() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for kind in MediaKind::ALL {
            let back: MediaKind = conn.query_row("SELECT ?1", [kind], |r| r.get(0)).unwrap();
            assert_eq!(back, kind);
        }
        let unknown: rusqlite::Result<MediaKind> =
            conn.query_row("SELECT 'sticker'", [], |r| r.get(0));
        assert!(unknown.is_err());
    }

    #[test]
    fn counts_add_up_by_kind() {
        let mut counts = KindCounts::default();
        counts.add(MediaKind::Font, 2);
        counts.add(MediaKind::Image, 1);
        counts.add(MediaKind::Model, 3);
        assert_eq!(
            (
                counts.image,
                counts.video,
                counts.audio,
                counts.font,
                counts.model
            ),
            (1, 0, 0, 2, 3)
        );
        assert_eq!(counts.total(), 6);
        assert_eq!(
            counts.kinds(),
            vec![MediaKind::Image, MediaKind::Font, MediaKind::Model]
        );
    }

    /// 取り込みの種類・MIME・形式の ID は、エンジンの形式の表と同じ（formats.rs の照合のテストをここへ移した）。
    #[test]
    fn classify_and_mime_follow_the_engine_format_table() {
        for f in FORMATS {
            for ext in f.exts {
                for e in [ext.to_string(), ext.to_ascii_uppercase()] {
                    assert_eq!(classify(&e), Some(MediaKind::from(f.item_kind)), "{e}");
                    assert_eq!(mime_for(&e), Some(f.mime), "{e}");
                    assert_eq!(format_id(&e), Some(f.id), "{e}");
                }
            }
        }
        // 今まで取り込めたものは同じ種類・MIME のまま
        let current: [(&str, MediaKind, &str); 26] = [
            ("jpg", MediaKind::Image, "image/jpeg"),
            ("jpeg", MediaKind::Image, "image/jpeg"),
            ("png", MediaKind::Image, "image/png"),
            ("gif", MediaKind::Image, "image/gif"),
            ("webp", MediaKind::Image, "image/webp"),
            ("heic", MediaKind::Image, "image/heic"),
            ("heif", MediaKind::Image, "image/heif"),
            ("avif", MediaKind::Image, "image/avif"),
            ("tif", MediaKind::Image, "image/tiff"),
            ("tiff", MediaKind::Image, "image/tiff"),
            ("bmp", MediaKind::Image, "image/bmp"),
            ("mp4", MediaKind::Video, "video/mp4"),
            ("m4v", MediaKind::Video, "video/x-m4v"),
            ("mov", MediaKind::Video, "video/quicktime"),
            ("webm", MediaKind::Video, "video/webm"),
            ("mkv", MediaKind::Video, "video/x-matroska"),
            ("avi", MediaKind::Video, "video/x-msvideo"),
            ("mp3", MediaKind::Audio, "audio/mpeg"),
            ("m4a", MediaKind::Audio, "audio/mp4"),
            ("aac", MediaKind::Audio, "audio/aac"),
            ("flac", MediaKind::Audio, "audio/flac"),
            ("wav", MediaKind::Audio, "audio/wav"),
            ("aif", MediaKind::Audio, "audio/aiff"),
            ("aiff", MediaKind::Audio, "audio/aiff"),
            ("ogg", MediaKind::Audio, "audio/ogg"),
            // Ogg に入った Opus（RFC 7845）。WebKit は audio/opus では再生しない（ADR-73）
            ("opus", MediaKind::Audio, "audio/ogg"),
        ];
        for (ext, kind, mime) in current {
            assert_eq!(
                (classify(ext), mime_for(ext)),
                (Some(kind), Some(mime)),
                "{ext}"
            );
        }
        // M11 で足した種類と形式
        for (ext, kind, mime) in [
            ("apng", MediaKind::Image, "image/png"),
            ("aseprite", MediaKind::Image, "image/x-aseprite"),
            ("ase", MediaKind::Image, "image/x-aseprite"),
            ("psd", MediaKind::Image, "image/vnd.adobe.photoshop"),
            ("psb", MediaKind::Image, "image/vnd.adobe.photoshop"),
            ("svg", MediaKind::Image, "image/svg+xml"),
            ("ttf", MediaKind::Font, "font/ttf"),
            ("otf", MediaKind::Font, "font/otf"),
            ("ttc", MediaKind::Font, "font/collection"),
            ("woff", MediaKind::Font, "font/woff"),
            ("woff2", MediaKind::Font, "font/woff2"),
            ("glb", MediaKind::Model, "model/gltf-binary"),
        ] {
            assert_eq!(
                (classify(ext), mime_for(ext)),
                (Some(kind), Some(mime)),
                "{ext}"
            );
        }
        for ext in [
            "txt", "", "gltf", "vrm", "usdz", "obj", "heics", "raw", "json",
        ] {
            assert_eq!(classify(ext), None, "{ext}");
            assert_eq!(mime_for(ext), None, "{ext}");
        }
    }

    /// `.ase`・`.aseprite` は先頭のバイトで Aseprite かを見分ける（Adobe の色見本を画像として並べない）。
    #[test]
    fn ase_is_imported_only_when_it_is_aseprite() {
        // Aseprite のヘッダ（長さ 4 バイト + 0xA5E0）
        let aseprite = [0x80, 0, 0, 0, 0xE0, 0xA5, 1, 0];
        let swatch = *b"ASEF\x00\x01\x00\x00";
        assert_eq!(check_head("aseprite", &aseprite), Ok(()));
        assert!(check_head("aseprite", &swatch).is_err());
        assert!(check_head("aseprite", &aseprite[..5]).is_err());
        assert_eq!(check_head("png", b"ASEF"), Ok(()));

        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let sprite = write("walk.ase", &aseprite);
        let sprite2 = write("walk.aseprite", &aseprite);
        let colors = write("colors.ase", &swatch);
        let empty = write("empty.ase", b"");
        assert_eq!(classify_file(&sprite, "ase"), Ok(MediaKind::Image));
        assert_eq!(classify_file(&sprite2, "ASEPRITE"), Ok(MediaKind::Image));
        let rejected = classify_file(&colors, "ase").unwrap_err();
        assert!(matches!(rejected, Rejected::NotThisFormat(_)));
        assert!(
            rejected.reason().contains("Aseprite"),
            "{}",
            rejected.reason()
        );
        assert!(matches!(
            classify_file(&empty, "ase"),
            Err(Rejected::NotThisFormat(_))
        ));
        assert!(matches!(
            classify_file(&dir.path().join("missing.ase"), "ase"),
            Err(Rejected::Unreadable(_))
        ));
        assert_eq!(
            classify_file(&dir.path().join("x.txt"), "txt"),
            Err(Rejected::Unsupported)
        );
        // 見分けない形式はファイルを開かない（ないファイルでも拡張子で決まる）
        assert_eq!(
            classify_file(&dir.path().join("missing.png"), "png"),
            Ok(MediaKind::Image)
        );
    }
}
