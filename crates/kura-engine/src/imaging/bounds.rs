//! 不透明な範囲（透明な余白のトリムと、段 8 の解析 `image.opaque_bounds` が同じ関数を使う。E26）。

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::pixels::{alpha_at_most, can_be_transparent};
use crate::monitor::Monitor;
use crate::value::Raster;

/// アルファが `threshold` より大きい画素（不透明とみなす画素）を囲む最小の矩形。そういう画素がなければ None。
/// 透明を表せない形式（アルファのない形式）は画像全体。タイルごとに並列に求め、タイルの順にまとめる。
pub fn opaque_bounds(
    px: &Raster,
    threshold: u8,
    mon: &dyn Monitor,
) -> Result<Option<IRect>, EngineError> {
    let (w, h) = (px.width(), px.height());
    if !can_be_transparent(px.format()) {
        return Ok(Some(IRect::new(0, 0, w, h)));
    }
    let fmt = px.format();
    let palette = px.palette().map(|p| p.colors.as_slice());
    let bpp = px.bpp();
    let (tx_n, ty_n) = (px.tiles_x(), px.tiles_y());
    let parts = par_blocks((tx_n as usize) * (ty_n as usize), mon, |i| {
        let (tx, ty) = (i as u32 % tx_n, i as u32 / tx_n);
        let rect = px.tile_rect(tx, ty);
        let tile = px.tile(tx, ty);
        // (x0, y0, x1, y1)：含む画素の範囲（タイルの中の座標）
        let mut b: Option<(u32, u32, u32, u32)> = None;
        for (r, row) in tile.chunks_exact(rect.w as usize * bpp).enumerate() {
            for (c, p) in row.chunks_exact(bpp).enumerate() {
                if alpha_at_most(fmt, palette, p, threshold) {
                    continue;
                }
                let (x, y) = (c as u32, r as u32);
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        Ok(b.map(|(x0, y0, x1, y1)| {
            (
                rect.x as u32 + x0,
                rect.y as u32 + y0,
                rect.x as u32 + x1,
                rect.y as u32 + y1,
            )
        }))
    })?;
    let merged = parts.into_iter().flatten().fold(
        None,
        |acc: Option<(u32, u32, u32, u32)>, (x0, y0, x1, y1)| {
            Some(match acc {
                None => (x0, y0, x1, y1),
                Some((a0, b0, a1, b1)) => (a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1)),
            })
        },
    );
    Ok(merged
        .map(|(x0, y0, x1, y1)| IRect::new(i64::from(x0), i64::from(y0), x1 - x0 + 1, y1 - y0 + 1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::value::{Palette, PixelFormat};

    #[test]
    fn finds_the_box_across_tiles_and_formats() {
        let (w, h) = (600u32, 300u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        for (x, y, a) in [(17u32, 280u32, 1u8), (530, 4, 200), (300, 150, 255)] {
            data[((y * w + x) * 4 + 3) as usize] = a;
        }
        let px = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data).unwrap();
        assert_eq!(
            opaque_bounds(&px, 0, &NoMonitor).unwrap(),
            Some(IRect::new(17, 4, 530 - 17 + 1, 280 - 4 + 1))
        );
        // しきい値 1 では、アルファ 1 の画素は透明とみなす
        assert_eq!(
            opaque_bounds(&px, 1, &NoMonitor).unwrap(),
            Some(IRect::new(300, 4, 530 - 300 + 1, 150 - 4 + 1))
        );
        assert_eq!(
            opaque_bounds(&px, 254, &NoMonitor).unwrap(),
            Some(IRect::new(300, 150, 1, 1))
        );
        let clear = Raster::filled(5, 5, PixelFormat::GrayA16, None, &[9, 9, 0, 0]).unwrap();
        assert_eq!(opaque_bounds(&clear, 0, &NoMonitor).unwrap(), None);
        let rgb = Raster::filled(5, 4, PixelFormat::Rgb8, None, &[0, 0, 0]).unwrap();
        assert_eq!(
            opaque_bounds(&rgb, 0, &NoMonitor).unwrap(),
            Some(IRect::new(0, 0, 5, 4))
        );
        let pal = Palette {
            colors: vec![[0, 0, 0, 0], [1, 1, 1, 255]],
        };
        let idx =
            Raster::from_rows(3, 2, PixelFormat::Indexed8, Some(pal), &[0, 0, 0, 0, 1, 0]).unwrap();
        assert_eq!(
            opaque_bounds(&idx, 0, &NoMonitor).unwrap(),
            Some(IRect::new(1, 1, 1, 1))
        );
    }
}
