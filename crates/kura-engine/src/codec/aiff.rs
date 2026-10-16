//! AIFF・AIFF-C（自作。Pinned）。
//!
//! - 読む：AIFF（符号付きの整数。幅は入れ物の 8・16・24・32 ビット、左詰め）と AIFF-C の `NONE`・`twos`・`in24`・
//!   `in32`・`raw `（符号なしの 8 ビット）・`fl32`/`FL32`・`fl64`/`FL64`・`sowt`（リトルエンディアン。16 ビットと、
//!   バイトの順によらない 8 ビット）。ほかの圧縮（ulaw・alaw・ima4 など）と 17 ビット以上の sowt は `Unsupported`
//!   （次の段へ回す）。
//!   チャンクの順は問わず、COMM・SSND・MARK・INST・テキストは最初のものを使う。フレーム数は SSND の長さ（ファイルの
//!   終わりで詰める）で決め、COMM の numSampleFrames は使わない（CoreAudio・ffmpeg・libsndfile と同じ。ADR-148）。
//!   サンプルレート（80 ビットの拡張精度）は整数だけで、最も近い整数（偶数への丸め）に読む。
//!   INST の sustainLoop（ForwardLooping か ForwardBackwardLooping で、始め < 終わり ≤ 長さ）をループ点にし、その 2 つの
//!   マーカーはマーカーの一覧から除く。MARK の位置は「フレームのあいだ」で、終わりのマーカーの位置はそのまま「含まない」
//!   終わりになる。NAME・AUTH・`(c) `・ANNO（最初のもの）を title・artist・copyright・comment にする（UTF-8 として読む）。
//!   メタデータの数と大きさは WAV と同じ予算（マーカーの上限・`max_meta_bytes`）で抑える。
//! - 書く：整数（8・16・24・32 ビット）は AIFF、浮動小数（32 ビット）は AIFF-C の fl32。丸めは `pcm::quantize`（偶数への
//!   丸め。ディザはしない）。ループ点は MARK の 2 つのマーカー（"beg loop"・"end loop"）と INST の sustainLoop
//!   （ForwardLooping）、マーカーは MARK、タグは NAME・AUTH・`(c) `・ANNO（UTF-8）。チャンクは COMM・MARK・INST・テキスト・
//!   SSND の順。int16・int24 を読んで同じビット数で書くと、ビット単位で元に戻る（E17）。

use std::io::{SeekFrom, Write};
use std::sync::Arc;

use serde_json::json;

use crate::codec::pcm::{self, quantize};
use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, caps_of, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Shape};
use crate::value::{Audio, AudioMeta, AudioSource, LoopRange, Marker, Value};

/// AIFF の読み書き。
pub struct AiffCodec;

/// 読み取りの約束の版。
const DEC_ID: &str = "aiff-kura-dec/1";
/// 書き出しの約束の版。
const ENC_ID: &str = "aiff-kura-enc/1";
/// 名前・タグとして読む 1 つの文字列の長さの上限（バイト）。
const TEXT_MAX: usize = 4_096;
/// メタデータの 1 項目ごとに予算から引く固定の重さ（バイト。WAV と同じ）。
const ITEM_COST: usize = 32;
/// AIFF-C の FVER の値。
const AIFC_VERSION: u32 = 0xA280_5140;

fn err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: message.into(),
    }
}

/// 標本の並び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// 符号付きの整数（ビッグエンディアン。幅 `bytes`）。
    Int {
        bytes: u16,
    },
    /// 符号付きの 16 ビット（リトルエンディアン。`sowt`）。
    IntLe16,
    /// 符号なしの 8 ビット（`raw `）。
    U8,
    F32,
    F64,
}

impl Layout {
    fn bytes(self) -> u16 {
        match self {
            Layout::Int { bytes } => bytes,
            Layout::IntLe16 => 2,
            Layout::U8 => 1,
            Layout::F32 => 4,
            Layout::F64 => 8,
        }
    }

    fn source(self, rate: u32) -> AudioSource {
        let (bits, float) = match self {
            Layout::Int { bytes } => (bytes * 8, false),
            Layout::IntLe16 => (16, false),
            Layout::U8 => (8, false),
            Layout::F32 => (32, true),
            Layout::F64 => (64, true),
        };
        AudioSource { bits, float, rate }
    }

    /// 1 標本を f32 にする。
    fn to_f32(self, s: &[u8]) -> f32 {
        match self {
            Layout::Int { bytes: 1 } => pcm::int_to_f32(i32::from(s[0] as i8), 8),
            Layout::Int { bytes: 2 } => {
                pcm::int_to_f32(i32::from(i16::from_be_bytes([s[0], s[1]])), 16)
            }
            Layout::Int { bytes: 3 } => {
                pcm::int_to_f32(i32::from_be_bytes([s[0], s[1], s[2], 0]) >> 8, 24)
            }
            Layout::Int { .. } => pcm::int_to_f32(i32::from_be_bytes([s[0], s[1], s[2], s[3]]), 32),
            Layout::IntLe16 => pcm::int_to_f32(i32::from(i16::from_le_bytes([s[0], s[1]])), 16),
            Layout::U8 => pcm::int_to_f32(i32::from(s[0]) - 128, 8),
            Layout::F32 => f32::from_be_bytes([s[0], s[1], s[2], s[3]]),
            Layout::F64 => {
                f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
            }
        }
    }
}

/// 80 ビットの拡張精度（符号・15 ビットの指数・64 ビットの仮数）を、最も近い整数（偶数への丸め）に読む。
/// 負・0・無限大・NaN・u32 に収まらないものは None。
pub fn read_ext80(b: &[u8; 10]) -> Option<u32> {
    let se = u16::from_be_bytes([b[0], b[1]]);
    let m = u64::from_be_bytes([b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]]);
    if se & 0x8000 != 0 || se == 0x7FFF || m == 0 {
        return None;
    }
    // 値 = m × 2^(e − 16383 − 63)
    let shift = i32::from(se) - 16_383 - 63;
    let v: u128 = if shift >= 0 {
        if shift > 64 {
            return None;
        }
        u128::from(m) << shift
    } else {
        let r = (-shift) as u32;
        if r > 127 {
            return None;
        }
        let q = u128::from(m) >> r;
        let rem = u128::from(m) & ((1u128 << r) - 1);
        let half = 1u128 << (r - 1);
        if rem > half || (rem == half && q & 1 == 1) {
            q + 1
        } else {
            q
        }
    };
    u32::try_from(v).ok().filter(|v| *v > 0)
}

/// 正の整数を 80 ビットの拡張精度にする（正確に）。
pub fn write_ext80(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let p = 31 - rate.leading_zeros();
    let e = 16_383 + p as u16;
    let m = u64::from(rate) << (63 - p);
    out[..2].copy_from_slice(&e.to_be_bytes());
    out[2..].copy_from_slice(&m.to_be_bytes());
    out
}

/// 文字列を読む（NUL で切り、UTF-8 として読めない所は置き換え、前後の空白を取る。WAV と同じ）。
fn text_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// メタデータの読み取りの予算。
struct Budget {
    markers: u64,
    bytes: u64,
}

impl Budget {
    fn take(&mut self, len: usize) -> bool {
        let len = len as u64;
        if len > self.bytes {
            self.bytes = 0;
            false
        } else {
            self.bytes -= len;
            true
        }
    }
}

/// 読んだヘッダ。
struct Header {
    channels: u16,
    rate: u32,
    layout: Layout,
    data_offset: u64,
    data_len: u64,
    meta: AudioMeta,
}

fn read_exact_or(src: &mut dyn ReadSeek, buf: &mut [u8], what: &str) -> Result<(), EngineError> {
    src.read_exact(buf)
        .map_err(|_| err(format!("{what}が途中で切れています")))
}

/// COMM の中身。
struct Comm {
    channels: u16,
    rate: u32,
    layout: Layout,
}

fn parse_comm(b: &[u8], aifc: bool) -> Result<Comm, EngineError> {
    if b.len() < 18 {
        return Err(err("COMM が短すぎます"));
    }
    let channels = i16::from_be_bytes([b[0], b[1]]);
    let bits = i16::from_be_bytes([b[6], b[7]]);
    let rate_bytes: [u8; 10] = b[8..18].try_into().map_err(|_| err("COMM が短すぎます"))?;
    if channels <= 0 {
        return Err(err("チャンネルの数が正しくありません"));
    }
    let rate = read_ext80(&rate_bytes).ok_or_else(|| err("サンプルレートが読めません"))?;
    // AIFF-C で 18 バイトしかないものは NONE とみなす
    let kind: [u8; 4] = if aifc && b.len() >= 22 {
        [b[18], b[19], b[20], b[21]]
    } else {
        *b"NONE"
    };
    let int_of = |bits: i16| -> Result<Layout, EngineError> {
        match bits {
            1..=8 => Ok(Layout::Int { bytes: 1 }),
            9..=16 => Ok(Layout::Int { bytes: 2 }),
            17..=24 => Ok(Layout::Int { bytes: 3 }),
            25..=32 => Ok(Layout::Int { bytes: 4 }),
            _ => Err(err(format!("{bits} ビットの AIFF は読めません"))),
        }
    };
    let layout = match &kind {
        b"NONE" | b"twos" | b"in24" | b"in32" => int_of(bits)?,
        b"raw " if bits == 8 => Layout::U8,
        b"fl32" | b"FL32" => Layout::F32,
        b"fl64" | b"FL64" => Layout::F64,
        b"sowt" if (9..=16).contains(&bits) => Layout::IntLe16,
        b"sowt" if (1..=8).contains(&bits) => Layout::Int { bytes: 1 },
        other => {
            return Err(EngineError::Unsupported(format!(
                "AIFF-C の圧縮 {} は読めません",
                String::from_utf8_lossy(other)
            )));
        }
    };
    Ok(Comm {
        channels: channels as u16,
        rate,
        layout,
    })
}

fn parse(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Header, EngineError> {
    let mut budget = Budget {
        markers: limits.max_markers,
        bytes: limits.max_meta_bytes,
    };
    let file_len = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    let mut head = [0u8; 12];
    read_exact_or(src, &mut head, "ヘッダ")?;
    if &head[0..4] != b"FORM" {
        return Err(err("AIFF ではありません"));
    }
    let aifc = match &head[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => return Err(err("AIFF ではありません")),
    };
    let mut comm: Option<Comm> = None;
    let mut ssnd: Option<(u64, u64)> = None;
    // id → 位置（INST の参照を解くため全部）、id → 名前（予算の中だけ）
    let mut mark_pos: std::collections::BTreeMap<i16, u32> = std::collections::BTreeMap::new();
    let mut mark_name: std::collections::BTreeMap<i16, String> = std::collections::BTreeMap::new();
    let mut seen_mark = false;
    let mut inst: Option<[u8; 20]> = None;
    let mut tags: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut pos = 12u64;
    let mut chunks = 0usize;
    while pos.saturating_add(8) <= file_len {
        chunks += 1;
        if chunks > crate::codec::MAX_CHUNKS {
            return Err(err("チャンクが多すぎます"));
        }
        crate::codec::seek_to(src, pos)?;
        let mut ch = [0u8; 8];
        read_exact_or(src, &mut ch, "チャンクの頭")?;
        let id: [u8; 4] = [ch[0], ch[1], ch[2], ch[3]];
        let size = u64::from(u32::from_be_bytes([ch[4], ch[5], ch[6], ch[7]]));
        let body = pos + 8;
        let available = file_len - body;
        let readable = size.min(available);
        match &id {
            b"COMM" if comm.is_none() => {
                // 18 バイト＋圧縮の種類（4）＋圧縮の名前（Pascal 文字列。最大 1 + 255 と詰め物 1）
                if readable > 18 + 4 + 257 {
                    return Err(err("COMM が大きすぎます"));
                }
                let mut b = vec![0u8; readable as usize];
                read_exact_or(src, &mut b, "COMM")?;
                comm = Some(parse_comm(&b, aifc)?);
            }
            b"SSND" if ssnd.is_none() => {
                if readable < 8 {
                    return Err(err("SSND が短すぎます"));
                }
                let mut b = [0u8; 8];
                read_exact_or(src, &mut b, "SSND")?;
                let offset = u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
                if offset > readable - 8 {
                    return Err(err("SSND の始まりが範囲の外です"));
                }
                ssnd = Some((body + 8 + offset, readable - 8 - offset));
            }
            b"MARK" if !seen_mark => {
                seen_mark = true;
                let mut n = [0u8; 2];
                if readable >= 2 {
                    read_exact_or(src, &mut n, "MARK")?;
                    let count = u16::from_be_bytes(n);
                    let mut left = readable - 2;
                    // 1 つずつ読む（途中で切れたら、それまでのもの）
                    for _ in 0..count {
                        if left < 7 {
                            break;
                        }
                        let mut m = [0u8; 7];
                        read_exact_or(src, &mut m, "MARK")?;
                        let mid = i16::from_be_bytes([m[0], m[1]]);
                        let mpos = u32::from_be_bytes([m[2], m[3], m[4], m[5]]);
                        let len = usize::from(m[6]);
                        let padded = (1 + len + ((1 + len) & 1)) as u64;
                        if left < 6 + padded {
                            break;
                        }
                        let mut name = vec![0u8; padded as usize - 1];
                        read_exact_or(src, &mut name, "MARK の名前")?;
                        left -= 6 + padded;
                        if mid <= 0 || mark_pos.contains_key(&mid) {
                            continue;
                        }
                        mark_pos.insert(mid, mpos);
                        let text = text_of(&name[..len.min(name.len())]);
                        if (mark_name.len() as u64) < budget.markers
                            && budget.take(text.len() + ITEM_COST)
                        {
                            mark_name.insert(mid, text);
                        }
                    }
                }
            }
            b"INST" if inst.is_none() => {
                if readable == 20 {
                    let mut b = [0u8; 20];
                    read_exact_or(src, &mut b, "INST")?;
                    inst = Some(b);
                }
            }
            b"NAME" | b"AUTH" | b"(c) " | b"ANNO" => {
                let key = match &id {
                    b"NAME" => "title",
                    b"AUTH" => "artist",
                    b"(c) " => "copyright",
                    _ => "comment",
                };
                if !tags.contains_key(key) && readable as usize <= TEXT_MAX {
                    let mut b = vec![0u8; readable as usize];
                    read_exact_or(src, &mut b, "テキスト")?;
                    let text = text_of(&b);
                    if budget.take(text.len() + ITEM_COST) {
                        tags.insert(key.to_string(), text);
                    }
                }
            }
            _ => {}
        }
        // 奇数長は 1 バイト詰める。必ず 8 バイト以上進む
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    let comm = comm.ok_or_else(|| err("COMM がありません"))?;
    let (data_offset, data_len) = ssnd.ok_or_else(|| err("SSND がありません"))?;
    if u64::from(comm.channels) > limits.max_channels {
        return Err(EngineError::Limit("チャンネルが多すぎます".into()));
    }
    if comm.rate > limits.max_sample_rate {
        return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
    }
    let block = u64::from(comm.channels) * u64::from(comm.layout.bytes());
    let frames = data_len / block;
    // ループ：INST の sustainLoop（id は 8・10 バイト目、playMode は 8 バイト目から）
    let mut loop_ids: Vec<i16> = Vec::new();
    let mut loop_range = None;
    if let Some(b) = inst {
        let mode = i16::from_be_bytes([b[8], b[9]]);
        let begin = i16::from_be_bytes([b[10], b[11]]);
        let end = i16::from_be_bytes([b[12], b[13]]);
        if matches!(mode, 1 | 2)
            && let (Some(s), Some(e)) = (mark_pos.get(&begin), mark_pos.get(&end))
            && s < e
            && u64::from(*e) <= frames
        {
            loop_range = Some(LoopRange {
                start: u64::from(*s),
                end: u64::from(*e),
            });
            loop_ids = vec![begin, end];
        }
    }
    let mut markers: Vec<(u32, i16, Marker)> = mark_name
        .into_iter()
        .filter(|(id, _)| !loop_ids.contains(id))
        .filter_map(|(id, name)| {
            let p = *mark_pos.get(&id)?;
            (u64::from(p) < frames).then(|| {
                (
                    p,
                    id,
                    Marker {
                        pos: u64::from(p),
                        name,
                    },
                )
            })
        })
        .collect();
    // 位置の順（同じ位置は id の順）
    markers.sort_by_key(|(p, id, _)| (*p, *id));
    let meta = AudioMeta {
        loop_range,
        markers: markers.into_iter().map(|(_, _, m)| m).collect(),
        source: Some(comm.layout.source(comm.rate)),
        tags,
    };
    Ok(Header {
        channels: comm.channels,
        rate: comm.rate,
        layout: comm.layout,
        data_offset,
        data_len,
        meta,
    })
}

impl Decoder for AiffCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "aiff"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let h = parse(src, limits)?;
        let block = u64::from(h.channels) * u64::from(h.layout.bytes());
        Ok(Shape::Audio {
            rate: h.rate,
            channels: h.channels,
            frames: Dim::Exact(h.data_len / block),
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
        let h = parse(src, limits)?;
        let sample = usize::from(h.layout.bytes());
        let block = usize::from(h.channels) * sample;
        let frames = h.data_len / block as u64;
        limits
            .check(&Shape::Audio {
                rate: h.rate,
                channels: h.channels,
                frames: Dim::Exact(frames),
            })
            .map_err(EngineError::Limit)?;
        let frames =
            usize::try_from(frames).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        let ch = usize::from(h.channels);
        let mut planes: Vec<Vec<f32>> = (0..ch)
            .map(|_| try_vec(frames, 0f32))
            .collect::<Result<_, _>>()?;
        src.seek(SeekFrom::Start(h.data_offset))?;
        let chunk_frames = 16_384usize;
        let mut buf = try_vec(chunk_frames * block, 0u8)?;
        let mut done = 0usize;
        while done < frames {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = (frames - done).min(chunk_frames);
            let bytes = &mut buf[..n * block];
            read_exact_or(src, bytes, "SSND")?;
            for f in 0..n {
                for (c, plane) in planes.iter_mut().enumerate() {
                    let at = f * block + c * sample;
                    plane[done + f] = h.layout.to_f32(&bytes[at..at + sample]);
                }
            }
            done += n;
        }
        let audio = Audio::new(h.rate, planes.into_iter().map(Arc::new).collect(), h.meta)?;
        Ok(Value::Audio(Arc::new(audio)))
    }
}

/// pstring（長さ 1 バイト＋本文、全体を偶数に詰める）。255 バイトを超える名前は文字の境目で切る。
fn pstring(s: &str) -> Vec<u8> {
    let mut end = s.len().min(255);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = vec![end as u8];
    out.extend_from_slice(&s.as_bytes()[..end]);
    if out.len() & 1 == 1 {
        out.push(0);
    }
    out
}

impl Encoder for AiffCodec {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "aiff" && matches!(shape, Shape::Audio { .. })
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "bits",
                    ParamKind::Int {
                        min: 8,
                        max: 32,
                        step: 8,
                    },
                    Unit::None,
                    json!(16),
                ),
                spec("float", ParamKind::Bool, Unit::None, json!(false)),
            ],
        }
    }

    fn caps(&self, format: &str, _opts: &serde_json::Value) -> MetaCaps {
        caps_of(format)
    }

    fn encode(
        &self,
        value: &Value,
        _format: &str,
        opts: &serde_json::Value,
        out: &mut dyn Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let Value::Audio(a) = value else {
            return Err(EngineError::Unsupported(
                "AIFF には音声だけを書けます".into(),
            ));
        };
        let (bits, float) = pcm::target(opts, a.meta.source, &[8, 16, 24, 32], true, "AIFF")?;
        let channels = i16::try_from(a.channels())
            .map_err(|_| EngineError::Unsupported("チャンネルが多すぎます".into()))?;
        let frames = a.frames() as u64;
        let frames32 = u32::try_from(frames)
            .map_err(|_| EngineError::Unsupported("長すぎて AIFF に書けません".into()))?;
        let sample = usize::from(bits / 8);
        let block = a.channels() * sample;
        let data_len = frames * block as u64;

        let mut chunks: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        if float {
            chunks.push((*b"FVER", AIFC_VERSION.to_be_bytes().to_vec()));
        }
        let mut comm = Vec::new();
        comm.extend_from_slice(&channels.to_be_bytes());
        comm.extend_from_slice(&frames32.to_be_bytes());
        comm.extend_from_slice(&(bits as i16).to_be_bytes());
        comm.extend_from_slice(&write_ext80(a.rate()));
        if float {
            comm.extend_from_slice(b"fl32");
            comm.extend_from_slice(&pstring("32-bit floating point"));
        }
        chunks.push((*b"COMM", comm));
        // マーカー：ふつうのマーカー（id 1..）とループの 2 つ
        let markers: Vec<&Marker> = a.meta.markers.iter().filter(|m| m.pos < frames).collect();
        let lp = a
            .meta
            .loop_range
            .filter(|l| l.start < l.end && l.end <= frames);
        let count = markers.len() + if lp.is_some() { 2 } else { 0 };
        if count > 0 {
            let count16 = u16::try_from(count)
                .ok()
                .filter(|c| *c < i16::MAX as u16)
                .ok_or_else(|| EngineError::Unsupported("マーカーが多すぎます".into()))?;
            let mut mark = count16.to_be_bytes().to_vec();
            let mut put = |id: i16, pos: u64, name: &str| {
                mark.extend_from_slice(&id.to_be_bytes());
                mark.extend_from_slice(&(pos as u32).to_be_bytes());
                mark.extend_from_slice(&pstring(name));
            };
            for (i, m) in markers.iter().enumerate() {
                put(i as i16 + 1, m.pos, &m.name);
            }
            let first_loop = markers.len() as i16 + 1;
            if let Some(l) = lp {
                put(first_loop, l.start, "beg loop");
                put(first_loop + 1, l.end, "end loop");
            }
            chunks.push((*b"MARK", mark));
            if lp.is_some() {
                let mut inst = vec![60u8, 0, 0, 127, 1, 127];
                inst.extend_from_slice(&0i16.to_be_bytes());
                for v in [1i16, first_loop, first_loop + 1, 0, 0, 0] {
                    inst.extend_from_slice(&v.to_be_bytes());
                }
                chunks.push((*b"INST", inst));
            }
        }
        for (key, id) in [
            ("title", *b"NAME"),
            ("artist", *b"AUTH"),
            ("copyright", *b"(c) "),
            ("comment", *b"ANNO"),
        ] {
            // 読み取りと同じ形（4,096 バイトまで・NUL まで・前後の空白なし）にして書く（書いて読み直すと同じ値）
            if let Some(t) = a.meta.tags.get(key) {
                let t = text_of(crate::codec::clip_text(t, TEXT_MAX).as_bytes());
                chunks.push((id, t.into_bytes()));
            }
        }
        let padded = |n: u64| 8 + n + (n & 1);
        let form_len = 4
            + chunks
                .iter()
                .map(|(_, b)| padded(b.len() as u64))
                .fold(0u64, u64::saturating_add)
            + padded(8 + data_len);
        let form_len = u32::try_from(form_len)
            .map_err(|_| EngineError::Unsupported("4GB を超える AIFF は書けません".into()))?;
        let mut written = 0u64;
        let mut put = |out: &mut dyn Write, bytes: &[u8]| -> Result<(), EngineError> {
            out.write_all(bytes)?;
            written += bytes.len() as u64;
            Ok(())
        };
        put(out, b"FORM")?;
        put(out, &form_len.to_be_bytes())?;
        put(out, if float { b"AIFC" } else { b"AIFF" })?;
        for (id, body) in &chunks {
            put(out, id)?;
            put(out, &(body.len() as u32).to_be_bytes())?;
            put(out, body)?;
            if body.len() & 1 == 1 {
                put(out, &[0])?;
            }
        }
        put(out, b"SSND")?;
        put(out, &((8 + data_len) as u32).to_be_bytes())?;
        put(out, &[0u8; 8])?;
        let chunk_frames = 16_384usize;
        let mut buf = try_vec(chunk_frames * block, 0u8)?;
        let total = a.frames();
        let mut done = 0usize;
        while done < total {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = (total - done).min(chunk_frames);
            for f in 0..n {
                for (c, plane) in a.planes().iter().enumerate() {
                    let s = plane[done + f];
                    let at = f * block + c * sample;
                    let dst = &mut buf[at..at + sample];
                    if float {
                        dst.copy_from_slice(&s.to_be_bytes());
                    } else {
                        let v = quantize(s, bits);
                        dst.copy_from_slice(&v.to_be_bytes()[8 - sample..]);
                    }
                }
            }
            put(out, &buf[..n * block])?;
            done += n;
        }
        if data_len & 1 == 1 {
            put(out, &[0])?;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::NoMonitor;
    use std::io::Cursor;

    fn decode(bytes: &[u8]) -> Result<Value, EngineError> {
        CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes.to_vec()),
                "aiff",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .map(|(v, _)| v)
    }

    fn encode(v: &Value, opts: serde_json::Value) -> Vec<u8> {
        CodecSet::pinned()
            .encode_bytes(v, "aiff", &opts, &NoMonitor)
            .unwrap()
            .0
    }

    #[test]
    fn ext80_round_trips_exactly() {
        for rate in [
            1u32,
            8_000,
            11_025,
            22_050,
            44_100,
            48_000,
            96_000,
            192_000,
            768_000,
            u32::MAX,
        ] {
            assert_eq!(read_ext80(&write_ext80(rate)), Some(rate));
        }
        assert_eq!(
            write_ext80(44_100),
            [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]
        );
        // 22254.545… は最も近い 22255
        let odd = [0x40, 0x0D, 0xAD, 0xDD, 0x17, 0x45, 0xD1, 0x74, 0x5D, 0x17];
        assert_eq!(read_ext80(&odd), Some(22_255));
        assert_eq!(read_ext80(&[0; 10]), None);
        assert_eq!(
            read_ext80(&[0xC0, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]),
            None
        );
    }

    #[test]
    fn round_trips_with_metadata() {
        let input = crate::testgen::InputGen::AudioNoise {
            rate: 44_100,
            channels: 2,
            frames: 5_001,
            seed: 4,
        };
        let mut a = match input.generate().unwrap() {
            Value::Audio(a) => (*a).clone(),
            _ => panic!(),
        };
        a.meta.loop_range = Some(LoopRange {
            start: 100,
            end: 5_001,
        });
        a.meta.markers = vec![
            Marker {
                pos: 0,
                name: "start".into(),
            },
            Marker {
                pos: 2_000,
                name: "爆発".into(),
            },
        ];
        a.meta.tags.insert("title".into(), "題名".into());
        a.meta.tags.insert("artist".into(), "作者".into());
        let v = Value::Audio(Arc::new(a));
        for (opts, bits) in [
            (json!({ "bits": 24 }), 24u16),
            (json!({ "float": true }), 32),
        ] {
            let bytes = encode(&v, opts.clone());
            let Value::Audio(back) = decode(&bytes).unwrap() else {
                panic!()
            };
            let Value::Audio(src) = &v else { panic!() };
            assert_eq!(back.planes(), src.planes(), "{opts}");
            assert_eq!(back.meta.loop_range, src.meta.loop_range);
            assert_eq!(back.meta.markers, src.meta.markers);
            assert_eq!(back.meta.tags, src.meta.tags);
            assert_eq!(back.meta.source.map(|s| s.bits), Some(bits));
            // 書き直しても同じバイト列
            assert_eq!(encode(&Value::Audio(back), opts), bytes);
        }
    }

    /// 小さなチャンクを大量に並べたファイル（検証で見つけた、走査で長く止まる形）は数で断る。少しなら読める。
    #[test]
    fn too_many_chunks_are_refused() {
        let v = crate::testgen::InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 10,
            seed: 3,
        }
        .generate()
        .unwrap();
        let base = encode(&v, serde_json::json!({ "bits": 16 }));
        let with_junk = |n: usize| {
            let mut b = base.clone();
            for _ in 0..n {
                b.extend_from_slice(b"JUNK\0\0\0\0");
            }
            let form = u32::try_from(b.len() - 8).unwrap();
            b[4..8].copy_from_slice(&form.to_be_bytes());
            b
        };
        assert_eq!(decode(&with_junk(1_000)).unwrap(), decode(&base).unwrap());
        assert!(matches!(
            decode(&with_junk(crate::codec::MAX_CHUNKS)),
            Err(EngineError::Decode { .. })
        ));
    }

    /// 壊れたファイルは落ちずにエラーか読める値になる。
    #[test]
    fn broken_files_are_errors() {
        use rand_chacha::rand_core::Rng;
        let v = crate::testgen::InputGen::Sine {
            rate: 22_050,
            channels: 1,
            frames: 600,
            freq: 440,
            loop_range: Some(LoopRange { start: 5, end: 500 }),
        }
        .generate()
        .unwrap();
        let good = encode(&v, json!({ "bits": 16 }));
        let mut r = crate::determinism::rng(11);
        for _ in 0..2_000 {
            let mut b = good.clone();
            for _ in 0..1 + r.next_u32() % 6 {
                let i = r.next_u32() as usize % b.len();
                b[i] = r.next_u32() as u8;
            }
            if r.next_u32().is_multiple_of(4) {
                b.truncate(r.next_u32() as usize % b.len());
            }
            let _ = decode(&b);
        }
        assert!(decode(b"FORM").is_err());
        assert!(decode(b"RIFF\0\0\0\0WAVE").is_err());
    }
}
