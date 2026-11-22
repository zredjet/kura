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
    for (s, d) in src
        .chunks_exact(bpp)
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
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

/// 1 画素を別の形式にする（Indexed8 へは扱わない。Indexed8 からはパレットの色で広げる）。整数だけで、式を凍結する。
/// 順番は ① グレーへ（元の深さで `pixels::luma`）② アルファを外す（元の深さで、下地 `matte` に `pixels::flatten`。
/// グレーへ行くときは下地もグレーにする）③ 深さを変える（`pixels::up8`・`pixels::down16`）。
pub fn convert_pixel(
    src_fmt: PixelFormat,
    palette: Option<&[[u8; 4]]>,
    src: &[u8],
    dst_fmt: PixelFormat,
    matte: [u8; 3],
    out: &mut [u8],
) -> Result<(), EngineError> {
    use crate::imaging::pixels::{channel_max, down16, flatten, is_gray, luma, read_rgba, up8};
    if dst_fmt == PixelFormat::Indexed8 {
        return Err(EngineError::Internal(
            "Indexed8 への変換は減色の経路で行います".into(),
        ));
    }
    let max_s = channel_max(src_fmt);
    let [mut r, mut g, mut b, mut a] = read_rgba(src_fmt, palette, src)?;
    let to_gray = is_gray(dst_fmt);
    if to_gray {
        let y = luma(r, g, b);
        (r, g, b) = (y, y, y);
    }
    if !dst_fmt.has_alpha() && a != max_s {
        let wide = |v: u8| {
            if max_s == 255 {
                u32::from(v)
            } else {
                up8(u32::from(v))
            }
        };
        let (mut mr, mut mg, mut mb) = (wide(matte[0]), wide(matte[1]), wide(matte[2]));
        if to_gray {
            let y = luma(mr, mg, mb);
            (mr, mg, mb) = (y, y, y);
        }
        r = flatten(r, a, mr, max_s);
        g = flatten(g, a, mg, max_s);
        b = flatten(b, a, mb, max_s);
        a = max_s;
    }
    let max_d = channel_max(dst_fmt);
    if max_s != max_d {
        let f: fn(u32) -> u32 = if max_d > max_s { up8 } else { down16 };
        (r, g, b, a) = (f(r), f(g), f(b), f(a));
    }
    crate::imaging::pixels::write_rgba(dst_fmt, [r, g, b, a], out)
}

fn too_large() -> EngineError {
    EngineError::Limit("画像が大きすぎます".into())
}

/// 画素を詰めた `src`（`fmt` の画素の並び）の各画素を、`convert_pixel(fmt → Rgba8)` と同じ 8 ビットの色にして、
/// `quantize::pack` の形（`r<<24 | g<<16 | b<<8 | a`）で順に `f` に渡す（`image.convert` が Indexed8 へ減色するときの色。
/// 解析 `image.colors` が同じ色で数える。E26）。`f` が false を返したらそこで止める。
/// グレーは [y, y, y, a]、アルファのない形式のアルファは 255、16 ビットは各チャンネルを四捨五入（`pixels::down16`）、
/// Indexed8 はパレットの色。画素ごとに `convert_pixel` を呼ぶより速い（形式の分岐を外に出す）。
pub fn for_each_rgba8_key(
    fmt: PixelFormat,
    palette: Option<&[[u8; 4]]>,
    src: &[u8],
    mut f: impl FnMut(u32) -> bool,
) -> Result<(), EngineError> {
    use crate::imaging::pixels::down16;
    let bpp = fmt.bytes_per_pixel();
    if !src.len().is_multiple_of(bpp) {
        return Err(EngineError::Internal(
            "画素の並びの大きさが合いません".into(),
        ));
    }
    let key = |r: u8, g: u8, b: u8, a: u8| u32::from_be_bytes([r, g, b, a]);
    // 16 ビットの値（リトルエンディアン）を 8 ビットに四捨五入（結果は 0〜255）
    let d = |lo: u8, hi: u8| down16(u32::from(u16::from_le_bytes([lo, hi]))) as u8;
    match fmt {
        PixelFormat::Gray8 => {
            for p in src {
                if !f(key(*p, *p, *p, 255)) {
                    break;
                }
            }
        }
        PixelFormat::GrayA8 => {
            for p in src.as_chunks::<2>().0 {
                if !f(key(p[0], p[0], p[0], p[1])) {
                    break;
                }
            }
        }
        PixelFormat::Rgb8 => {
            for p in src.as_chunks::<3>().0 {
                if !f(key(p[0], p[1], p[2], 255)) {
                    break;
                }
            }
        }
        PixelFormat::Rgba8 => {
            for p in src.as_chunks::<4>().0 {
                if !f(key(p[0], p[1], p[2], p[3])) {
                    break;
                }
            }
        }
        PixelFormat::Gray16 => {
            for p in src.as_chunks::<2>().0 {
                let y = d(p[0], p[1]);
                if !f(key(y, y, y, 255)) {
                    break;
                }
            }
        }
        PixelFormat::GrayA16 => {
            for p in src.as_chunks::<4>().0 {
                let y = d(p[0], p[1]);
                if !f(key(y, y, y, d(p[2], p[3]))) {
                    break;
                }
            }
        }
        PixelFormat::Rgb16 => {
            for p in src.as_chunks::<6>().0 {
                if !f(key(d(p[0], p[1]), d(p[2], p[3]), d(p[4], p[5]), 255)) {
                    break;
                }
            }
        }
        PixelFormat::Rgba16 => {
            for p in src.as_chunks::<8>().0 {
                if !f(key(
                    d(p[0], p[1]),
                    d(p[2], p[3]),
                    d(p[4], p[5]),
                    d(p[6], p[7]),
                )) {
                    break;
                }
            }
        }
        PixelFormat::Indexed8 => {
            let palette = palette
                .ok_or_else(|| EngineError::Internal("Indexed8 にパレットがありません".into()))?;
            for p in src {
                let c = palette.get(usize::from(*p)).ok_or_else(|| {
                    EngineError::Internal("パレットにない番号の画素があります".into())
                })?;
                if !f(u32::from_be_bytes(*c)) {
                    break;
                }
            }
        }
    }
    Ok(())
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

    fn conv(src: PixelFormat, px: &[u8], dst: PixelFormat) -> Vec<u8> {
        let mut out = vec![0u8; dst.bytes_per_pixel()];
        convert_pixel(
            src,
            Some(&[[10, 20, 30, 128]]),
            px,
            dst,
            [255, 255, 255],
            &mut out,
        )
        .unwrap();
        out
    }

    #[test]
    fn converts_between_formats_with_the_frozen_formulas() {
        // 深さ
        assert_eq!(
            conv(PixelFormat::Rgb8, &[1, 2, 255], PixelFormat::Rgb16),
            [1, 1, 2, 2, 255, 255]
        );
        assert_eq!(
            conv(PixelFormat::Gray16, &[128, 0], PixelFormat::Gray8),
            [0]
        );
        assert_eq!(
            conv(PixelFormat::Gray16, &[129, 0], PixelFormat::Gray8),
            [1]
        );
        // グレーへ（Rec. 709）・グレーから
        assert_eq!(
            conv(PixelFormat::Rgb8, &[255, 0, 0], PixelFormat::Gray8),
            [54]
        );
        assert_eq!(
            conv(PixelFormat::Gray8, &[7], PixelFormat::Rgba8),
            [7, 7, 7, 255]
        );
        // アルファを外すときは白に重ねる
        assert_eq!(
            conv(PixelFormat::Rgba8, &[0, 0, 0, 0], PixelFormat::Rgb8),
            [255, 255, 255]
        );
        assert_eq!(
            conv(PixelFormat::Rgba8, &[0, 0, 0, 128], PixelFormat::Rgb8),
            [127, 127, 127]
        );
        assert_eq!(
            conv(PixelFormat::Rgba8, &[9, 8, 7, 255], PixelFormat::Rgb8),
            [9, 8, 7]
        );
        // パレットから
        assert_eq!(
            conv(PixelFormat::Indexed8, &[0], PixelFormat::Rgba8),
            [10, 20, 30, 128]
        );
        // 同じ形式はそのまま
        for fmt in PixelFormat::ALL {
            if fmt == PixelFormat::Indexed8 {
                continue;
            }
            let px: Vec<u8> = (0..fmt.bytes_per_pixel() as u8)
                .map(|i| i * 13 + 3)
                .collect();
            assert_eq!(conv(fmt, &px, fmt), px, "{fmt:?}");
        }
        let mut out = [0u8];
        assert!(
            convert_pixel(
                PixelFormat::Gray8,
                None,
                &[0],
                PixelFormat::Indexed8,
                [0; 3],
                &mut out
            )
            .is_err()
        );
    }

    /// 速い読み手（`for_each_rgba8_key`）が、すべての形式で `convert_pixel(→ Rgba8)` を pack したものと同じ。
    /// 1・2 バイトの形式は全数、ほかは ChaCha8 の標本（16 ビットの丸めの境目 128・129 の倍数の近くを含む）。
    #[test]
    fn rgba8_keys_match_convert_pixel() {
        use crate::imaging::quantize::pack;
        use rand_chacha::rand_core::Rng;
        let palette: Vec<[u8; 4]> = (0..=255u8)
            .map(|i| [i, i.wrapping_mul(7), 255 - i, i.wrapping_mul(13)])
            .collect();
        let mut r = crate::determinism::rng(8);
        for fmt in PixelFormat::ALL {
            let bpp = fmt.bytes_per_pixel();
            let samples: Vec<Vec<u8>> = if bpp <= 2 {
                (0..1usize << (8 * bpp))
                    .map(|v| v.to_le_bytes()[..bpp].to_vec())
                    .collect()
            } else {
                (0..20_000)
                    .map(|i| {
                        (0..bpp)
                            .map(|b| {
                                let v = r.next_u32();
                                // 16 ビットの丸めの境目の近く（257 の倍数 ± 128）を多めに
                                if i % 3 == 0 && b % 2 == 0 {
                                    ((v % 256) * 257 + 128) as u8
                                } else {
                                    v as u8
                                }
                            })
                            .collect()
                    })
                    .collect()
            };
            let src: Vec<u8> = samples.concat();
            let mut keys = Vec::new();
            for_each_rgba8_key(fmt, Some(&palette), &src, |k| {
                keys.push(k);
                true
            })
            .unwrap();
            assert_eq!(keys.len(), samples.len(), "{fmt:?}");
            for (s, k) in samples.iter().zip(&keys) {
                let mut out = [0u8; 4];
                convert_pixel(
                    fmt,
                    Some(&palette),
                    s,
                    PixelFormat::Rgba8,
                    [9, 9, 9],
                    &mut out,
                )
                .unwrap();
                assert_eq!(*k, pack(out), "{fmt:?} {s:?}");
            }
        }
        // 止める・パレットにない番号
        let mut n = 0;
        for_each_rgba8_key(PixelFormat::Gray8, None, &[1, 2, 3], |_| {
            n += 1;
            n < 2
        })
        .unwrap();
        assert_eq!(n, 2);
        assert!(
            for_each_rgba8_key(PixelFormat::Indexed8, Some(&palette[..2]), &[2], |_| true).is_err()
        );
        assert!(for_each_rgba8_key(PixelFormat::Rgb8, None, &[1, 2], |_| true).is_err());
    }
}
