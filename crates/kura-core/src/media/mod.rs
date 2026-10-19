//! メディア情報とサムネイル（docs/architecture.md §7）。
//!
//! kura-core は `MediaBackend` trait だけを知り、実装は起動時に注入する（macOS は kura-mac、
//! 扱えない形式は ffmpeg）。テストでは偽物の実装を使う。
//!
//! M11：サムネイルは形式ごとの道（[`route`]）で作る。エンジンの道（[`engine_thumbs`]）は kura-engine で読み・描き、OS の道は
//! `MediaBackend`。作り方の指紋（`'thumbs.plan'`）・作っている途中の印（[`inflight`]）・キャッシュの容量（[`usage`]）もここ。

pub mod engine_thumbs;
mod ffmpeg;
pub mod inflight;
mod job;
pub mod route;
pub mod usage;

pub use ffmpeg::{FfmpegBackend, parse_ffprobe};
pub use job::{THUMB_BATCH, ThumbnailHandler, prepare_on_open};
pub use route::{ThumbRoute, thumb_route};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::import::MediaKind;
pub use crate::library::ThumbFormat;
use crate::library::{Layout, ThumbSize};

/// メディア情報。幅・高さは表示上の向き（回転を反映したもの）。フォント・3D の幅・高さは空（M11）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaInfo {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// 撮影・作成日時（UTC ミリ秒）。
    pub captured_at: Option<i64>,
    /// 主なコーデック（例：`h264`、`hevc`、`aac`、`flac`、`jpeg`）。フォントは形式（`ttf`・`woff2` など）、3D は `glb`。
    pub codec: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    /// フォントの情報（kura-mac の CoreText。`kura_probe` の種類 3）。
    pub font: Option<FontInfo>,
    /// 3D モデルの情報（kura-engine の glb の読み手。JSON の塊だけから数える）。
    pub model: Option<ModelInfo>,
}

impl MediaInfo {
    /// items.codec_info に入れる要約（JSON）：`{"codec":…}`、フォントは `"font":{…}`、3D は `"model":{…}` も足す。
    /// どれもなければ None。
    pub fn codec_json(&self) -> Option<String> {
        if self.codec.is_none() && self.font.is_none() && self.model.is_none() {
            return None;
        }
        let mut out = serde_json::Map::new();
        if let Some(codec) = &self.codec {
            out.insert("codec".into(), codec.as_str().into());
        }
        if let Some(font) = &self.font {
            out.insert("font".into(), serde_json::to_value(font).ok()?);
        }
        if let Some(model) = &self.model {
            out.insert("model".into(), serde_json::to_value(model).ok()?);
        }
        Some(serde_json::Value::Object(out).to_string())
    }
}

/// フォントの情報（インスペクタの素材の欄。M11 の既定：書体名・スタイル・字の数・形式）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FontInfo {
    /// 書体名（ファミリー）。
    pub family: Option<String>,
    /// スタイル（Regular・Bold など）。
    pub style: Option<String>,
    pub full_name: Option<String>,
    pub postscript_name: Option<String>,
    /// 字（グリフ）の数。
    pub glyphs: u32,
    /// ファイルの中の書体の数（コレクションは 2 以上）。
    pub faces: u32,
    /// 可変フォント（変化の軸がある）か。
    pub variable: bool,
}

/// 3D モデルの情報（インスペクタの素材の欄。M11 の既定：頂点・三角形・大きさ・材質の数・未対応の拡張の印）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelInfo {
    pub vertices: u64,
    pub triangles: u64,
    pub meshes: u32,
    pub materials: u32,
    pub textures: u32,
    pub animations: u32,
    pub skins: u32,
    pub nodes: u32,
    /// 大きさ（範囲の max − min。x・y・z の順。glb の単位はメートル）。数えられなければ None。
    pub size: Option<[f64; 3]>,
    /// 作った道具（asset.generator）。
    pub generator: Option<String>,
    pub extensions_required: Vec<String>,
    /// 描けない理由（必須の Draco・meshopt・知らない拡張の名前、`skin`）。空なら描ける（サムネイルを作る）。
    pub unsupported: Vec<String>,
}

impl From<&kura_engine::model::ModelInfo> for ModelInfo {
    fn from(m: &kura_engine::model::ModelInfo) -> Self {
        Self {
            vertices: m.vertices,
            triangles: m.triangles,
            meshes: m.meshes,
            materials: m.materials,
            textures: m.textures,
            animations: m.animations,
            skins: m.skins,
            nodes: m.nodes,
            size: m.bounds.map(|b| b.size()),
            generator: m.generator.clone(),
            extensions_required: m.extensions_required.clone(),
            unsupported: m.unsupported.clone(),
        }
    }
}

/// サムネイルの書き先（長辺 1600 と 512）。拡張子のない stem で、書いた形式の拡張子（`.jpg`・`.png`）を足したファイルに
/// なる（Swift の `stemURL` と同じ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbTargets {
    pub large: PathBuf,
    pub small: PathBuf,
}

impl ThumbTargets {
    /// ライブラリのサムネイルの書き先（`thumbs/ab/<hash>_l`・`_s`）。
    pub fn for_hash(layout: &Layout, hash_hex: &str) -> Self {
        let stem = |size| {
            layout
                .thumb_path_as(hash_hex, size, ThumbFormat::Jpeg)
                .with_extension("")
        };
        Self {
            large: stem(ThumbSize::Large),
            small: stem(ThumbSize::Small),
        }
    }

    /// `format` で書いたときのファイル（大・小）。
    pub fn files(&self, format: ThumbFormat) -> (PathBuf, PathBuf) {
        (
            with_ext(&self.large, format.ext()),
            with_ext(&self.small, format.ext()),
        )
    }

    /// `written` で書いたあと、ほかの形式の古いファイル（作り直す前のもの）を消す。なければ何もしない。
    pub fn remove_others(&self, written: ThumbFormat) -> std::io::Result<()> {
        for format in ThumbFormat::ALL {
            if format == written {
                continue;
            }
            let (large, small) = self.files(format);
            for path in [large, small] {
                match std::fs::remove_file(&path) {
                    Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// stem に拡張子を足す（stem に `.` があっても置き換えない）。
fn with_ext(stem: &Path, ext: &str) -> PathBuf {
    let mut s = stem.as_os_str().to_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
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

/// メディア情報とサムネイルを作る実装（OS のデコーダ・外部ツール）。新しいメソッドには既定の実装を置かない
/// （`ChainBackend`・`RoutedBackend` が転送を書き忘れると黙って `Unsupported` になるので、コンパイルで気づけるようにする）。
pub trait MediaBackend: Send + Sync {
    /// メディア情報を読む（フォントは書体名・字の数なども。3D は kura-core が kura-engine で読むので扱わない）。
    fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError>;
    /// 画像のサムネイル（長辺 1600 と 512）を `out` の stem に書き、書いた形式を返す。透明な画素があれば PNG、
    /// なければ JPEG（M11 の決定 4）。
    fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError>;
    /// 動画の `at_sec` 秒の地点のサムネイルを作り、書いた形式（JPEG）を返す。
    fn video_thumbnails(
        &self,
        path: &Path,
        at_sec: f64,
        out: &ThumbTargets,
    ) -> Result<ThumbFormat, MediaError>;
    /// 音楽のジャケットを `art` に書き、サムネイルも作って書いた形式を返す。ジャケットがなければ Ok(None)。
    fn audio_artwork(
        &self,
        path: &Path,
        art: &Path,
        out: &ThumbTargets,
    ) -> Result<Option<ThumbFormat>, MediaError>;
    /// フォントの見本（正方形の透明の地に、字の形だけのアルファ）を書き、書いた形式（PNG）を返す。
    fn font_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError>;
}

/// 何も扱えない実装（テストや、ツールがないとき）。
pub struct NoBackend;

impl MediaBackend for NoBackend {
    fn probe(&self, _: &Path, _: MediaKind) -> Result<MediaInfo, MediaError> {
        Err(MediaError::Unsupported)
    }
    fn image_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
        Err(MediaError::Unsupported)
    }
    fn video_thumbnails(
        &self,
        _: &Path,
        _: f64,
        _: &ThumbTargets,
    ) -> Result<ThumbFormat, MediaError> {
        Err(MediaError::Unsupported)
    }
    fn audio_artwork(
        &self,
        _: &Path,
        _: &Path,
        _: &ThumbTargets,
    ) -> Result<Option<ThumbFormat>, MediaError> {
        Err(MediaError::Unsupported)
    }
    fn font_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
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
    fn image_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
        self.first(|b| b.image_thumbnails(path, out))
    }
    fn video_thumbnails(
        &self,
        path: &Path,
        at_sec: f64,
        out: &ThumbTargets,
    ) -> Result<ThumbFormat, MediaError> {
        self.first(|b| b.video_thumbnails(path, at_sec, out))
    }
    fn audio_artwork(
        &self,
        path: &Path,
        art: &Path,
        out: &ThumbTargets,
    ) -> Result<Option<ThumbFormat>, MediaError> {
        let mut result = Err(MediaError::Unsupported);
        for backend in &self.backends {
            match backend.audio_artwork(path, art, out) {
                Ok(Some(format)) => return Ok(Some(format)),
                Ok(None) => result = Ok(None),
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
    fn font_thumbnails(&self, path: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
        self.first(|b| b.font_thumbnails(path, out))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! テスト用の偽物：画像は 1600×900、動画は 1920×1080・10 秒、音楽は 3 分、フォントは「偽の書体」。サムネイルは小さな
    //! ファイルを書く。画像は `.heic`・`.tif` を「透明のある画像」として PNG で、ほかは JPEG で書く。
    use super::*;

    pub struct FakeBackend;

    fn touch(path: &Path, bytes: &[u8]) -> Result<(), MediaError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// `format` で大・小を書く。
    pub fn write(
        out: &ThumbTargets,
        format: ThumbFormat,
        l: &[u8],
        s: &[u8],
    ) -> Result<(), MediaError> {
        let (large, small) = out.files(format);
        touch(&large, l)?;
        touch(&small, s)
    }

    impl MediaBackend for FakeBackend {
        fn probe(&self, path: &Path, kind: MediaKind) -> Result<MediaInfo, MediaError> {
            if path.extension().is_some_and(|e| e == "bad") {
                return Err(MediaError::Failed("壊れています".into()));
            }
            Ok(match kind {
                MediaKind::Model => return Err(MediaError::Unsupported),
                MediaKind::Font => MediaInfo {
                    codec: Some("ttf".into()),
                    font: Some(FontInfo {
                        family: Some("偽の書体".into()),
                        style: Some("Regular".into()),
                        glyphs: 42,
                        faces: 1,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
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
        fn image_thumbnails(
            &self,
            path: &Path,
            out: &ThumbTargets,
        ) -> Result<ThumbFormat, MediaError> {
            let format = if path.extension().is_some_and(|e| e == "heic" || e == "tif") {
                ThumbFormat::Png
            } else {
                ThumbFormat::Jpeg
            };
            write(out, format, b"L", b"S")?;
            Ok(format)
        }
        fn video_thumbnails(
            &self,
            _: &Path,
            _: f64,
            out: &ThumbTargets,
        ) -> Result<ThumbFormat, MediaError> {
            write(out, ThumbFormat::Jpeg, b"VL", b"VS")?;
            Ok(ThumbFormat::Jpeg)
        }
        fn audio_artwork(
            &self,
            path: &Path,
            art: &Path,
            out: &ThumbTargets,
        ) -> Result<Option<ThumbFormat>, MediaError> {
            if path.to_string_lossy().contains("noart") {
                return Ok(None);
            }
            touch(art, b"A")?;
            write(out, ThumbFormat::Jpeg, b"AL", b"AS")?;
            Ok(Some(ThumbFormat::Jpeg))
        }
        fn font_thumbnails(&self, _: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
            write(out, ThumbFormat::Png, b"FL", b"FS")?;
            Ok(ThumbFormat::Png)
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
        fn image_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
            Err(MediaError::Failed("壊れています".into()))
        }
        fn video_thumbnails(
            &self,
            _: &Path,
            _: f64,
            _: &ThumbTargets,
        ) -> Result<ThumbFormat, MediaError> {
            Err(MediaError::Unsupported)
        }
        fn audio_artwork(
            &self,
            _: &Path,
            _: &Path,
            _: &ThumbTargets,
        ) -> Result<Option<ThumbFormat>, MediaError> {
            Ok(None)
        }
        fn font_thumbnails(&self, _: &Path, _: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
            Err(MediaError::Unsupported)
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
        assert!(matches!(
            chain.font_thumbnails(Path::new("x"), &targets),
            Err(MediaError::Unsupported)
        ));
        // フォントの見本も次の実装へ回す
        let with_fonts = ChainBackend::new(vec![Arc::new(NoArt), Arc::new(fake::FakeBackend)]);
        let dir = tempfile::tempdir().unwrap();
        let out = ThumbTargets {
            large: dir.path().join("x_l"),
            small: dir.path().join("x_s"),
        };
        assert_eq!(
            with_fonts
                .font_thumbnails(Path::new("x.ttf"), &out)
                .unwrap(),
            ThumbFormat::Png
        );
        assert!(out.files(ThumbFormat::Png).1.is_file());
    }

    #[test]
    fn chain_keeps_looking_for_artwork() {
        let dir = tempfile::tempdir().unwrap();
        let targets = ThumbTargets {
            large: dir.path().join("l"),
            small: dir.path().join("s"),
        };
        let art = dir.path().join("a.jpg");
        let chain = ChainBackend::new(vec![Arc::new(NoArt), Arc::new(fake::FakeBackend)]);
        assert_eq!(
            chain
                .audio_artwork(Path::new("song.flac"), &art, &targets)
                .unwrap(),
            Some(ThumbFormat::Jpeg)
        );
        assert!(targets.files(ThumbFormat::Jpeg).0.is_file());
        let none = ChainBackend::new(vec![Arc::new(NoArt), Arc::new(NoBackend)]);
        assert_eq!(
            none.audio_artwork(Path::new("song.flac"), &art, &targets)
                .unwrap(),
            None
        );
    }

    /// 書き先は拡張子のない stem（`thumbs/ab/<hash>_l`）で、書いた形式の拡張子を足す。作り直したら、ほかの形式の古い
    /// ファイルを消す。
    #[test]
    fn targets_are_stems_and_old_formats_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let hex = "ab".repeat(32);
        let out = ThumbTargets::for_hash(&layout, &hex);
        assert_eq!(out.large, dir.path().join(format!("thumbs/ab/{hex}_l")));
        assert_eq!(
            out.files(ThumbFormat::Png),
            (
                layout.thumb_path_as(&hex, ThumbSize::Large, ThumbFormat::Png),
                layout.thumb_path_as(&hex, ThumbSize::Small, ThumbFormat::Png)
            )
        );
        assert_eq!(
            out.files(ThumbFormat::Jpeg).1,
            layout.thumb_path(&hex, ThumbSize::Small)
        );
        fake::write(&out, ThumbFormat::Jpeg, b"l", b"s").unwrap();
        fake::write(&out, ThumbFormat::Png, b"l", b"s").unwrap();
        out.remove_others(ThumbFormat::Png).unwrap();
        let (jl, js) = out.files(ThumbFormat::Jpeg);
        let (pl, ps) = out.files(ThumbFormat::Png);
        assert!(!jl.exists() && !js.exists());
        assert!(pl.is_file() && ps.is_file());
        // もうないものを消しても失敗しない
        out.remove_others(ThumbFormat::Png).unwrap();
        // stem に `.` があっても拡張子を置き換えない
        let dotted = ThumbTargets {
            large: PathBuf::from("/x/a.b_l"),
            small: PathBuf::from("/x/a.b_s"),
        };
        assert_eq!(
            dotted.files(ThumbFormat::Png).0,
            PathBuf::from("/x/a.b_l.png")
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

    /// フォント・3D の情報は codec_info の `font`・`model` に入る（M11 の既定）。Swift の JSON（`{"codec","font"}`）から読める。
    #[test]
    fn font_and_model_info_go_into_the_codec_summary() {
        let font: MediaInfo = serde_json::from_str(
            r#"{"codec":"woff2","font":{"family":"Fira Sans","style":"Regular","fullName":"Fira Sans","postscriptName":"FiraSans-Regular","glyphs":1686,"faces":1,"variable":false}}"#,
        )
        .unwrap();
        assert_eq!((font.width, font.height), (None, None));
        let json: serde_json::Value = serde_json::from_str(&font.codec_json().unwrap()).unwrap();
        assert_eq!(json["codec"], "woff2");
        assert_eq!(json["font"]["family"], "Fira Sans");
        assert_eq!(json["font"]["glyphs"], 1686);
        assert!(json.get("model").is_none());

        let glbs = kura_engine::model::sample_glbs().unwrap();
        let glb = &glbs.iter().find(|(n, _)| *n == "triangle").unwrap().1;
        let engine = kura_engine::model::read_info(glb, &Default::default()).unwrap();
        let model = MediaInfo {
            codec: Some("glb".into()),
            model: Some(ModelInfo::from(&engine)),
            ..Default::default()
        };
        let json: serde_json::Value = serde_json::from_str(&model.codec_json().unwrap()).unwrap();
        assert_eq!(json["model"]["vertices"], 3);
        assert_eq!(json["model"]["triangles"], 1);
        assert!(json["model"]["size"].is_array());
        assert_eq!(json["model"]["unsupported"], serde_json::json!([]));
    }
}
