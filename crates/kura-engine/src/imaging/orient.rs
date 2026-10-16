//! EXIF の向き（Orientation 1〜8）を画素に当てる。
//!
//! 値は「保存された画素を、表示するときにどう変えるか」。
//! 1 そのまま・2 左右反転・3 180° 回転・4 上下反転・5 転置（左上と右下を結ぶ線で反転）・6 時計回りに 90°・
//! 7 反転の転置（右上と左下を結ぶ線で反転）・8 反時計回りに 90°。5〜8 は幅と高さが入れ替わる。

use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::value::Raster;

/// 向きを当てたときの大きさ（5〜8 は幅と高さを入れ替える）。範囲の外の値は 1 として扱う。
pub fn oriented_size(w: u32, h: u32, orientation: u8) -> (u32, u32) {
    if swaps_axes(orientation) {
        (h, w)
    } else {
        (w, h)
    }
}

/// 5〜8 か（幅と高さが入れ替わるか）。
pub fn swaps_axes(orientation: u8) -> bool {
    matches!(orientation, 5..=8)
}

/// 出力の (x, y) が、元のどの画素か。`w`・`h` は元の大きさ。
fn source_of(orientation: u8, w: u32, h: u32, x: u32, y: u32) -> (u32, u32) {
    match orientation {
        2 => (w - 1 - x, y),
        3 => (w - 1 - x, h - 1 - y),
        4 => (x, h - 1 - y),
        5 => (y, x),
        6 => (y, h - 1 - x),
        7 => (w - 1 - y, h - 1 - x),
        8 => (w - 1 - y, x),
        _ => (x, y),
    }
}

/// 向きを当てた画像を返す（1 や範囲の外の値なら、そのまま複製を返す）。
pub fn orient(px: &Raster, orientation: u8, mon: &dyn Monitor) -> Result<Raster, EngineError> {
    if !(2..=8).contains(&orientation) {
        return Ok(px.clone());
    }
    let (w, h) = (px.width(), px.height());
    let (ow, oh) = oriented_size(w, h, orientation);
    let bpp = px.bpp();
    px.derive(ow, oh, mon, |rect, out| {
        let row_len = rect.w as usize * bpp;
        for (r, row) in out.chunks_exact_mut(row_len).enumerate() {
            let y = rect.y as u32 + r as u32;
            for (c, dst) in row.chunks_exact_mut(bpp).enumerate() {
                let x = rect.x as u32 + c as u32;
                let (sx, sy) = source_of(orientation, w, h, x, y);
                let src = px.pixel(sx, sy).ok_or_else(|| {
                    EngineError::Internal("向きの計算が画像の外を指しました".into())
                })?;
                dst.copy_from_slice(src);
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::value::PixelFormat;

    /// 3×2 の画像（番号は左上から 0..6）に各向きを当てた結果を、手で書いた期待値と照らす。
    #[test]
    fn all_eight_orientations() {
        let px = Raster::from_rows(3, 2, PixelFormat::Gray8, None, &[0, 1, 2, 3, 4, 5]).unwrap();
        let cases: [(u8, (u32, u32), [u8; 6]); 8] = [
            (1, (3, 2), [0, 1, 2, 3, 4, 5]),
            (2, (3, 2), [2, 1, 0, 5, 4, 3]),
            (3, (3, 2), [5, 4, 3, 2, 1, 0]),
            (4, (3, 2), [3, 4, 5, 0, 1, 2]),
            (5, (2, 3), [0, 3, 1, 4, 2, 5]),
            (6, (2, 3), [3, 0, 4, 1, 5, 2]),
            (7, (2, 3), [5, 2, 4, 1, 3, 0]),
            (8, (2, 3), [2, 5, 1, 4, 0, 3]),
        ];
        for (o, size, want) in cases {
            let out = orient(&px, o, &NoMonitor).unwrap();
            assert_eq!((out.width(), out.height()), size, "{o}");
            assert_eq!(out.to_rows().unwrap(), want, "{o}");
        }
        // 範囲の外はそのまま
        assert_eq!(orient(&px, 0, &NoMonitor).unwrap(), px);
        assert_eq!(orient(&px, 9, &NoMonitor).unwrap(), px);
    }

    /// 向きを戻す組み合わせ：6 の結果に 8 を当てると元に戻る。2・3・4・5・7 は 2 回で戻る。
    #[test]
    fn inverses_round_trip() {
        let data: Vec<u8> = (0..(300 * 260 * 2)).map(|i| (i % 253) as u8).collect();
        let px = Raster::from_rows(300, 260, PixelFormat::GrayA8, None, &data).unwrap();
        let back = orient(&orient(&px, 6, &NoMonitor).unwrap(), 8, &NoMonitor).unwrap();
        assert_eq!(back, px);
        for o in [2, 3, 4, 5, 7] {
            let twice = orient(&orient(&px, o, &NoMonitor).unwrap(), o, &NoMonitor).unwrap();
            assert_eq!(twice, px, "{o}");
        }
    }
}
