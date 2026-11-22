//! AudioSpecificConfig（ISO/IEC 14496-3 1.6.2.1）と ADTS の頭から、読める形（AAC-LC・1024 フレーム・配置 1〜7）を決める。

use super::{decode_err, unsupported};
use crate::error::EngineError;

/// サンプルレートの番号の表（0〜12）。
pub(crate) const RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// 読む形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Config {
    pub rate: u32,
    /// channel_configuration（1〜7）。
    pub layout: u8,
    pub channels: usize,
}

impl Config {
    /// 配置からチャンネルの数（7 は 7.1 の 8）。配置 0（PCE）・8 以上は `Unsupported`。
    pub fn new(rate: u32, layout: u8) -> Result<Self, EngineError> {
        let channels = match layout {
            1..=6 => usize::from(layout),
            7 => 8,
            _ => {
                return Err(unsupported(
                    "PCE で決める配置（channel_configuration 0）か知らない配置です",
                ));
            }
        };
        if rate == 0 {
            return Err(decode_err("サンプルレートが 0 です"));
        }
        Ok(Self {
            rate,
            layout,
            channels,
        })
    }
}

/// MSB から読む小さな読み手（ASC 用）。
struct Rd<'a> {
    b: &'a [u8],
    at: usize,
}

impl Rd<'_> {
    fn bits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.b.get(self.at / 8)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.at % 8))) & 1);
            self.at += 1;
        }
        Some(v)
    }
}

/// AudioSpecificConfig を読む。AAC-LC（object type 2）だけを読み、HE-AAC（5・29、同期の拡張の sbrPresentFlag）・ほかの
/// object type・960 フレーム（frameLengthFlag）・dependsOnCoreCoder・配置 0 は `Unsupported`（次の段へ）。
pub(crate) fn parse_asc(asc: &[u8]) -> Result<Config, EngineError> {
    let broken = || decode_err("AudioSpecificConfig が正しくありません");
    if crate::codec::symph::is_he_aac(asc) {
        return Err(unsupported("HE-AAC は次の段で読みます"));
    }
    let mut r = Rd { b: asc, at: 0 };
    let mut aot = r.bits(5).ok_or_else(broken)?;
    if aot == 31 {
        aot = 32 + r.bits(6).ok_or_else(broken)?;
    }
    let index = r.bits(4).ok_or_else(broken)?;
    let rate = if index == 15 {
        r.bits(24).ok_or_else(broken)?
    } else {
        *RATES.get(index as usize).ok_or_else(broken)?
    };
    let layout = r.bits(4).ok_or_else(broken)? as u8;
    if aot != 2 {
        return Err(unsupported(
            "AAC-LC でない AAC（Main・SSR・LTP・HE など）は次の段で読みます",
        ));
    }
    // GASpecificConfig
    let frame_length_flag = r.bits(1).ok_or_else(broken)?;
    let depends_on_core = r.bits(1).ok_or_else(broken)?;
    if frame_length_flag == 1 {
        return Err(unsupported("960 フレームの AAC は次の段で読みます"));
    }
    if depends_on_core == 1 {
        return Err(unsupported("コアの符号に依る AAC は次の段で読みます"));
    }
    Config::new(rate, layout)
}

/// ADTS の頭から読む形（profile は object type − 1）。
pub(crate) fn from_adts(profile: u32, rate_index: u32, layout: u32) -> Result<Config, EngineError> {
    if profile != 1 {
        return Err(unsupported("AAC-LC でない ADTS は次の段で読みます"));
    }
    let rate = *RATES
        .get(rate_index as usize)
        .ok_or_else(|| decode_err("ADTS のサンプルレートが正しくありません"))?;
    Config::new(rate, layout as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lc_configs_and_refusals() {
        // 44.1 kHz ステレオの LC（12 10）
        assert_eq!(
            parse_asc(&[0x12, 0x10]).unwrap(),
            Config::new(44_100, 2).unwrap()
        );
        // 48 kHz 5.1（11 b0）
        assert_eq!(parse_asc(&[0x11, 0xb0]).unwrap().channels, 6);
        // HE-AAC（object type 5）
        assert!(matches!(
            parse_asc(&[0x2b, 0x92, 0x08, 0x00]),
            Err(EngineError::Unsupported(_))
        ));
        // 配置 0
        assert!(matches!(
            parse_asc(&[0x12, 0x00]),
            Err(EngineError::Unsupported(_))
        ));
        // 960 フレーム
        assert!(matches!(
            parse_asc(&[0x12, 0x14]),
            Err(EngineError::Unsupported(_))
        ));
        assert!(matches!(
            parse_asc(&[0x12]),
            Err(EngineError::Decode { .. })
        ));
    }
}
