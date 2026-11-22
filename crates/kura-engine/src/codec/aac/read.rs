//! AAC-LC のデコーダの口（`m4a-aac-kura-dec/1`・`aac-adts-kura-dec/1`）。入れ物は m4a（`codec::mp4_read`）と ADTS
//! （[`super::adts`]）。
//!
//! 形・切り取り・長さの決め方は前の symphonia の読み手（`lossy.rs` の AAC。`m4a-aac-symphonia-dec/2`・
//! `aac-adts-symphonia-dec/2`）と同じ：
//! - 形は上限（`AtMost`）。m4a は priming を自前で切る：iTunSMPB（Apple）があればそれ、なければ MP4 の elst（ffmpeg）の
//!   最初の空でない編集（`symph::Media::mp4_edit`）。どちらもなければ切らない。ADTS は切らない。
//! - HE-AAC：AudioSpecificConfig の明示の印は開くときに、帯域の中の印（SBR の fill 要素）は形を決めるときに最初の
//!   [`SBR_PROBE_PACKETS`] パケットを読んで、読むときは見つけた所で `Unsupported`（次の段の AudioToolbox・ffmpeg へ）。
//! - 標本は f32（NaN は `symph::canonical_nan` でそろえる）。元の形式は 16 ビットの整数とみなす。タグは MP4 の 7 つのキー。

use std::io::BufReader;
use std::path::Path;

use super::adts::{self, Adts};
use super::config::{self, Config};
use super::decoder::AacDecoder;
use super::{ADTS_ID, M4A_ID};
use crate::codec::held::{self, Held};
use crate::codec::mp4_read::{self, Mp4Track, Want};
use crate::codec::stream::{self, DecodeInfo, DecodeOut};
use crate::codec::symph::{self, Media};
use crate::codec::{Decoder, ReadSeek, Tier};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::shape::{Dim, Shape};
use crate::value::{AudioSource, Value};

/// 形を決めるときに、SBR の拡張を探す AAC のパケットの数（先頭から。前の読み手と同じ）。
const SBR_PROBE_PACKETS: usize = 16;

/// m4a のサンプルの大きさの上限の、チャンネルごとの分（AAC のフレームは 1 チャンネル 6,144 ビット = 768 バイトまで。
/// 余裕をみて 8 KiB）。超えるものは作業領域の見積もりの外なので `Limit`。
const PACKET_PER_CHANNEL: u64 = 8 << 10;

/// どの入れ物か。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    M4a,
    Adts,
}

/// AAC-LC の読み取り（m4a か ADTS）。
pub struct AacCodec {
    kind: Kind,
}

impl AacCodec {
    /// MP4 の中の AAC-LC（`m4a-aac-kura-dec/1`）。
    pub fn m4a() -> Self {
        Self { kind: Kind::M4a }
    }

    /// ADTS の AAC-LC（`aac-adts-kura-dec/1`）。
    pub fn adts() -> Self {
        Self { kind: Kind::Adts }
    }
}

/// 誤りの識別子をこの読み手のものにする（中の部品は m4a の識別子で作る）。
fn own(e: EngineError, id: &str) -> EngineError {
    match e {
        EngineError::Decode { message, .. } => EngineError::Decode {
            codec: id.to_string(),
            message,
        },
        EngineError::Unsupported(m) => EngineError::Unsupported(format!("{id}: {m}")),
        e => e,
    }
}

/// パケットの列。
enum Packets<'a> {
    Mp4(Box<Mp4Track<'a>>, u64),
    Adts(Adts<'a>),
}

impl Packets<'_> {
    /// 次のパケット（バイト, 長さ）。
    fn next(&mut self) -> Result<Option<(&[u8], u64)>, EngineError> {
        match self {
            Packets::Mp4(t, max) => {
                let Some(s) = t.next_sample()? else {
                    return Ok(None);
                };
                if u64::from(s.size) > *max {
                    return Err(EngineError::Limit(format!(
                        "AAC のパケットが大きすぎます（{} バイト）",
                        s.size
                    )));
                }
                Ok(Some((t.read_sample(&s)?, u64::from(s.dur))))
            }
            Packets::Adts(a) => Ok(a.next()?.map(|b| (b, 1024))),
        }
    }
}

/// 開いた形とタグ。
struct Opened<'a> {
    packets: Packets<'a>,
    cfg: Config,
    tags: Vec<(String, String)>,
}

impl AacCodec {
    fn open<'a>(
        &self,
        media: &'a mut Media<'_>,
        limits: &Limits,
        mon: &'a dyn Monitor,
    ) -> Result<Opened<'a>, EngineError> {
        let (packets, cfg, tags) = match self.kind {
            Kind::M4a => {
                let mut t = mp4_read::open(media, mon, M4A_ID, Want::Aac)?;
                if t.cookie.is_empty() {
                    // esds に DecoderSpecificInfo がない（前の symphonia は入れ物のレートとチャンネルで LC とみなした）。
                    // 形を決めきれないので次の段へ
                    return Err(super::unsupported(
                        "AudioSpecificConfig のない m4a の AAC は次の段で読みます",
                    ));
                }
                let cfg = config::parse_asc(&t.cookie)?;
                let tags = std::mem::take(&mut t.tags);
                let max = PACKET_PER_CHANNEL * cfg.channels as u64;
                (Packets::Mp4(Box::new(t), max), cfg, tags)
            }
            Kind::Adts => {
                let a = adts::open(media, mon)?;
                let cfg = a.cfg;
                (Packets::Adts(a), cfg, Vec::new())
            }
        };
        if cfg.channels as u64 > limits.max_channels {
            return Err(EngineError::Limit("チャンネルが多すぎます".into()));
        }
        if cfg.rate > limits.max_sample_rate {
            return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
        }
        Ok(Opened { packets, cfg, tags })
    }

    /// 開いて HE-AAC の帯域の中の印を探し、長さと切り取りを決める。
    fn probe(
        &self,
        media: &mut Media<'_>,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Probe, EngineError> {
        // 帯域の中の SBR の印（最初の数パケット。読めなくなったら、見たところまでで答える）
        {
            let mut o = self.open(media, limits, mon)?;
            let mut dec = AacDecoder::new(o.cfg)?;
            for _ in 0..SBR_PROBE_PACKETS {
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
                let Ok(Some((p, _))) = o.packets.next() else {
                    break;
                };
                match dec.decode(p) {
                    Ok(_) => {}
                    Err(e @ EngineError::Unsupported(_)) => return Err(e),
                    Err(_) => break,
                }
            }
        }
        let mut o = self.open(media, limits, mon)?;
        let cfg = o.cfg;
        let shape = |n: u64| Shape::Audio {
            rate: cfg.rate,
            channels: cfg.channels as u16,
            frames: Dim::Exact(n),
        };
        let mut counted = 0u64;
        while let Some((_, dur)) = o.packets.next()? {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            counted = counted.saturating_add(dur);
            limits
                .check(&shape(counted))
                .map_err(|m| EngineError::Limit(format!("デコードする量が大きすぎます（{m}）")))?;
        }
        let tags = std::mem::take(&mut o.tags);
        drop(o);
        let (frames, trim) = match self.kind {
            Kind::M4a => {
                let smpb = tags
                    .iter()
                    .find(|(k, _)| k.ends_with("iTunSMPB"))
                    .and_then(|(_, v)| symph::parse_itunsmpb(v));
                let trim = match smpb {
                    Some(t) => Some(t),
                    None => media.mp4_edit(cfg.rate)?,
                };
                // 切り取りの長さは中身より長いと言っても信じない（出うる長さで抑える）
                let bound = trim.map_or(counted.saturating_add(4_096), |(_, len)| {
                    len.min(counted.saturating_add(4_096))
                });
                (Dim::AtMost(bound), trim)
            }
            // ADTS は 1 パケットあたり 1,024 フレーム。パケットの長さの和を上限にする
            Kind::Adts => (Dim::AtMost(counted.saturating_add(4_096)), None),
        };
        Ok(Probe {
            cfg,
            frames,
            trim,
            counted,
            tags,
        })
    }

    /// 読んで標本を `out` に渡す（`decode`・`decode_stream` が通る）。
    fn decode_media(
        &self,
        media: &mut Media<'_>,
        limits: &Limits,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let mut p = self.probe(media, limits, mon)?;
        limits.check(&shape_of(&p)).map_err(EngineError::Limit)?;
        // 上限は形の上限。切るときは、切る前の長さ（パケットの長さの和）まで読んでから切る
        let max = match (p.frames, p.trim) {
            (Dim::Exact(n), _) => n,
            (Dim::AtMost(_), Some(_)) => p.counted.saturating_add(8_192),
            (Dim::AtMost(n), None) => n,
        };
        let expect = match (p.frames, p.trim) {
            (Dim::Exact(n), _) => n,
            (Dim::AtMost(_), Some((start, len))) => len.min(p.counted.saturating_sub(start)),
            (Dim::AtMost(_), None) => p.counted,
        };
        // タグは開き直した読み口のものを使う（2 つを同時に持たない。`held_bytes` は 1 つ分）
        p.tags = Vec::new();
        let mut o = self.open(media, limits, mon)?;
        out.begin(DecodeInfo {
            rate: p.cfg.rate,
            channels: p.cfg.channels,
            frames: p.frames,
            expect,
        })?;
        if let Some((start, len)) = p.trim {
            out.window(start, len);
        }
        let mut dec = AacDecoder::new(p.cfg)?;
        let mut planes: Vec<Vec<f32>> = (0..p.cfg.channels).map(|_| Vec::new()).collect();
        let mut total = 0u64;
        while let Some((packet, _)) = o.packets.next()? {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = dec.decode(packet)?;
            total = total.saturating_add(n as u64);
            if total > max {
                return Err(EngineError::Decode {
                    codec: self.id().to_string(),
                    message: "長さがヘッダより長い".into(),
                });
            }
            for (plane, src) in planes.iter_mut().zip(&dec.planes) {
                plane.clear();
                plane
                    .try_reserve(n)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                plane.extend(src[..n].iter().map(|s| symph::canonical_nan(*s)));
            }
            let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            out.push_frames(&views)?;
        }
        let got = out.frames();
        if !p.frames.admits(got) {
            return Err(EngineError::Decode {
                codec: self.id().to_string(),
                message: "長さがヘッダと合いません".into(),
            });
        }
        let mut meta = symph::meta_from_tags(&o.tags, got, false, limits);
        meta.source = Some(AudioSource {
            bits: 16,
            float: false,
            rate: p.cfg.rate,
        });
        out.end(meta)
    }
}

/// 開いて形を決めた結果。
struct Probe {
    cfg: Config,
    frames: Dim,
    /// 切り取り（始まり, 長さ）。
    trim: Option<(u64, u64)>,
    /// パケットの長さの和。
    counted: u64,
    tags: Vec<(String, String)>,
}

fn shape_of(p: &Probe) -> Shape {
    Shape::Audio {
        rate: p.cfg.rate,
        channels: p.cfg.channels as u16,
        frames: p.frames,
    }
}

impl Decoder for AacCodec {
    fn id(&self) -> &'static str {
        match self.kind {
            Kind::M4a => M4A_ID,
            Kind::Adts => ADTS_ID,
        }
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn reproducible(&self) -> bool {
        true
    }

    fn can_decode(&self, format: &str) -> bool {
        match self.kind {
            Kind::M4a => format == "m4a",
            Kind::Adts => format == "aac",
        }
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let data = symph::read_all(src, limits)?;
        let p = self
            .probe(&mut Media::Bytes(&data), limits, &NoMonitor)
            .map_err(|e| own(e, self.id()))?;
        Ok(shape_of(&p))
    }

    fn shape_file(
        &self,
        path: &Path,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.shape_file_monitored(path, format, opts, limits, &NoMonitor)
    }

    fn shape_file_monitored(
        &self,
        path: &Path,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Shape, EngineError> {
        let mut f = BufReader::new(std::fs::File::open(path)?);
        let p = self
            .probe(&mut Media::Reader(&mut f), limits, mon)
            .map_err(|e| own(e, self.id()))?;
        Ok(shape_of(&p))
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let data = symph::read_all(src, limits)?;
        stream::collect(mon, |out| {
            self.decode_media(&mut Media::Bytes(&data), limits, out, mon)
                .map_err(|e| own(e, self.id()))
        })
    }

    fn streams(&self, _format: &str) -> bool {
        true
    }

    /// 前の symphonia の見積もりのまま（読み口の 4 MiB と、8,192 フレーム × 8 バイト × チャンネル）。自作の読み手は
    /// チャンネルごとに 1,024 フレームの面・重ね合わせと、ICS 2 つ・フィルタバンクの作業列（どれも数十 KiB）を持つ。
    fn stream_work_bytes(&self, _format: &str, shape: &Shape, _len: u64) -> u64 {
        let Shape::Audio { channels, .. } = shape else {
            return 0;
        };
        (4 << 20) + 8_192 * u64::from(*channels) * 8
    }

    /// m4a は読み手が持つ表とタグ（`codec::held::mp4` の自作の読み手の数え方）、ADTS は読み手の緩衝だけ。
    fn held_bytes(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _limits: &Limits,
    ) -> Result<Held, EngineError> {
        Ok(match self.kind {
            Kind::M4a => held::mp4(src, held::Mp4Reader::Kura)?,
            Kind::Adts => Held {
                open: held::READER,
                decode: 0,
            },
        })
    }

    fn held_bytes_file(
        &self,
        path: &Path,
        format: &str,
        limits: &Limits,
    ) -> Result<Held, EngineError> {
        match self.kind {
            Kind::M4a => {
                let mut f = BufReader::new(std::fs::File::open(path)?);
                self.held_bytes(&mut f, format, limits)
            }
            Kind::Adts => Ok(Held {
                open: held::READER,
                decode: 0,
            }),
        }
    }

    fn decode_stream(
        &self,
        src: &mut (dyn ReadSeek + Send),
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        _shape: &Shape,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.decode_media(&mut Media::Reader(src), limits, out, mon)
            .map_err(|e| own(e, self.id()))
    }
}
