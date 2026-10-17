//! FLAC（読みは symphonia、書きは自作。どちらも Pinned で整数だけ）。
//!
//! - 読む（`flac-symphonia-dec/1`）：symphonia の FLAC の読み口とデコーダ（STREAMINFO の MD5 を照らし、合わなければ
//!   壊れたファイル）。標本は 32 ビットに左詰めした整数を `pcm::int_to_f32(v, 32)`（WAV・AIFF と同じ値）。
//!   元の形式のビット数は入れ物の幅（8・16・24・32）。Vorbis コメントのタグ・LOOPSTART・LOOPLENGTH を読む（`symph`）。
//! - 書く（`flac-kura-enc/1`。ADR-148）：8・16・24 ビットの整数（`pcm::quantize`）、1〜8 チャンネル、レートは 1〜655,350 Hz。
//!   ブロックは 4,096 フレームの固定（最後だけ短い。STREAMINFO の最小・最大のブロックは 4,096）。サブフレームは定数・生・固定の予測子 0〜4 から最小のビット数のものを
//!   選び（下位の 0 のビット（wasted bits）も使う）、残差は分割ライス（分割の次数 0〜8。パラメタは平均から見積もって
//!   前後を正確に比べる。14 を超えれば RICE2）。ステレオは独立・左と差・差と右・中央と差の 4 通りから最小を選ぶ。
//!   どれも整数だけで決まるので、すべてのターゲットで同じバイト列（R2）。フレームは互いに独立なので並列に作り、番号の順に
//!   並べる。STREAMINFO には最小・最大のブロック（最後のブロックを除く）とフレームの大きさ、総フレーム数、標本の MD5
//!   （リトルエンディアンの入れ物の幅で並べたもの）を書く。VORBIS_COMMENT（vendor は "kura"）にタグと LOOPSTART・
//!   LOOPLENGTH を書く。SEEKTABLE・PADDING は書かない。

use std::io::Write;
use std::sync::Arc;

use md5::{Digest as _, Md5};
use serde_json::json;
use symphonia_core::codecs::audio::AudioDecoder as _;

use crate::codec::symph::{self, Container};
use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, caps_of, pcm};
use crate::determinism::{block_count, par_blocks};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::{Monitor, NoMonitor};
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Shape};
use crate::value::{Audio, AudioSource, Value};

/// FLAC の読み書き。
pub struct FlacCodec;

/// 読み取りの約束の版。
const DEC_ID: &str = "flac-symphonia-dec/1";
/// 書き出しの約束の版。
const ENC_ID: &str = "flac-kura-enc/1";
/// ブロックの大きさ（フレーム）。
const BLOCK: usize = 4_096;
/// 分割ライスの分割の次数の上限。
const MAX_PARTITION_ORDER: u32 = 8;

fn dec_err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: message.into(),
    }
}

/// 読んだ形とメタデータ。
struct Probe {
    rate: u32,
    channels: u16,
    frames: u64,
    bits: Option<u32>,
}

fn probe(data: &[u8], limits: &Limits, mon: &dyn Monitor) -> Result<Probe, EngineError> {
    let mut o = symph::open(DEC_ID, data, Container::Flac, mon, limits)?;
    let frames = match o.track.num_frames.filter(|n| *n > 0) {
        Some(n) => n,
        None => o.count_frames(DEC_ID, mon, limits)?,
    };
    Ok(Probe {
        rate: o.rate,
        channels: u16::try_from(o.channels).map_err(|_| dec_err("チャンネルが多すぎます"))?,
        frames,
        bits: o.params.bits_per_sample,
    })
}

impl Decoder for FlacCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "flac"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let data = symph::read_all(src, limits)?;
        let p = probe(&data, limits, &NoMonitor)?;
        Ok(Shape::Audio {
            rate: p.rate,
            channels: p.channels,
            frames: Dim::Exact(p.frames),
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
        let data = symph::read_all(src, limits)?;
        let p = probe(&data, limits, mon)?;
        limits
            .check(&Shape::Audio {
                rate: p.rate,
                channels: p.channels,
                frames: Dim::Exact(p.frames),
            })
            .map_err(EngineError::Limit)?;
        let mut o = symph::open(DEC_ID, &data, Container::Flac, mon, limits)?;
        let raw = o.raw_tags();
        let opts = symph::decoder_options().verify(true);
        let mut dec = symphonia_bundle_flac::FlacDecoder::try_new(&o.params, &opts)
            .map_err(|e| symph::sym_err(DEC_ID, e, mon))?;
        let planes = o.decode_all(DEC_ID, &mut dec, p.frames, mon)?;
        if dec.finalize().verify_ok == Some(false) {
            return Err(dec_err("MD5 が合いません（壊れたファイル）"));
        }
        if planes.first().map_or(0, Vec::len) as u64 != p.frames {
            return Err(dec_err("長さがヘッダと合いません"));
        }
        let mut meta = symph::meta_from_tags(&raw, p.frames, true, limits);
        meta.source = Some(AudioSource {
            bits: pcm::container_bits(p.bits.unwrap_or(16).clamp(1, 32) as u16),
            float: false,
            rate: p.rate,
        });
        let audio = Audio::new(p.rate, planes.into_iter().map(Arc::new).collect(), meta)?;
        Ok(Value::Audio(Arc::new(audio)))
    }
}

/// ビットの書き手（上位のビットから）。
struct Bits {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    fn put(&mut self, v: u64, bits: u32) {
        if bits == 0 {
            return;
        }
        if bits > 32 {
            self.put(v >> 32, bits - 32);
            self.put(v & 0xffff_ffff, 32);
            return;
        }
        let v = v & ((1u64 << bits) - 1);
        self.acc = (self.acc << bits) | v;
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.buf.push((self.acc >> self.n) as u8);
        }
        self.acc &= (1u64 << self.n) - 1;
    }

    fn put_signed(&mut self, v: i64, bits: u32) {
        self.put(v as u64, bits);
    }

    fn unary(&mut self, mut q: u64) {
        while q >= 32 {
            self.put(0, 32);
            q -= 32;
        }
        self.put(1, q as u32 + 1);
    }

    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
    }
    c
}

fn crc16(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c ^= u16::from(b) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
        }
    }
    c
}

/// フレームの番号の UTF-8 風の符号。
fn utf8_num(v: u64, out: &mut Vec<u8>) {
    if v < 0x80 {
        out.push(v as u8);
        return;
    }
    let len: u32 = match v {
        0..0x800 => 2,
        0x800..0x1_0000 => 3,
        0x1_0000..0x20_0000 => 4,
        0x20_0000..0x400_0000 => 5,
        0x400_0000..0x8000_0000 => 6,
        _ => 7,
    };
    let lead = if len == 7 {
        0xFE
    } else {
        let mask = !((1u16 << (8 - len)) as u8).wrapping_sub(1);
        mask | ((v >> (6 * (len - 1))) as u8 & ((1u8 << (7 - len)) - 1))
    };
    out.push(lead);
    for i in (0..len - 1).rev() {
        out.push(0x80 | ((v >> (6 * i)) & 0x3f) as u8);
    }
}

fn fold(r: i64) -> u64 {
    ((r << 1) ^ (r >> 63)) as u64
}

fn residual(x: &[i64], order: usize, out: &mut Vec<i64>) {
    out.clear();
    for i in order..x.len() {
        let r = match order {
            0 => x[i],
            1 => x[i] - x[i - 1],
            2 => x[i] - 2 * x[i - 1] + x[i - 2],
            3 => x[i] - 3 * x[i - 1] + 3 * x[i - 2] - x[i - 3],
            _ => x[i] - 4 * x[i - 1] + 6 * x[i - 2] - 4 * x[i - 3] + x[i - 4],
        };
        out.push(r);
    }
}

/// 分割ライスの計画。
struct RicePlan {
    order: u32,
    params: Vec<u32>,
    bits: u64,
    rice2: bool,
}

fn rice_bits(u: &[u64], k: u32) -> u64 {
    u.iter()
        .fold(0u64, |s, v| s.saturating_add((v >> k) + 1 + u64::from(k)))
}

fn plan_rice(res: &[i64], pred_order: usize, block: usize) -> Option<RicePlan> {
    let u: Vec<u64> = res.iter().map(|&r| fold(r)).collect();
    let mut best: Option<RicePlan> = None;
    for po in 0..=MAX_PARTITION_ORDER {
        if !block.is_multiple_of(1usize << po) {
            break;
        }
        let psize = block >> po;
        if psize <= pred_order {
            break;
        }
        let mut params = Vec::with_capacity(1 << po);
        let mut bits = 0u64;
        let mut start = 0usize;
        let mut rice2 = false;
        for p in 0..(1usize << po) {
            let n = if p == 0 { psize - pred_order } else { psize };
            let part = &u[start..start + n];
            start += n;
            let sum = part.iter().fold(0u64, |a, v| a.saturating_add(*v));
            let mean = if n == 0 { 0 } else { sum / n as u64 };
            let est = if mean == 0 {
                0
            } else {
                63 - mean.leading_zeros()
            };
            let (lo, hi) = (est.saturating_sub(1), (est + 1).min(30));
            let mut k = lo;
            let mut kbits = rice_bits(part, lo);
            for cand in lo + 1..=hi {
                let b = rice_bits(part, cand);
                if b < kbits {
                    (k, kbits) = (cand, b);
                }
            }
            rice2 |= k > 14;
            bits = bits.saturating_add(kbits);
            params.push(k);
        }
        let pbits: u64 = if rice2 { 5 } else { 4 };
        bits = bits.saturating_add(2 + 4 + pbits * (1u64 << po));
        if best.as_ref().is_none_or(|b| bits < b.bits) {
            best = Some(RicePlan {
                order: po,
                params,
                bits,
                rice2,
            });
        }
    }
    best
}

enum Sub {
    Constant(i64),
    Verbatim,
    Fixed {
        order: usize,
        plan: RicePlan,
        res: Vec<i64>,
    },
}

struct Chosen {
    sub: Sub,
    wasted: u32,
    bits: u64,
}

fn choose_subframe(x: &[i64], bps: u32) -> Chosen {
    let n = x.len();
    if x.iter().all(|&v| v == x[0]) {
        return Chosen {
            sub: Sub::Constant(x[0]),
            wasted: 0,
            bits: 8 + u64::from(bps),
        };
    }
    let or = x.iter().fold(0i64, |a, &v| a | v);
    let wasted = if or == 0 {
        0
    } else {
        or.trailing_zeros().min(bps - 1)
    };
    let shifted: Vec<i64>;
    let xs: &[i64] = if wasted > 0 {
        shifted = x.iter().map(|&v| v >> wasted).collect();
        &shifted
    } else {
        x
    };
    let ebps = bps - wasted;
    let header = 8 + u64::from(wasted);
    let mut best_bits = header + u64::from(ebps) * n as u64;
    let mut best = Sub::Verbatim;
    let mut res = Vec::with_capacity(n);
    for order in 0..=4usize {
        if order >= n {
            break;
        }
        residual(xs, order, &mut res);
        if res
            .iter()
            .any(|&r| r > i64::from(i32::MAX) || r < i64::from(i32::MIN))
        {
            continue;
        }
        let Some(plan) = plan_rice(&res, order, n) else {
            continue;
        };
        let bits = header + u64::from(ebps) * order as u64 + plan.bits;
        if bits < best_bits {
            best_bits = bits;
            best = Sub::Fixed {
                order,
                plan,
                res: res.clone(),
            };
        }
    }
    Chosen {
        sub: best,
        wasted,
        bits: best_bits,
    }
}

fn write_wasted(w: &mut Bits, wasted: u32) {
    if wasted == 0 {
        w.put(0, 1);
    } else {
        w.put(1, 1);
        w.unary(u64::from(wasted - 1));
    }
}

fn write_subframe(w: &mut Bits, x: &[i64], bps: u32, c: &Chosen) {
    match &c.sub {
        Sub::Constant(v) => {
            w.put(0, 8);
            w.put_signed(*v, bps);
        }
        Sub::Verbatim => {
            w.put(0b000_0001, 7);
            write_wasted(w, c.wasted);
            for &v in x {
                w.put_signed(v >> c.wasted, bps - c.wasted);
            }
        }
        Sub::Fixed { order, plan, res } => {
            w.put(0b000_1000 | (*order as u64), 7);
            write_wasted(w, c.wasted);
            let ebps = bps - c.wasted;
            for &v in &x[..*order] {
                w.put_signed(v >> c.wasted, ebps);
            }
            w.put(u64::from(plan.rice2), 2);
            w.put(u64::from(plan.order), 4);
            let pbits = if plan.rice2 { 5 } else { 4 };
            let psize = x.len() >> plan.order;
            let mut start = 0;
            for (p, &k) in plan.params.iter().enumerate() {
                let n = if p == 0 { psize - order } else { psize };
                w.put(u64::from(k), pbits);
                for &r in &res[start..start + n] {
                    let u = fold(r);
                    w.unary(u >> k);
                    w.put(u & ((1u64 << k) - 1), k);
                }
                start += n;
            }
        }
    }
}

fn rate_code(rate: u32) -> u64 {
    match rate {
        88_200 => 0b0001,
        176_400 => 0b0010,
        192_000 => 0b0011,
        8_000 => 0b0100,
        16_000 => 0b0101,
        22_050 => 0b0110,
        24_000 => 0b0111,
        32_000 => 0b1000,
        44_100 => 0b1001,
        48_000 => 0b1010,
        96_000 => 0b1011,
        _ => 0b0000,
    }
}

fn size_code(bps: u32) -> u64 {
    match bps {
        8 => 0b001,
        16 => 0b100,
        24 => 0b110,
        _ => 0b000,
    }
}

/// 1 フレーム（フレームの番号 `no`、各チャンネルの標本 `xs`）のバイト列。
fn frame(no: u64, xs: &[Vec<i64>], rate: u32, bps: u32) -> Vec<u8> {
    let nch = xs.len();
    let n = xs[0].len();
    let (assign, chosen, inputs, bpss): (u64, Vec<Chosen>, Vec<Vec<i64>>, Vec<u32>) = if nch == 2 {
        let (l, r) = (&xs[0], &xs[1]);
        let side: Vec<i64> = l.iter().zip(r).map(|(a, b)| a - b).collect();
        let mid: Vec<i64> = l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect();
        let cl = choose_subframe(l, bps);
        let cr = choose_subframe(r, bps);
        let cs = choose_subframe(&side, bps + 1);
        let cm = choose_subframe(&mid, bps);
        // 同じ大きさなら、並びの先のもの
        let cands = [
            (cl.bits + cr.bits, 0b0001u64),
            (cl.bits + cs.bits, 0b1000),
            (cs.bits + cr.bits, 0b1001),
            (cm.bits + cs.bits, 0b1010),
        ];
        let mut best = cands[0];
        for c in &cands[1..] {
            if c.0 < best.0 {
                best = *c;
            }
        }
        match best.1 {
            0b0001 => (
                best.1,
                vec![cl, cr],
                vec![l.clone(), r.clone()],
                vec![bps, bps],
            ),
            0b1000 => (
                best.1,
                vec![cl, cs],
                vec![l.clone(), side],
                vec![bps, bps + 1],
            ),
            0b1001 => (
                best.1,
                vec![cs, cr],
                vec![side, r.clone()],
                vec![bps + 1, bps],
            ),
            _ => (best.1, vec![cm, cs], vec![mid, side], vec![bps, bps + 1]),
        }
    } else {
        let chosen = xs.iter().map(|x| choose_subframe(x, bps)).collect();
        ((nch - 1) as u64, chosen, xs.to_vec(), vec![bps; nch])
    };
    let mut hdr = Vec::with_capacity(16);
    let (bcode, bextra): (u64, Option<(u64, u32)>) = match n {
        192 => (1, None),
        576 => (2, None),
        1152 => (3, None),
        2304 => (4, None),
        4608 => (5, None),
        256 => (8, None),
        512 => (9, None),
        1024 => (10, None),
        2048 => (11, None),
        4096 => (12, None),
        8192 => (13, None),
        16384 => (14, None),
        32768 => (15, None),
        _ if n <= 256 => (6, Some(((n - 1) as u64, 8))),
        _ => (7, Some(((n - 1) as u64, 16))),
    };
    hdr.push(0xFF);
    hdr.push(0xF8);
    hdr.push(((bcode << 4) | rate_code(rate)) as u8);
    hdr.push(((assign << 4) | (size_code(bps) << 1)) as u8);
    utf8_num(no, &mut hdr);
    if let Some((v, b)) = bextra {
        if b == 8 {
            hdr.push(v as u8);
        } else {
            hdr.extend_from_slice(&(v as u16).to_be_bytes());
        }
    }
    hdr.push(crc8(&hdr));
    let mut w = Bits::new();
    for b in &hdr {
        w.put(u64::from(*b), 8);
    }
    for ((x, bps), c) in inputs.iter().zip(&bpss).zip(&chosen) {
        write_subframe(&mut w, x, *bps, c);
    }
    w.align();
    let crc = crc16(&w.buf);
    w.put(u64::from(crc), 16);
    w.buf
}

/// メタデータのブロック 1 つ（種類、本体）。16 MiB を超えるものは書けない。
fn meta_block(last: bool, kind: u8, body: &[u8], out: &mut Vec<u8>) -> Result<(), EngineError> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|l| *l < 1 << 24)
        .ok_or_else(|| EngineError::Unsupported("FLAC のメタデータが大きすぎます".into()))?;
    out.push(if last { 0x80 | kind } else { kind });
    out.extend_from_slice(&len.to_be_bytes()[1..]);
    out.extend_from_slice(body);
    Ok(())
}

/// VORBIS_COMMENT の本体（vendor は "kura"）。
fn vorbis_comment_block(comments: &[(String, String)]) -> Vec<u8> {
    let mut b = Vec::new();
    let vendor = b"kura";
    b.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    b.extend_from_slice(vendor);
    b.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (k, v) in comments {
        let entry = format!("{k}={v}");
        b.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        b.extend_from_slice(entry.as_bytes());
    }
    b
}

impl Encoder for FlacCodec {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "flac" && matches!(shape, Shape::Audio { .. })
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "bits",
                ParamKind::Int {
                    min: 8,
                    max: 24,
                    step: 8,
                },
                Unit::None,
                json!(16),
            )],
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
                "FLAC には音声だけを書けます".into(),
            ));
        };
        let (bits, _) = pcm::target(opts, a.meta.source, &[8, 16, 24], false, "FLAC")?;
        let bps = u32::from(bits);
        let nch = a.channels();
        if !(1..=8).contains(&nch) {
            return Err(EngineError::Unsupported(
                "FLAC に書けるのは 1〜8 チャンネルです".into(),
            ));
        }
        let rate = a.rate();
        if rate == 0 || rate > 655_350 {
            return Err(EngineError::Unsupported(
                "このサンプルレートは FLAC に書けません".into(),
            ));
        }
        let total = a.frames();
        let total64 = total as u64;
        // 総フレーム数 0 は FLAC では「不明」の意味で、フレームのない FLAC は読み直せない（flac も空の入力は書かない）
        if total == 0 {
            return Err(EngineError::Unsupported(
                "空の音声は FLAC に書けません".into(),
            ));
        }
        if total64 >= 1 << 36 {
            return Err(EngineError::Unsupported(
                "長すぎて FLAC に書けません".into(),
            ));
        }
        // フレームは互いに独立なので並列に作り、番号の順に並べる（バイト列はスレッド数によらない）
        let frames = par_blocks(block_count(total, BLOCK), mon, |b| {
            let start = b * BLOCK;
            let end = (start + BLOCK).min(total);
            let xs: Vec<Vec<i64>> = a
                .planes()
                .iter()
                .map(|p| {
                    p[start..end]
                        .iter()
                        .map(|s| pcm::quantize(*s, bits))
                        .collect()
                })
                .collect();
            Ok(frame(b as u64, &xs, rate, bps))
        })?;
        // 標本の MD5（入れ物の幅のリトルエンディアンを、フレームの順・チャンネルの順に）
        let mut md5 = Md5::new();
        let bytes = usize::from(bits / 8);
        let mut buf = Vec::with_capacity(BLOCK * nch * bytes);
        let mut i = 0usize;
        while i < total {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            buf.clear();
            let end = (i + BLOCK).min(total);
            for t in i..end {
                for p in a.planes() {
                    buf.extend_from_slice(&pcm::quantize(p[t], bits).to_le_bytes()[..bytes]);
                }
            }
            md5.update(&buf);
            i = end;
        }
        let digest = md5.finalize();
        let (min_fs, max_fs) = frames.iter().fold((u32::MAX, 0u32), |(lo, hi), f| {
            let n = f.len() as u32;
            (lo.min(n), hi.max(n))
        });
        let mut si = Bits::new();
        // 最小・最大のブロックは決まった大きさ（最後の短いブロックは数えない。libFLAC と同じ）
        si.put(BLOCK as u64, 16);
        si.put(BLOCK as u64, 16);
        si.put(u64::from(if min_fs == u32::MAX { 0 } else { min_fs }), 24);
        si.put(u64::from(max_fs), 24);
        si.put(u64::from(rate), 20);
        si.put((nch - 1) as u64, 3);
        si.put(u64::from(bps - 1), 5);
        si.put(total64, 36);
        for b in digest.iter() {
            si.put(u64::from(*b), 8);
        }
        let comments = symph::vorbis_comments_for(&a.meta, total64);
        let mut head = b"fLaC".to_vec();
        meta_block(comments.is_empty(), 0, &si.buf, &mut head)?;
        if !comments.is_empty() {
            meta_block(true, 4, &vorbis_comment_block(&comments), &mut head)?;
        }
        out.write_all(&head)?;
        let mut written = head.len() as u64;
        for f in &frames {
            out.write_all(f)?;
            written += f.len() as u64;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecSet;
    use crate::value::{AudioMeta, LoopRange};
    use std::io::Cursor;

    fn decode(bytes: &[u8]) -> Result<Value, EngineError> {
        CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes.to_vec()),
                "flac",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .map(|(v, _)| v)
    }

    fn encode(v: &Value, opts: serde_json::Value) -> Vec<u8> {
        CodecSet::pinned()
            .encode_bytes(v, "flac", &opts, &NoMonitor)
            .unwrap()
            .0
    }

    fn grid(bits: u32, channels: usize, frames: usize, seed: u64) -> Value {
        let scale = f64::from(1u32 << (bits - 1));
        let mut state = seed;
        let planes = (0..channels)
            .map(|c| {
                Arc::new(
                    (0..frames)
                        .map(|i| {
                            state = state
                                .wrapping_mul(6_364_136_223_846_793_005)
                                .wrapping_add(1);
                            // 音らしい波形（前後の相関）と雑音を混ぜる
                            let wave = (crate::math::sin(i as f64 * (0.01 + c as f64 * 0.003))
                                * 0.6
                                * scale) as i64;
                            let noise = ((state >> 40) as i64 % 64) - 32;
                            let v = (wave + noise).clamp(-(scale as i64), scale as i64 - 1);
                            (v as f64 / scale) as f32
                        })
                        .collect::<Vec<f32>>(),
                )
            })
            .collect();
        Value::Audio(Arc::new(
            Audio::new(44_100, planes, AudioMeta::default()).unwrap(),
        ))
    }

    #[test]
    fn round_trips_bit_exactly_and_compresses() {
        for bits in [8u32, 16, 24] {
            for channels in [1usize, 2, 3, 8] {
                for frames in [1usize, 4_095, 4_096, 10_000] {
                    let v = grid(bits, channels, frames, u64::from(bits) + channels as u64);
                    let bytes = encode(&v, json!({ "bits": bits }));
                    let back = decode(&bytes)
                        .unwrap_or_else(|e| panic!("{bits} {channels} {frames}: {e}"));
                    let (Value::Audio(x), Value::Audio(y)) = (&v, &back) else {
                        panic!()
                    };
                    assert_eq!(x.planes(), y.planes(), "{bits} {channels} {frames}");
                    assert_eq!(y.meta.source.map(|s| s.bits), Some(bits as u16));
                    if frames == 10_000 && bits == 16 {
                        let raw = frames * channels * 2;
                        assert!(bytes.len() < raw * 3 / 4, "{} / {raw}", bytes.len());
                    }
                }
            }
        }
    }

    #[test]
    fn keeps_tags_and_loop_points() {
        let Value::Audio(a) = grid(16, 2, 9_000, 3) else {
            panic!()
        };
        let mut a = (*a).clone();
        a.meta.loop_range = Some(LoopRange {
            start: 10,
            end: 9_000,
        });
        a.meta.tags.insert("title".into(), "曲".into());
        a.meta.tags.insert("vorbis:ENCODER".into(), "x".into());
        let v = Value::Audio(Arc::new(a));
        let bytes = encode(&v, json!({}));
        let Value::Audio(back) = decode(&bytes).unwrap() else {
            panic!()
        };
        assert_eq!(
            back.meta.loop_range,
            Some(LoopRange {
                start: 10,
                end: 9_000
            })
        );
        assert_eq!(back.meta.tags.get("title").map(String::as_str), Some("曲"));
        assert_eq!(
            back.meta.tags.get("vorbis:ENCODER").map(String::as_str),
            Some("x")
        );
        assert_eq!(encode(&Value::Audio(back), json!({})), bytes);
    }

    #[test]
    fn broken_files_are_errors() {
        use rand_chacha::rand_core::Rng;
        let good = encode(&grid(16, 2, 5_000, 9), json!({}));
        let mut r = crate::determinism::rng(12);
        for _ in 0..1_000 {
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
        assert!(decode(b"fLaC").is_err());
    }
}
