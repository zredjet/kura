//! 画素の実体（docs/editing.md §5.2）。256×256 の CoW のタイルで持つ。
//!
//! タイルは `Arc` で共有し、書き換えるときだけ複製する（ペン・塗りつぶしは触れたタイルだけ）。
//! 取り消しの履歴とキャッシュがタイルを共有するので、メモリが増えにくい。
//! 並び・ダイジェスト・等しさはタイルの分け方によらない（行を左から順に読んだもの）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::determinism::par_blocks;
use crate::digest::Hasher;
use crate::error::EngineError;
use crate::geom::IRect;
use crate::monitor::Monitor;
use crate::render::region::Tiles;
use crate::value::pixel::{Palette, PixelFormat};

/// タイルの 1 辺。
pub const TILE: u32 = 256;

/// ダイジェストで行を詰める帯の大きさの上限（バイト）。足し方だけを決め、値に効かない。幅が最大（32,768）の Rgba16 でも
/// 一時の領域がこれを超えない（1 行がこれを超えるときだけ 1 行の帯）。
pub const DIGEST_BAND_BYTES: usize = 4 << 20;

/// ダイジェストの帯の行の数（max(1, 帯の上限 ÷ 行のバイト数)）。
pub fn digest_band_rows(row_len: usize) -> usize {
    (DIGEST_BAND_BYTES / row_len.max(1)).max(1)
}

/// 画素の実体。幅・高さは 1 以上。
#[derive(Debug, Clone)]
pub struct Raster {
    w: u32,
    h: u32,
    fmt: PixelFormat,
    palette: Option<Arc<Palette>>,
    /// タイルを行の順に並べたもの（`ty * tiles_x + tx`）。各タイルは行を詰めて並べる。
    tiles: Vec<Arc<Vec<u8>>>,
    /// 範囲の描画（段 10。`render::region`）で描かなかった無効なタイル。全体を描いた値は None。
    holes: Option<Arc<Holes>>,
}

/// 無効なタイルの集合（範囲の描画で描かなかったタイル）。中身は仮のもの（0 か古い値）で、読むと `read` が立つ
/// （`read_rect` は失敗する）。範囲の描画が申告した依存（Reach）の外を読んだことを、実行器とテストが見つけるため。
#[derive(Debug)]
pub(crate) struct Holes {
    /// タイルの番号（`ty * tiles_x + tx`）ごとのビット（1 が無効）。
    bits: Vec<u64>,
    /// 無効なタイルを読んだ（中身を見た）。
    read: AtomicBool,
}

impl Holes {
    fn new(count: usize) -> Self {
        Self {
            bits: vec![0; count.div_ceil(64)],
            read: AtomicBool::new(false),
        }
    }

    fn get(&self, i: usize) -> bool {
        self.bits
            .get(i / 64)
            .is_some_and(|w| (w >> (i % 64)) & 1 == 1)
    }

    fn set(&mut self, i: usize) {
        if let Some(w) = self.bits.get_mut(i / 64) {
            *w |= 1 << (i % 64);
        }
    }

    fn any(&self) -> bool {
        self.bits.iter().any(|w| *w != 0)
    }

    /// タイル i が無効なら、読んだことを記録して true。
    fn touch(&self, i: usize) -> bool {
        if self.get(i) {
            self.read.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
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

    /// タイルの中身（行を詰めたもの）。無効なタイル（範囲の描画で描かなかったもの）は仮の中身を返し、読んだことを記録する。
    pub fn tile(&self, tx: u32, ty: u32) -> &[u8] {
        let i = (ty * self.tiles_x() + tx) as usize;
        self.touch(i);
        &self.tiles[i]
    }

    /// タイルを書き換える（共有していれば複製してから）。
    pub fn tile_mut(&mut self, tx: u32, ty: u32) -> &mut [u8] {
        let i = (ty * self.tiles_x() + tx) as usize;
        self.touch(i);
        Arc::make_mut(&mut self.tiles[i]).as_mut_slice()
    }

    /// タイル i が無効なら、読んだことを記録して true。
    fn touch(&self, i: usize) -> bool {
        self.holes.as_ref().is_some_and(|h| h.touch(i))
    }

    /// すべてのタイルが有効（範囲の描画で作った値でない）なら true。
    pub fn is_complete(&self) -> bool {
        self.holes.is_none()
    }

    /// タイル (tx, ty) が無効（範囲の描画で描かなかったもの）なら true。範囲の外は false。
    pub fn is_hole(&self, tx: u32, ty: u32) -> bool {
        tx < self.tiles_x()
            && ty < self.tiles_y()
            && self
                .holes
                .as_ref()
                .is_some_and(|h| h.get((ty * self.tiles_x() + tx) as usize))
    }

    /// 無効なタイルを読んだことがあれば true（範囲の描画が申告した依存の外を読んだ印）。
    pub fn holes_read(&self) -> bool {
        self.holes
            .as_ref()
            .is_some_and(|h| h.read.load(Ordering::Relaxed))
    }

    /// 有効なタイルの集合（タイルの 1 辺は [`TILE`]）。
    pub fn valid_tiles(&self) -> Tiles {
        let mut t = Tiles::all(self.w, self.h, TILE);
        if let Some(h) = &self.holes {
            for ty in 0..self.tiles_y() {
                for tx in 0..self.tiles_x() {
                    if h.get((ty * self.tiles_x() + tx) as usize) {
                        t.remove(tx, ty);
                    }
                }
            }
        }
        t
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
            holes: None,
        })
    }

    /// 同じ形式・パレットで、別の大きさの画像をタイルごとに作る（操作の出力に使う）。
    pub fn derive<F>(&self, w: u32, h: u32, mon: &dyn Monitor, f: F) -> Result<Self, EngineError>
    where
        F: Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send,
    {
        Self::from_tile_fn(w, h, self.fmt, self.palette.as_deref().cloned(), mon, f)
    }

    /// [`Raster::derive`] の範囲の描画（段 10）：`pick` のタイルだけを `f` で作り、ほかは `base` のタイルか無効なタイルにする
    /// （[`Raster::from_tiles_picked`]）。
    pub fn derive_picked<F>(
        &self,
        w: u32,
        h: u32,
        mon: &dyn Monitor,
        pick: &Tiles,
        base: Option<&Raster>,
        f: F,
    ) -> Result<Self, EngineError>
    where
        F: Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send,
    {
        Self::from_tiles_picked(
            w,
            h,
            self.fmt,
            self.palette.as_deref().cloned(),
            mon,
            pick,
            base,
            f,
        )
    }

    /// 範囲の描画（段 10）：`pick`（1 辺 [`TILE`] のこの大きさのタイルの集合）のタイルだけを `f` で並列に作る（`from_tile_fn`
    /// と同じ閉包なら、同じタイルは同じ値）。ほかのタイルは `base`（大きさ・形式・パレットが同じ前の値）のタイルを共有するか、
    /// 無効なタイル（0 の仮のタイルを大きさごとに 1 つ作って共有する）にする。`base` の無効なタイルは無効のまま。
    #[allow(clippy::too_many_arguments)]
    pub fn from_tiles_picked<F>(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<Palette>,
        mon: &dyn Monitor,
        pick: &Tiles,
        base: Option<&Raster>,
        f: F,
    ) -> Result<Self, EngineError>
    where
        F: Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send,
    {
        Self::check_header(w, h, fmt, palette.as_ref())?;
        Self::check_pick(w, h, pick)?;
        let palette = palette.map(Arc::new);
        if let Some(b) = base
            && (b.w != w || b.h != h || b.fmt != fmt || b.palette != palette)
        {
            return Err(EngineError::Internal(
                "範囲の描画の前の値の大きさ・形式・パレットが違います".into(),
            ));
        }
        let (tx_n, _) = Self::tiles_of(w, h);
        let bpp = fmt.bytes_per_pixel();
        let picked: Vec<(u32, u32)> = pick.iter().collect();
        let made = par_blocks(picked.len(), mon, |k| {
            let (tx, ty) = picked[k];
            let rect = tile_rect(w, h, tx, ty);
            let mut buf = alloc_zeroed(rect.w as usize * rect.h as usize * bpp)?;
            f(rect, &mut buf)?;
            if let Some(p) = &palette
                && buf.iter().any(|b| usize::from(*b) >= p.colors.len())
            {
                return Err(EngineError::Internal(
                    "パレットにない番号の画素があります".into(),
                ));
            }
            Ok(Arc::new(buf))
        })?;
        let mut made = made.into_iter();
        Self::assemble(w, h, fmt, palette, tx_n, base, |tx, ty| {
            if pick.contains(tx, ty) {
                made.next()
            } else {
                None
            }
        })
    }

    /// 範囲の描画（段 10）：`pick` のタイルは `src`（中身をそのまま使う操作の入力）のタイルを共有し、ほかは `base`（大きさ・
    /// 形式が同じ前の値）のタイルか無効なタイルにする。パレットは `src` のもの。`src` の選んだタイルが無効なら `Internal`。
    pub fn splice(src: &Raster, pick: &Tiles, base: Option<&Raster>) -> Result<Self, EngineError> {
        Self::check_pick(src.w, src.h, pick)?;
        if let Some(b) = base
            && (b.w != src.w || b.h != src.h || b.fmt != src.fmt)
        {
            return Err(EngineError::Internal(
                "範囲の描画の前の値の大きさ・形式が違います".into(),
            ));
        }
        let tx_n = src.tiles_x();
        for (tx, ty) in pick.iter() {
            if src.touch((ty * tx_n + tx) as usize) {
                return Err(hole_error());
            }
        }
        Self::assemble(
            src.w,
            src.h,
            src.fmt,
            src.palette.clone(),
            tx_n,
            base,
            |tx, ty| {
                pick.contains(tx, ty)
                    .then(|| src.tiles[(ty * tx_n + tx) as usize].clone())
            },
        )
    }

    /// 同じ画素のまま、`tiles`（1 辺 [`TILE`]）のタイルを無効にした値（中身は共有したまま。読むと記録される）。
    pub fn invalidate(&self, tiles: &Tiles) -> Self {
        let mut holes = Holes::new(self.tiles.len());
        if let Some(h) = &self.holes {
            holes.bits.clone_from(&h.bits);
        }
        let tx_n = self.tiles_x();
        for (tx, ty) in tiles.iter() {
            if tx < tx_n && ty < self.tiles_y() {
                holes.set((ty * tx_n + tx) as usize);
            }
        }
        Self {
            holes: holes.any().then(|| Arc::new(holes)),
            ..self.clone()
        }
    }

    /// 選んだタイルの集合が、この大きさのタイルの格子と合うか。
    fn check_pick(w: u32, h: u32, pick: &Tiles) -> Result<(), EngineError> {
        if pick.size() != TILE || pick.width() != w || pick.height() != h {
            return Err(EngineError::Internal(format!(
                "範囲の描画のタイルの格子が値と合いません（{}×{}、1 辺 {}）",
                pick.width(),
                pick.height(),
                pick.size()
            )));
        }
        Ok(())
    }

    /// タイルを並べて値にする：`made(tx, ty)` が返すタイルを使い、なければ `base` のタイル（`base` の無効は無効のまま）、
    /// それもなければ無効な仮のタイル。
    fn assemble(
        w: u32,
        h: u32,
        fmt: PixelFormat,
        palette: Option<Arc<Palette>>,
        tx_n: u32,
        base: Option<&Raster>,
        mut made: impl FnMut(u32, u32) -> Option<Arc<Vec<u8>>>,
    ) -> Result<Self, EngineError> {
        let (_, ty_n) = Self::tiles_of(w, h);
        let count = tx_n as usize * ty_n as usize;
        let bpp = fmt.bytes_per_pixel();
        let mut holes = Holes::new(count);
        let mut blanks: Vec<(Blank, Arc<Vec<u8>>)> = Vec::new();
        let mut tiles = Vec::with_capacity(count);
        for ty in 0..ty_n {
            for tx in 0..tx_n {
                let i = (ty * tx_n + tx) as usize;
                if let Some(t) = made(tx, ty) {
                    tiles.push(t);
                    continue;
                }
                if let Some(b) = base {
                    tiles.push(b.tiles[i].clone());
                    if b.holes.as_ref().is_some_and(|bh| bh.get(i)) {
                        holes.set(i);
                    }
                    continue;
                }
                let rect = tile_rect(w, h, tx, ty);
                let key = (rect.w, rect.h);
                let blank = match blanks.iter().find(|(k, _)| *k == key) {
                    Some((_, t)) => t.clone(),
                    None => {
                        let t = Arc::new(alloc_zeroed(rect.w as usize * rect.h as usize * bpp)?);
                        blanks.push((key, t.clone()));
                        t
                    }
                };
                tiles.push(blank);
                holes.set(i);
            }
        }
        Ok(Self {
            w,
            h,
            fmt,
            palette,
            tiles,
            holes: holes.any().then(|| Arc::new(holes)),
        })
    }

    /// `pick` が真のタイルだけを、並列に書き換える。ほかと共有しているタイルだけを複製し（CoW）、
    /// この画像だけが持つタイルはその場で書き換える。`f` はタイルの矩形と中身を受け取って書き換える。
    /// 結果はスレッド数によらない。
    pub fn update_tiles<P, F>(
        &mut self,
        mon: &dyn Monitor,
        pick: P,
        f: F,
    ) -> Result<(), EngineError>
    where
        P: Fn(IRect) -> bool + Sync + Send,
        F: Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send,
    {
        let (w, h) = (self.w, self.h);
        let tx_n = self.tiles_x();
        let palette_len = self.palette.as_ref().map(|p| p.colors.len());
        let holes = self.holes.clone();
        // 各タイルを 1 つのスレッドだけが触るように、タイルごとの鍵に入れて渡す（鍵の取り合いは起きない）
        let cells: Vec<std::sync::Mutex<Arc<Vec<u8>>>> = std::mem::take(&mut self.tiles)
            .into_iter()
            .map(std::sync::Mutex::new)
            .collect();
        let result = par_blocks(cells.len(), mon, |i| {
            let (tx, ty) = (i as u32 % tx_n, i as u32 / tx_n);
            let rect = tile_rect(w, h, tx, ty);
            if !pick(rect) {
                return Ok(());
            }
            if let Some(h) = &holes {
                h.touch(i);
            }
            let mut guard = cells[i]
                .lock()
                .map_err(|_| EngineError::Internal("タイルの鍵が壊れています".into()))?;
            let buf = Arc::make_mut(&mut guard);
            f(rect, buf)?;
            if let Some(n) = palette_len
                && buf.iter().any(|b| usize::from(*b) >= n)
            {
                return Err(EngineError::Internal(
                    "パレットにない番号の画素があります".into(),
                ));
            }
            Ok(())
        });
        let mut tiles = Vec::with_capacity(cells.len());
        for c in cells {
            tiles.push(
                c.into_inner()
                    .map_err(|_| EngineError::Internal("タイルの鍵が壊れています".into()))?,
            );
        }
        // 途中で失敗しても、タイルは元の位置に戻す（書きかけのタイルが残るので、呼ぶ側は失敗した画像を捨てる）
        self.tiles = tiles;
        result.map(|_| ())
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
                if self.touch((ty * self.tiles_x() + tx) as usize) {
                    return Err(hole_error());
                }
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

    /// 1 画素（画像の中）。無効なタイルの画素は仮の中身を返し、読んだことを記録する（`tile` を通る）。
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
    ///
    /// 足すバイト列は行を左から順に読んだもので、足し方（何バイトずつ渡すか）は値に効かない（BLAKE3 は流れのハッシュ）。
    /// 速くするために、行を [`DIGEST_BAND_BYTES`] までの帯に詰めてからまとめて足す（細切れに渡すと BLAKE3 の SIMD の
    /// 道に乗らない）。幅が 1 タイルに収まる画像は、タイルの中身が行を詰めたものなのでそのまま足す。
    pub fn digest_into(&self, h: &mut Hasher) {
        // 無効なタイルのある値は、読んだことにする（`Value::digest` は先に断る）
        if let Some(holes) = &self.holes {
            holes.read.store(true, Ordering::Relaxed);
        }
        self.digest_header(h);
        if self.tiles_x() == 1 {
            for tile in &self.tiles {
                h.raw(tile);
            }
            return;
        }
        let row_len = self.w as usize * self.bpp();
        self.digest_bands_into(h, digest_band_rows(row_len));
    }

    /// 行を `rows` 行ずつの帯に詰めて足す（頭は足さない）。
    fn digest_bands_into(&self, h: &mut Hasher, rows: usize) {
        let row_len = self.w as usize * self.bpp();
        let rows = rows.max(1);
        let mut band = Vec::new();
        // 帯の領域を取れなければ、行ごとに足す（値は同じ）
        if band
            .try_reserve_exact(row_len.saturating_mul(rows))
            .is_err()
        {
            self.digest_rows_into(h, 0, self.h);
            return;
        }
        let step = u32::try_from(rows).unwrap_or(u32::MAX);
        let mut y = 0;
        while y < self.h {
            let end = self.h.min(y.saturating_add(step));
            band.clear();
            for row in y..end {
                self.extend_row(&mut band, row);
            }
            h.raw(&band);
            y = end;
        }
    }

    /// ダイジェストの頭（大きさ・形式・パレット）。
    fn digest_header(&self, h: &mut Hasher) {
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
    }

    /// 行 `y0..y1` を、タイルの行ごとに足す（帯に詰める前の足し方。帯を取れないときと、照合のテストに使う）。
    fn digest_rows_into(&self, h: &mut Hasher, y0: u32, y1: u32) {
        let bpp = self.bpp();
        for y in y0..y1 {
            let ty = y / TILE;
            for tx in 0..self.tiles_x() {
                let tr = self.tile_rect(tx, ty);
                let stride = tr.w as usize * bpp;
                let start = (y - tr.y as u32) as usize * stride;
                h.raw(&self.tile(tx, ty)[start..start + stride]);
            }
        }
    }

    /// 行 `y` を左から順に `out` の後ろに足す。
    fn extend_row(&self, out: &mut Vec<u8>, y: u32) {
        let bpp = self.bpp();
        let ty = y / TILE;
        for tx in 0..self.tiles_x() {
            let tr = self.tile_rect(tx, ty);
            let stride = tr.w as usize * bpp;
            let start = (y - tr.y as u32) as usize * stride;
            out.extend_from_slice(&self.tile(tx, ty)[start..start + stride]);
        }
    }

    /// タイルの実体ごとの（番地, バイト数）を `out` に足す。番地は `Arc` の指す先で、値には効かない（キャッシュが共有を
    /// 数えるため）。番地は、そのタイルを持つ値が生きている間だけ意味を持つ。
    pub(crate) fn chunks(&self, out: &mut Vec<(usize, u64)>) {
        for t in &self.tiles {
            out.push((Arc::as_ptr(t).addr(), t.len() as u64));
        }
    }

    /// 同じ画素のまま、パレットだけを替えた画像（タイルはそのまま共有する）。Indexed8 で、番号がすべて新しい
    /// パレットの色の数より小さいときだけ（そうでなければ `Internal`）。
    pub fn with_palette(&self, palette: Palette, mon: &dyn Monitor) -> Result<Self, EngineError> {
        Self::check_header(self.w, self.h, self.fmt, Some(&palette))?;
        if palette.colors.len() < self.palette.as_ref().map_or(0, |p| p.colors.len()) {
            // 色を減らすときは、使っている番号が収まるかを確かめる
            let n = palette.colors.len();
            let hole = |i: usize| self.holes.as_ref().is_some_and(|h| h.get(i));
            par_blocks(self.tiles.len(), mon, |i| {
                if !hole(i) && self.tiles[i].iter().any(|b| usize::from(*b) >= n) {
                    Err(EngineError::Internal(
                        "パレットにない番号の画素があります".into(),
                    ))
                } else {
                    Ok(())
                }
            })?;
        }
        Ok(Self {
            w: self.w,
            h: self.h,
            fmt: self.fmt,
            palette: Some(Arc::new(palette)),
            tiles: self.tiles.clone(),
            holes: self.holes.clone(),
        })
    }
}

/// 仮のタイルの大きさ（幅, 高さ）。
type Blank = (u32, u32);

/// 無効なタイルを読んだときの誤り。
fn hole_error() -> EngineError {
    EngineError::Internal("範囲の描画で、無効なタイル（申告した依存の外）を読みました".into())
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
            holes: None,
        })
    }
}

impl PartialEq for Raster {
    fn eq(&self, other: &Self) -> bool {
        self.w == other.w
            && self.h == other.h
            && self.fmt == other.fmt
            && self.palette == other.palette
            && self.holes.as_ref().map(|h| &h.bits) == other.holes.as_ref().map(|h| &h.bits)
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

    /// 帯に詰めて足しても、行をタイルごとに足した旧い足し方と同じダイジェスト（ChaCha8 で選んだ大きさ・形式・帯の行の数。
    /// 端のタイル・1 タイルの幅・帯の端が画像の端と合わないものを含む）。
    #[test]
    fn banded_digest_equals_row_by_row() {
        use rand_chacha::rand_core::Rng;
        let mut r = crate::determinism::rng(36);
        let old = |px: &Raster| {
            let mut h = Hasher::new("t");
            px.digest_header(&mut h);
            px.digest_rows_into(&mut h, 0, px.h);
            h.finish()
        };
        let mut cases: Vec<(u32, u32, PixelFormat)> = vec![
            (1, 1, PixelFormat::Gray8),
            (256, 3, PixelFormat::Rgba8),
            (257, 1, PixelFormat::Rgba16),
            (513, 300, PixelFormat::Rgb8),
        ];
        for _ in 0..24 {
            let w = 1 + r.next_u32() % 800;
            let h = 1 + r.next_u32() % 600;
            let fmt = PixelFormat::ALL[(r.next_u32() % 8) as usize];
            cases.push((w, h, fmt));
        }
        for (w, h, fmt) in cases {
            let mut data = vec![0u8; w as usize * h as usize * fmt.bytes_per_pixel()];
            r.fill_bytes(&mut data);
            let px = Raster::from_rows(w, h, fmt, None, &data).unwrap();
            let want = old(&px);
            let mut got = Hasher::new("t");
            px.digest_into(&mut got);
            assert_eq!(got.finish(), want, "{w}x{h} {fmt:?}");
            for rows in [1usize, 2, 7, 255, 256, 257, h as usize, h as usize + 3] {
                let mut banded = Hasher::new("t");
                px.digest_header(&mut banded);
                px.digest_bands_into(&mut banded, rows);
                assert_eq!(banded.finish(), want, "{w}x{h} {fmt:?} {rows} 行");
            }
        }
        // パレットの画像も同じ
        let pal = Palette {
            colors: (0..=255u8).map(|i| [i, 0, 255 - i, 255]).collect(),
        };
        let mut data = vec![0u8; 700 * 9];
        r.fill_bytes(&mut data);
        let px = Raster::from_rows(700, 9, PixelFormat::Indexed8, Some(pal), &data).unwrap();
        let mut got = Hasher::new("t");
        px.digest_into(&mut got);
        assert_eq!(got.finish(), old(&px));
        // 帯の行の数：4 MiB ÷ 行のバイト数、最低 1 行
        assert_eq!(digest_band_rows(4096 * 4), 256);
        assert_eq!(digest_band_rows(32_768 * 8), 16);
        assert_eq!(digest_band_rows(DIGEST_BAND_BYTES + 1), 1);
        assert_eq!(digest_band_rows(0), DIGEST_BAND_BYTES);
    }

    /// パレットだけを替えた画像はタイルを共有し、行を作り直したものと等しい。色を減らすときは番号を確かめる。
    #[test]
    fn with_palette_shares_tiles() {
        let pal = |n: u8| Palette {
            colors: (0..n).map(|i| [i, i, i, 255]).collect(),
        };
        let data: Vec<u8> = (0..600 * 300).map(|i| (i % 7) as u8).collect();
        let a = Raster::from_rows(600, 300, PixelFormat::Indexed8, Some(pal(8)), &data).unwrap();
        let mut colors = pal(8);
        colors.colors[3] = [1, 2, 3, 4];
        let b = a
            .with_palette(colors.clone(), &crate::monitor::NoMonitor)
            .unwrap();
        let rebuilt =
            Raster::from_rows(600, 300, PixelFormat::Indexed8, Some(colors), &data).unwrap();
        assert_eq!(b, rebuilt);
        for (tx, ty) in [(0, 0), (2, 1)] {
            assert!(b.shares_tile(&a, tx, ty));
        }
        let mut h1 = Hasher::new("t");
        b.digest_into(&mut h1);
        let mut h2 = Hasher::new("t");
        rebuilt.digest_into(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
        // 番号 6 を使っているので 6 色には減らせない。7 色なら減らせる
        assert!(a.with_palette(pal(6), &crate::monitor::NoMonitor).is_err());
        assert!(a.with_palette(pal(7), &crate::monitor::NoMonitor).is_ok());
        // パレットのない形式には付けられない
        let rgb = Raster::filled(2, 2, PixelFormat::Rgb8, None, &[1, 2, 3]).unwrap();
        assert!(
            rgb.with_palette(pal(2), &crate::monitor::NoMonitor)
                .is_err()
        );
    }

    #[test]
    fn update_tiles_copies_only_picked_tiles() {
        let a = Raster::filled(600, 300, PixelFormat::Gray8, None, &[9]).unwrap();
        let mut b = a.clone();
        b.update_tiles(
            &crate::monitor::NoMonitor,
            |r| r.x == 256,
            |_, buf| {
                buf.fill(1);
                Ok(())
            },
        )
        .unwrap();
        assert!(b.shares_tile(&a, 0, 0) && b.shares_tile(&a, 2, 1));
        assert!(!b.shares_tile(&a, 1, 0) && !b.shares_tile(&a, 1, 1));
        assert_eq!(b.pixel(300, 299), Some(&[1u8][..]));
        assert_eq!(b.pixel(255, 0), Some(&[9u8][..]));
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
