//! 整数・浮動小数の PCM と f32 の値のあいだの変換（WAV・AIFF・FLAC で同じ式。docs/editing.md §5.3）。
//!
//! - 読む：符号付き整数の標本 v（入れ物の幅 8・16・24・32 ビット）は v / 2^(幅−1)。32 ビットは f64 で割ってから
//!   f32 に丸める（8・16・24 ビットは f32 で正確に表せるので、どの式でも同じ値）。左詰めの 32 ビットの整数（FLAC・ALAC の
//!   デコーダの出力）も 32 ビットの式で読むと、元の幅の式と同じ値になる。
//! - 書く：round_ties_even(s · 2^(幅−1)) を範囲で止める（NaN は 0。ディザはしない。ディザは出力の量子化 §4.1）。
//!   8・16・24 ビットを読んで同じ幅で書くと、ビット単位で元に戻る（E17）。

use crate::error::EngineError;
use crate::value::AudioSource;

/// 符号付き整数の標本（入れ物の幅 `bits`：8・16・24・32）を −1〜1 の f32 にする。
pub fn int_to_f32(v: i32, bits: u16) -> f32 {
    match bits {
        8 => v as f32 / 128.0,
        16 => v as f32 / 32_768.0,
        24 => v as f32 / 8_388_608.0,
        _ => (f64::from(v) / 2_147_483_648.0) as f32,
    }
}

/// −1〜1 の f32 を `bits` ビットの整数の標本にする（偶数への丸め、範囲で止める。NaN は 0）。
pub fn quantize(s: f32, bits: u16) -> i64 {
    let scale = (1i64 << (bits - 1)) as f64;
    let v = (f64::from(s) * scale).round_ties_even();
    let v = if v.is_nan() { 0.0 } else { v };
    (v as i64).clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
}

/// 書き出しの形式 (ビット数, 浮動小数か)。`float` の指定 > `bits` の指定 > 元の形式 > 16 ビットの整数（docs/editing.md §5.3）。
/// 浮動小数は 32 ビットだけ。整数は `ints` のビット数だけ。`name` はエラーの文言に使う形式の名前。
/// 元の形式から決めるとき、浮動小数を書けない形式（FLAC）では、浮動小数の元は整数の最大の幅に、書けない幅の元
/// （32 ビットの整数）はそれ以下の最大の幅にする。浮動小数を書ける形式（WAV・AIFF）で `float: false` を指定し、
/// 元が浮動小数なら 16 ビット（WAV の段 2 からの決め方）。指定した幅が書けなければ `Unsupported`。
pub fn target(
    opts: &serde_json::Value,
    source: Option<AudioSource>,
    ints: &[u16],
    allow_float: bool,
    name: &str,
) -> Result<(u16, bool), EngineError> {
    let bits_opt = opts.get("bits").and_then(serde_json::Value::as_u64);
    let float = match (
        opts.get("float").and_then(serde_json::Value::as_bool),
        bits_opt,
    ) {
        (Some(f), _) => f,
        (None, Some(_)) => false,
        (None, None) => allow_float && source.is_some_and(|s| s.float),
    };
    if float {
        if !allow_float {
            return Err(EngineError::Unsupported(format!(
                "浮動小数の {name} は書けません"
            )));
        }
        if bits_opt.is_some_and(|b| b != 32) {
            return Err(EngineError::Unsupported(format!(
                "浮動小数の {name} は 32 ビットだけを書けます"
            )));
        }
        return Ok((32, true));
    }
    if let Some(b) = bits_opt {
        return match u16::try_from(b) {
            Ok(b) if ints.contains(&b) => Ok((b, false)),
            _ => Err(EngineError::Unsupported(format!(
                "{b} ビットの {name} は書けません"
            ))),
        };
    }
    let widest = ints.iter().copied().max().unwrap_or(16);
    let from_source = source.and_then(|s| {
        if s.float {
            // 浮動小数を書けない形式だけ、整数の最大の幅に（書ける形式で float: false なら 16）
            (!allow_float).then_some(widest)
        } else {
            ints.iter().copied().filter(|i| *i <= s.bits).max()
        }
    });
    Ok((from_source.unwrap_or(16), false))
}

/// 入れ物の幅（8・16・24・32）に切り上げたビット数（12 ビットなどの左詰めの標本を、入れ物の幅として扱う）。
pub fn container_bits(bits: u16) -> u16 {
    match bits {
        0..=8 => 8,
        9..=16 => 16,
        17..=24 => 24,
        _ => 32,
    }
}

/// 標本の符号化（WAV・AIFF の data・SSND の 1 標本。M13 段 1：デコーダと音声の源が同じ変換を通る）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEncoding {
    /// 符号なしの 8 ビット（WAV の 8 ビット・AIFF-C の `raw `）。
    U8,
    /// 符号付きの整数（リトルエンディアン。幅 `bytes` バイト = 入れ物の幅）。
    IntLe {
        bytes: u16,
    },
    /// 符号付きの整数（ビッグエンディアン）。
    IntBe {
        bytes: u16,
    },
    F32Le,
    F32Be,
    F64Le,
    F64Be,
}

impl SampleEncoding {
    /// 1 標本のバイト数。
    pub fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::IntLe { bytes } | Self::IntBe { bytes } => usize::from(bytes),
            Self::F32Le | Self::F32Be => 4,
            Self::F64Le | Self::F64Be => 8,
        }
    }

    /// 1 標本（`bytes()` バイト）を −1〜1 の f32 にする（式は [`int_to_f32`]。浮動小数はそのまま、64 ビットは f32 に丸める）。
    pub fn to_f32(self, s: &[u8]) -> f32 {
        match self {
            Self::U8 => int_to_f32(i32::from(s[0]) - 128, 8),
            Self::IntLe { bytes: 1 } | Self::IntBe { bytes: 1 } => {
                int_to_f32(i32::from(s[0] as i8), 8)
            }
            Self::IntLe { bytes: 2 } => int_to_f32(i32::from(i16::from_le_bytes([s[0], s[1]])), 16),
            Self::IntBe { bytes: 2 } => int_to_f32(i32::from(i16::from_be_bytes([s[0], s[1]])), 16),
            Self::IntLe { bytes: 3 } => {
                int_to_f32(i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8, 24)
            }
            Self::IntBe { bytes: 3 } => {
                int_to_f32(i32::from_be_bytes([s[0], s[1], s[2], 0]) >> 8, 24)
            }
            Self::IntLe { .. } => int_to_f32(i32::from_le_bytes([s[0], s[1], s[2], s[3]]), 32),
            Self::IntBe { .. } => int_to_f32(i32::from_be_bytes([s[0], s[1], s[2], s[3]]), 32),
            Self::F32Le => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            Self::F32Be => f32::from_be_bytes([s[0], s[1], s[2], s[3]]),
            Self::F64Le => {
                f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
            }
            Self::F64Be => {
                f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32
            }
        }
    }
}

/// インターリーブした PCM の配置（ファイルの中の標本の始まり・フレーム数・チャンネル数・符号化）。WAV・AIFF のデコーダと
/// 音声の源（`audio::source::PcmFileSource`）が同じ変換（[`PcmLayout::decode_frames`]）を通る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmLayout {
    /// 最初のフレームのファイルの中の位置（バイト）。
    pub data_offset: u64,
    /// フレーム数。
    pub frames: u64,
    /// チャンネル数（1 以上）。
    pub channels: u16,
    pub encoding: SampleEncoding,
}

impl PcmLayout {
    /// 1 フレームのバイト数。
    pub fn block(&self) -> usize {
        usize::from(self.channels) * self.encoding.bytes()
    }

    /// インターリーブしたフレームのバイト列 `bytes`（フレームのバイト数の倍数）を、チャンネルごとの列 `out`（チャンネルの
    /// 数だけ）にする（`out` は空にしてから詰める）。
    pub fn decode_frames(&self, bytes: &[u8], out: &mut [Vec<f32>]) -> Result<(), EngineError> {
        let block = self.block();
        if out.len() != usize::from(self.channels)
            || block == 0
            || !bytes.len().is_multiple_of(block)
        {
            return Err(EngineError::Internal(
                "PCM の配置とバイト列・チャンネルの数が合いません".into(),
            ));
        }
        let sample = self.encoding.bytes();
        let n = bytes.len() / block;
        for (c, plane) in out.iter_mut().enumerate() {
            plane.clear();
            plane
                .try_reserve(n)
                .map_err(|_| EngineError::Limit("音声の作業領域を確保できません".into()))?;
            let off = c * sample;
            plane.extend(
                bytes
                    .chunks_exact(block)
                    .map(|f| self.encoding.to_f32(&f[off..off + sample])),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 8・16・24 ビットのすべての値で、読んで書くと元に戻る。
    #[test]
    fn integer_round_trips_are_exact() {
        for bits in [8u16, 16, 24] {
            let half = 1i32 << (bits - 1);
            let step = if bits == 24 { 7 } else { 1 };
            let mut v = -half;
            while v < half {
                assert_eq!(
                    quantize(int_to_f32(v, bits), bits),
                    i64::from(v),
                    "{bits} {v}"
                );
                v += step;
            }
        }
        // 左詰めの 32 ビットの式は、元の幅の式と同じ値
        for v in [-32_768i32, -1, 0, 1, 12_345, 32_767] {
            assert_eq!(int_to_f32(v << 16, 32), int_to_f32(v, 16));
        }
    }

    #[test]
    fn target_follows_the_documented_order() {
        let int24 = Some(AudioSource {
            bits: 24,
            float: false,
            rate: 48_000,
        });
        let float = Some(AudioSource {
            bits: 32,
            float: true,
            rate: 48_000,
        });
        let all = [8u16, 16, 24, 32];
        assert_eq!(
            target(&json!({}), int24, &all, true, "WAV").unwrap(),
            (24, false)
        );
        assert_eq!(
            target(&json!({}), float, &all, true, "WAV").unwrap(),
            (32, true)
        );
        assert_eq!(
            target(&json!({ "bits": 16 }), float, &all, true, "WAV").unwrap(),
            (16, false)
        );
        assert_eq!(
            target(&json!({}), None, &all, true, "WAV").unwrap(),
            (16, false)
        );
        assert!(
            target(
                &json!({ "float": true, "bits": 16 }),
                None,
                &all,
                true,
                "WAV"
            )
            .is_err()
        );
        // 浮動小数を書ける形式で float: false を指定し、元が浮動小数なら 16（WAV の段 2 からの決め方。検証で見つけた）
        assert_eq!(
            target(&json!({ "float": false }), float, &all, true, "WAV").unwrap(),
            (16, false)
        );
        // 書けない幅の元（32 ビットの整数の FLAC）は、それ以下の最大の幅。指定した幅が書けなければ断る
        let int32 = Some(AudioSource {
            bits: 32,
            float: false,
            rate: 48_000,
        });
        assert_eq!(
            target(&json!({}), int32, &[8, 16, 24], false, "FLAC").unwrap(),
            (24, false)
        );
        assert!(target(&json!({ "bits": 32 }), int32, &[8, 16, 24], false, "FLAC").is_err());
        // 浮動小数を書けない形式は、元が浮動小数なら整数の最大の幅
        assert_eq!(
            target(&json!({}), float, &[8, 16, 24], false, "FLAC").unwrap(),
            (24, false)
        );
        assert!(target(&json!({ "float": true }), None, &[16], false, "FLAC").is_err());
    }
}
