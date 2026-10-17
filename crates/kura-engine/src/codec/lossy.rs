//! symphonia で読む Ogg Vorbis・MP4（AAC-LC・ALAC）・ADTS の AAC（読みだけ。Pinned。docs/editing.md §9.2）。
//!
//! - **Vorbis・AAC は値を再現できない**（symphonia のデコーダの中で std の超越関数（OS の libm）が表を作る）ので
//!   `reproducible()` は偽（初めて編集するときにアンカーを作る。§9.4。ADR-148）。ALAC は整数だけなので真。
//!   m4a は中身で再現の可否が違うので、ALAC 用と AAC 用のデコーダを分け、扱わない中身は `Unsupported` で次へ回す。
//! - Vorbis：Ogg の読み口が付けるグラニュールの切り取り（gapless）をそのまま使う。デコーダを作る前に setup ヘッダの
//!   codebook が作る表の大きさを見積もり、`min(max_value_bytes, 256 MiB)` を超えれば `Unsupported`（小さなファイルで
//!   数百 MB を確保させる形を断る）。Vorbis コメント（タグ・LOOPSTART・LOOPLENGTH）を読む。Opus は次の段へ。
//! - AAC：symphonia は priming（エンコーダの遅延）を切らないので自前で切る：iTunSMPB（Apple）があればそれ、なければ
//!   MP4 の elst（ffmpeg）の最初の空でない編集。どちらもなければ切らない（ADTS は常に切らない）。形は上限（`AtMost`）。
//!   HE-AAC（SBR・PS）の明示の印があれば `Unsupported`（symphonia は半分のレートで読んでしまう）。
//! - ALAC：MP4 の中の ALAC。整数を `pcm::int_to_f32(v, 32)` で読み、長さはパケットの長さの和（形は `Exact`）。

use std::sync::Arc;

use symphonia_core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_VORBIS};

use crate::codec::symph::{self, Container, read_all};
use crate::codec::{Decoder, ReadSeek, Tier, pcm};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::shape::{Dim, Shape};
use crate::value::{Audio, AudioMeta, AudioSource, Value};

/// どの中身を読むか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    OggVorbis,
    Mp4Alac,
    Mp4Aac,
    AdtsAac,
}

/// symphonia で読む非可逆（と ALAC）のデコーダ。
pub struct SymphoniaDecoder {
    kind: Kind,
}

impl SymphoniaDecoder {
    /// Ogg Vorbis（`ogg-vorbis-symphonia-dec/2`）。
    pub fn ogg_vorbis() -> Self {
        Self {
            kind: Kind::OggVorbis,
        }
    }

    /// MP4 の中の ALAC（`m4a-alac-symphonia-dec/1`）。
    pub fn m4a_alac() -> Self {
        Self {
            kind: Kind::Mp4Alac,
        }
    }

    /// MP4 の中の AAC-LC（`m4a-aac-symphonia-dec/1`）。
    pub fn m4a_aac() -> Self {
        Self { kind: Kind::Mp4Aac }
    }

    /// ADTS の AAC-LC（`aac-adts-symphonia-dec/1`）。
    pub fn adts_aac() -> Self {
        Self {
            kind: Kind::AdtsAac,
        }
    }

    fn container(&self) -> Container {
        match self.kind {
            Kind::OggVorbis => Container::Ogg,
            Kind::Mp4Alac | Kind::Mp4Aac => Container::Mp4,
            Kind::AdtsAac => Container::Adts,
        }
    }

    fn err(&self, message: impl Into<String>) -> EngineError {
        EngineError::Decode {
            codec: self.id().to_string(),
            message: message.into(),
        }
    }
}

/// 開いて形を決めた結果。
struct Probe {
    rate: u32,
    channels: u16,
    frames: Dim,
    /// AAC の切り取り（始まり, 長さ）。
    trim: Option<(u64, u64)>,
    /// パケットの長さの和（切り取る前のデコードの上限に使う）。
    counted: u64,
    raw_tags: Vec<(String, String)>,
}

impl SymphoniaDecoder {
    /// 開いて、中身の種類・HE-AAC・Vorbis の表の大きさを確かめ、長さを決める。
    fn probe(&self, data: &[u8], limits: &Limits, mon: &dyn Monitor) -> Result<Probe, EngineError> {
        let id = self.id();
        let mut o = symph::open(id, data, self.container(), mon, limits)?;
        let want = match self.kind {
            Kind::OggVorbis => CODEC_ID_VORBIS,
            Kind::Mp4Alac => CODEC_ID_ALAC,
            Kind::Mp4Aac | Kind::AdtsAac => CODEC_ID_AAC,
        };
        if o.params.codec != want {
            return Err(EngineError::Unsupported(format!(
                "{id}: このデコーダの扱う中身ではありません"
            )));
        }
        if want == CODEC_ID_AAC && o.params.extra_data.as_deref().is_some_and(symph::is_he_aac) {
            return Err(EngineError::Unsupported(format!(
                "{id}: HE-AAC は次の段で読みます"
            )));
        }
        if want == CODEC_ID_VORBIS {
            let budget = limits.max_value_bytes.min(256 << 20);
            match o
                .params
                .extra_data
                .as_deref()
                .and_then(symph::vorbis_setup_bytes)
            {
                Some(b) if b <= budget => {}
                Some(_) => {
                    return Err(EngineError::Unsupported(format!(
                        "{id}: Vorbis の codebook が大きすぎます"
                    )));
                }
                None => return Err(self.err("Vorbis の setup ヘッダが読めません")),
            }
        }
        let raw_tags = o.raw_tags();
        let rate = o.rate;
        let channels = u16::try_from(o.channels).map_err(|_| self.err("チャンネルが多すぎます"))?;
        let counted = o.count_frames(id, mon, limits)?;
        let (frames, trim) = match self.kind {
            Kind::Mp4Aac => {
                let smpb = raw_tags
                    .iter()
                    .find(|(k, _)| k.ends_with("iTunSMPB"))
                    .and_then(|(_, v)| symph::parse_itunsmpb(v));
                let trim = smpb.or_else(|| symph::mp4_edit(data, rate));
                // 切り取りの長さは中身より長いと言っても信じない（出うる長さで抑える）
                let bound = trim.map_or(counted.saturating_add(4_096), |(_, len)| {
                    len.min(counted.saturating_add(4_096))
                });
                (Dim::AtMost(bound), trim)
            }
            // ADTS は 1 パケットあたり最大 1,024 フレーム。パケットの長さの和を上限にする
            Kind::AdtsAac => (Dim::AtMost(counted.saturating_add(4_096)), None),
            _ => (Dim::Exact(counted), None),
        };
        Ok(Probe {
            rate,
            channels,
            frames,
            trim,
            counted,
            raw_tags,
        })
    }
}

impl Decoder for SymphoniaDecoder {
    fn id(&self) -> &'static str {
        match self.kind {
            Kind::OggVorbis => "ogg-vorbis-symphonia-dec/2",
            Kind::Mp4Alac => "m4a-alac-symphonia-dec/1",
            Kind::Mp4Aac => "m4a-aac-symphonia-dec/1",
            Kind::AdtsAac => "aac-adts-symphonia-dec/1",
        }
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn reproducible(&self) -> bool {
        self.kind == Kind::Mp4Alac
    }

    fn can_decode(&self, format: &str) -> bool {
        match self.kind {
            Kind::OggVorbis => format == "ogg",
            Kind::Mp4Alac | Kind::Mp4Aac => format == "m4a",
            Kind::AdtsAac => format == "aac",
        }
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let data = read_all(src, limits)?;
        let p = self.probe(&data, limits, &NoMonitor)?;
        Ok(Shape::Audio {
            rate: p.rate,
            channels: p.channels,
            frames: p.frames,
        })
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let id = self.id();
        let data = read_all(src, limits)?;
        let p = self.probe(&data, limits, mon)?;
        limits
            .check(&Shape::Audio {
                rate: p.rate,
                channels: p.channels,
                frames: p.frames,
            })
            .map_err(EngineError::Limit)?;
        let mut o = symph::open(id, &data, self.container(), mon, limits)?;
        let opts = symph::decoder_options();
        let e = |e| symph::sym_err(id, e, mon);
        let mut dec: Box<dyn symphonia_core::codecs::audio::AudioDecoder> = match self.kind {
            Kind::OggVorbis => Box::new(
                symphonia_codec_vorbis::VorbisDecoder::try_new(&o.params, &opts).map_err(e)?,
            ),
            Kind::Mp4Alac => {
                Box::new(symphonia_codec_alac::AlacDecoder::try_new(&o.params, &opts).map_err(e)?)
            }
            Kind::Mp4Aac | Kind::AdtsAac => {
                Box::new(symphonia_codec_aac::AacDecoder::try_new(&o.params, &opts).map_err(e)?)
            }
        };
        // 上限は形の上限。AAC を切るときは、切る前の長さ（パケットの長さの和）まで読んでから切る（編集リストが
        // 中身の後ろを大きく切るファイルを断らないように。検証で見つけた）
        let max = match (p.frames, p.trim) {
            (Dim::Exact(n), _) => n,
            (Dim::AtMost(_), Some(_)) => p.counted.saturating_add(8_192),
            (Dim::AtMost(n), None) => n,
        };
        let mut planes = o.decode_all(id, dec.as_mut(), max, mon)?;
        if let Some((start, len)) = p.trim {
            for plane in &mut planes {
                let s = usize::try_from(start)
                    .unwrap_or(usize::MAX)
                    .min(plane.len());
                plane.drain(..s);
                plane.truncate(usize::try_from(len).unwrap_or(usize::MAX));
            }
        }
        let got = planes.first().map_or(0, Vec::len) as u64;
        if !p.frames.admits(got) {
            return Err(self.err("長さがヘッダと合いません"));
        }
        let frames = got;
        let vorbis = self.kind == Kind::OggVorbis;
        let mut meta: AudioMeta = symph::meta_from_tags(&p.raw_tags, frames, vorbis, limits);
        meta.source = Some(match self.kind {
            Kind::Mp4Alac => AudioSource {
                bits: pcm::container_bits(u16::from(
                    o.params
                        .extra_data
                        .as_deref()
                        .and_then(symph::alac_bits)
                        .unwrap_or(16),
                )),
                float: false,
                rate: p.rate,
            },
            _ => AudioSource {
                bits: 16,
                float: false,
                rate: p.rate,
            },
        });
        let audio = Audio::new(p.rate, planes.into_iter().map(Arc::new).collect(), meta)?;
        Ok(Value::Audio(Arc::new(audio)))
    }
}
