//! コーデック（docs/editing.md §9）。ファイルと値のあいだの読み書き。
//!
//! - 段（`Tier`）：Kura のバイナリに入る Rust のもの（`Pinned`。版を固定し、結果を約束する）→ macOS の機能（`Platform`。kura-mac が注入）
//!   → 外部の ffmpeg（`External`。kura-core が注入）の順に試し、使ったものの識別子を記録する。
//! - 外から来たファイルを読むので、確保の前に上限で判断し、第三者のデコーダの境界では panic を受け止めてエラーにする。
//! - Platform・External のコーデックの識別子は kura の呼び方と変換の約束の版だけを持ち、OS・フレームワーク・ffmpeg の版は
//!   `env()`（読み書きの記録の `env`）に出す。作り直しの照合は値のダイジェストで行う（E29）。
//! - 入力はファイルのパスでも渡せる（[`Decoder::shape_file`]・[`Decoder::decode_file`]。既定は開いて読み口の口に渡す。
//!   外部のツールはパスをそのまま渡すために上書きする）。
//! - 能力の表は [`CodecSet::capabilities`]（`caps.rs`）。Platform の受け口（OS を知らない backend の trait と adapter）は
//!   `platform`。

pub mod aiff;
pub mod anchor;
pub mod aseprite;
pub mod caps;
pub mod exif;
pub mod flac;
pub mod formats;
pub mod gif;
pub mod jpeg;
pub mod lossy;
pub mod mp3;
pub(crate) mod ogg;
pub mod pcm;
pub mod platform;
pub mod png;
pub mod psd;
pub mod recorded;
pub mod svg;
pub(crate) mod symph;
pub mod vorbis;
pub mod wav;
pub mod webp;

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Seek};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use caps::{FormatCaps, ReadCap, WriteCap};
pub use formats::{
    AlphaCaps, FormatDesc, ItemKind, MetaCaps, caps_of, format_by_ext, format_by_id,
};

use crate::digest::{Digest, Hasher, canonical_json};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::op::ParamSchema;
use crate::shape::Shape;
use crate::value::Value;

/// 読める・シークできる入力。
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// 文字の境目で `max` バイト以下に切り詰める（AIFF・FLAC の書き出しで、タグを読み取りの上限 4,096 バイトにそろえ、
/// 書いて読み直したものが同じになるようにする）。
pub(crate) fn clip_text(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// RIFF・IFF のチャンクの数の上限（WAV・AIFF。ふつうのファイルは数十まで。小さなチャンクを並べた走査で止まらないように）。
pub(crate) const MAX_CHUNKS: usize = 1 << 16;

/// `pos` へ移る。今の位置と同じなら何もせず、違えば `seek_relative`（`BufReader` はバッファの中ならバッファを捨てない）。
pub(crate) fn seek_to(src: &mut dyn ReadSeek, pos: u64) -> std::io::Result<()> {
    let cur = src.stream_position()?;
    if cur == pos {
        return Ok(());
    }
    match i64::try_from(i128::from(pos) - i128::from(cur)) {
        Ok(d) => src.seek_relative(d),
        Err(_) => src.seek(std::io::SeekFrom::Start(pos)).map(|_| ()),
    }
}

/// コーデックの段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tier {
    /// Kura のバイナリに入る Rust のもの。版を固定し、結果を約束する（R1・R2）。
    Pinned,
    /// macOS の機能。OS の版で結果が変わりうる。
    Platform,
    /// 外部のツール（ffmpeg）。ツールの版で結果が変わりうる。
    External,
}

/// 読み書きした環境（Platform・External だけ。§8.1 の「環境依存」）。版の記録に残し、「なぜ変わったか」の表示と調べ物に使う。
/// 実装の選び分けには使わない（識別子は kura の呼び方の約束の版だけで、OS・ツールの版を入れない。入れると OS を上げた
/// だけで記録の識別子のデコーダが見つからなくなる）。順序は `BTreeMap` で決まる（正規の JSON で同じ並び）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodecEnv {
    /// 何を通ったか（`"macOS"`・`"ffmpeg"`）。
    pub tool: String,
    /// その版（`"26.7 (25G229)"`・`"9.0.2"`）。
    pub version: String,
    /// 部品の版などの細目（例 `imageio = "2784.6.4"`、`libavcodec = "63.1.102"`、`codec = "libopus"`、`lame = "4.0"`、
    /// `arch = "aarch64"`、`fingerprint = "…"`）。空なら書かない。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, String>,
}

impl CodecEnv {
    /// 道具と版だけの記録。
    pub fn new(tool: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            version: version.into(),
            components: BTreeMap::new(),
        }
    }

    /// 細目を 1 つ足す（同じ名前は置き換える）。
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.components.insert(key.into(), value.into());
        self
    }
}

/// 読み取りの記録（版の記録に残す。保存のために読み戻せる）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecodeStamp {
    /// 使ったデコーダ（例 `png-rs-dec/1`）。作り直しでは `CodecSet::decode_with` でこのデコーダを選ぶ。
    pub codec: String,
    pub tier: Tier,
    /// 同じ識別子のデコーダなら、どの環境でも同じ値を読むか（偽なら、初めて編集するときにアンカーを作る。§9.4）。
    pub reproducible: bool,
    /// 読み取った値のダイジェスト（作り直しで照らす。E29）。
    pub value_digest: Digest,
    /// 読んだ環境（Platform・External のデコーダの `env()`）。固定の段は `None` で、JSON に書かない（段 9 より前の記録と同じ形）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<CodecEnv>,
}

/// 書き出しの記録（版の記録に残す。保存のために読み戻せる。知らない項目は拒む）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EncodedStamp {
    /// 使ったエンコーダ（例 `png-rs-enc/1`）。同じ書き手で書き直すときは `CodecSet::encode_bytes_with` に渡す。
    pub codec: String,
    pub tier: Tier,
    /// 書いた形式の ID。
    pub format: String,
    /// 確かめて正規の形にした書き出しの設定。
    pub options: serde_json::Value,
    pub bytes: u64,
    /// 書いたファイルの中身のダイジェスト（R2 の確認に使う）。
    pub file_digest: Digest,
    /// 同じ値・同じ設定なら、どの環境でも同じバイト列を書くか（R2。`Encoder::reproducible`）。偽なら、作り直したファイルの
    /// バイト列が記録と違っても失敗にせず知らせるだけにする（値は R1 で照らす）。真なら JSON に書かず、書かれていなければ
    /// 真として読む（段 9 より前の形）。
    #[serde(default = "always_true", skip_serializing_if = "is_true")]
    pub reproducible: bool,
    /// 書いた環境（Platform・External のエンコーダの `env()`）。固定の段は `None` で、JSON に書かない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<CodecEnv>,
}

fn is_true(b: &bool) -> bool {
    *b
}

fn always_true() -> bool {
    true
}

/// デコーダ。
pub trait Decoder: Send + Sync {
    /// 識別子（例 `png-rs-dec/1`。約束の版を含む）。読む値が変わる変更は版を上げ、旧版を `add_legacy_decoder` で残す（§9.1）。
    /// Platform・External は kura の呼び方と変換の約束の版だけを持ち（OS・ツールの版は [`Decoder::env`]）、旧版を残さない
    /// （再現を約束しないので、作り直しはアンカーから読む）。
    fn id(&self) -> &'static str;
    fn tier(&self) -> Tier;
    /// 同じ識別子なら、どの環境でも同じ値を読むか。既定は固定の段なら真（OS の機能・外部のツールは偽）。
    /// 固定の段でも、中で OS の libm を使うもの（SVG）は偽にする。固定の段でないものは真にできない（`add_decoder` が断る）。
    fn reproducible(&self) -> bool {
        self.tier() == Tier::Pinned
    }
    /// 読む環境（OS・フレームワーク・ツールの版。読み取りの記録の `env` に入る）。既定は `None`（固定の段）。
    /// 呼ぶたびにプロセスを起動したり OS に問い合わせたりしない（組み立てたときに調べた値を返す）。
    fn env(&self) -> Option<CodecEnv> {
        None
    }
    /// この形式を読めるか。
    fn can_decode(&self, format: &str) -> bool;
    /// ヘッダだけ読んで形を返す（描く前の上限の判断に使う）。扱えない変種は、ここか `decode` で `Unsupported` を返す
    /// （次の段のデコーダへ回る）。
    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError>;
    /// 値を読む。
    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError>;
    /// ファイルの形（パスで読む口）。既定はファイルを開いて [`Decoder::shape`] に渡す。外部のツールなど、パスをそのまま
    /// 渡したいデコーダが上書きする（`CodecSet::shape_path` はこちらを呼ぶ）。
    fn shape_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let mut src = BufReader::new(std::fs::File::open(path)?);
        self.shape(&mut src, format, opts, limits)
    }
    /// 中止を見ながら形を返す（`CodecSet` が値を読む前に呼ぶ。中止は `Canceled`）。既定は [`Decoder::shape`]（ヘッダだけ
    /// 読むもの）。形を決めるのに中身を読むデコーダ（外部のツールで 1 回デコードして数える ffmpeg の段など）が上書きする。
    fn shape_monitored(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        _mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        self.shape(src, format, opts, limits)
    }
    /// [`Decoder::shape_monitored`] のパスで読む口。既定は [`Decoder::shape_file`]。
    fn shape_file_monitored(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        _mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        self.shape_file(path, format, opts, limits)
    }
    /// ファイルの値（パスで読む口）。既定はファイルを開いて [`Decoder::decode`] に渡す（`CodecSet::decode_path`・
    /// `decode_with_path` はこちらを呼ぶ）。上書きするときも、読み口の口と同じ値を読むこと。
    fn decode_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let mut src = BufReader::new(std::fs::File::open(path)?);
        self.decode(&mut src, format, opts, limits, mon)
    }
}

/// エンコーダ。
pub trait Encoder: Send + Sync {
    /// 識別子（例 `png-rs-enc/1`。約束の版を含む）。書くバイト列が変わる変更は版を上げる（R2。旧版は残さなくてよい）。
    fn id(&self) -> &'static str;
    fn tier(&self) -> Tier;
    /// 同じ値・同じ設定なら、どの環境でも同じバイト列を書くか（R2）。既定は固定の段なら真。固定の段でないものは真に
    /// できない（`add_encoder` が断る）。
    fn reproducible(&self) -> bool {
        self.tier() == Tier::Pinned
    }
    /// 書く環境（書き出しの記録の `env` に入る）。既定は `None`（固定の段）。
    fn env(&self) -> Option<CodecEnv> {
        None
    }
    /// この形式・この形の値を書けるか。
    fn can_encode(&self, format: &str, shape: &Shape) -> bool;
    /// この形式を（どれかの形の値で）書けるか（能力の表に使う）。既定は、形式の表の値の種類ごとの代表の形
    /// （[`caps::probe_shapes`]）のどれかを `can_encode` が受け付けるか。代表の形で決まらないエンコーダは上書きする。
    fn writes_format(&self, format: &str) -> bool {
        caps::probe_shapes(format)
            .iter()
            .any(|s| self.can_encode(format, s))
    }
    /// 書き出しの設定の記述（UI と検証）。
    fn options(&self, format: &str) -> ParamSchema;
    /// この設定で実際に保存できるもの（失うものを先に知らせる。§6）。形式の表の能力ではなく、このエンコーダの能力。
    fn caps(&self, format: &str, opts: &serde_json::Value) -> MetaCaps;
    /// 書く。`out` に書き、書いたバイト数を返す。
    fn encode(
        &self,
        value: &Value,
        format: &str,
        opts: &serde_json::Value,
        out: &mut dyn std::io::Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError>;
}

/// 読み取りか書き出しか（受け止めた panic をどちらのエラーにするか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Decode,
    Encode,
}

/// 第三者のコードの panic を受け止めてエラーにする（自分のコードでは panic を禁じたまま。docs/editing.md §13）。
pub fn contain<T>(
    codec: &str,
    stage: Stage,
    f: impl FnOnce() -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(match stage {
            Stage::Decode => EngineError::Decode {
                codec: codec.to_string(),
                message: "読み取りの途中で異常が起きました".into(),
            },
            Stage::Encode => EngineError::Encode {
                codec: codec.to_string(),
                message: "書き出しの途中で異常が起きました".into(),
            },
        }),
    }
}

/// 固定の段のコーデックの設定。
#[derive(Clone, Default)]
pub struct PinnedOptions {
    /// SVG の文字を描くフォント（kura-mac が OS のフォントを読んで渡す）。ないときは文字を含む SVG を読まない。
    pub svg_fonts: Option<Arc<resvg::usvg::fontdb::Database>>,
}

/// 形式ごとに、段の順に試すコーデックの組。
#[derive(Clone, Default)]
pub struct CodecSet {
    decoders: Vec<Arc<dyn Decoder>>,
    /// 旧版のデコーダ（ふつうの読み取りでは選ばない。記録した識別子で作り直すときだけ使う）。
    legacy_decoders: Vec<Arc<dyn Decoder>>,
    encoders: Vec<Arc<dyn Encoder>>,
}

impl CodecSet {
    /// Kura のバイナリに入る Rust のコーデックだけの組（既定の設定）。
    pub fn pinned() -> Self {
        Self::pinned_with(&PinnedOptions::default())
    }

    /// 固定の段のコーデックを、設定（SVG の文字のフォントなど）を付けて組む。kura-mac・kura-core はここから組み、
    /// Platform・External のものを `add_decoder`・`add_encoder` で足す。
    pub fn pinned_with(opts: &PinnedOptions) -> Self {
        let png: Arc<png::PngCodec> = Arc::new(png::PngCodec);
        let jpeg: Arc<jpeg::JpegCodec> = Arc::new(jpeg::JpegCodec);
        let gif: Arc<gif::GifCodec> = Arc::new(gif::GifCodec);
        let webp: Arc<webp::WebpCodec> = Arc::new(webp::WebpCodec);
        let wav: Arc<wav::WavCodec> = Arc::new(wav::WavCodec);
        let aiff: Arc<aiff::AiffCodec> = Arc::new(aiff::AiffCodec);
        let flac: Arc<flac::FlacCodec> = Arc::new(flac::FlacCodec);
        let svg = match &opts.svg_fonts {
            Some(fonts) => svg::SvgCodec::with_fonts(fonts.clone()),
            None => svg::SvgCodec::default(),
        };
        Self {
            decoders: vec![
                png.clone(),
                jpeg.clone(),
                gif.clone(),
                webp.clone(),
                Arc::new(aseprite::AsepriteCodec),
                Arc::new(psd::PsdCodec),
                Arc::new(svg),
                wav.clone(),
                aiff.clone(),
                flac.clone(),
                Arc::new(mp3::Mp3Codec),
                Arc::new(lossy::SymphoniaDecoder::ogg_vorbis()),
                Arc::new(lossy::SymphoniaDecoder::m4a_alac()),
                Arc::new(lossy::SymphoniaDecoder::m4a_aac()),
                Arc::new(lossy::SymphoniaDecoder::adts_aac()),
                Arc::new(anchor::AnchorDecoder),
            ],
            legacy_decoders: png::legacy_decoders(),
            encoders: vec![
                png,
                jpeg,
                gif,
                webp,
                wav,
                aiff,
                flac,
                Arc::new(vorbis::OggVorbisEncoder),
            ],
        }
    }

    /// 旧版のデコーダを足す（読む値を変えた版を上げたとき、前の版を残す。ふつうの読み取りでは選ばない）。同じ識別子は断る。
    pub fn add_legacy_decoder(&mut self, d: Arc<dyn Decoder>) -> Result<(), EngineError> {
        self.check_new_decoder(d.id())?;
        self.legacy_decoders.push(d);
        Ok(())
    }

    fn check_new_decoder(&self, id: &str) -> Result<(), EngineError> {
        if self.decoder_by_id(id).is_some() {
            return Err(EngineError::Internal(format!(
                "同じ識別子のデコーダがあります（{id}）。記録の識別子で読み直すときに選べなくなります"
            )));
        }
        Ok(())
    }

    /// 識別子でデコーダを探す（今の版と旧版の両方から）。
    pub fn decoder_by_id(&self, id: &str) -> Option<&Arc<dyn Decoder>> {
        self.decoders
            .iter()
            .chain(self.legacy_decoders.iter())
            .find(|d| d.id() == id)
    }

    /// 識別子でエンコーダを探す。
    pub fn encoder_by_id(&self, id: &str) -> Option<&Arc<dyn Encoder>> {
        self.encoders.iter().find(|e| e.id() == id)
    }

    /// デコーダを足す（Platform・External。段の順に並べ直す。同じ段なら足した順）。同じ識別子は断る。
    /// 固定の段でないのに値の再現を約束する（`reproducible()` が真）デコーダも断る（記録を偽れないように。アンカーを作らない
    /// ことになるため）。
    pub fn add_decoder(&mut self, d: Arc<dyn Decoder>) -> Result<(), EngineError> {
        self.check_new_decoder(d.id())?;
        if d.tier() != Tier::Pinned && d.reproducible() {
            return Err(EngineError::Internal(format!(
                "固定の段でないデコーダは値の再現を約束できません（{}）",
                d.id()
            )));
        }
        self.decoders.push(d);
        self.decoders.sort_by_key(|d| d.tier());
        Ok(())
    }

    /// エンコーダを足す（段の順に並べ直す）。同じ識別子と、固定の段でないのにバイト列の再現（R2）を約束するものは断る。
    pub fn add_encoder(&mut self, e: Arc<dyn Encoder>) -> Result<(), EngineError> {
        if self.encoder_by_id(e.id()).is_some() {
            return Err(EngineError::Internal(format!(
                "同じ識別子のエンコーダがあります（{}）",
                e.id()
            )));
        }
        if e.tier() != Tier::Pinned && e.reproducible() {
            return Err(EngineError::Internal(format!(
                "固定の段でないエンコーダはバイト列の再現を約束できません（{}）",
                e.id()
            )));
        }
        self.encoders.push(e);
        self.encoders.sort_by_key(|e| e.tier());
        Ok(())
    }

    /// この形式を読むデコーダ（段の順）。
    pub fn decoders_for(&self, format: &str) -> Vec<&Arc<dyn Decoder>> {
        self.decoders
            .iter()
            .filter(|d| d.can_decode(format))
            .collect()
    }

    /// この形式を読むデコーダ（今の版の段の順に続けて、旧版も）。ファズと照合で使う。
    pub fn all_decoders_for(&self, format: &str) -> Vec<&Arc<dyn Decoder>> {
        self.decoders
            .iter()
            .chain(self.legacy_decoders.iter())
            .filter(|d| d.can_decode(format))
            .collect()
    }

    /// この形式・形を書くエンコーダ（段の順で最初のもの）。
    pub fn encoder(&self, format: &str, shape: &Shape) -> Option<&Arc<dyn Encoder>> {
        self.encoders.iter().find(|e| e.can_encode(format, shape))
    }

    /// 段の順に `shape` を試し、扱えないと答えたもの（`Unsupported`）は次へ回す。読めたデコーダと形を返す。
    fn pick(
        &self,
        input: &mut Input<'_>,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<(&Arc<dyn Decoder>, Shape), EngineError> {
        let mut last = EngineError::Unsupported(format!("{format} を読めません"));
        for d in self.decoders_for(format) {
            match contain(d.id(), Stage::Decode, || {
                input.shape(d.as_ref(), format, opts, limits, &NoMonitor)
            }) {
                Ok(shape) => return Ok((d, shape)),
                Err(e @ EngineError::Unsupported(_)) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// 読み口の形（ヘッダだけ）。段の順に試し、読めたデコーダの識別子と、上限で確かめた形を返す。
    pub fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<(Shape, &'static str), EngineError> {
        let (d, shape) = self.pick(&mut Input::Reader(src), format, opts, limits)?;
        limits.check(&shape).map_err(EngineError::Limit)?;
        Ok((shape, d.id()))
    }

    /// ファイルの形（ヘッダだけ。各デコーダの [`Decoder::shape_file`] を呼ぶ）。
    pub fn shape_path(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<(Shape, &'static str), EngineError> {
        let (d, shape) = self.pick(&mut Input::Path(path), format, opts, limits)?;
        limits.check(&shape).map_err(EngineError::Limit)?;
        Ok((shape, d.id()))
    }

    /// 段の順に読む（読み口かパス）。
    fn decode_any(
        &self,
        input: &mut Input<'_>,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        let mut last = EngineError::Unsupported(format!("{format} を読めません"));
        for d in self.decoders_for(format) {
            match decode_one(d, input, format, opts, limits, mon) {
                Err(e @ EngineError::Unsupported(_)) => last = e,
                other => return other,
            }
        }
        Err(last)
    }

    /// 読む。段の順に試し、`shape` か `decode` で扱えないと答えたもの（`Unsupported`）は次の段へ回す。
    /// 形を上限で確かめてから中身を読み、読み取りの記録を返す。
    pub fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        self.decode_any(&mut Input::Reader(src), format, opts, limits, mon)
    }

    /// 記録の識別子のデコーダ（旧版を含む）。
    fn recorded_decoder(&self, id: &str, format: &str) -> Result<&Arc<dyn Decoder>, EngineError> {
        self.decoder_by_id(id)
            .filter(|d| d.can_decode(format))
            .ok_or_else(|| {
                EngineError::Unsupported(format!("{format} を読むデコーダ {id} がありません"))
            })
    }

    /// 記録した識別子のデコーダ（旧版を含む）で読む。作り直しで、版の記録と同じ読み方をするときに使う（§8.4・§9.1）。
    pub fn decode_with(
        &self,
        id: &str,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        let d = self.recorded_decoder(id, format)?;
        decode_one(d, &mut Input::Reader(src), format, opts, limits, mon)
    }

    /// ファイルを読む（各デコーダの [`Decoder::shape_file`]・[`Decoder::decode_file`] を呼ぶ）。
    pub fn decode_path(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        self.decode_any(&mut Input::Path(path), format, opts, limits, mon)
    }

    /// 記録した識別子のデコーダ（旧版を含む）でファイルを読む（[`CodecSet::decode_with`] のパスの版）。
    pub fn decode_with_path(
        &self,
        id: &str,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        let d = self.recorded_decoder(id, format)?;
        decode_one(d, &mut Input::Path(path), format, opts, limits, mon)
    }

    /// 書いて、バイト列と記録を返す。書き手は段の順で最初にこの形を書けるもの（[`CodecSet::encoder`]）。設定は正規の
    /// JSON に直してから、エンコーダの記述で厳しく確かめる（知らないキー・型の違い・範囲の外はエラー。黙って既定に落とさない）。
    pub fn encode_bytes(
        &self,
        value: &Value,
        format: &str,
        opts: &serde_json::Value,
        mon: &dyn Monitor,
    ) -> Result<(Vec<u8>, EncodedStamp), EngineError> {
        let e = self
            .encoder(format, &value.shape())
            .ok_or_else(|| EngineError::Unsupported(format!("{format} には書けません")))?;
        encode_one(e, value, format, opts, mon)
    }

    /// 識別子で選んだ書き手で書く（能力の表の `WriteCap.codec`・書き出しの記録の `codec` をそのまま渡せる）。設定の確かめ方・
    /// panic の受け止め方・記録は [`CodecSet::encode_bytes`] と同じ。その識別子のエンコーダがない・この形式のこの形の値を
    /// 書けない（`can_encode` が偽）ときは `Unsupported`（段の順の別の書き手へ黙って回さない）。
    pub fn encode_bytes_with(
        &self,
        id: &str,
        value: &Value,
        format: &str,
        opts: &serde_json::Value,
        mon: &dyn Monitor,
    ) -> Result<(Vec<u8>, EncodedStamp), EngineError> {
        let e = self
            .encoder_by_id(id)
            .filter(|e| e.can_encode(format, &value.shape()))
            .ok_or_else(|| {
                EngineError::Unsupported(format!(
                    "{format} のこの値を書くエンコーダ {id} がありません"
                ))
            })?;
        encode_one(e, value, format, opts, mon)
    }
}

/// 1 つのエンコーダで書く：設定を正規の JSON に直してエンコーダの記述で確かめ、panic を受け止めて書き、記録を返す。
fn encode_one(
    e: &Arc<dyn Encoder>,
    value: &Value,
    format: &str,
    opts: &serde_json::Value,
    mon: &dyn Monitor,
) -> Result<(Vec<u8>, EncodedStamp), EngineError> {
    let opts: serde_json::Value = serde_json::from_slice(&canonical_json(opts)?)
        .map_err(|err| EngineError::Internal(format!("設定を正規の形にできません: {err}")))?;
    let issues = e.options(format).validate(&opts);
    if issues.has_errors() {
        return Err(EngineError::Invalid(issues));
    }
    let mut out = Vec::new();
    let bytes = contain(e.id(), Stage::Encode, || {
        e.encode(value, format, &opts, &mut out, mon)
    })?;
    let file_digest = Hasher::new("kura-file-v1").bytes_with_len(&out).finish();
    Ok((
        out,
        EncodedStamp {
            codec: e.id().to_string(),
            tier: e.tier(),
            format: format.to_string(),
            options: opts,
            bytes,
            file_digest,
            reproducible: e.reproducible(),
            env: e.env(),
        },
    ))
}

/// 読む元（読み口か、ファイルのパス）。
enum Input<'a> {
    Reader(&'a mut dyn ReadSeek),
    Path(&'a Path),
}

impl Input<'_> {
    fn shape(
        &mut self,
        d: &dyn Decoder,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        match self {
            Input::Reader(src) => {
                src.rewind()?;
                d.shape_monitored(&mut **src, format, opts, limits, mon)
            }
            Input::Path(path) => d.shape_file_monitored(path, format, opts, limits, mon),
        }
    }

    fn decode(
        &mut self,
        d: &dyn Decoder,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        match self {
            Input::Reader(src) => {
                src.rewind()?;
                d.decode(&mut **src, format, opts, limits, mon)
            }
            Input::Path(path) => d.decode_file(path, format, opts, limits, mon),
        }
    }
}

/// 1 つのデコーダで読む：形を上限で確かめてから中身を読み、ヘッダと中身が合うか確かめる。
fn decode_one(
    d: &Arc<dyn Decoder>,
    input: &mut Input<'_>,
    format: &str,
    opts: &DecodeOptions,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<(Value, DecodeStamp), EngineError> {
    let shape = contain(d.id(), Stage::Decode, || {
        input.shape(d.as_ref(), format, opts, limits, mon)
    })?;
    limits.check(&shape).map_err(EngineError::Limit)?;
    let value = contain(d.id(), Stage::Decode, || {
        input.decode(d.as_ref(), format, opts, limits, mon)
    })?;
    if !shape.admits(&value.shape()) {
        return Err(EngineError::Decode {
            codec: d.id().to_string(),
            message: "ヘッダと中身の大きさが合いません".into(),
        });
    }
    let value_digest = value.digest()?;
    Ok((
        value,
        DecodeStamp {
            codec: d.id().to_string(),
            tier: d.tier(),
            reproducible: d.reproducible(),
            value_digest,
            env: d.env(),
        },
    ))
}

/// 外から来た大きさで確保するときの補助（足りなければ落ちずにエラー）。
pub fn try_vec<T: Clone>(len: usize, fill: T) -> Result<Vec<T>, EngineError> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| EngineError::Limit(format!("{len} 個の領域を確保できません")))?;
    v.resize(len, fill);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::shape::Dim;
    use crate::testfiles::FileGen;
    use crate::testgen::InputGen;
    use crate::value::{Audio, AudioMeta};
    use serde_json::json;
    use std::io::Cursor;

    /// 書き出しの設定は厳しく確かめる（知らないキー・型の違い・範囲の外はエラー）。正規の形に直してから確かめるので、24.0 は 24 と同じ。
    #[test]
    fn encoder_options_are_validated_and_canonicalized() {
        let set = CodecSet::pinned();
        let img = (InputGen::Noise {
            w: 4,
            h: 4,
            fmt: crate::value::PixelFormat::Rgb8,
            seed: 1,
        })
        .generate()
        .unwrap();
        let snd = (InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 10,
            seed: 1,
        })
        .generate()
        .unwrap();
        for bad in [
            json!({ "compression": "best" }),
            json!({ "compresion": "fast" }),
            json!({ "compression": 3 }),
            json!(["fast"]),
        ] {
            assert!(
                matches!(
                    set.encode_bytes(&img, "png", &bad, &NoMonitor),
                    Err(EngineError::Invalid(_))
                ),
                "{bad}"
            );
        }
        for bad in [
            json!({ "bits": "24" }),
            json!({ "bits": 12 }),
            json!({ "float": "true" }),
            json!({ "bitz": 24 }),
        ] {
            assert!(
                matches!(
                    set.encode_bytes(&snd, "wav", &bad, &NoMonitor),
                    Err(EngineError::Invalid(_))
                ),
                "{bad}"
            );
        }
        let (a, sa) = set
            .encode_bytes(&snd, "wav", &json!({ "bits": 24.0 }), &NoMonitor)
            .unwrap();
        let (b, sb) = set
            .encode_bytes(&snd, "wav", &json!({ "bits": 24 }), &NoMonitor)
            .unwrap();
        assert_eq!((a, sa.options), (b, sb.options));
    }

    /// 固定のデコーダが扱えないと答えた変種（IMA ADPCM の WAV など）は、次の段のデコーダへ回す。
    #[test]
    fn unsupported_variants_fall_through_to_the_next_tier() {
        struct Fake;
        impl Decoder for Fake {
            fn id(&self) -> &'static str {
                "fake-platform/1"
            }
            fn tier(&self) -> Tier {
                Tier::Platform
            }
            fn can_decode(&self, format: &str) -> bool {
                format == "wav"
            }
            fn shape(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
            ) -> Result<Shape, EngineError> {
                Ok(Shape::Audio {
                    rate: 8_000,
                    channels: 1,
                    frames: Dim::Exact(1),
                })
            }
            fn decode(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
                _: &dyn Monitor,
            ) -> Result<Value, EngineError> {
                Ok(Value::Audio(Arc::new(Audio::new(
                    8_000,
                    vec![Arc::new(vec![0.0])],
                    AudioMeta::default(),
                )?)))
            }
        }
        let adpcm = FileGen::Wav {
            tag: 0x11,
            bits: 16,
            channels: 1,
            rate: 8_000,
            frames: 4,
            extensible: false,
            smpl: None,
            cues: vec![],
            labels: vec![],
            info: vec![],
            unknown_size: false,
            seed: 1,
        }
        .bytes()
        .unwrap();
        let pinned = CodecSet::pinned();
        assert!(matches!(
            pinned.decode(
                &mut Cursor::new(adpcm.clone()),
                "wav",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor
            ),
            Err(EngineError::Unsupported(_))
        ));
        let mut set = CodecSet::pinned();
        set.add_decoder(Arc::new(Fake)).unwrap();
        let (_, stamp) = set
            .decode(
                &mut Cursor::new(adpcm),
                "wav",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier),
            ("fake-platform/1", Tier::Platform)
        );
        // ふつうの WAV は固定のデコーダが読む
        let pcm = FileGen::Wav {
            tag: 1,
            bits: 16,
            channels: 1,
            rate: 8_000,
            frames: 4,
            extensible: false,
            smpl: None,
            cues: vec![],
            labels: vec![],
            info: vec![],
            unknown_size: false,
            seed: 1,
        }
        .bytes()
        .unwrap();
        let (_, stamp) = set
            .decode(
                &mut Cursor::new(pcm),
                "wav",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(stamp.tier, Tier::Pinned);
    }

    /// 形は読めても中身で扱えないと分かった（`decode` が `Unsupported`）ときも、次の段へ回す。
    /// 旧版のデコーダはふつうの読み取りでは選ばれず、記録した識別子でだけ選べる。
    #[test]
    fn decode_stage_unsupported_falls_through_and_legacy_is_only_by_id() {
        struct Picky(&'static str, Tier, bool);
        impl Decoder for Picky {
            fn id(&self) -> &'static str {
                self.0
            }
            fn tier(&self) -> Tier {
                self.1
            }
            fn can_decode(&self, format: &str) -> bool {
                format == "raw"
            }
            fn shape(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
            ) -> Result<Shape, EngineError> {
                Ok(Shape::Audio {
                    rate: 8_000,
                    channels: 1,
                    frames: Dim::Exact(1),
                })
            }
            fn decode(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
                _: &dyn Monitor,
            ) -> Result<Value, EngineError> {
                if !self.2 {
                    return Err(EngineError::Unsupported("この変種は読めません".into()));
                }
                Ok(Value::Audio(Arc::new(Audio::new(
                    8_000,
                    vec![Arc::new(vec![0.0])],
                    AudioMeta::default(),
                )?)))
            }
        }
        let mut set = CodecSet::default();
        set.add_decoder(Arc::new(Picky("pinned-dec/2", Tier::Pinned, false)))
            .unwrap();
        set.add_decoder(Arc::new(Picky("platform-dec/1", Tier::Platform, true)))
            .unwrap();
        set.add_legacy_decoder(Arc::new(Picky("pinned-dec/1", Tier::Pinned, true)))
            .unwrap();
        let read = |set: &CodecSet, id: Option<&str>| {
            let mut src = Cursor::new(vec![0u8; 4]);
            let opts = DecodeOptions::default();
            let limits = Limits::default();
            match id {
                Some(id) => set.decode_with(id, &mut src, "raw", &opts, &limits, &NoMonitor),
                None => set.decode(&mut src, "raw", &opts, &limits, &NoMonitor),
            }
        };
        assert_eq!(read(&set, None).unwrap().1.codec, "platform-dec/1");
        assert_eq!(
            read(&set, Some("pinned-dec/1")).unwrap().1.codec,
            "pinned-dec/1"
        );
        assert!(matches!(
            read(&set, Some("pinned-dec/2")),
            Err(EngineError::Unsupported(_))
        ));
        assert!(matches!(
            read(&set, Some("missing-dec/1")),
            Err(EngineError::Unsupported(_))
        ));
        assert!(set.decoder_by_id("pinned-dec/1").is_some());
        let pinned = CodecSet::pinned();
        assert!(pinned.decoder_by_id("png-rs-dec/1").is_some());
        assert!(pinned.encoder_by_id("png-rs-enc/1").is_some());
        assert!(pinned.decoder_by_id("png-rs-enc/1").is_none());
    }

    /// 同じ識別子のコーデックは重ねて登録できない（記録の識別子で読み直すときに選べなくなるため）。
    #[test]
    fn duplicate_ids_are_refused() {
        let mut set = CodecSet::pinned();
        assert!(set.add_decoder(Arc::new(svg::SvgCodec::default())).is_err());
        assert!(set.add_legacy_decoder(Arc::new(png::PngDecoderV1)).is_err());
        assert!(set.add_encoder(Arc::new(png::PngCodec)).is_err());
        let with = CodecSet::pinned_with(&PinnedOptions {
            svg_fonts: Some(Arc::new(resvg::usvg::fontdb::Database::new())),
        });
        assert_eq!(with.decoders_for("svg").len(), 1);
    }

    /// パスで読む口：`decode_path`・`shape_path`・`decode_with_path` はデコーダの `*_file` を呼ぶ。既定の実装は読み口の口と
    /// 同じ値を読み、上書きしたデコーダにはパスがそのまま渡る。
    #[test]
    fn path_routes_call_the_file_methods() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let path = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/psd-gray8.sips.png"
        ));
        let set = CodecSet::pinned();
        let opts = DecodeOptions::default();
        let limits = Limits::default();
        let bytes = std::fs::read(&path).unwrap();
        let (v1, s1) = set
            .decode(
                &mut Cursor::new(bytes.clone()),
                "png",
                &opts,
                &limits,
                &NoMonitor,
            )
            .unwrap();
        let (v2, s2) = set
            .decode_path(&path, "png", &opts, &limits, &NoMonitor)
            .unwrap();
        assert_eq!((v1, &s1), (v2, &s2));
        let (v3, s3) = set
            .decode_with_path(&s1.codec, &path, "png", &opts, &limits, &NoMonitor)
            .unwrap();
        assert_eq!(s3, s1);
        let (shape, id) = set.shape_path(&path, "png", &opts, &limits).unwrap();
        assert!(shape.admits(&v3.shape()));
        assert_eq!(
            set.shape(&mut Cursor::new(bytes), "png", &opts, &limits)
                .unwrap(),
            (shape, id)
        );

        /// 外部のツールのように、パスを直接受け取るデコーダ（読み口の口は使わない）。
        struct ByPath {
            seen: std::sync::Mutex<Vec<std::path::PathBuf>>,
            reader_calls: AtomicUsize,
        }
        impl Decoder for ByPath {
            fn id(&self) -> &'static str {
                "by-path-dec/1"
            }
            fn tier(&self) -> Tier {
                Tier::External
            }
            fn can_decode(&self, format: &str) -> bool {
                format == "opus"
            }
            fn shape(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
            ) -> Result<Shape, EngineError> {
                self.reader_calls.fetch_add(1, Ordering::SeqCst);
                Err(EngineError::Internal("読み口では読まない".into()))
            }
            fn decode(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
                _: &dyn Monitor,
            ) -> Result<Value, EngineError> {
                self.reader_calls.fetch_add(1, Ordering::SeqCst);
                Err(EngineError::Internal("読み口では読まない".into()))
            }
            fn shape_file(
                &self,
                path: &Path,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
            ) -> Result<Shape, EngineError> {
                self.seen.lock().unwrap().push(path.to_path_buf());
                Ok(Shape::Audio {
                    rate: 48_000,
                    channels: 1,
                    frames: Dim::Exact(2),
                })
            }
            fn decode_file(
                &self,
                path: &Path,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
                _: &dyn Monitor,
            ) -> Result<Value, EngineError> {
                self.seen.lock().unwrap().push(path.to_path_buf());
                Ok(Value::Audio(Arc::new(Audio::new(
                    48_000,
                    vec![Arc::new(vec![0.5, -0.5])],
                    AudioMeta::default(),
                )?)))
            }
            fn env(&self) -> Option<CodecEnv> {
                Some(CodecEnv::new("ffmpeg", "9.0.2").with("codec", "libopus"))
            }
        }
        let d = Arc::new(ByPath {
            seen: std::sync::Mutex::new(Vec::new()),
            reader_calls: AtomicUsize::new(0),
        });
        let mut set = CodecSet::pinned();
        set.add_decoder(d.clone()).unwrap();
        let missing = std::path::Path::new("/nonexistent/kura/a.opus");
        let (_, stamp) = set
            .decode_path(missing, "opus", &opts, &limits, &NoMonitor)
            .unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            ("by-path-dec/1", Tier::External, false)
        );
        assert_eq!(
            stamp.env.as_ref().and_then(|e| e.components.get("codec")),
            Some(&"libopus".to_string())
        );
        set.decode_with_path("by-path-dec/1", missing, "opus", &opts, &limits, &NoMonitor)
            .unwrap();
        set.shape_path(missing, "opus", &opts, &limits).unwrap();
        assert_eq!(d.reader_calls.load(Ordering::SeqCst), 0);
        assert!(d.seen.lock().unwrap().iter().all(|p| p == missing));
        assert_eq!(d.seen.lock().unwrap().len(), 5);
        // 既定の実装（固定の段）は、ないファイルを入出力のエラーにする
        assert!(matches!(
            CodecSet::pinned().decode_path(missing, "png", &opts, &limits, &NoMonitor),
            Err(EngineError::Io(_))
        ));
    }

    /// 記録の JSON：固定の段の読み取りの記録は段 9 より前と同じ形（env なし）で、読み戻せる。Platform の記録は env を持ち、
    /// 知らない項目は拒む。書き出しの記録は、固定の段なら reproducible・env を書かない。
    #[test]
    fn stamps_keep_their_json_shape_and_read_back() {
        let set = CodecSet::pinned();
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/psd-gray8.sips.png"
        ))
        .unwrap();
        let (v, stamp) = set
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let json = serde_json::to_value(&stamp).unwrap();
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["codec", "reproducible", "tier", "valueDigest"]);
        let back: DecodeStamp = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, stamp);
        let platform = DecodeStamp {
            codec: "imageio-dec/1".into(),
            tier: Tier::Platform,
            reproducible: false,
            value_digest: stamp.value_digest,
            env: Some(
                CodecEnv::new("macOS", "26.7 (25G229)")
                    .with("imageio", "2784.6.4")
                    .with("build", "25G229"),
            ),
        };
        let text = String::from_utf8(crate::digest::canonical(&platform).unwrap()).unwrap();
        assert!(text.contains(
            r#""env":{"components":{"build":"25G229","imageio":"2784.6.4"},"tool":"macOS","version":"26.7 (25G229)"}"#
        ));
        assert_eq!(
            serde_json::from_str::<DecodeStamp>(&text).unwrap(),
            platform
        );
        let mut extra = json;
        extra["format"] = serde_json::json!("png");
        assert!(serde_json::from_value::<DecodeStamp>(extra).is_err());
        // 書き出しの記録：固定の段は段 8 までと同じ JSON（項目・並び・値の書き方）で、読み戻せる
        let (_, enc) = set
            .encode_bytes(&v, "png", &serde_json::json!({}), &NoMonitor)
            .unwrap();
        assert!(enc.reproducible && enc.env.is_none());
        let json = serde_json::to_value(&enc).unwrap();
        assert!(json.get("reproducible").is_none() && json.get("env").is_none());
        let stage8 = format!(
            r#"{{"codec":"png-rs-enc/1","tier":"pinned","format":"png","options":{{}},"bytes":{},"fileDigest":"{}"}}"#,
            enc.bytes, enc.file_digest
        );
        assert_eq!(serde_json::to_string(&enc).unwrap(), stage8);
        assert_eq!(
            String::from_utf8(crate::digest::canonical(&enc).unwrap()).unwrap(),
            format!(
                r#"{{"bytes":{},"codec":"png-rs-enc/1","fileDigest":"{}","format":"png","options":{{}},"tier":"pinned"}}"#,
                enc.bytes, enc.file_digest
            )
        );
        // 段 8 までの記録（reproducible・env を書いていない）は、再現できる・環境なしとして読む
        let back: EncodedStamp = serde_json::from_str(&stage8).unwrap();
        assert_eq!(back, enc);
        assert!(back.reproducible && back.env.is_none());
        // Platform の書き出しの記録は reproducible: false と env を書き、読み戻せる
        let platform = EncodedStamp {
            codec: "aac-audiotoolbox-enc/1".into(),
            tier: Tier::Platform,
            format: "m4a".into(),
            options: serde_json::json!({ "bitrate": 256, "mode": "vbrConstrained" }),
            bytes: 1234,
            file_digest: enc.file_digest,
            reproducible: false,
            env: Some(CodecEnv::new("macOS", "26.7 (25G229)").with("audiotoolbox", "1.0")),
        };
        let text = String::from_utf8(crate::digest::canonical(&platform).unwrap()).unwrap();
        assert!(text.contains(
            r#""env":{"components":{"audiotoolbox":"1.0"},"tool":"macOS","version":"26.7 (25G229)"}"#
        ));
        assert!(text.contains(r#""reproducible":false"#));
        assert_eq!(
            serde_json::from_str::<EncodedStamp>(&text).unwrap(),
            platform
        );
        // 知らない項目・足りない項目は拒む
        let mut extra = json.clone();
        extra["valueDigest"] = serde_json::json!(enc.file_digest);
        assert!(serde_json::from_value::<EncodedStamp>(extra).is_err());
        let mut missing = json;
        missing.as_object_mut().unwrap().remove("format");
        assert!(serde_json::from_value::<EncodedStamp>(missing).is_err());
    }

    /// 識別子で書き手を選んで書く：能力の表の `WriteCap.codec` をそのまま渡せ、段の順の先頭でない書き手でも書ける。設定の
    /// 確かめ方（正規の形・知らないキー）・panic の受け止め・記録は `encode_bytes` と同じ。ない・その形を書けない識別子は
    /// `Unsupported`（別の書き手へ回さない）。
    #[test]
    fn encode_bytes_with_picks_the_writer_by_id() {
        use crate::codec::platform::aac::PlatformAacEncoder;
        use crate::testing::platform::FakeAacBackend;
        /// 書くときに panic する偽の書き手（第三者のコードの異常を写す）。
        struct Panicky;
        impl Encoder for Panicky {
            fn id(&self) -> &'static str {
                "panicky-enc/1"
            }
            fn tier(&self) -> Tier {
                Tier::External
            }
            fn reproducible(&self) -> bool {
                false
            }
            fn can_encode(&self, format: &str, shape: &Shape) -> bool {
                format == "m4a" && shape.kind() == crate::shape::ValueKind::Audio
            }
            fn options(&self, _: &str) -> ParamSchema {
                ParamSchema::default()
            }
            fn caps(&self, _: &str, _: &serde_json::Value) -> MetaCaps {
                formats::NONE
            }
            fn encode(
                &self,
                _: &Value,
                _: &str,
                _: &serde_json::Value,
                _: &mut dyn std::io::Write,
                _: &dyn Monitor,
            ) -> Result<u64, EngineError> {
                panic!("偽の書き手の異常");
            }
        }
        let aac = FakeAacBackend::new();
        let aac_opens = aac.counters();
        let mut alac = FakeAacBackend::new();
        alac.id = "fake-alac-enc/1";
        alac.bitrates = vec![64, 128];
        alac.env = Some(CodecEnv::new("fake", "2.0").with("codec", "alac"));
        let alac_last = alac.last_settings();
        let mut set = CodecSet::pinned();
        set.add_encoder(Arc::new(PlatformAacEncoder::new(aac)))
            .unwrap();
        set.add_encoder(Arc::new(PlatformAacEncoder::new(alac)))
            .unwrap();
        set.add_encoder(Arc::new(Panicky)).unwrap();
        let v = Value::Audio(Arc::new(
            Audio::new(
                48_000,
                vec![Arc::new(vec![0.1; 100]), Arc::new(vec![-0.2; 100])],
                AudioMeta::default(),
            )
            .unwrap(),
        ));
        let caps = set.format_caps("m4a").unwrap();
        assert_eq!(
            caps.write.iter().map(|w| w.codec).collect::<Vec<_>>(),
            ["fake-aac-enc/1", "fake-alac-enc/1", "panicky-enc/1"]
        );
        // 既定（encode_bytes）は段の順の先頭
        let (_, s) = set.encode_bytes(&v, "m4a", &json!({}), &NoMonitor).unwrap();
        assert_eq!(s.codec, "fake-aac-enc/1");
        assert_eq!(aac_opens.opens.load(std::sync::atomic::Ordering::SeqCst), 1);
        // 能力の表の識別子をそのまま渡す：記録はその書き手のもの
        for w in caps.write.iter().filter(|w| w.codec != "panicky-enc/1") {
            let (bytes, s) = set
                .encode_bytes_with(w.codec, &v, "m4a", &json!({}), &NoMonitor)
                .unwrap();
            assert_eq!(
                (s.codec.as_str(), s.tier, s.reproducible, &s.env),
                (w.codec, w.tier, w.reproducible, &w.env)
            );
            assert_eq!((s.format.as_str(), s.bytes), ("m4a", bytes.len() as u64));
            assert_eq!(
                s.file_digest,
                Hasher::new("kura-file-v1").bytes_with_len(&bytes).finish()
            );
        }
        assert_eq!(aac_opens.opens.load(std::sync::atomic::Ordering::SeqCst), 2);
        // 2 つめの書き手が自分の能力（選べるビットレート）で書いた
        assert_eq!(alac_last.lock().unwrap().unwrap().bitrate_kbps, 128);
        // 設定は正規の形にしてから確かめる（128.0 は 128）。知らないキー・範囲の外は Invalid
        let (a, sa) = set
            .encode_bytes_with(
                "fake-alac-enc/1",
                &v,
                "m4a",
                &json!({ "bitrate": 64.0 }),
                &NoMonitor,
            )
            .unwrap();
        let (b, sb) = set
            .encode_bytes_with(
                "fake-alac-enc/1",
                &v,
                "m4a",
                &json!({ "bitrate": 64 }),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!((a, &sa.options), (b, &sb.options));
        assert_eq!(sa.options, json!({ "bitrate": 64 }));
        assert_eq!(alac_last.lock().unwrap().unwrap().bitrate_kbps, 64);
        for bad in [json!({ "profile": "alac" }), json!({ "quality": 200 })] {
            assert!(
                matches!(
                    set.encode_bytes_with("fake-alac-enc/1", &v, "m4a", &bad, &NoMonitor),
                    Err(EngineError::Invalid(_))
                ),
                "{bad}"
            );
        }
        // panic は書き出しのエラーにする
        match set.encode_bytes_with("panicky-enc/1", &v, "m4a", &json!({}), &NoMonitor) {
            Err(EngineError::Encode { codec, .. }) => assert_eq!(codec, "panicky-enc/1"),
            other => panic!("{other:?}"),
        }
        // ない識別子・別の形式の書き手・その形を書けない書き手は Unsupported（先頭の書き手へ回さない）
        let hi = Value::Audio(Arc::new(
            Audio::new(96_000, vec![Arc::new(vec![0.1; 10])], AudioMeta::default()).unwrap(),
        ));
        let before = alac_last.lock().unwrap().unwrap();
        for (id, value, format) in [
            ("missing-enc/1", &v, "m4a"),
            ("png-rs-enc/1", &v, "m4a"),
            ("fake-alac-enc/1", &v, "mp3"),
            ("fake-alac-enc/1", &hi, "m4a"),
            ("png-rs-dec/2", &v, "m4a"),
        ] {
            assert!(
                matches!(
                    set.encode_bytes_with(id, value, format, &json!({}), &NoMonitor),
                    Err(EngineError::Unsupported(_))
                ),
                "{id} {format}"
            );
        }
        assert_eq!(alac_last.lock().unwrap().unwrap(), before);
        assert_eq!(aac_opens.opens.load(std::sync::atomic::Ordering::SeqCst), 2);
        // 固定の段の書き手も同じ（識別子で選んでも encode_bytes と同じバイト列・記録）
        let img = InputGen::Noise {
            w: 3,
            h: 2,
            fmt: crate::value::PixelFormat::Rgba8,
            seed: 2,
        }
        .generate()
        .unwrap();
        let pinned = CodecSet::pinned();
        for format in ["png", "webp", "gif", "jpeg"] {
            let Some(w) = pinned
                .format_caps(format)
                .and_then(|c| c.default_writer().cloned())
            else {
                panic!("{format} を書ける");
            };
            let opts = json!({});
            let by_id = pinned.encode_bytes_with(w.codec, &img, format, &opts, &NoMonitor);
            let by_tier = pinned.encode_bytes(&img, format, &opts, &NoMonitor);
            match (by_id, by_tier) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "{format}"),
                (Err(a), Err(b)) => assert_eq!(a.code(), b.code(), "{format}"),
                (a, b) => panic!("{format}: {a:?} / {b:?}"),
            }
        }
    }

    /// 固定の段でないコーデックは、値・バイト列の再現を約束できない（記録を偽れないように）。
    #[test]
    fn non_pinned_codecs_cannot_claim_reproducible() {
        struct Liar(Tier);
        impl Decoder for Liar {
            fn id(&self) -> &'static str {
                "liar-dec/1"
            }
            fn tier(&self) -> Tier {
                self.0
            }
            fn reproducible(&self) -> bool {
                true
            }
            fn can_decode(&self, _: &str) -> bool {
                false
            }
            fn shape(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
            ) -> Result<Shape, EngineError> {
                Err(EngineError::Unsupported(String::new()))
            }
            fn decode(
                &self,
                _: &mut dyn ReadSeek,
                _: &str,
                _: &DecodeOptions,
                _: &Limits,
                _: &dyn Monitor,
            ) -> Result<Value, EngineError> {
                Err(EngineError::Unsupported(String::new()))
            }
        }
        struct LiarEnc;
        impl Encoder for LiarEnc {
            fn id(&self) -> &'static str {
                "liar-enc/1"
            }
            fn tier(&self) -> Tier {
                Tier::External
            }
            fn reproducible(&self) -> bool {
                true
            }
            fn can_encode(&self, _: &str, _: &Shape) -> bool {
                false
            }
            fn options(&self, _: &str) -> ParamSchema {
                ParamSchema::default()
            }
            fn caps(&self, _: &str, _: &serde_json::Value) -> MetaCaps {
                formats::NONE
            }
            fn encode(
                &self,
                _: &Value,
                _: &str,
                _: &serde_json::Value,
                _: &mut dyn std::io::Write,
                _: &dyn Monitor,
            ) -> Result<u64, EngineError> {
                Ok(0)
            }
        }
        let mut set = CodecSet::pinned();
        assert!(set.add_decoder(Arc::new(Liar(Tier::Platform))).is_err());
        assert!(set.add_decoder(Arc::new(Liar(Tier::External))).is_err());
        assert!(set.add_decoder(Arc::new(Liar(Tier::Pinned))).is_ok());
        assert!(set.add_encoder(Arc::new(LiarEnc)).is_err());
        // 既定の答え（Platform・External は偽）
        assert!(!LiarEncDefault.reproducible());
        struct LiarEncDefault;
        impl Encoder for LiarEncDefault {
            fn id(&self) -> &'static str {
                "x-enc/1"
            }
            fn tier(&self) -> Tier {
                Tier::Platform
            }
            fn can_encode(&self, format: &str, shape: &Shape) -> bool {
                format == "m4a" && shape.kind() == crate::shape::ValueKind::Audio
            }
            fn options(&self, _: &str) -> ParamSchema {
                ParamSchema::default()
            }
            fn caps(&self, _: &str, _: &serde_json::Value) -> MetaCaps {
                formats::NONE
            }
            fn encode(
                &self,
                _: &Value,
                _: &str,
                _: &serde_json::Value,
                _: &mut dyn std::io::Write,
                _: &dyn Monitor,
            ) -> Result<u64, EngineError> {
                Ok(0)
            }
        }
        assert!(LiarEncDefault.writes_format("m4a"));
        assert!(!LiarEncDefault.writes_format("mp3"));
        assert!(LiarEncDefault.env().is_none());
    }

    /// エンコーダの caps は、そのエンコーダが書けるものと食い違わない（アニメ・透明）。
    #[test]
    fn encoder_caps_match_what_they_write() {
        use crate::testgen::InputGen;
        use crate::value::PixelFormat;
        let set = CodecSet::pinned();
        let frames = InputGen::NoiseFrames {
            w: 8,
            h: 8,
            fmt: PixelFormat::Rgba8,
            count: 2,
            seed: 1,
        }
        .generate()
        .unwrap();
        let sprite = InputGen::Sprite {
            w: 8,
            h: 8,
            rect: crate::geom::IRect::new(2, 2, 3, 3),
        }
        .generate()
        .unwrap();
        for format in ["png", "jpeg", "gif", "webp"] {
            for e in set.encoders.iter().filter(|e| {
                e.can_encode(format, &sprite.shape()) || e.can_encode(format, &frames.shape())
            }) {
                let caps = e.caps(format, &serde_json::json!({}));
                assert_eq!(
                    e.can_encode(format, &frames.shape()),
                    caps.animation,
                    "{format} {}",
                    e.id()
                );
                let alpha_ok = set
                    .encode_bytes(
                        &sprite,
                        format_by_id(format).map_or("", |f| f.id),
                        &serde_json::json!({}),
                        &NoMonitor,
                    )
                    .is_ok();
                assert_eq!(alpha_ok, caps.alpha != AlphaCaps::None, "{format}");
            }
        }
    }
}
