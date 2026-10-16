//! WAV（自作。Pinned）。
//!
//! - 読む：PCM の 8/16/24/32 ビットと浮動小数の 32/64 ビット（`WAVE_FORMAT_EXTENSIBLE` を含む）。
//!   `smpl` の最初のループをループ点（終わりは含まない形に直す）、`cue ` と `LIST adtl` の `labl` をマーカー、
//!   `LIST INFO` をタグにする。メタデータのチャンクは大きすぎれば読まない（展開や巨大な確保を避ける）。
//! - 書く：PCM の 8/16/24/32 ビットか浮動小数の 32 ビット。整数への丸めは偶数への丸めで、範囲で止める（ディザはしない）。
//!   int16・int24 を読んで同じビット数で書くと、ビット単位で元に戻る（E17）。

use std::io::{SeekFrom, Write};
use std::sync::Arc;

use serde_json::json;

use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, format_by_id, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Shape};
use crate::value::{Audio, AudioMeta, AudioSource, LoopRange, Marker, Value};

/// WAV の読み書き。
pub struct WavCodec;

const ID: &str = "wav-kura/1";
/// メタデータのチャンクとして読む大きさの上限。
const META_CHUNK_MAX: u32 = 1 << 20;

fn err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: ID.to_string(),
        message: message.into(),
    }
}

/// `LIST INFO` の ID とタグの名前。
const INFO_TAGS: [(&[u8; 4], &str); 8] = [
    (b"INAM", "title"),
    (b"IART", "artist"),
    (b"IPRD", "album"),
    (b"ICMT", "comment"),
    (b"ICRD", "date"),
    (b"IGNR", "genre"),
    (b"ISFT", "software"),
    (b"ICOP", "copyright"),
];

/// 読んだヘッダ。
struct Header {
    channels: u16,
    rate: u32,
    bits: u16,
    float: bool,
    block_align: u16,
    data_offset: u64,
    data_len: u64,
    meta: AudioMeta,
}

fn read_exact_or(src: &mut dyn ReadSeek, buf: &mut [u8], what: &str) -> Result<(), EngineError> {
    src.read_exact(buf)
        .map_err(|_| err(format!("{what}が途中で切れています")))
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn text_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// チャンクを順に読み、ヘッダとメタデータを集める（標本は読まない）。
/// 名前・タグとして読む 1 つの文字列の長さの上限（バイト）。
const TEXT_MAX: usize = 4_096;

/// メタデータの読み取りの予算（マーカーの数とバイト数）。
struct Budget {
    markers: u64,
    bytes: u64,
}

impl Budget {
    /// `len` バイトを使えれば true（使えなければ、そのメタデータは読まない）。
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

fn parse(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Header, EngineError> {
    let mut budget = Budget {
        markers: limits.max_markers,
        bytes: limits.max_meta_bytes,
    };
    let file_len = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    let mut riff = [0u8; 12];
    read_exact_or(src, &mut riff, "RIFF のヘッダ")?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(err("WAV ではありません"));
    }
    let mut fmt: Option<(u16, u32, u16, bool, u16)> = None;
    let mut data: Option<(u64, u64)> = None;
    let mut meta = AudioMeta::default();
    // cue の ID → 位置（同じ ID は最初のもの）と、ID → 名前（最初のもの）
    let mut cues: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let mut labels: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    let mut pos = 12u64;
    while pos.saturating_add(8) <= file_len {
        src.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 8];
        read_exact_or(src, &mut head, "チャンクのヘッダ")?;
        let id: [u8; 4] = [head[0], head[1], head[2], head[3]];
        let size = u32_at(&head, 4);
        let body = pos + 8;
        let available = file_len - body;
        match &id {
            b"fmt " => {
                if size < 16 || u64::from(size) > available {
                    return Err(err("fmt チャンクが正しくありません"));
                }
                let mut b = [0u8; 40];
                let n = size.min(40) as usize;
                read_exact_or(src, &mut b[..n], "fmt チャンク")?;
                let mut tag = u16_at(&b, 0);
                let channels = u16_at(&b, 2);
                let rate = u32_at(&b, 4);
                let block_align = u16_at(&b, 12);
                let bits = u16_at(&b, 14);
                if tag == 0xFFFE {
                    if n < 40 {
                        return Err(err("拡張の fmt チャンクが短すぎます"));
                    }
                    tag = u16_at(&b, 24);
                }
                let float = match (tag, bits) {
                    (1, 8 | 16 | 24 | 32) => false,
                    (3, 32 | 64) => true,
                    _ => {
                        return Err(EngineError::Unsupported(format!(
                            "この WAV の形式（{tag}・{bits} ビット）は読めません"
                        )));
                    }
                };
                if channels == 0
                    || rate == 0
                    || u32::from(block_align) != u32::from(channels) * u32::from(bits / 8)
                {
                    return Err(err("fmt チャンクの値が正しくありません"));
                }
                fmt = Some((channels, rate, bits, float, block_align));
            }
            b"data" => {
                // 大きさが分からない（0xFFFFFFFF）・ファイルより長いときは、ファイルの終わりまで
                let len = if size == u32::MAX {
                    available
                } else {
                    u64::from(size).min(available)
                };
                data = Some((body, len));
            }
            b"smpl" if (36..=META_CHUNK_MAX).contains(&size) && u64::from(size) <= available => {
                let mut b = try_vec(size as usize, 0u8)?;
                read_exact_or(src, &mut b, "smpl チャンク")?;
                let loops = u32_at(&b, 28);
                if loops > 0 && b.len() >= 36 + 24 {
                    let (start, end) = (u32_at(&b, 36 + 8), u32_at(&b, 36 + 12));
                    // smpl の終わりは含む。値では含まない形に直す
                    if end >= start {
                        meta.loop_range = Some(LoopRange {
                            start: u64::from(start),
                            end: u64::from(end) + 1,
                        });
                    }
                }
            }
            b"cue " if (4..=META_CHUNK_MAX).contains(&size) && u64::from(size) <= available => {
                let mut b = try_vec(size as usize, 0u8)?;
                read_exact_or(src, &mut b, "cue チャンク")?;
                let count = (u32_at(&b, 0) as usize).min((b.len() - 4) / 24);
                for i in 0..count {
                    if cues.len() as u64 >= budget.markers {
                        break;
                    }
                    let o = 4 + i * 24;
                    cues.entry(u32_at(&b, o))
                        .or_insert(u64::from(u32_at(&b, o + 20)));
                }
            }
            b"LIST" if (4..=META_CHUNK_MAX).contains(&size) && u64::from(size) <= available => {
                let mut b = try_vec(size as usize, 0u8)?;
                read_exact_or(src, &mut b, "LIST チャンク")?;
                let kind = &b[0..4];
                let mut o = 4usize;
                while o + 8 <= b.len() {
                    let sub = &b[o..o + 4];
                    let len = u32_at(&b, o + 4) as usize;
                    let start = o + 8;
                    let end = start.saturating_add(len).min(b.len());
                    let content = &b[start..end];
                    if kind == b"INFO" {
                        let key = INFO_TAGS
                            .iter()
                            .find(|(id, _)| id.as_slice() == sub)
                            .map(|(_, k)| (*k).to_string())
                            .unwrap_or_else(|| format!("riff:{}", String::from_utf8_lossy(sub)));
                        let text = text_of(&content[..content.len().min(TEXT_MAX)]);
                        if !text.is_empty()
                            && !meta.tags.contains_key(&key)
                            && budget.take(key.len() + text.len())
                        {
                            meta.tags.insert(key, text);
                        }
                    } else if kind == b"adtl" && sub == b"labl" && content.len() >= 4 {
                        let id = u32_at(content, 0);
                        let text = text_of(&content[4..content.len().min(4 + TEXT_MAX)]);
                        if !labels.contains_key(&id) && budget.take(text.len()) {
                            labels.insert(id, text);
                        }
                    }
                    o = start.saturating_add(len).saturating_add(len & 1);
                }
            }
            _ => {}
        }
        // 次のチャンク（大きさが奇数なら 1 バイト詰める）
        pos = body
            .saturating_add(u64::from(size))
            .saturating_add(u64::from(size & 1));
        if id == *b"data" && size == u32::MAX {
            break;
        }
    }
    let (channels, rate, bits, float, block_align) =
        fmt.ok_or_else(|| err("fmt チャンクがありません"))?;
    let (data_offset, data_len) = data.ok_or_else(|| err("data チャンクがありません"))?;
    let frames = data_len / u64::from(block_align);
    meta.markers = cues
        .iter()
        .filter(|(_, p)| **p < frames)
        .map(|(id, p)| Marker {
            pos: *p,
            name: labels.get(id).cloned().unwrap_or_default(),
        })
        .collect();
    meta.markers.sort_by_key(|m| m.pos);
    if let Some(l) = meta.loop_range
        && (l.start >= frames || l.end > frames)
    {
        meta.loop_range = None;
    }
    meta.source = Some(AudioSource { bits, float, rate });
    Ok(Header {
        channels,
        rate,
        bits,
        float,
        block_align,
        data_offset,
        data_len,
        meta,
    })
}

impl Decoder for WavCodec {
    fn id(&self) -> &'static str {
        ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "wav"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let h = parse(src, limits)?;
        Ok(Shape::Audio {
            rate: h.rate,
            channels: h.channels,
            frames: Dim::Exact(h.data_len / u64::from(h.block_align)),
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
        let frames = h.data_len / u64::from(h.block_align);
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
        let block = usize::from(h.block_align);
        let sample = usize::from(h.bits / 8);
        let chunk_frames = 16_384usize;
        let mut buf = try_vec(chunk_frames * block, 0u8)?;
        let mut done = 0usize;
        while done < frames {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let n = (frames - done).min(chunk_frames);
            let bytes = &mut buf[..n * block];
            read_exact_or(src, bytes, "data チャンク")?;
            for f in 0..n {
                for (c, plane) in planes.iter_mut().enumerate() {
                    let s = &bytes[f * block + c * sample..f * block + (c + 1) * sample];
                    plane[done + f] = to_f32(s, h.bits, h.float);
                }
            }
            done += n;
        }
        let audio = Audio::new(h.rate, planes.into_iter().map(Arc::new).collect(), h.meta)?;
        Ok(Value::Audio(Arc::new(audio)))
    }
}

/// 1 標本（リトルエンディアン）を -1〜1 の f32 にする。
fn to_f32(s: &[u8], bits: u16, float: bool) -> f32 {
    match (bits, float) {
        (8, false) => (f32::from(s[0]) - 128.0) / 128.0,
        (16, false) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32_768.0,
        (24, false) => {
            let v = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
            v as f32 / 8_388_608.0
        }
        (32, false) => {
            (f64::from(i32::from_le_bytes([s[0], s[1], s[2], s[3]])) / 2_147_483_648.0) as f32
        }
        (32, true) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        (64, true) => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
        _ => 0.0,
    }
}

/// -1〜1 の f32 を整数の標本にする（偶数への丸め、範囲で止める）。
fn quantize(s: f32, bits: u16) -> i64 {
    let scale = (1i64 << (bits - 1)) as f64;
    let v = (f64::from(s) * scale).round_ties_even();
    let v = if v.is_nan() { 0.0 } else { v };
    (v as i64).clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
}

impl Encoder for WavCodec {
    fn id(&self) -> &'static str {
        ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "wav" && matches!(shape, Shape::Audio { .. })
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

    fn caps(&self, format: &str) -> MetaCaps {
        format_by_id(format)
            .or_else(|| format_by_id("wav"))
            .map(|f| f.caps)
            .unwrap_or(MetaCaps {
                lossless: true,
                alpha: false,
                palette: false,
                high_bit_depth: true,
                icc: false,
                dpi: false,
                text: false,
                animation: false,
                loop_points: true,
                markers: true,
                tags: true,
            })
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
                "WAV には音声だけを書けます".into(),
            ));
        };
        let source = a.meta.source;
        // float を指定すればそれ、bits だけを指定すれば整数、どちらもなければ元の形式（docs/editing.md §5.3）
        let float = match (
            opts.get("float").and_then(serde_json::Value::as_bool),
            opts.get("bits"),
        ) {
            (Some(f), _) => f,
            (None, Some(_)) => false,
            (None, None) => source.is_some_and(|s| s.float),
        };
        if float
            && opts
                .get("bits")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|b| b != 32)
        {
            return Err(EngineError::Unsupported(
                "浮動小数の WAV は 32 ビットだけを書けます".into(),
            ));
        }
        let bits = if float {
            32
        } else {
            let wanted = opts
                .get("bits")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| source.filter(|s| !s.float).map(|s| u64::from(s.bits)))
                .unwrap_or(16);
            match wanted {
                8 | 16 | 24 | 32 => wanted as u16,
                _ => {
                    return Err(EngineError::Unsupported(format!(
                        "{wanted} ビットの WAV は書けません"
                    )));
                }
            }
        };
        let channels = u16::try_from(a.channels())
            .map_err(|_| EngineError::Unsupported("チャンネルが多すぎます".into()))?;
        let block_align = u32::from(channels) * u32::from(bits / 8);
        let frames = a.frames() as u64;
        let data_len = frames * u64::from(block_align);
        let extensible = channels > 2;

        // メタデータのチャンクを先に作る（RIFF の大きさを決めるため）
        let mut fmt = Vec::new();
        let tag: u16 = if float { 3 } else { 1 };
        fmt.extend_from_slice(&(if extensible { 0xFFFEu16 } else { tag }).to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&a.rate().to_le_bytes());
        let byte_rate = a.rate().checked_mul(block_align).ok_or_else(|| {
            EngineError::Unsupported("サンプルレートが大きすぎて WAV に書けません".into())
        })?;
        let block_align_16 = u16::try_from(block_align).map_err(|_| {
            EngineError::Unsupported("チャンネルが多すぎて WAV に書けません".into())
        })?;
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&block_align_16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        if extensible {
            fmt.extend_from_slice(&22u16.to_le_bytes());
            fmt.extend_from_slice(&bits.to_le_bytes());
            fmt.extend_from_slice(&0u32.to_le_bytes());
            // KSDATAFORMAT_SUBTYPE_PCM / IEEE_FLOAT
            fmt.extend_from_slice(&tag.to_le_bytes());
            fmt.extend_from_slice(&[
                0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
            ]);
        }
        let mut chunks: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        if let Some(l) = a
            .meta
            .loop_range
            .filter(|l| l.start < l.end && l.end <= frames && l.end <= u64::from(u32::MAX))
        {
            let mut b = Vec::new();
            for v in [0u32, 0, 1_000_000_000 / a.rate().max(1), 60, 0, 0, 0, 1, 0] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            for v in [0u32, 0, l.start as u32, (l.end - 1) as u32, 0, 0] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            chunks.push((*b"smpl", b));
        }
        let markers: Vec<&Marker> = a
            .meta
            .markers
            .iter()
            .filter(|m| m.pos < frames && m.pos <= u64::from(u32::MAX))
            .collect();
        if !markers.is_empty() {
            let mut cue = (markers.len() as u32).to_le_bytes().to_vec();
            let mut adtl = b"adtl".to_vec();
            for (i, m) in markers.iter().enumerate() {
                let id = i as u32 + 1;
                for v in [id, m.pos as u32] {
                    cue.extend_from_slice(&v.to_le_bytes());
                }
                cue.extend_from_slice(b"data");
                for v in [0u32, 0, m.pos as u32] {
                    cue.extend_from_slice(&v.to_le_bytes());
                }
                let mut labl = id.to_le_bytes().to_vec();
                labl.extend_from_slice(m.name.as_bytes());
                labl.push(0);
                push_sub(&mut adtl, b"labl", &labl);
            }
            chunks.push((*b"cue ", cue));
            chunks.push((*b"LIST", adtl));
        }
        let mut info = b"INFO".to_vec();
        for (key, text) in &a.meta.tags {
            let id: Option<[u8; 4]> = INFO_TAGS
                .iter()
                .find(|(_, k)| k == key)
                .map(|(id, _)| **id)
                .or_else(|| {
                    key.strip_prefix("riff:")
                        .and_then(|s| s.as_bytes().try_into().ok())
                });
            if let Some(id) = id {
                let mut t = text.as_bytes().to_vec();
                t.push(0);
                push_sub(&mut info, &id, &t);
            }
        }
        if info.len() > 4 {
            chunks.push((*b"LIST", info));
        }

        let padded = |n: u64| 8 + n + (n & 1);
        let riff_len = 4
            + padded(fmt.len() as u64)
            + padded(data_len)
            + chunks
                .iter()
                .map(|(_, b)| padded(b.len() as u64))
                .sum::<u64>();
        let riff_len = u32::try_from(riff_len)
            .map_err(|_| EngineError::Unsupported("4GB を超える WAV は書けません".into()))?;
        let mut written = 0u64;
        let mut put = |out: &mut dyn Write, bytes: &[u8]| -> Result<(), EngineError> {
            out.write_all(bytes)?;
            written += bytes.len() as u64;
            Ok(())
        };
        put(out, b"RIFF")?;
        put(out, &riff_len.to_le_bytes())?;
        put(out, b"WAVE")?;
        put(out, b"fmt ")?;
        put(out, &(fmt.len() as u32).to_le_bytes())?;
        put(out, &fmt)?;
        put(out, b"data")?;
        put(out, &(data_len as u32).to_le_bytes())?;
        let chunk_frames = 16_384usize;
        let sample = usize::from(bits / 8);
        let mut buf = try_vec(chunk_frames * block_align as usize, 0u8)?;
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
                    let at = f * block_align as usize + c * sample;
                    let dst = &mut buf[at..at + sample];
                    if float {
                        dst.copy_from_slice(&s.to_le_bytes());
                    } else if bits == 8 {
                        dst[0] = (quantize(s, 8) + 128) as u8;
                    } else {
                        let v = quantize(s, bits);
                        dst.copy_from_slice(&v.to_le_bytes()[..sample]);
                    }
                }
            }
            put(out, &buf[..n * block_align as usize])?;
            done += n;
        }
        if data_len & 1 == 1 {
            put(out, &[0])?;
        }
        for (id, body) in &chunks {
            put(out, id)?;
            put(out, &(body.len() as u32).to_le_bytes())?;
            put(out, body)?;
            if body.len() & 1 == 1 {
                put(out, &[0])?;
            }
        }
        Ok(written)
    }
}

/// LIST の中の小さなチャンクを足す（奇数なら 1 バイト詰める）。
fn push_sub(list: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    list.extend_from_slice(id);
    list.extend_from_slice(&(body.len() as u32).to_le_bytes());
    list.extend_from_slice(body);
    if body.len() & 1 == 1 {
        list.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::NoMonitor;
    use crate::testgen::InputGen;
    use std::io::Cursor;

    fn decode(bytes: Vec<u8>) -> Result<Value, EngineError> {
        CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "wav",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .map(|(v, _)| v)
    }

    fn encode(v: &Value, opts: serde_json::Value) -> Vec<u8> {
        CodecSet::pinned()
            .encode_bytes(v, "wav", &opts, &NoMonitor)
            .unwrap()
            .0
    }

    /// 整数の標本を読んで同じビット数で書くと、ビット単位で元に戻る（E17）。ループ点・マーカー・タグも往復する（E20）。
    #[test]
    fn integer_round_trips_are_exact_and_keep_metadata() {
        for (bits, channels) in [(8u16, 1u16), (16, 2), (24, 2), (32, 1), (16, 6)] {
            // 整数の標本を直接作った WAV（メタデータ付き）
            let frames = 1_001u64;
            let mut r = crate::determinism::rng(u64::from(bits));
            use rand_chacha::rand_core::Rng;
            let planes: Vec<Arc<Vec<f32>>> = (0..channels)
                .map(|_| {
                    Arc::new(
                        (0..frames)
                            .map(|_| {
                                let v = (r.next_u64() as i64) >> (64 - bits);
                                if bits == 8 {
                                    v as f32 / 128.0
                                } else {
                                    (v as f64 / (1u64 << (bits - 1)) as f64) as f32
                                }
                            })
                            .collect(),
                    )
                })
                .collect();
            let meta = AudioMeta {
                loop_range: Some(LoopRange {
                    start: 10,
                    end: 900,
                }),
                markers: vec![
                    Marker {
                        pos: 5,
                        name: "attack".into(),
                    },
                    Marker {
                        pos: 700,
                        name: "".into(),
                    },
                ],
                source: Some(AudioSource {
                    bits,
                    float: false,
                    rate: 44_100,
                }),
                tags: [
                    ("title".to_string(), "爆発".to_string()),
                    ("riff:ISBJ".to_string(), "se".to_string()),
                ]
                .into(),
            };
            let v = Value::Audio(Arc::new(Audio::new(44_100, planes, meta).unwrap()));
            let bytes = encode(&v, json!({ "bits": bits }));
            let back = decode(bytes.clone()).unwrap();
            assert_eq!(back, v, "{bits} ビット・{channels} チャンネル");
            assert_eq!(encode(&back, json!({ "bits": bits })), bytes);
        }
    }

    #[test]
    fn float_round_trip_is_exact() {
        let v = (InputGen::Sine {
            rate: 48_000,
            channels: 2,
            frames: 5_000,
            freq: 440,
            loop_range: None,
        })
        .generate()
        .unwrap();
        let Value::Audio(a) = &v else { panic!() };
        let back = decode(encode(&v, json!({ "float": true }))).unwrap();
        let Value::Audio(b) = &back else { panic!() };
        assert_eq!(a.planes(), b.planes());
        assert_eq!(
            b.meta.source,
            Some(AudioSource {
                bits: 32,
                float: true,
                rate: 48_000
            })
        );
    }

    #[test]
    fn quantization_rounds_half_even_and_clips() {
        assert_eq!(quantize(1.0, 16), 32_767);
        assert_eq!(quantize(-1.0, 16), -32_768);
        assert_eq!(quantize(2.0, 16), 32_767);
        assert_eq!(quantize(0.5 / 32_768.0, 16), 0);
        assert_eq!(quantize(1.5 / 32_768.0, 16), 2);
        assert_eq!(quantize(f32::NAN, 24), 0);
    }

    /// 壊れたファイル・巨大な大きさを書いたファイルは、落ちずにエラーになる（E25）。panic を受け止めずに直接呼ぶ。
    #[test]
    fn broken_files_are_errors_not_panics() {
        let v = (InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: 300,
            seed: 1,
        })
        .generate()
        .unwrap();
        let mut a = match &v {
            Value::Audio(a) => Arc::unwrap_or_clone(a.clone()),
            _ => panic!(),
        };
        a.meta.loop_range = Some(LoopRange { start: 1, end: 200 });
        a.meta.markers = vec![Marker {
            pos: 3,
            name: "m".into(),
        }];
        a.meta.tags.insert("artist".into(), "kura".into());
        let bytes = encode(&Value::Audio(Arc::new(a)), json!({ "bits": 24 }));
        assert!(decode(bytes.clone()).is_ok());
        assert!(decode(b"RIFF\0\0\0\0WAVE".to_vec()).is_err());
        assert!(decode(b"not a wav at all".to_vec()).is_err());
        let small = Limits {
            max_audio_frames: 10,
            ..Limits::default()
        };
        assert!(matches!(
            CodecSet::pinned().decode(
                &mut Cursor::new(bytes.clone()),
                "wav",
                &DecodeOptions::default(),
                &small,
                &NoMonitor
            ),
            Err(EngineError::Limit(_))
        ));
        let tight = Limits {
            max_audio_frames: 1 << 16,
            max_markers: 64,
            max_meta_bytes: 1 << 16,
            ..Limits::default()
        };
        let mut r = crate::determinism::rng(3);
        use rand_chacha::rand_core::Rng;
        let (mut ok, mut failed) = (0, 0);
        for _ in 0..2_000 {
            let mut broken = bytes.clone();
            for _ in 0..1 + (r.next_u32() % 6) {
                let i = r.next_u32() as usize % broken.len();
                broken[i] = r.next_u32() as u8;
            }
            if r.next_u32().is_multiple_of(4) {
                let cut = r.next_u32() as usize % broken.len();
                broken.truncate(cut);
            }
            match WavCodec.shape(
                &mut Cursor::new(broken.clone()),
                "wav",
                &DecodeOptions::default(),
                &tight,
            ) {
                Ok(shape) if tight.check(&shape).is_ok() => {
                    match WavCodec.decode(
                        &mut Cursor::new(broken),
                        "wav",
                        &DecodeOptions::default(),
                        &tight,
                        &NoMonitor,
                    ) {
                        Ok(_) => ok += 1,
                        Err(_) => failed += 1,
                    }
                }
                _ => failed += 1,
            }
        }
        assert!(ok > 100 && failed > 50, "ok {ok} failed {failed}");
    }

    /// 同じ ID の cue を大量に並べ、長い labl を付けても、名前は複製されず、マーカーとメタデータの上限で止まる。
    #[test]
    fn cue_and_label_floods_are_bounded() {
        use crate::testfiles::FileGen;
        let cues: Vec<(u32, u32)> = (0..20_000)
            .map(|i| (i % 3, i % 50))
            .chain((0..20_000).map(|i| (100 + i, i % 50)))
            .collect();
        let labels: Vec<(u32, String)> = vec![
            (0, "x".repeat(100_000)),
            (1, "y".repeat(10)),
            (0, "dup".into()),
        ];
        let bytes = FileGen::Wav {
            tag: 1,
            bits: 16,
            channels: 1,
            rate: 8_000,
            frames: 50,
            extensible: false,
            smpl: None,
            cues,
            labels,
            info: vec![],
            unknown_size: false,
            seed: 1,
        }
        .bytes()
        .unwrap();
        let limits = Limits {
            max_markers: 1_000,
            ..Limits::default()
        };
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "wav",
                &DecodeOptions::default(),
                &limits,
                &NoMonitor,
            )
            .unwrap();
        let Value::Audio(a) = v else { panic!() };
        assert!(a.meta.markers.len() <= 1_000);
        let total: usize = a.meta.markers.iter().map(|m| m.name.len()).sum();
        // 名前は上限の長さで切り、同じ ID は 1 回だけ
        assert!(total <= TEXT_MAX + 10, "{total}");
    }

    /// 浮動小数の元に bits だけを指定すると整数で書き、設定がなければ元の形式・ビット数のまま元のバイト列に戻る（E17）。
    #[test]
    fn explicit_bits_win_and_defaults_follow_the_source() {
        let src = (InputGen::AudioNoise {
            rate: 22_050,
            channels: 1,
            frames: 100,
            seed: 4,
        })
        .generate()
        .unwrap();
        let float_file = encode(&src, json!({ "float": true }));
        let float_value = decode(float_file.clone()).unwrap();
        let int16 = encode(&float_value, json!({ "bits": 16 }));
        let Value::Audio(back) = decode(int16).unwrap() else {
            panic!()
        };
        assert_eq!(
            back.meta.source,
            Some(AudioSource {
                bits: 16,
                float: false,
                rate: 22_050
            })
        );
        assert_eq!(encode(&float_value, json!({})), float_file);
        let int24_file = encode(&src, json!({ "bits": 24 }));
        assert_eq!(
            encode(&decode(int24_file.clone()).unwrap(), json!({})),
            int24_file
        );
        assert!(
            CodecSet::pinned()
                .encode_bytes(
                    &float_value,
                    "wav",
                    &json!({ "float": true, "bits": 16 }),
                    &NoMonitor
                )
                .is_err()
        );
    }

    /// 上限を超えるサンプルレートは読む段階で断り、書くときもあふれずにエラーにする。
    #[test]
    fn extreme_rates_are_rejected_without_overflow() {
        use crate::testfiles::FileGen;
        let bytes = FileGen::Wav {
            tag: 3,
            bits: 32,
            channels: 1,
            rate: 0x4000_0000,
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
        assert!(matches!(decode(bytes.clone()), Err(EngineError::Limit(_))));
        let loose = Limits {
            max_sample_rate: u32::MAX,
            ..Limits::default()
        };
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "wav",
                &DecodeOptions::default(),
                &loose,
                &NoMonitor,
            )
            .unwrap();
        assert!(matches!(
            CodecSet::pinned().encode_bytes(&v, "wav", &json!({}), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
    }

    /// 自前のエンコーダが書かない形（64 ビットの浮動小数・拡張形式の浮動小数・大きさの分からない data・他のツールのメタデータ）を読む。
    #[test]
    fn reads_shapes_our_encoder_does_not_write() {
        use crate::testfiles::FileGen;
        let make = |tag, bits, extensible, unknown_size| FileGen::Wav {
            tag,
            bits,
            channels: 2,
            rate: 44_100,
            frames: 64,
            extensible,
            smpl: Some((4, 9)),
            cues: vec![(7, 2), (8, 60)],
            labels: vec![(8, "end".into())],
            info: vec![
                ("INAM".into(), "name".into()),
                ("ISBJ".into(), "subject".into()),
            ],
            unknown_size,
            seed: 5,
        };
        for (tag, bits, ext, unknown) in [
            (3u16, 64u16, false, false),
            (3, 32, true, false),
            (1, 24, true, true),
            (1, 8, false, true),
        ] {
            let v = decode(make(tag, bits, ext, unknown).bytes().unwrap()).unwrap();
            let Value::Audio(a) = v else { panic!() };
            assert_eq!((a.channels(), a.frames()), (2, 64), "{tag}/{bits}");
            assert!(
                a.planes()
                    .iter()
                    .all(|p| p.iter().all(|s| (-1.0..=1.0).contains(s)))
            );
            assert_eq!(a.meta.loop_range, Some(LoopRange { start: 4, end: 10 }));
            assert_eq!(
                a.meta.markers,
                vec![
                    Marker {
                        pos: 2,
                        name: "".into()
                    },
                    Marker {
                        pos: 60,
                        name: "end".into()
                    }
                ]
            );
            assert_eq!(a.meta.tags.get("title").map(String::as_str), Some("name"));
            assert_eq!(
                a.meta.tags.get("riff:ISBJ").map(String::as_str),
                Some("subject")
            );
        }
    }
}
