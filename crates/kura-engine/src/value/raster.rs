//! 画素の実体（docs/editing.md §5.2）。256×256 の CoW のタイルで持つ。
//!
//! タイルは `Arc` で共有し、書き換えるときだけ複製する（ペン・塗りつぶしは触れたタイルだけ）。
//! 取り消しの履歴とキャッシュがタイルを共有するので、メモリが増えにくい。
//! 並び・ダイジェスト・等しさはタイルの分け方によらない（行を左から順に読んだもの）。

use std::sync::Arc;

use crate::determinism::par_blocks;
use crate::digest::Hasher;
use crate::error::EngineError;
use crate::geom::IRect;
use crate::monitor::Monitor;
use crate::value::pixel::{Palette, PixelFormat};

/// タイルの 1 辺。
pub const TILE: u32 = 256;

/// 画素の実体。幅・高さは 1 以上。
#[derive(Debug, Clone)]
pub struct Raster {
    w: u32,
    h: u32,
    fmt: PixelFormat,
    palette: Option<Arc<Palette>>,
    /// タイルを行の順に並べたもの（`ty * tiles_x + tx`）。各タイルは行を詰めて並べる。
    tiles: Vec<Arc<Vec<u8>>>,
}

/// 0 で埋めた領域を確保する。足りなければ落ちずにエラーにする。
pub fn alloc_zeroed(len: usize) -> Result<Vec<u8>, EngineError> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| EngineError::Limit(format!("{len} バイトを確保できません")))?;
    v.resize(len, 0);
    Ok(v)
}

impl Raster {
    fn check_header(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<&Palette>,
    ) -> Result<(), EngineError> {
        if w == 0 || h == 0 {
            return Err(EngineError::Internal(format!(
                "画像の大きさが 0 です（{w}×{h}）"
            )));
        }
        match (fmt, palette) {
            (PixelFormat::Indexed8, Some(p)) if !p.colors.is_empty() && p.colors.len() <= 256 => {
                Ok(())
            }
            (PixelFormat::Indexed8, _) => Err(EngineError::Internal(
                "Indexed8 には 1〜256 色のパレットが要ります".into(),
            )),
            (_, Some(_)) => Err(EngineError::Internal(
                "パレットは Indexed8 だけが持ちます".into(),
            )),
            (_, None) => Ok(()),
        }
    }

    fn tiles_of(w: u32, h: u32) -> (u32, u32) {
        (w.div_ceil(TILE), h.div_ceil(TILE))
    }

    /// 幅。
    pub fn width(&self) -> u32 {
        self.w
    }

    /// 高さ。
    pub fn height(&self) -> u32 {
        self.h
    }

    /// 画素の形式。
    pub fn format(&self) -> PixelFormat {
        self.fmt
    }

    /// パレット（Indexed8 のとき）。
    pub fn palette(&self) -> Option<&Arc<Palette>> {
        self.palette.as_ref()
    }

    /// 1 画素のバイト数。
    pub fn bpp(&self) -> usize {
        self.fmt.bytes_per_pixel()
    }

    /// 横のタイル数。
    pub fn tiles_x(&self) -> u32 {
        Self::tiles_of(self.w, self.h).0
    }

    /// 縦のタイル数。
    pub fn tiles_y(&self) -> u32 {
        Self::tiles_of(self.w, self.h).1
    }

    /// タイルが覆う矩形。
    pub fn tile_rect(&self, tx: u32, ty: u32) -> IRect {
        tile_rect(self.w, self.h, tx, ty)
    }

    /// タイルの中身（行を詰めたもの）。
    pub fn tile(&self, tx: u32, ty: u32) -> &[u8] {
        &self.tiles[(ty * self.tiles_x() + tx) as usize]
    }

    /// タイルを書き換える（共有していれば複製してから）。
    pub fn tile_mut(&mut self, tx: u32, ty: u32) -> &mut [u8] {
        let i = (ty * self.tiles_x() + tx) as usize;
        Arc::make_mut(&mut self.tiles[i]).as_mut_slice()
    }

    /// 同じタイルの実体を共有していれば true（複製が起きていないことのテスト用）。
    pub fn shares_tile(&self, other: &Raster, tx: u32, ty: u32) -> bool {
        let i = (ty * self.tiles_x() + tx) as usize;
        self.tiles
            .get(i)
            .zip(other.tiles.get(i))
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    }

    /// 1 画素の色で埋めた画像。
    pub fn filled(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<Palette>,
        px: &[u8],
    ) -> Result<Self, EngineError> {
        Self::check_header(w, h, fmt, palette.as_ref())?;
        if px.len() != fmt.bytes_per_pixel() {
            return Err(EngineError::Internal(
                "画素の大きさが形式と合いません".into(),
            ));
        }
        Self::from_tile_fn(w, h, fmt, palette, &crate::monitor::NoMonitor, |_, out| {
            for chunk in out.chunks_exact_mut(px.len()) {
                chunk.copy_from_slice(px);
            }
            Ok(())
        })
    }

    /// 行を詰めて並べたバイト列から作る。
    pub fn from_rows(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<Palette>,
        data: &[u8],
    ) -> Result<Self, EngineError> {
        Self::check_header(w, h, fmt, palette.as_ref())?;
        let bpp = fmt.bytes_per_pixel();
        let expected = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(bpp));
        if expected != Some(data.len()) {
            return Err(EngineError::Internal(
                "画素のバイト数が大きさと合いません".into(),
            ));
        }
        let stride = w as usize * bpp;
        Self::from_tile_fn(
            w,
            h,
            fmt,
            palette,
            &crate::monitor::NoMonitor,
            |rect, out| {
                let row_len = rect.w as usize * bpp;
                for (r, dst) in out.chunks_exact_mut(row_len).enumerate() {
                    let start = (rect.y as usize + r) * stride + rect.x as usize * bpp;
                    dst.copy_from_slice(&data[start..start + row_len]);
                }
                Ok(())
            },
        )
    }

    /// タイルごとに中身を計算して作る（タイルは並列に計算する。結果はスレッド数によらない）。
    /// `f` はタイルの矩形と、その中身（行を詰めた領域）を受け取って書く。
    pub fn from_tile_fn<F>(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<Palette>,
        mon: &dyn Monitor,
        f: F,
    ) -> Result<Self, EngineError>
    where
        F: Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send,
    {
        Self::check_header(w, h, fmt, palette.as_ref())?;
        let (tx_n, ty_n) = Self::tiles_of(w, h);
        let bpp = fmt.bytes_per_pixel();
        let count = (tx_n as usize) * (ty_n as usize);
        let tiles = par_blocks(count, mon, |i| {
            let (tx, ty) = (i as u32 % tx_n, i as u32 / tx_n);
            let rect = tile_rect(w, h, tx, ty);
            let mut buf = alloc_zeroed(rect.w as usize * rect.h as usize * bpp)?;
            f(rect, &mut buf)?;
            // パレットの番号は色の数より小さいこと
            if let Some(p) = &palette
                && buf.iter().any(|b| usize::from(*b) >= p.colors.len())
            {
                return Err(EngineError::Internal(
                    "パレットにない番号の画素があります".into(),
                ));
            }
            Ok(Arc::new(buf))
        })?;
        Ok(Self {
            w,
            h,
            fmt,
            palette: palette.map(Arc::new),
            tiles,
        })
    }

    /// 同じ形式・パレットで、別の大きさの画像をタイルごとに作る（操作の出力に使う）。
    pub fn derive<F>(&self, w: u32, h: u32, mon: &dyn Monitor, f: F) -> Result<Self, EngineError>
    where
        F: Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send,
    {
        Self::from_tile_fn(w, h, self.fmt, self.palette.as_deref().cloned(), mon, f)
    }

    /// 矩形（画像の中に収まるもの）の画素を、行を詰めて `out` に写す。
    pub fn read_rect(&self, rect: IRect, out: &mut [u8]) -> Result<(), EngineError> {
        if !rect.inside(self.w, self.h) {
            return Err(EngineError::Internal(format!(
                "範囲が画像の外です（{rect:?}）"
            )));
        }
        let bpp = self.bpp();
        let row_len = rect.w as usize * bpp;
        if out.len() != row_len * rect.h as usize {
            return Err(EngineError::Internal("写す先の大きさが合いません".into()));
        }
        let (x0, x1) = (rect.x as u32, rect.right() as u32);
        for r in 0..rect.h {
            let y = rect.y as u32 + r;
            let ty = y / TILE;
            let dst_row = &mut out[r as usize * row_len..(r as usize + 1) * row_len];
            let mut x = x0;
            while x < x1 {
                let tx = x / TILE;
                let tr = self.tile_rect(tx, ty);
                let tile_end = (tr.right() as u32).min(x1);
                let tile = self.tile(tx, ty);
                let tile_stride = tr.w as usize * bpp;
                let src_start =
                    (y - tr.y as u32) as usize * tile_stride + (x - tr.x as u32) as usize * bpp;
                let n = (tile_end - x) as usize * bpp;
                let dst_start = (x - x0) as usize * bpp;
                dst_row[dst_start..dst_start + n].copy_from_slice(&tile[src_start..src_start + n]);
                x = tile_end;
            }
        }
        Ok(())
    }

    /// 行を詰めて並べたバイト列にする。
    pub fn to_rows(&self) -> Result<Vec<u8>, EngineError> {
        let mut out = alloc_zeroed(self.w as usize * self.h as usize * self.bpp())?;
        self.read_rect(IRect::new(0, 0, self.w, self.h), &mut out)?;
        Ok(out)
    }

    /// 1 画素（画像の中）。
    pub fn pixel(&self, x: u32, y: u32) -> Option<&[u8]> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let (tx, ty) = (x / TILE, y / TILE);
        let tr = self.tile_rect(tx, ty);
        let bpp = self.bpp();
        let i = ((y - tr.y as u32) as usize * tr.w as usize + (x - tr.x as u32) as usize) * bpp;
        Some(&self.tile(tx, ty)[i..i + bpp])
    }

    /// ダイジェストに足す（タイルの分け方によらない：大きさ・形式・パレット・行を左から順に）。
    pub fn digest_into(&self, h: &mut Hasher) {
        h.u32(self.w).u32(self.h).u8(self.fmt.code());
        match &self.palette {
            Some(p) => {
                h.u8(1).u32(p.colors.len() as u32);
                for c in &p.colors {
                    h.raw(c);
                }
            }
            None => {
                h.u8(0);
            }
        }
        let bpp = self.bpp();
        for y in 0..self.h {
            let ty = y / TILE;
            for tx in 0..self.tiles_x() {
                let tr = self.tile_rect(tx, ty);
                let stride = tr.w as usize * bpp;
                let start = (y - tr.y as u32) as usize * stride;
                h.raw(&self.tile(tx, ty)[start..start + stride]);
            }
        }
    }
}

/// 行を上から順に受け取ってタイルを組む。読み取りの作業領域を 1 タイル行（256 行）ぶんに抑えるので、
/// 大きな画像でも、値の大きさに加えて 1 タイル行ぶんのメモリで読める（docs/editing.md §13）。
pub struct RasterBuilder {
    w: u32,
    h: u32,
    fmt: PixelFormat,
    palette: Option<Palette>,
    /// いま組んでいるタイル行（幅いっぱい × 最大 TILE 行）。
    band: Vec<u8>,
    /// 受け取った行の数。
    rows: u32,
    tiles: Vec<Arc<Vec<u8>>>,
}

impl RasterBuilder {
    /// 大きさ・形式・パレットを決めて始める。
    pub fn new(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<Palette>,
    ) -> Result<Self, EngineError> {
        Raster::check_header(w, h, fmt, palette.as_ref())?;
        let (tx_n, ty_n) = Raster::tiles_of(w, h);
        let band_len = (w as usize)
            .checked_mul(TILE.min(h) as usize)
            .and_then(|n| n.checked_mul(fmt.bytes_per_pixel()))
            .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
        let band = alloc_zeroed(band_len)?;
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(tx_n as usize * ty_n as usize)
            .map_err(|_| EngineError::Limit("タイルの表を確保できません".into()))?;
        Ok(Self {
            w,
            h,
            fmt,
            palette,
            band,
            rows: 0,
            tiles,
        })
    }

    /// 1 行の長さ（バイト）。
    pub fn row_len(&self) -> usize {
        self.w as usize * self.fmt.bytes_per_pixel()
    }

    /// 次の行を足す。
    pub fn push_row(&mut self, row: &[u8]) -> Result<(), EngineError> {
        let row_len = self.row_len();
        if row.len() != row_len {
            return Err(EngineError::Internal("行の長さが合いません".into()));
        }
        if self.rows >= self.h {
            return Err(EngineError::Internal("行が多すぎます".into()));
        }
        if let Some(p) = &self.palette
            && row.iter().any(|b| usize::from(*b) >= p.colors.len())
        {
            return Err(EngineError::Internal(
                "パレットにない番号の画素があります".into(),
            ));
        }
        let r = (self.rows % TILE) as usize;
        self.band[r * row_len..(r + 1) * row_len].copy_from_slice(row);
        self.rows += 1;
        if self.rows.is_multiple_of(TILE) || self.rows == self.h {
            self.flush_band()?;
        }
        Ok(())
    }

    fn flush_band(&mut self) -> Result<(), EngineError> {
        let ty = (self.rows - 1) / TILE;
        let bpp = self.fmt.bytes_per_pixel();
        let row_len = self.row_len();
        for tx in 0..self.w.div_ceil(TILE) {
            let rect = tile_rect(self.w, self.h, tx, ty);
            let tile_row = rect.w as usize * bpp;
            let mut buf = alloc_zeroed(tile_row * rect.h as usize)?;
            for r in 0..rect.h as usize {
                let src = r * row_len + rect.x as usize * bpp;
                buf[r * tile_row..(r + 1) * tile_row]
                    .copy_from_slice(&self.band[src..src + tile_row]);
            }
            self.tiles.push(Arc::new(buf));
        }
        Ok(())
    }

    /// すべての行を受け取ったら画像にする。
    pub fn finish(self) -> Result<Raster, EngineError> {
        if self.rows != self.h {
            return Err(EngineError::Internal(format!(
                "行が足りません（{} / {}）",
                self.rows, self.h
            )));
        }
        Ok(Raster {
            w: self.w,
            h: self.h,
            fmt: self.fmt,
            palette: self.palette.map(Arc::new),
            tiles: self.tiles,
        })
    }
}

impl PartialEq for Raster {
    fn eq(&self, other: &Self) -> bool {
        self.w == other.w
            && self.h == other.h
            && self.fmt == other.fmt
            && self.palette == other.palette
            && self
                .tiles
                .iter()
                .zip(&other.tiles)
                .all(|(a, b)| Arc::ptr_eq(a, b) || a == b)
    }
}

/// 大きさ `w × h` の画像の、タイル (tx, ty) の矩形。
pub fn tile_rect(w: u32, h: u32, tx: u32, ty: u32) -> IRect {
    let x = tx * TILE;
    let y = ty * TILE;
    IRect::new(i64::from(x), i64::from(y), TILE.min(w - x), TILE.min(h - y))
}

#[cfg(test)]
mod tests {
    /// 行を順に足して組んだ画像は、行を並べたバイト列から作ったものと同じ（タイルの境目をまたぐ大きさで）。
    #[test]
    fn builder_matches_from_rows() {
        for (w, h) in [(1u32, 1u32), (300, 257), (256, 512), (513, 3)] {
            let bpp = 3usize;
            let data: Vec<u8> = (0..w as usize * h as usize * bpp)
                .map(|i| (i * 7 % 251) as u8)
                .collect();
            let mut b = RasterBuilder::new(w, h, PixelFormat::Rgb8, None).unwrap();
            for row in data.chunks_exact(w as usize * bpp) {
                b.push_row(row).unwrap();
            }
            let built = b.finish().unwrap();
            assert_eq!(
                built,
                Raster::from_rows(w, h, PixelFormat::Rgb8, None, &data).unwrap()
            );
        }
        let mut short = RasterBuilder::new(2, 2, PixelFormat::Gray8, None).unwrap();
        short.push_row(&[1, 2]).unwrap();
        assert!(short.push_row(&[1]).is_err());
        assert!(short.finish().is_err());
        let palette = Palette {
            colors: vec![[0, 0, 0, 255]; 2],
        };
        let mut idx = RasterBuilder::new(2, 1, PixelFormat::Indexed8, Some(palette)).unwrap();
        assert!(idx.push_row(&[0, 2]).is_err());
    }

    use super::*;
    use crate::digest::Hasher;

    fn gradient(w: u32, h: u32, fmt: PixelFormat) -> Vec<u8> {
        let bpp = fmt.bytes_per_pixel();
        (0..w as usize * h as usize * bpp)
            .map(|i| (i * 31 % 251) as u8)
            .collect()
    }

    #[test]
    fn rows_round_trip_across_tile_edges() {
        for (w, h) in [(1, 1), (255, 3), (256, 256), (257, 513), (600, 7)] {
            for fmt in [PixelFormat::Gray8, PixelFormat::Rgb8, PixelFormat::Rgba16] {
                let data = gradient(w, h, fmt);
                let r = Raster::from_rows(w, h, fmt, None, &data).unwrap();
                assert_eq!(r.to_rows().unwrap(), data, "{w}x{h} {fmt:?}");
            }
        }
    }

    #[test]
    fn read_rect_matches_rows() {
        let (w, h) = (700, 300);
        let data = gradient(w, h, PixelFormat::Rgba8);
        let r = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data).unwrap();
        let rect = IRect::new(250, 100, 300, 180);
        let mut out = vec![0u8; 300 * 180 * 4];
        r.read_rect(rect, &mut out).unwrap();
        for row in 0..180usize {
            let src = ((100 + row) * w as usize + 250) * 4;
            assert_eq!(&out[row * 1200..(row + 1) * 1200], &data[src..src + 1200]);
        }
        assert!(
            r.read_rect(IRect::new(600, 0, 101, 1), &mut [0u8; 404])
                .is_err()
        );
    }

    #[test]
    fn digest_ignores_tiling_but_sees_content() {
        let data = gradient(300, 300, PixelFormat::Rgb8);
        let a = Raster::from_rows(300, 300, PixelFormat::Rgb8, None, &data).unwrap();
        // 同じ行を自前で書いたもの
        let mut h = Hasher::new("t");
        h.u32(300)
            .u32(300)
            .u8(PixelFormat::Rgb8.code())
            .u8(0)
            .raw(&data);
        let mut h2 = Hasher::new("t");
        a.digest_into(&mut h2);
        assert_eq!(h.finish(), h2.finish());
        let mut changed = data.clone();
        changed[12345] ^= 1;
        let b = Raster::from_rows(300, 300, PixelFormat::Rgb8, None, &changed).unwrap();
        let mut h3 = Hasher::new("t");
        b.digest_into(&mut h3);
        assert_ne!(h2.finish(), h3.finish());
    }

    #[test]
    fn writing_a_tile_copies_only_that_tile() {
        let a = Raster::filled(600, 600, PixelFormat::Gray8, None, &[9]).unwrap();
        let mut b = a.clone();
        b.tile_mut(1, 1)[0] = 1;
        assert!(b.shares_tile(&a, 0, 0));
        assert!(!b.shares_tile(&a, 1, 1));
        assert_eq!(a.pixel(256, 256), Some(&[9u8][..]));
        assert_eq!(b.pixel(256, 256), Some(&[1u8][..]));
        assert_ne!(a, b);
    }

    #[test]
    fn rejects_bad_headers() {
        assert!(Raster::filled(0, 1, PixelFormat::Gray8, None, &[0]).is_err());
        assert!(Raster::filled(1, 1, PixelFormat::Indexed8, None, &[0]).is_err());
        assert!(
            Raster::filled(
                1,
                1,
                PixelFormat::Gray8,
                Some(Palette {
                    colors: vec![[0; 4]]
                }),
                &[0]
            )
            .is_err()
        );
        assert!(Raster::from_rows(2, 2, PixelFormat::Gray8, None, &[0; 3]).is_err());
        let two = Palette {
            colors: vec![[0; 4], [255; 4]],
        };
        assert!(Raster::from_rows(2, 1, PixelFormat::Indexed8, Some(two.clone()), &[0, 1]).is_ok());
        assert!(Raster::from_rows(2, 1, PixelFormat::Indexed8, Some(two), &[0, 2]).is_err());
    }
}
