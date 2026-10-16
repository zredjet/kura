//! 重ね合わせ（ストレートのアルファの source-over）。APNG・アニメ WebP などのフレームの合成に使う。
//!
//! 式（凍結する）：チャンネルの最大を M（8 ビットは 255、16 ビットは 65535）とし、整数で計算する。
//! - `sa = M` なら上の画素、`sa = 0` なら下の画素のまま。
//! - そうでなければ `da' = ⌊(da·(M − sa) + ⌊M/2⌋) / M⌋`、`oa = sa + da'`、
//!   各色 `oc = ⌊(sc·sa + dc·da' + ⌊oa/2⌋) / oa⌋`（`oa = 0` なら全部 0）。

use crate::error::EngineError;
use crate::value::PixelFormat;

/// `src` の 1 画素を `dst` の 1 画素の上に重ねる（どちらも `fmt` の画素のバイト列）。アルファのない形式は上書き。
pub fn over(fmt: PixelFormat, dst: &mut [u8], src: &[u8]) -> Result<(), EngineError> {
    let bpp = fmt.bytes_per_pixel();
    if dst.len() != bpp || src.len() != bpp {
        return Err(EngineError::Internal(
            "重ねる画素の大きさが合いません".into(),
        ));
    }
    match fmt {
        PixelFormat::GrayA8 | PixelFormat::Rgba8 => {
            let n = bpp - 1;
            let mut s = [0u64; 4];
            let mut d = [0u64; 4];
            for i in 0..bpp {
                s[i] = u64::from(src[i]);
                d[i] = u64::from(dst[i]);
            }
            let out = over_channels(&s[..bpp], &d[..bpp], n, 255);
            for i in 0..bpp {
                dst[i] = out[i] as u8;
            }
        }
        PixelFormat::GrayA16 | PixelFormat::Rgba16 => {
            let ch = bpp / 2;
            let n = ch - 1;
            let mut s = [0u64; 4];
            let mut d = [0u64; 4];
            for i in 0..ch {
                s[i] = u64::from(u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]));
                d[i] = u64::from(u16::from_le_bytes([dst[i * 2], dst[i * 2 + 1]]));
            }
            let out = over_channels(&s[..ch], &d[..ch], n, 65_535);
            for i in 0..ch {
                dst[i * 2..i * 2 + 2].copy_from_slice(&(out[i] as u16).to_le_bytes());
            }
        }
        _ => dst.copy_from_slice(src),
    }
    Ok(())
}

/// 色 `n` 個＋アルファの並び（アルファは最後）を重ねる。
fn over_channels(s: &[u64], d: &[u64], n: usize, max: u64) -> [u64; 4] {
    let mut out = [0u64; 4];
    let (sa, da) = (s[n], d[n]);
    if sa == max {
        out[..=n].copy_from_slice(s);
        return out;
    }
    if sa == 0 {
        out[..=n].copy_from_slice(d);
        return out;
    }
    let da2 = (da * (max - sa) + max / 2) / max;
    let oa = sa + da2;
    if oa == 0 {
        return out;
    }
    for i in 0..n {
        out[i] = (s[i] * sa + d[i] * da2 + oa / 2) / oa;
    }
    out[n] = oa;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_8_bit_cases() {
        let mut d = [10, 20, 30, 255];
        over(PixelFormat::Rgba8, &mut d, &[200, 100, 0, 0]).unwrap();
        assert_eq!(d, [10, 20, 30, 255]);
        over(PixelFormat::Rgba8, &mut d, &[200, 100, 0, 255]).unwrap();
        assert_eq!(d, [200, 100, 0, 255]);
        // 半透明を不透明の上に：アルファは 255 のまま、色はおよそ中間
        let mut d = [0, 0, 0, 255];
        over(PixelFormat::Rgba8, &mut d, &[255, 255, 255, 128]).unwrap();
        assert_eq!(d, [128, 128, 128, 255]);
        // 透明の上に重ねると上の画素そのもの
        let mut d = [9, 9, 9, 0];
        over(PixelFormat::Rgba8, &mut d, &[100, 50, 25, 77]).unwrap();
        assert_eq!(d, [100, 50, 25, 77]);
        // 半透明どうし（式どおりの値を凍結）
        let mut d = [0, 0, 255, 128];
        over(PixelFormat::Rgba8, &mut d, &[255, 0, 0, 128]).unwrap();
        // da' = (128·127 + 127) / 255 = 64、oa = 192、r = (255·128 + 96) / 192 = 170、b = (255·64 + 96) / 192 = 85
        assert_eq!(d, [170, 0, 85, 192]);
        let mut g = [50, 100];
        over(PixelFormat::GrayA8, &mut g, &[250, 255]).unwrap();
        assert_eq!(g, [250, 255]);
        let mut rgb = [1, 2, 3];
        over(PixelFormat::Rgb8, &mut rgb, &[4, 5, 6]).unwrap();
        assert_eq!(rgb, [4, 5, 6]);
    }

    #[test]
    fn over_16_bit_matches_the_formula() {
        let enc = |v: [u16; 4]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
        let mut d = enc([0, 0, 65_535, 32_768]);
        over(PixelFormat::Rgba16, &mut d, &enc([65_535, 0, 0, 32_768])).unwrap();
        let da2 = (32_768u64 * 32_767 + 32_767) / 65_535;
        let oa = 32_768 + da2;
        let r = (65_535 * 32_768 + oa / 2) / oa;
        let b = (65_535 * da2 + oa / 2) / oa;
        assert_eq!(d, enc([r as u16, 0, b as u16, oa as u16]));
    }
}
