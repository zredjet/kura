//! 不透明な範囲。解析 `image.opaque_bounds` の中身で、トリム（`image.trim_alpha`）はここを直接呼ばず、解析を申告して
//! 結果を受け取る（§11.2。E26 のソースの走査が確かめる）。計画の段階でない値（`sprite.split_grid` が切り出した
//! セルの空の判定）には、`opaque_bounds` を部品として使ってよい（E26 の許可の一覧）。

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::pixels::{alpha_at_most, can_be_transparent};
use crate::monitor::Monitor;
use crate::value::{Frames, Raster};

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

/// 2 つの矩形を囲む最小の矩形（`a` が None なら `b`）。
pub fn union_rect(a: Option<IRect>, b: IRect) -> IRect {
    match a {
        None => b,
        Some(a) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
            let len = |d: i64| u32::try_from(d).unwrap_or(u32::MAX);
            IRect::new(
                x0,
                y0,
                len(x1.saturating_sub(x0)),
                len(y1.saturating_sub(y0)),
            )
        }
    }
}

/// フレームの列の不透明な範囲：各フレームの範囲（キャンバスの座標。そのフレームが全部透明とみなす画素なら None）と、
/// その合併（どれもなければ None）。フレームの外（キャンバスの覆われない部分）は数えない（透明とみなす）。
/// 透明なフレームの合間で中止を確かめる。解析 `image.opaque_bounds` のフレームの列の中身。トリム（`image.trim_alpha`
/// の `union`・`perFrame`）は、この結果を解析の申告で受け取る（直接呼ばない。E26）。
pub fn frames_opaque_bounds(
    fr: &Frames,
    threshold: u8,
    mon: &dyn Monitor,
) -> Result<(Vec<Option<IRect>>, Option<IRect>), EngineError> {
    let mut all: Option<IRect> = None;
    let mut per = Vec::new();
    per.try_reserve_exact(fr.frames().len())
        .map_err(|_| EngineError::Limit("フレームが多すぎます".into()))?;
    for f in fr.frames() {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let b = opaque_bounds(&f.px, threshold, mon)?.map(|b| {
            IRect::new(
                f.offset.x.saturating_add(b.x),
                f.offset.y.saturating_add(b.y),
                b.w,
                b.h,
            )
        });
        if let Some(b) = b {
            all = Some(union_rect(all, b));
        }
        per.push(b);
    }
    Ok((per, all))
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
