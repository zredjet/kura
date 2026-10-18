//! symphonia で読む Ogg Vorbis・MP4（AAC-LC・ALAC）・ADTS の AAC（読みだけ。Pinned。docs/editing.md §9.2）。
//!
//! - **どれも値を再現できる**（`reproducible()` は真。アンカーを作らない）。Vorbis・AAC は、symphonia の表（MDCT・FFT・
//!   窓・TNS・逆量子化・スケールファクター・floor 0・codebook）を作る超越関数を、写し（`third_party/symphonia-core`・
//!   `symphonia-codec-aac`・`symphonia-codec-vorbis`。各 KURA.md）で OS の libm から libm クレート（kura-engine と同じ
//!   =0.2.16）に差し替えたので、どの環境でも同じ値を読む（段 9。そのために識別子を上げた：Vorbis は /3、AAC は /2。
//!   公開前なので旧版は残さない）。ALAC は整数だけ。m4a は ALAC 用と AAC 用のデコーダを分け、扱わない中身は
//!   `Unsupported` で次へ回す。
//! - Vorbis：Ogg の読み口が付けるグラニュールの切り取り（gapless）をそのまま使う。デコーダを作る前に setup ヘッダの
//!   codebook が作る表の大きさを見積もり、`min(max_value_bytes, 256 MiB)` を超えれば `Unsupported`（小さなファイルで
//!   数百 MB を確保させる形を断る）。Vorbis コメント（タグ・LOOPSTART・LOOPLENGTH）を読む。Opus は次の段へ。
//! - AAC：symphonia は priming（エンコーダの遅延）を切らないので自前で切る：iTunSMPB（Apple）があればそれ、なければ
//!   MP4 の elst（ffmpeg）の最初の空でない編集。どちらもなければ切らない（ADTS は常に切らない）。形は上限（`AtMost`）。
//! - 3 チャンネル以上の AAC は `Unsupported`（symphonia の AAC は 1・2 チャンネルだけ。チャンネルの構成があっても、
//!   形の段階でデコーダを作ってみて断り、形も読みも後の段が答える）。
//! - HE-AAC（SBR・PS）は `Unsupported`（symphonia は SBR を持たず、LC の芯だけを半分のレートで読んでしまう。後の段の
//!   AudioToolbox・ffmpeg が読む）。見分け方は 2 つ：AudioSpecificConfig の明示の印（`symph::is_he_aac`）と、帯域の中の
//!   印（暗黙の SBR。ADTS はいつもこれ、m4a も ASC に印がなければこれ）。帯域の中の印は、形を決めるときに最初の
//!   [`SBR_PROBE_PACKETS`] パケットをデコードして、fill 要素に SBR の拡張（`EXT_SBR_DATA`・`EXT_SBR_DATA_CRC`）が
//!   あるかを写しのデコーダ（`AacDecoder::sbr_seen`。third_party/symphonia-codec-aac/KURA.md）に聞いて見る。読むときも
//!   全部のパケットを読んだあとに聞き、それより後で出てきたものも断る（読む値は返さない）。SBR のない AAC-LC の値は
//!   変わらない（見るだけで、デコードの経路は同じ）。
//! - ALAC：MP4 の中の ALAC。整数を `pcm::int_to_f32(v, 32)` で読み、長さはパケットの長さの和（形は `Exact`）。

use std::sync::Arc;

use symphonia_core::codecs::audio::AudioDecoder as _;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_VORBIS};

use crate::codec::symph::{self, Container, read_all};
use crate::codec::{Decoder, ReadSeek, Tier, pcm};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::shape::{Dim, Shape};
use crate::value::{Audio, AudioMeta, AudioSource, Value};

/// symphonia の写し（`third_party/`。ルートと `fuzz/` の `[patch.crates-io]`）が実際に組まれていることを、組み立てで
/// 確かめる。`KURA_PATCHED` は写しにだけある印なので、差し替えが外れて crates.io の上流のクレートが組まれると（`[patch]`
/// の行が抜けた、版を上げて写しの版と合わなくなった、など。cargo は警告しか出さない）、ここで組み立てが失敗する。
/// Vorbis・AAC の値の再現（`reproducible()`）と residue・`lookup1_values`・SBR の判定の修正は、どれも写しが前提
/// （各 KURA.md）。上流の表は、手元の機械では f32 に丸めると写しと同じ値になるので、ゴールデンでは気づけない（段 9 の検証）。
const _: [u32; 4] = [
    symphonia_core::KURA_PATCHED,
    symphonia_codec_aac::KURA_PATCHED,
    symphonia_codec_vorbis::KURA_PATCHED,
    symphonia_format_ogg::KURA_PATCHED,
];

/// 形を決めるときに、SBR の拡張を探す AAC のパケットの数（先頭から）。HE-AAC のエンコーダ（afconvert・FDK など）は、
/// SBR のデータを最初のフレームから毎フレーム書くので、先頭の数パケットで足りる。これより後で初めて出てくるものは、
/// 読むとき（全部のパケットを読んだあと）に断る。
const SBR_PROBE_PACKETS: usize = 16;

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
    /// Ogg Vorbis（`ogg-vorbis-symphonia-dec/3`）。
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

    /// MP4 の中の AAC-LC（`m4a-aac-symphonia-dec/2`）。
    pub fn m4a_aac() -> Self {
        Self { kind: Kind::Mp4Aac }
    }

    /// ADTS の AAC-LC（`aac-adts-symphonia-dec/2`）。
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

    /// HE-AAC を次の段へ回すときのエラー。
    fn he_aac(&self) -> EngineError {
        EngineError::Unsupported(format!("{}: HE-AAC は次の段で読みます", self.id()))
    }

    /// AAC の先頭の [`SBR_PROBE_PACKETS`] パケットをデコードして、帯域の中の SBR の拡張（暗黙の HE-AAC の印）が
    /// あるかを見る。デコーダを作れないもの（3 チャンネル以上は "aac too complex"）は、読むときと同じ理由（`Unsupported`
    /// なら次の段へ）で、形の段階で断る（形と読みを同じ段が答えるように）。途中で読めなくなったものは、見たところまでで
    /// 答える（読むときに今までどおりの理由で断る）。
    fn sbr_in_first_packets(
        &self,
        data: &[u8],
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<bool, EngineError> {
        let mut o = symph::open(self.id(), data, self.container(), mon, limits)?;
        let mut dec =
            symphonia_codec_aac::AacDecoder::try_new(&o.params, &symph::decoder_options())
                .map_err(|e| symph::sym_err(self.id(), e, mon))?;
        let mut seen = 0;
        while seen < SBR_PROBE_PACKETS {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let Ok(Some(pkt)) = o.reader.next_packet() else {
                break;
            };
            if pkt.track_id != o.track.id {
                continue;
            }
            seen += 1;
            if dec.decode(&pkt).is_err() {
                break;
            }
            if dec.sbr_seen() {
                return Ok(true);
            }
        }
        Ok(false)
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
        if want == CODEC_ID_AAC
            && (o.params.extra_data.as_deref().is_some_and(symph::is_he_aac)
                || self.sbr_in_first_packets(data, limits, mon)?)
        {
            return Err(self.he_aac());
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
            // 版の歴史（旧版は残さない。値を再現できなかった版の作り直しはアンカーから読む約束だったが、公開前で
            // アンカーを作った版はない）：
            // - Vorbis /1 → /2：3 チャンネル以上の residue 0/1 の不具合を写しで直した（段 7。ADR-149）
            // - Vorbis /2 → /3、AAC /1 → /2：表の超越関数を libm クレートにして値を再現できるようにした（段 9）
            Kind::OggVorbis => "ogg-vorbis-symphonia-dec/3",
            Kind::Mp4Alac => "m4a-alac-symphonia-dec/1",
            Kind::Mp4Aac => "m4a-aac-symphonia-dec/2",
            Kind::AdtsAac => "aac-adts-symphonia-dec/2",
        }
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    /// どの中身も、どの環境でも同じ値を読む（Vorbis・AAC は写しで超越関数を libm クレートにした。モジュールの説明）。
    fn reproducible(&self) -> bool {
        true
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
        // 上限は形の上限。AAC を切るときは、切る前の長さ（パケットの長さの和）まで読んでから切る（編集リストが
        // 中身の後ろを大きく切るファイルを断らないように。検証で見つけた）
        let max = match (p.frames, p.trim) {
            (Dim::Exact(n), _) => n,
            (Dim::AtMost(_), Some(_)) => p.counted.saturating_add(8_192),
            (Dim::AtMost(n), None) => n,
        };
        let mut planes = match self.kind {
            Kind::OggVorbis => {
                let mut dec =
                    symphonia_codec_vorbis::VorbisDecoder::try_new(&o.params, &opts).map_err(e)?;
                o.decode_all(id, &mut dec, max, mon)?
            }
            Kind::Mp4Alac => {
                let mut dec =
                    symphonia_codec_alac::AlacDecoder::try_new(&o.params, &opts).map_err(e)?;
                o.decode_all(id, &mut dec, max, mon)?
            }
            Kind::Mp4Aac | Kind::AdtsAac => {
                let mut dec =
                    symphonia_codec_aac::AacDecoder::try_new(&o.params, &opts).map_err(e)?;
                let planes = o.decode_all(id, &mut dec, max, mon)?;
                // 形を決めたときに見た先頭より後で SBR の拡張が出てきたものも、半分のレートの値を返さずに次の段へ
                if dec.sbr_seen() {
                    return Err(self.he_aac());
                }
                planes
            }
        };
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
