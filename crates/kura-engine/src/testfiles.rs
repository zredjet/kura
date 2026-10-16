//! テストとゴールデンのためのファイルを、下の層で組む（docs/editing.md §8.5）。
//!
//! 自前のエンコーダが書かない形（1/2/4 ビット、tRNS、インターレース、64 ビットの浮動小数、拡張形式、他のツールのメタデータ、
//! 大きさの分からない data など）を、記述から決定的に作り、デコーダだけの経路をゴールデンで凍結できるようにする。
//! PNG は無圧縮の deflate（stored ブロック）とフィルタなしの行で組むので、圧縮器に頼らない。

use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};

use crate::determinism;
use crate::error::EngineError;

/// ファイルの作り方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
pub enum FileGen {
    /// PNG。`color` は PNG の色の種類（0 グレー・2 RGB・3 パレット・4 グレー＋アルファ・6 RGBA）、`depth` はビット深度。
    Png {
        w: u32,
        h: u32,
        color: u8,
        depth: u8,
        interlace: bool,
        /// パレットの色の数（color 3 のとき）。
        palette_len: u16,
        trns: Option<Vec<u8>>,
        text: Vec<(String, String)>,
        icc: Option<Vec<u8>>,
        /// pHYs（x・y・単位）。
        phys: Option<(u32, u32, u8)>,
        seed: u64,
    },
    /// WAV。`tag` は形式（1 PCM・3 浮動小数）。
    Wav {
        tag: u16,
        bits: u16,
        channels: u16,
        rate: u32,
        frames: u32,
        extensible: bool,
        /// smpl のループ（始め・終わり。終わりは含む）。
        smpl: Option<(u32, u32)>,
        /// cue（ID・位置）。
        cues: Vec<(u32, u32)>,
        /// adtl の labl（ID・名前）。
        labels: Vec<(u32, String)>,
        /// INFO（4 文字の ID・文字列）。
        info: Vec<(String, String)>,
        /// data の大きさを 0xFFFFFFFF（分からない）と書く（data を最後に置く）。
        unknown_size: bool,
        seed: u64,
    },
}

impl FileGen {
    /// 形式の ID。
    pub fn format(&self) -> &'static str {
        match self {
            Self::Png { .. } => "png",
            Self::Wav { .. } => "wav",
        }
    }

    /// バイト列を作る。
    pub fn bytes(&self) -> Result<Vec<u8>, EngineError> {
        match self {
            Self::Png {
                w,
                h,
                color,
                depth,
                interlace,
                palette_len,
                trns,
                text,
                icc,
                phys,
                seed,
            } => png_bytes(
                *w,
                *h,
                *color,
                *depth,
                *interlace,
                *palette_len,
                trns.as_deref(),
                text,
                icc.as_deref(),
                *phys,
                *seed,
            ),
            Self::Wav {
                tag,
                bits,
                channels,
                rate,
                frames,
                extensible,
                smpl,
                cues,
                labels,
                info,
                unknown_size,
                seed,
            } => Ok(wav_bytes(
                *tag,
                *bits,
                *channels,
                *rate,
                *frames,
                *extensible,
                *smpl,
                cues,
                labels,
                info,
                *unknown_size,
                *seed,
            )),
        }
    }
}

/// CRC-32（PNG のチャンク）。
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32（zlib）。
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for x in bytes {
        a = (a + u32::from(*x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// 無圧縮の deflate（stored ブロック）で包んだ zlib のバイト列。
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(u8::from(last));
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Adam7 の各段の（x の始め・y の始め・x の間隔・y の間隔）。
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

#[allow(clippy::too_many_arguments)]
fn png_bytes(
    w: u32,
    h: u32,
    color: u8,
    depth: u8,
    interlace: bool,
    palette_len: u16,
    trns: Option<&[u8]>,
    text: &[(String, String)],
    icc: Option<&[u8]>,
    phys: Option<(u32, u32, u8)>,
    seed: u64,
) -> Result<Vec<u8>, EngineError> {
    let channels: u32 = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => {
            return Err(EngineError::Internal(format!(
                "PNG の色の種類 {color} は作れません"
            )));
        }
    };
    if w == 0
        || h == 0
        || !matches!(depth, 1 | 2 | 4 | 8 | 16)
        || (color == 3 && (palette_len == 0 || depth == 16))
    {
        return Err(EngineError::Internal(
            "PNG の大きさ・深度が正しくありません".into(),
        ));
    }
    let mask: u32 = if depth == 16 {
        0xFFFF
    } else {
        (1 << depth) - 1
    };
    // 画素 (x, y) のチャンネル c の値（位置から決まるので、インターレースでも同じ値になる）
    let sample = |x: u32, y: u32, c: u32| -> u32 {
        let mut r = determinism::rng_at(seed, u64::from(y), u128::from(x * channels + c));
        let v = r.next_u32();
        if color == 3 {
            v % u32::from(palette_len)
        } else {
            v & mask
        }
    };
    let pack_row = |xs: &[u32], y: u32| -> Vec<u8> {
        let mut row = vec![0u8]; // フィルタなし
        let mut acc = 0u32;
        let mut nbits = 0u32;
        for &x in xs {
            for c in 0..channels {
                let v = sample(x, y, c);
                if depth == 16 {
                    row.extend_from_slice(&(v as u16).to_be_bytes());
                } else if depth == 8 {
                    row.push(v as u8);
                } else {
                    acc = (acc << depth) | v;
                    nbits += u32::from(depth);
                    if nbits == 8 {
                        row.push(acc as u8);
                        acc = 0;
                        nbits = 0;
                    }
                }
            }
        }
        if nbits > 0 {
            row.push((acc << (8 - nbits)) as u8);
        }
        row
    };
    let mut raw = Vec::new();
    if interlace {
        for (x0, y0, dx, dy) in ADAM7 {
            let xs: Vec<u32> = (x0..w).step_by(dx as usize).collect();
            if xs.is_empty() {
                continue;
            }
            for y in (y0..h).step_by(dy as usize) {
                raw.extend(pack_row(&xs, y));
            }
        }
    } else {
        let xs: Vec<u32> = (0..w).collect();
        for y in 0..h {
            raw.extend(pack_row(&xs, y));
        }
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, u8::from(interlace)]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    if let Some(icc) = icc {
        let mut d = b"icc\0\0".to_vec();
        d.extend(zlib_stored(icc));
        png_chunk(&mut out, b"iCCP", &d);
    }
    if let Some((x, y, unit)) = phys {
        let mut d = x.to_be_bytes().to_vec();
        d.extend_from_slice(&y.to_be_bytes());
        d.push(unit);
        png_chunk(&mut out, b"pHYs", &d);
    }
    if color == 3 {
        let mut r = determinism::rng_stream(seed, u64::MAX);
        let pal: Vec<u8> = (0..u32::from(palette_len) * 3)
            .map(|_| r.next_u32() as u8)
            .collect();
        png_chunk(&mut out, b"PLTE", &pal);
    }
    if let Some(t) = trns {
        png_chunk(&mut out, b"tRNS", t);
    }
    for (k, v) in text {
        let mut d = k.as_bytes().to_vec();
        d.push(0);
        d.extend_from_slice(v.as_bytes());
        png_chunk(&mut out, b"tEXt", &d);
    }
    png_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn wav_bytes(
    tag: u16,
    bits: u16,
    channels: u16,
    rate: u32,
    frames: u32,
    extensible: bool,
    smpl: Option<(u32, u32)>,
    cues: &[(u32, u32)],
    labels: &[(u32, String)],
    info: &[(String, String)],
    unknown_size: bool,
    seed: u64,
) -> Vec<u8> {
    let sample_bytes = u32::from(bits / 8);
    let block = u32::from(channels) * sample_bytes;
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if extensible { 0xFFFEu16 } else { tag }).to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&rate.wrapping_mul(block).to_le_bytes());
    fmt.extend_from_slice(&(block as u16).to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes());
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
        ]);
    }
    let mut data = Vec::with_capacity((frames * block) as usize);
    for f in 0..frames {
        for c in 0..channels {
            let mut r = determinism::rng_at(seed, u64::from(c), u128::from(f) * 2);
            let raw = r.next_u64();
            match (tag, bits) {
                (3, 32) => data
                    .extend_from_slice(&(((raw >> 40) as f32 / 8_388_608.0) - 1.0).to_le_bytes()),
                (3, 64) => data.extend_from_slice(
                    &(((raw >> 11) as f64 / 4_503_599_627_370_496.0) - 1.0).to_le_bytes(),
                ),
                _ => data.extend_from_slice(&raw.to_le_bytes()[..sample_bytes as usize]),
            }
        }
    }
    let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"fmt ", fmt)];
    if let Some((start, end)) = smpl {
        let mut b = Vec::new();
        for v in [0u32, 0, 22_675, 60, 0, 0, 0, 1, 0, 0, 0, start, end, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        chunks.push((b"smpl", b));
    }
    if !cues.is_empty() {
        let mut b = (cues.len() as u32).to_le_bytes().to_vec();
        for (id, pos) in cues {
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&pos.to_le_bytes());
            b.extend_from_slice(b"data");
            for v in [0u32, 0, *pos] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        chunks.push((b"cue ", b));
    }
    let sub = |list: &mut Vec<u8>, id: &[u8], body: &[u8]| {
        list.extend_from_slice(id);
        list.extend_from_slice(&(body.len() as u32).to_le_bytes());
        list.extend_from_slice(body);
        if body.len() % 2 == 1 {
            list.push(0);
        }
    };
    if !labels.is_empty() {
        let mut b = b"adtl".to_vec();
        for (id, name) in labels {
            let mut body = id.to_le_bytes().to_vec();
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            sub(&mut b, b"labl", &body);
        }
        chunks.push((b"LIST", b));
    }
    if !info.is_empty() {
        let mut b = b"INFO".to_vec();
        for (id, text) in info {
            let mut body = text.as_bytes().to_vec();
            body.push(0);
            let id4: Vec<u8> = id.bytes().chain(std::iter::repeat(b' ')).take(4).collect();
            sub(&mut b, &id4, &body);
        }
        chunks.push((b"LIST", b));
    }
    let mut body = b"WAVE".to_vec();
    for (id, b) in &chunks {
        sub(&mut body, *id, b);
    }
    body.extend_from_slice(b"data");
    body.extend_from_slice(
        &(if unknown_size {
            u32::MAX
        } else {
            data.len() as u32
        })
        .to_le_bytes(),
    );
    body.extend_from_slice(&data);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let z = zlib_stored(&[1, 2, 3]);
        assert_eq!(&z[..2], &[0x78, 0x01]);
    }
}
