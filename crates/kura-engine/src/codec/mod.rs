//! コーデック（docs/editing.md §9）。ファイルと値のあいだの読み書き。
//!
//! - 段（`Tier`）：Kura のバイナリに入る Rust のもの（`Pinned`。版を固定し、結果を約束する）→ macOS の機能（`Platform`。kura-mac が注入）
//!   → 外部の ffmpeg（`External`。kura-core が注入）の順に試し、使ったものの識別子を記録する。
//! - 外から来たファイルを読むので、確保の前に上限で判断し、第三者のデコーダの境界では panic を受け止めてエラーにする。

pub mod aiff;
pub mod aseprite;
pub mod exif;
pub mod flac;
pub mod formats;
pub mod gif;
pub mod jpeg;
pub mod lossy;
pub mod mp3;
pub(crate) mod ogg;
pub mod pcm;
pub mod png;
pub mod psd;
pub mod svg;
pub(crate) mod symph;
pub mod vorbis;
pub mod wav;
pub mod webp;

use std::io::{BufReader, Read, Seek};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

pub use formats::{
    AlphaCaps, FormatDesc, ItemKind, MetaCaps, caps_of, format_by_ext, format_by_id,
};

use crate::digest::{Digest, Hasher, canonical_json};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Tier {
    /// Kura のバイナリに入る Rust のもの。版を固定し、結果を約束する（R1・R2）。
    Pinned,
    /// macOS の機能。OS の版で結果が変わりうる。
    Platform,
    /// 外部のツール（ffmpeg）。ツールの版で結果が変わりうる。
    External,
}

/// 読み取りの記録（版の記録に残す）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodeStamp {
    /// 使ったデコーダ（例 `png-rs-dec/1`）。作り直しでは `CodecSet::decode_with` でこのデコーダを選ぶ。
    pub codec: String,
    pub tier: Tier,
    /// 同じ識別子のデコーダなら、どの環境でも同じ値を読むか（偽なら、初めて編集するときにアンカーを作る。§9.4）。
    pub reproducible: bool,
    /// 読み取った値のダイジェスト（作り直しで照らす。E29）。
    pub value_digest: Digest,
}

/// 書き出しの記録。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodedStamp {
    pub codec: String,
    pub tier: Tier,
    pub format: &'static str,
    /// 確かめて正規の形にした書き出しの設定。
    pub options: serde_json::Value,
    pub bytes: u64,
    /// 書いたファイルの中身のダイジェスト（R2 の確認に使う）。
    pub file_digest: Digest,
}

/// デコーダ。
pub trait Decoder: Send + Sync {
    /// 識別子（例 `png-rs-dec/1`。約束の版を含む）。読む値が変わる変更は版を上げ、旧版を `add_legacy_decoder` で残す（§9.1）。
    fn id(&self) -> &'static str;
    fn tier(&self) -> Tier;
    /// 同じ識別子なら、どの環境でも同じ値を読むか。既定は固定の段なら真（OS の機能・外部のツールは偽）。
    /// 固定の段でも、中で OS の libm を使うもの（SVG）は偽にする。
    fn reproducible(&self) -> bool {
        self.tier() == Tier::Pinned
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
}

/// エンコーダ。
pub trait Encoder: Send + Sync {
    /// 識別子（例 `png-rs-enc/1`。約束の版を含む）。書くバイト列が変わる変更は版を上げる（R2。旧版は残さなくてよい）。
    fn id(&self) -> &'static str;
    fn tier(&self) -> Tier;
    /// この形式・この形の値を書けるか。
    fn can_encode(&self, format: &str, shape: &Shape) -> bool;
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
    pub fn add_decoder(&mut self, d: Arc<dyn Decoder>) -> Result<(), EngineError> {
        self.check_new_decoder(d.id())?;
        self.decoders.push(d);
        self.decoders.sort_by_key(|d| d.tier());
        Ok(())
    }

    /// エンコーダを足す（段の順に並べ直す）。同じ識別子は断る。
    pub fn add_encoder(&mut self, e: Arc<dyn Encoder>) -> Result<(), EngineError> {
        if self.encoder_by_id(e.id()).is_some() {
            return Err(EngineError::Internal(format!(
                "同じ識別子のエンコーダがあります（{}）",
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
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<(&Arc<dyn Decoder>, Shape), EngineError> {
        let mut last = EngineError::Unsupported(format!("{format} を読めません"));
        for d in self.decoders_for(format) {
            src.rewind()?;
            match contain(d.id(), Stage::Decode, || d.shape(src, format, opts, limits)) {
                Ok(shape) => return Ok((d, shape)),
                Err(e @ EngineError::Unsupported(_)) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// ファイルの形（ヘッダだけ）。
    pub fn shape_path(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<(Shape, &'static str), EngineError> {
        let mut src = BufReader::new(std::fs::File::open(path)?);
        let (d, shape) = self.pick(&mut src, format, opts, limits)?;
        limits.check(&shape).map_err(EngineError::Limit)?;
        Ok((shape, d.id()))
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
        let mut last = EngineError::Unsupported(format!("{format} を読めません"));
        for d in self.decoders_for(format) {
            match decode_one(d, src, format, opts, limits, mon) {
                Err(e @ EngineError::Unsupported(_)) => last = e,
                other => return other,
            }
        }
        Err(last)
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
        let d = self
            .decoder_by_id(id)
            .filter(|d| d.can_decode(format))
            .ok_or_else(|| {
                EngineError::Unsupported(format!("{format} を読むデコーダ {id} がありません"))
            })?;
        decode_one(d, src, format, opts, limits, mon)
    }

    /// ファイルを読む。
    pub fn decode_path(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<(Value, DecodeStamp), EngineError> {
        let mut src = BufReader::new(std::fs::File::open(path)?);
        self.decode(&mut src, format, opts, limits, mon)
    }

    /// 書いて、バイト列と記録を返す。設定は正規の JSON に直してから、エンコーダの記述で厳しく確かめる
    /// （知らないキー・型の違い・範囲の外はエラー。黙って既定に落とさない）。
    pub fn encode_bytes(
        &self,
        value: &Value,
        format: &'static str,
        opts: &serde_json::Value,
        mon: &dyn Monitor,
    ) -> Result<(Vec<u8>, EncodedStamp), EngineError> {
        let e = self
            .encoder(format, &value.shape())
            .ok_or_else(|| EngineError::Unsupported(format!("{format} には書けません")))?;
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
                format,
                options: opts,
                bytes,
                file_digest,
            },
        ))
    }
}

/// 1 つのデコーダで読む：形を上限で確かめてから中身を読み、ヘッダと中身が合うか確かめる。
fn decode_one(
    d: &Arc<dyn Decoder>,
    src: &mut dyn ReadSeek,
    format: &str,
    opts: &DecodeOptions,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<(Value, DecodeStamp), EngineError> {
    src.rewind()?;
    let shape = contain(d.id(), Stage::Decode, || d.shape(src, format, opts, limits))?;
    limits.check(&shape).map_err(EngineError::Limit)?;
    src.rewind()?;
    let value = contain(d.id(), Stage::Decode, || {
        d.decode(src, format, opts, limits, mon)
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
