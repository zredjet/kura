//! 画素の色の扱い（段 5 の操作で共有する）。どれも整数だけで計算し、式を凍結する（docs/editing.md §5.2）。
//!
//! - 色の引数はストレートの RGBA8。16 ビットの形式へは 257 倍して広げる（`up8`）。
//! - 深さを下げるときは四捨五入：`⌊(2v + 257) / 514⌋`（v / 257 にいちばん近い整数。ちょうど半分は起きない。`down16`）。
//! - グレーにするときは Rec. 709 の係数の整数の式：`⌊(2126·R + 7152·G + 722·B + 5000) / 10000⌋`
//!   （符号化された値のまま。線形光にはしない。`luma`）。
//! - アルファを外すときは下地の色 m に重ねる：`⌊(c·a + m·(M − a) + ⌊M/2⌋) / M⌋`（M はチャンネルの最大。`flatten`）。

use crate::error::EngineError;
use crate::value::{Palette, PixelFormat};

/// チャンネルの最大（16 ビットの形式は 65535、ほかは 255。Indexed8 はパレットの色なので 255）。
pub fn channel_max(fmt: PixelFormat) -> u32 {
    if is_16(fmt) { 65_535 } else { 255 }
}

/// 16 ビットの形式か。
pub fn is_16(fmt: PixelFormat) -> bool {
    matches!(
        fmt,
        PixelFormat::Gray16 | PixelFormat::GrayA16 | PixelFormat::Rgb16 | PixelFormat::Rgba16
    )
}

/// グレーの形式か。
pub fn is_gray(fmt: PixelFormat) -> bool {
    matches!(
        fmt,
        PixelFormat::Gray8 | PixelFormat::GrayA8 | PixelFormat::Gray16 | PixelFormat::GrayA16
    )
}

/// アルファ付きの形式（アルファのない形式はアルファを足したもの。アルファ付きと Indexed8 はそのまま）。
pub fn with_alpha(fmt: PixelFormat) -> PixelFormat {
    match fmt {
        PixelFormat::Gray8 => PixelFormat::GrayA8,
        PixelFormat::Rgb8 => PixelFormat::Rgba8,
        PixelFormat::Gray16 => PixelFormat::GrayA16,
        PixelFormat::Rgb16 => PixelFormat::Rgba16,
        other => other,
    }
}

/// 8 ビットの値を 16 ビットへ（257 倍）。
pub fn up8(v: u32) -> u32 {
    v * 257
}

/// 16 ビットの値を 8 ビットへ（四捨五入）。
pub fn down16(v: u32) -> u32 {
    (2 * v + 257) / 514
}

/// 符号化された RGB からグレーの値（Rec. 709 の係数の整数の式）。
pub fn luma(r: u32, g: u32, b: u32) -> u32 {
    let y = (2126 * u64::from(r) + 7152 * u64::from(g) + 722 * u64::from(b) + 5000) / 10_000;
    y as u32
}

/// アルファ a の色 c を下地の色 m に重ねる（M はチャンネルの最大）。
pub fn flatten(c: u32, a: u32, m: u32, max: u32) -> u32 {
    let (c, a, m, max) = (u64::from(c), u64::from(a), u64::from(m), u64::from(max));
    ((c * a + m * (max - a) + max / 2) / max) as u32
}

fn bad(msg: &str) -> EngineError {
    EngineError::Internal(msg.into())
}

fn u16_at(px: &[u8], i: usize) -> u32 {
    u32::from(u16::from_le_bytes([px[i * 2], px[i * 2 + 1]]))
}

/// 1 画素を、その形式の深さのまま (r, g, b, a) にする。グレーは r = g = b、アルファのない形式のアルファは最大。
/// Indexed8 はパレットの色（8 ビット）。
pub fn read_rgba(
    fmt: PixelFormat,
    palette: Option<&[[u8; 4]]>,
    px: &[u8],
) -> Result<[u32; 4], EngineError> {
    if px.len() != fmt.bytes_per_pixel() {
        return Err(bad("画素の大きさが形式と合いません"));
    }
    let max = channel_max(fmt);
    Ok(match fmt {
        PixelFormat::Gray8 => {
            let y = u32::from(px[0]);
            [y, y, y, max]
        }
        PixelFormat::GrayA8 => {
            let y = u32::from(px[0]);
            [y, y, y, u32::from(px[1])]
        }
        PixelFormat::Rgb8 => [u32::from(px[0]), u32::from(px[1]), u32::from(px[2]), max],
        PixelFormat::Rgba8 => [
            u32::from(px[0]),
            u32::from(px[1]),
            u32::from(px[2]),
            u32::from(px[3]),
        ],
        PixelFormat::Gray16 => {
            let y = u16_at(px, 0);
            [y, y, y, max]
        }
        PixelFormat::GrayA16 => {
            let y = u16_at(px, 0);
            [y, y, y, u16_at(px, 1)]
        }
        PixelFormat::Rgb16 => [u16_at(px, 0), u16_at(px, 1), u16_at(px, 2), max],
        PixelFormat::Rgba16 => [u16_at(px, 0), u16_at(px, 1), u16_at(px, 2), u16_at(px, 3)],
        PixelFormat::Indexed8 => {
            let c = palette
                .and_then(|p| p.get(usize::from(px[0])))
                .ok_or_else(|| bad("パレットにない番号の画素があります"))?;
            [
                u32::from(c[0]),
                u32::from(c[1]),
                u32::from(c[2]),
                u32::from(c[3]),
            ]
        }
    })
}

/// (r, g, b, a)（その形式の深さの値）を 1 画素に書く。グレーは r を、アルファのない形式はアルファを捨てる。
/// Indexed8 は書けない（番号はパレットで決める）。
pub fn write_rgba(fmt: PixelFormat, c: [u32; 4], out: &mut [u8]) -> Result<(), EngineError> {
    if out.len() != fmt.bytes_per_pixel() {
        return Err(bad("画素の大きさが形式と合いません"));
    }
    let channels: &[u32] = match fmt {
        PixelFormat::Gray8 | PixelFormat::Gray16 => &c[..1],
        PixelFormat::GrayA8 | PixelFormat::GrayA16 => &[c[0], c[3]],
        PixelFormat::Rgb8 | PixelFormat::Rgb16 => &c[..3],
        PixelFormat::Rgba8 | PixelFormat::Rgba16 => &c,
        PixelFormat::Indexed8 => return Err(bad("Indexed8 には色を直接書けません")),
    };
    if is_16(fmt) {
        for (i, v) in channels.iter().enumerate() {
            let v = u16::try_from(*v).map_err(|_| bad("16 ビットの値が範囲の外です"))?;
            out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
    } else {
        for (i, v) in channels.iter().enumerate() {
            out[i] = u8::try_from(*v).map_err(|_| bad("8 ビットの値が範囲の外です"))?;
        }
    }
    Ok(())
}

/// 色の引数（ストレートの RGBA8）を、その形式の 1 画素にする（Indexed8 は扱わない）。
/// 黙って色を変えないように、グレーの形式は灰色（r = g = b）だけ、アルファのない形式は不透明な色だけを受け付ける。
pub fn color_bytes(fmt: PixelFormat, c: [u8; 4]) -> Result<Vec<u8>, String> {
    if fmt == PixelFormat::Indexed8 {
        return Err("パレットの画像には番号で色を指定します".into());
    }
    if is_gray(fmt) && !(c[0] == c[1] && c[1] == c[2]) {
        return Err("グレーの画像には灰色（R = G = B）を指定します".into());
    }
    if !fmt.has_alpha() && c[3] != 255 {
        return Err("アルファのない画像には不透明な色を指定します".into());
    }
    let wide = |v: u8| {
        if is_16(fmt) {
            up8(u32::from(v))
        } else {
            u32::from(v)
        }
    };
    let mut out = vec![0u8; fmt.bytes_per_pixel()];
    write_rgba(
        fmt,
        [wide(c[0]), wide(c[1]), wide(c[2]), wide(c[3])],
        &mut out,
    )
    .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 画素のアルファが、8 ビットのしきい値以下か（16 ビットはしきい値の 257 倍と比べる。アルファのない形式は常に false）。
pub fn alpha_at_most(
    fmt: PixelFormat,
    palette: Option<&[[u8; 4]]>,
    px: &[u8],
    threshold: u8,
) -> bool {
    let t = u32::from(threshold);
    match fmt {
        PixelFormat::GrayA8 => u32::from(px[1]) <= t,
        PixelFormat::Rgba8 => u32::from(px[3]) <= t,
        PixelFormat::GrayA16 => u16_at(px, 1) <= up8(t),
        PixelFormat::Rgba16 => u16_at(px, 3) <= up8(t),
        PixelFormat::Indexed8 => palette
            .and_then(|p| p.get(usize::from(px[0])))
            .is_some_and(|c| u32::from(c[3]) <= t),
        _ => false,
    }
}

/// 形式が透明を表せるか（アルファ付きか Indexed8）。
pub fn can_be_transparent(fmt: PixelFormat) -> bool {
    fmt.has_alpha() || fmt == PixelFormat::Indexed8
}

/// パレットの中で色 `c` と同じ最初の番号。なければ後ろに足す。256 色でいっぱいなら None。
pub fn palette_index(p: &Palette, c: [u8; 4]) -> Option<(Palette, u8)> {
    if let Some(i) = p.colors.iter().position(|x| *x == c) {
        return Some((p.clone(), i as u8));
    }
    if p.colors.len() >= 256 {
        return None;
    }
    let mut out = p.clone();
    out.colors.push(c);
    let i = (out.colors.len() - 1) as u8;
    Some((out, i))
}

/// 透明な色（アルファ 0）の最初の番号。
pub fn palette_clear_index(p: &Palette) -> Option<u8> {
    p.colors.iter().position(|c| c[3] == 0).map(|i| i as u8)
}

/// 透明な色の番号。なければ [0, 0, 0, 0] を後ろに足す。256 色でいっぱいなら None。
pub fn palette_clear(p: &Palette) -> Option<(Palette, u8)> {
    match palette_clear_index(p) {
        Some(i) => Some((p.clone(), i)),
        None => palette_index(p, [0, 0, 0, 0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_round_trips_and_rounds() {
        for v in 0..=255u32 {
            assert_eq!(down16(up8(v)), v);
        }
        assert_eq!(down16(128), 0);
        assert_eq!(down16(129), 1);
        assert_eq!(down16(65_535), 255);
        // v / 257 にいちばん近い整数
        for v in (0..=65_535u32).step_by(97) {
            let want = (f64::from(v) / 257.0).round() as u32;
            assert_eq!(down16(v), want, "{v}");
        }
    }

    #[test]
    fn luma_and_flatten_formulas() {
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(65_535, 65_535, 65_535), 65_535);
        assert_eq!(luma(255, 0, 0), 54);
        assert_eq!(luma(0, 255, 0), 182);
        assert_eq!(luma(0, 0, 255), 18);
        assert_eq!(flatten(200, 255, 0, 255), 200);
        assert_eq!(flatten(200, 0, 17, 255), 17);
        assert_eq!(flatten(255, 128, 0, 255), 128);
        assert_eq!(flatten(65_535, 32_768, 0, 65_535), 32_768);
    }

    #[test]
    fn colors_are_checked_against_the_format() {
        assert_eq!(
            color_bytes(PixelFormat::Rgba16, [1, 2, 3, 255]).unwrap(),
            [1, 1, 2, 2, 3, 3, 255, 255]
        );
        assert_eq!(
            color_bytes(PixelFormat::GrayA8, [9, 9, 9, 7]).unwrap(),
            [9, 7]
        );
        assert!(color_bytes(PixelFormat::Gray8, [1, 2, 3, 255]).is_err());
        assert!(color_bytes(PixelFormat::Rgb8, [1, 2, 3, 254]).is_err());
        assert!(color_bytes(PixelFormat::Indexed8, [0; 4]).is_err());
    }

    #[test]
    fn read_and_write_every_format() {
        let pal = [[1u8, 2, 3, 4]];
        for fmt in PixelFormat::ALL {
            let bpp = fmt.bytes_per_pixel();
            let px: Vec<u8> = if fmt == PixelFormat::Indexed8 {
                vec![0]
            } else {
                (0..bpp as u8).map(|i| i * 11 + 5).collect()
            };
            let c = read_rgba(fmt, Some(&pal), &px).unwrap();
            if fmt == PixelFormat::Indexed8 {
                assert_eq!(c, [1, 2, 3, 4]);
                continue;
            }
            let mut back = vec![0u8; bpp];
            write_rgba(fmt, c, &mut back).unwrap();
            assert_eq!(back, px, "{fmt:?}");
        }
        assert!(read_rgba(PixelFormat::Indexed8, Some(&pal), &[1]).is_err());
    }

    #[test]
    fn palettes_find_or_append() {
        let p = Palette {
            colors: vec![[9, 9, 9, 255], [0, 0, 0, 0]],
        };
        assert_eq!(palette_clear(&p).unwrap().1, 1);
        assert_eq!(palette_index(&p, [9, 9, 9, 255]).unwrap().1, 0);
        let (q, i) = palette_index(&p, [1, 1, 1, 1]).unwrap();
        assert_eq!((q.colors.len(), i), (3, 2));
        let full = Palette {
            colors: vec![[7, 7, 7, 255]; 256],
        };
        assert!(palette_clear(&full).is_none());
        assert_eq!(palette_index(&full, [7, 7, 7, 255]).unwrap().1, 0);
    }
}
