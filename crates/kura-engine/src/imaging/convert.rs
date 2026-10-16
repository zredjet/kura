//! 画素の形式の変換（情報を失わないものだけ）。

use crate::error::EngineError;
use crate::value::raster::alloc_zeroed;
use crate::value::{PixelFormat, Raster};

/// 8 ビットの形式（とパレット）を、行を詰めた RGBA8 に広げる。情報は失わない。
/// 16 ビットの形式は、黙って精度を落とさないようにエラーにする（先に変換の操作で 8 ビットにする）。
pub fn to_rgba8_rows(px: &Raster) -> Result<Vec<u8>, EngineError> {
    let rows = px.to_rows()?;
    let n = px.width() as usize * px.height() as usize;
    let mut out = alloc_zeroed(n.checked_mul(4).ok_or_else(too_large)?)?;
    expand_into(
        px.format(),
        px.palette().map(|p| p.colors.as_slice()),
        &rows,
        &mut out,
    )?;
    Ok(out)
}

/// 1 行（または並べた画素）を RGBA8 に広げる。`src` は `fmt` の画素を詰めたもの、`out` は画素数 × 4 バイト。
pub fn expand_into(
    fmt: PixelFormat,
    palette: Option<&[[u8; 4]]>,
    src: &[u8],
    out: &mut [u8],
) -> Result<(), EngineError> {
    let bpp = fmt.bytes_per_pixel();
    if src.len() / bpp != out.len() / 4
        || !src.len().is_multiple_of(bpp)
        || !out.len().is_multiple_of(4)
    {
        return Err(EngineError::Internal("広げる先の大きさが合いません".into()));
    }
    for (s, d) in src.chunks_exact(bpp).zip(out.chunks_exact_mut(4)) {
        let rgba =
            match fmt {
                PixelFormat::Gray8 => [s[0], s[0], s[0], 255],
                PixelFormat::GrayA8 => [s[0], s[0], s[0], s[1]],
                PixelFormat::Rgb8 => [s[0], s[1], s[2], 255],
                PixelFormat::Rgba8 => [s[0], s[1], s[2], s[3]],
                PixelFormat::Indexed8 => *palette
                    .and_then(|p| p.get(usize::from(s[0])))
                    .ok_or_else(|| {
                        EngineError::Internal("パレットにない番号の画素があります".into())
                    })?,
                PixelFormat::Gray16
                | PixelFormat::GrayA16
                | PixelFormat::Rgb16
                | PixelFormat::Rgba16 => {
                    return Err(EngineError::Unsupported(
                        "16 ビットの画像は、先に 8 ビットへ変換してください".into(),
                    ));
                }
            };
        d.copy_from_slice(&rgba);
    }
    Ok(())
}

fn too_large() -> EngineError {
    EngineError::Limit("画像が大きすぎます".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Palette;

    #[test]
    fn expands_every_8_bit_format() {
        let cases: [(PixelFormat, Vec<u8>, [u8; 4]); 4] = [
            (PixelFormat::Gray8, vec![7], [7, 7, 7, 255]),
            (PixelFormat::GrayA8, vec![7, 9], [7, 7, 7, 9]),
            (PixelFormat::Rgb8, vec![1, 2, 3], [1, 2, 3, 255]),
            (PixelFormat::Rgba8, vec![1, 2, 3, 4], [1, 2, 3, 4]),
        ];
        for (fmt, px, want) in cases {
            let r = Raster::from_rows(1, 1, fmt, None, &px).unwrap();
            assert_eq!(to_rgba8_rows(&r).unwrap(), want, "{fmt:?}");
        }
        let pal = Palette {
            colors: vec![[1, 2, 3, 0], [4, 5, 6, 255]],
        };
        let r = Raster::from_rows(2, 1, PixelFormat::Indexed8, Some(pal), &[1, 0]).unwrap();
        assert_eq!(to_rgba8_rows(&r).unwrap(), [4, 5, 6, 255, 1, 2, 3, 0]);
        let r16 = Raster::from_rows(1, 1, PixelFormat::Gray16, None, &[0, 1]).unwrap();
        assert!(matches!(
            to_rgba8_rows(&r16),
            Err(EngineError::Unsupported(_))
        ));
    }
}
