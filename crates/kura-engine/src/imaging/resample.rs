//! リサイズ（`image.resize`。docs/editing.md §5.2）。式を凍結する（変えるなら操作の版を上げる）。
//!
//! **最近傍**（整数だけ）：出力の x の元の位置は `⌊(2x + 1)·in / (2·out)⌋`（画素の中心どうしを合わせる）。
//! k 倍の拡大は各画素を k×k に、1/k の縮小は k×k の真ん中（偶数なら右下寄り）の画素を選ぶので、k 倍してから 1/k にすると元に戻る（E12）。
//! 形式はそのまま（Indexed8 はパレットのまま）。
//!
//! **box・bilinear・Lanczos3**（浮動小数。`crate::math` の超越関数を使う）：
//! 1. 各画素を線形光の乗算済みの f32 にする。色は sRGB の伝達関数（`v ≤ 0.04045 なら v/12.92、ほかは ((v+0.055)/1.055)^2.4`）を
//!    8 ビット・16 ビットの表で引き、アルファ `a = v / M` を掛ける。アルファのない形式は a = 1。Indexed8 はパレットの色で広げる。
//! 2. 出力の座標 x ごとの重み：倍率 `s = in / out`、`f = max(s, 1)`、中心 `c = (x + 0.5)·s`、範囲 `⌊c − R·f⌋ .. ⌈c + R·f⌉`
//!    （R は box 0.5・bilinear 1・Lanczos3 3）を画像の中に切り、元の画素 i の重みを `K((i + 0.5 − c) / f)` とする（f64）。
//!    K は box：`−0.5 ≤ t < 0.5` で 1、bilinear：`max(0, 1 − |t|)`、Lanczos3：`sinc(t)·sinc(t/3)`（|t| < 3）。
//!    合計で割って f32 にし、両端の重み 0 を落とす。合計が 0 なら、中心にいちばん近い画素の重みを 1 にする。
//!    大きさが変わらない軸は、各画素の重みを 1 だけにする（その軸はそのまま通る）。
//! 3. 横に畳み、縦に畳む。各出力の画素は、横の重みで元の行を畳んだ値 H を、縦の重みの順（元の行の小さい順）に
//!    0.0 から f32 で足していく（`acc += w·H`）。タイル・帯の分け方によらず、同じ順で同じ値を足す。
//! 4. アルファを `[0, 1]` に切り、0 なら全部 0。ほかは色をアルファで割って `[0, 1]` に切り、sRGB に戻す
//!    （戻す値 v は、`decode((v + 0.5)/M)` のしきい値の表で、いちばん近い符号化の値を選ぶ）。アルファは `⌊a·M + 0.5⌋`。
//!    出力の形式は入力のまま（Indexed8 は Rgba8）。

use std::sync::OnceLock;

use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::pixels::{channel_max, is_16, is_gray, read_rgba, write_rgba};
use crate::math;
use crate::monitor::Monitor;
use crate::render::region::Tiles;
use crate::value::{PixelFormat, Raster};

/// リサイズのフィルタ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Box,
    Bilinear,
    Lanczos3,
}

impl Filter {
    /// 重みの範囲の幅 2R（box 1・bilinear 2・Lanczos3 6。作業領域の見積もりに使う整数）。
    fn two_r(self) -> u64 {
        match self {
            Self::Nearest | Self::Box => 1,
            Self::Bilinear => 2,
            Self::Lanczos3 => 6,
        }
    }

    fn support(self) -> f64 {
        match self {
            Self::Nearest | Self::Box => 0.5,
            Self::Bilinear => 1.0,
            Self::Lanczos3 => 3.0,
        }
    }

    fn kernel(self, t: f64) -> f64 {
        match self {
            Self::Nearest | Self::Box => {
                if (-0.5..0.5).contains(&t) {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Bilinear => {
                let v = 1.0 - t.abs();
                if v > 0.0 { v } else { 0.0 }
            }
            Self::Lanczos3 => {
                if t == 0.0 {
                    1.0
                } else if t.abs() < 3.0 {
                    sinc(t) * sinc(t / 3.0)
                } else {
                    0.0
                }
            }
        }
    }
}

fn sinc(x: f64) -> f64 {
    let px = std::f64::consts::PI * x;
    math::sin(px) / px
}

/// 最近傍の元の位置。
pub fn nearest_index(x: u32, inn: u32, out: u32) -> u32 {
    let v = (2 * u64::from(x) + 1) * u64::from(inn) / (2 * u64::from(out));
    (v as u32).min(inn - 1)
}

/// 平滑なフィルタで、出力の座標 `x` が読む入力の範囲（両端の重み 0 を落とす前の範囲。`taps` と同じ浮動小数の式なので、
/// `taps(..)[x]` はいつもこの中に収まる）。大きさが変わらない軸は `[x, x + 1)`。
fn tap_range(inn: u32, out: u32, filter: Filter, x: u32) -> (u32, u32) {
    if inn == out {
        return (x, x + 1);
    }
    let s = f64::from(inn) / f64::from(out);
    let f = if s > 1.0 { s } else { 1.0 };
    let support = filter.support() * f;
    let c = (f64::from(x) + 0.5) * s;
    let lo = (c - support).floor();
    let hi = (c + support).ceil();
    let lo = if lo < 0.0 { 0 } else { lo as u32 };
    let hi = if hi > f64::from(inn) { inn } else { hi as u32 };
    // 合計が 0 のときの「中心にいちばん近い画素」も、この範囲の中にある（lo ≤ ⌊c⌋ < hi）
    let nearest = (c.floor() as u32).min(inn - 1);
    (lo.min(nearest), hi.max(nearest + 1))
}

/// 単調に偽から真になる述語の、最初の真の位置（`0..n` になければ n）。
fn first_true(n: u32, pred: impl Fn(u32) -> bool) -> u32 {
    let (mut lo, mut hi) = (0u32, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// 出力の軸の区間 `[x0, x1)` が読む入力の軸の区間（段 10。範囲の逆写しに使う。保守的）。最近傍は `nearest_index` の
/// 範囲、平滑は各座標の重みの範囲の和。区間は出力の大きさで切り、空なら `(0, 0)`。
pub fn source_span(inn: u32, out: u32, filter: Filter, x0: u32, x1: u32) -> (u32, u32) {
    let x1 = x1.min(out);
    if x0 >= x1 || inn == 0 {
        return (0, 0);
    }
    match filter {
        Filter::Nearest => (
            nearest_index(x0, inn, out),
            nearest_index(x1 - 1, inn, out) + 1,
        ),
        _ => (
            tap_range(inn, out, filter, x0).0,
            tap_range(inn, out, filter, x1 - 1).1,
        ),
    }
}

/// 入力の軸の区間 `[a, b)` を読む出力の軸の区間（段 10。変わった範囲を前へ写すのに使う。保守的）。空なら `(0, 0)`。
pub fn dest_span(inn: u32, out: u32, filter: Filter, a: u32, b: u32) -> (u32, u32) {
    let b = b.min(inn);
    if a >= b || out == 0 {
        return (0, 0);
    }
    // 読む範囲の両端は出力の座標について単調に増えるので、端を 2 分探索で求める
    let (first, end) = match filter {
        Filter::Nearest => (
            first_true(out, |x| nearest_index(x, inn, out) >= a),
            first_true(out, |x| nearest_index(x, inn, out) >= b),
        ),
        _ => (
            first_true(out, |x| tap_range(inn, out, filter, x).1 > a),
            first_true(out, |x| tap_range(inn, out, filter, x).0 >= b),
        ),
    };
    if first < end { (first, end) } else { (0, 0) }
}

/// 最近傍で大きさを変える（形式はそのまま）。
pub fn resize_nearest(
    px: &Raster,
    w: u32,
    h: u32,
    mon: &dyn Monitor,
) -> Result<Raster, EngineError> {
    px.derive(w, h, mon, nearest_tile(px, w, h))
}

/// 最近傍で、`pick` のタイルだけを描いたもの（段 10。範囲の描画。ほかは `base` のタイルか無効なタイル）。
/// 描いたタイルは [`resize_nearest`] と同じ値。
pub fn resize_nearest_picked(
    px: &Raster,
    w: u32,
    h: u32,
    mon: &dyn Monitor,
    pick: &Tiles,
    base: Option<&Raster>,
) -> Result<Raster, EngineError> {
    px.derive_picked(w, h, mon, pick, base, nearest_tile(px, w, h))
}

/// 最近傍の出力のタイル 1 枚を描く閉包（全体の描画と範囲の描画が同じものを使う）。
fn nearest_tile(
    px: &Raster,
    w: u32,
    h: u32,
) -> impl Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send + '_ {
    let (iw, ih, bpp) = (px.width(), px.height(), px.bpp());
    move |t, out| {
        let xs: Vec<u32> = (t.x as u32..t.right() as u32)
            .map(|x| nearest_index(x, iw, w))
            .collect();
        let (x0, x1) = (xs[0], xs[xs.len() - 1]);
        let span = (x1 - x0 + 1) as usize;
        let mut row = vec![0u8; span * bpp];
        let row_len = t.w as usize * bpp;
        let mut last_sy = None;
        for (r, dst) in out.chunks_exact_mut(row_len).enumerate() {
            let sy = nearest_index(t.y as u32 + r as u32, ih, h);
            if last_sy != Some(sy) {
                px.read_rect(
                    IRect::new(i64::from(x0), i64::from(sy), span as u32, 1),
                    &mut row,
                )?;
                last_sy = Some(sy);
            }
            for (c, sx) in xs.iter().enumerate() {
                let s = (sx - x0) as usize * bpp;
                dst[c * bpp..(c + 1) * bpp].copy_from_slice(&row[s..s + bpp]);
            }
        }
        Ok(())
    }
}

/// 出力の座標 1 つの重み。
#[derive(Debug, Clone, PartialEq)]
pub struct Taps {
    pub start: u32,
    pub weights: Vec<f32>,
}

impl Taps {
    fn end(&self) -> u32 {
        self.start + self.weights.len() as u32
    }
}

/// 出力の各座標の重み（§2）。大きさが変わらない軸は、各画素の重みを 1 だけにする（そのまま通す）。
pub fn taps(inn: u32, out: u32, filter: Filter) -> Vec<Taps> {
    if inn == out {
        return (0..out)
            .map(|x| Taps {
                start: x,
                weights: vec![1.0],
            })
            .collect();
    }
    let s = f64::from(inn) / f64::from(out);
    let f = if s > 1.0 { s } else { 1.0 };
    let support = filter.support() * f;
    (0..out)
        .map(|x| {
            let c = (f64::from(x) + 0.5) * s;
            let lo = (c - support).floor();
            let hi = (c + support).ceil();
            let lo = if lo < 0.0 { 0 } else { lo as u32 };
            let hi = if hi > f64::from(inn) { inn } else { hi as u32 };
            let raw: Vec<f64> = (lo..hi)
                .map(|i| filter.kernel((f64::from(i) + 0.5 - c) / f))
                .collect();
            let sum: f64 = raw.iter().fold(0.0, |a, w| a + w);
            if sum == 0.0 || raw.is_empty() {
                let nearest = (c.floor() as u32).min(inn - 1);
                return Taps {
                    start: nearest,
                    weights: vec![1.0],
                };
            }
            let mut weights: Vec<f32> = raw.iter().map(|w| (w / sum) as f32).collect();
            let mut start = lo;
            while weights.len() > 1 && weights[0] == 0.0 {
                weights.remove(0);
                start += 1;
            }
            while weights.len() > 1 && weights[weights.len() - 1] == 0.0 {
                weights.pop();
            }
            Taps { start, weights }
        })
        .collect()
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        math::pow((v + 0.055) / 1.055, 2.4)
    }
}

struct Tables {
    dec: Vec<f32>,
    /// `enc[v] = decode((v + 0.5)/M)`。線形光の値 l は、`enc[v] ≤ l` となる v の数に符号化する。
    enc: Vec<f32>,
}

fn tables(sixteen: bool) -> &'static Tables {
    static T8: OnceLock<Tables> = OnceLock::new();
    static T16: OnceLock<Tables> = OnceLock::new();
    let build = |max: u32| Tables {
        dec: (0..=max)
            .map(|v| srgb_to_linear(f64::from(v) / f64::from(max)) as f32)
            .collect(),
        enc: (0..max)
            .map(|v| srgb_to_linear((f64::from(v) + 0.5) / f64::from(max)) as f32)
            .collect(),
    };
    if sixteen {
        T16.get_or_init(|| build(65_535))
    } else {
        T8.get_or_init(|| build(255))
    }
}

/// しきい値の表で、線形光の値をいちばん近い符号化の値にする（表は狭義に増える）。
fn encode(l: f32, enc: &[f32]) -> u32 {
    let (mut lo, mut hi) = (0usize, enc.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if enc[mid] <= l {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo as u32
}

fn clamp01(x: f32) -> f32 {
    if x > 1.0 {
        1.0
    } else if x >= 0.0 {
        x
    } else {
        0.0
    }
}

/// 平滑なリサイズの出力の形式（Indexed8 は Rgba8、ほかはそのまま）。
pub fn smooth_format(fmt: PixelFormat) -> PixelFormat {
    if fmt == PixelFormat::Indexed8 {
        PixelFormat::Rgba8
    } else {
        fmt
    }
}

/// box・bilinear・Lanczos3 で大きさを変える（§1〜4）。
pub fn resize_smooth(
    px: &Raster,
    w: u32,
    h: u32,
    filter: Filter,
    mon: &dyn Monitor,
) -> Result<Raster, EngineError> {
    smooth(px, w, h, filter, mon, None)
}

/// box・bilinear・Lanczos3 で、`pick` のタイルだけを描いたもの（段 10。範囲の描画。ほかは `base` のタイルか無効なタイル）。
/// 描いたタイルは [`resize_smooth`] と同じ値（同じ閉包で描く）。
pub fn resize_smooth_picked(
    px: &Raster,
    w: u32,
    h: u32,
    filter: Filter,
    mon: &dyn Monitor,
    pick: &Tiles,
    base: Option<&Raster>,
) -> Result<Raster, EngineError> {
    smooth(px, w, h, filter, mon, Some((pick, base)))
}

/// [`resize_smooth`] の本体。`region` があれば、そのタイルだけを描く（同じ閉包）。
fn smooth(
    px: &Raster,
    w: u32,
    h: u32,
    filter: Filter,
    mon: &dyn Monitor,
    region: Option<(&Tiles, Option<&Raster>)>,
) -> Result<Raster, EngineError> {
    let src_fmt = px.format();
    let dst_fmt = smooth_format(src_fmt);
    let gray = is_gray(src_fmt);
    let has_alpha = src_fmt.has_alpha() || src_fmt == PixelFormat::Indexed8;
    // 計算に使うチャンネル（色＋アルファ）
    let nc: usize = if gray { 1 } else { 3 };
    let ch = nc + 1;
    let src_max = channel_max(src_fmt) as f32;
    let dst_max = channel_max(dst_fmt);
    let src_t = tables(is_16(src_fmt));
    let dst_t = tables(is_16(dst_fmt));
    let palette = px.palette().map(|p| p.colors.as_slice());
    let tx = taps(px.width(), w, filter);
    let ty = taps(px.height(), h, filter);
    let bpp = px.bpp();
    let out_bpp = dst_fmt.bytes_per_pixel();
    let tile = |t: IRect, out: &mut [u8]| -> Result<(), EngineError> {
        let xs = &tx[t.x as usize..t.right() as usize];
        let ys = &ty[t.y as usize..t.bottom() as usize];
        let cx0 = xs.iter().map(|k| k.start).min().unwrap_or(0);
        let cx1 = xs.iter().map(Taps::end).max().unwrap_or(0);
        let ry0 = ys.iter().map(|k| k.start).min().unwrap_or(0);
        let ry1 = ys.iter().map(Taps::end).max().unwrap_or(0);
        let cols = (cx1 - cx0) as usize;
        let mut raw = vec![0u8; cols * bpp];
        let mut lin = vec![0f32; cols * ch];
        let mut hrow = vec![0f32; t.w as usize * ch];
        let mut acc = vec![0f32; t.w as usize * t.h as usize * ch];
        for (n, r) in (ry0..ry1).enumerate() {
            if n % 64 == 63 && mon.canceled() {
                return Err(EngineError::Canceled);
            }
            px.read_rect(
                IRect::new(i64::from(cx0), i64::from(r), cols as u32, 1),
                &mut raw,
            )?;
            // 1. 線形光の乗算済みにする
            for (p, l) in raw.chunks_exact(bpp).zip(lin.chunks_exact_mut(ch)) {
                let c = read_rgba(src_fmt, palette, p)?;
                let a = if has_alpha {
                    c[3] as f32 / src_max
                } else {
                    1.0
                };
                for k in 0..nc {
                    l[k] = src_t.dec[c[k] as usize] * a;
                }
                l[nc] = a;
            }
            // 3. 横に畳む
            for (j, k) in xs.iter().enumerate() {
                let o = &mut hrow[j * ch..(j + 1) * ch];
                o.fill(0.0);
                for (i, wgt) in k.weights.iter().enumerate() {
                    let s = (k.start - cx0) as usize + i;
                    for c in 0..ch {
                        o[c] += wgt * lin[s * ch + c];
                    }
                }
            }
            // 縦：この行を使う出力の行に、重みの順で足す
            for (i, k) in ys.iter().enumerate() {
                if r < k.start || r >= k.end() {
                    continue;
                }
                let wgt = k.weights[(r - k.start) as usize];
                let row = &mut acc[i * t.w as usize * ch..(i + 1) * t.w as usize * ch];
                for (a, hv) in row.iter_mut().zip(hrow.iter()) {
                    *a += wgt * hv;
                }
            }
        }
        // 4. 戻す
        for (p, o) in acc.chunks_exact(ch).zip(out.chunks_exact_mut(out_bpp)) {
            let a = clamp01(p[nc]);
            let mut c = [0u32; 4];
            if a > 0.0 {
                for k in 0..nc {
                    c[k] = encode(clamp01(p[k] / a), &dst_t.enc);
                }
                c[3] = (a * dst_max as f32 + 0.5) as u32;
            }
            if gray {
                c[1] = c[0];
                c[2] = c[0];
            }
            if !dst_fmt.has_alpha() {
                c[3] = dst_max;
            }
            write_rgba(dst_fmt, c, o)?;
        }
        Ok(())
    };
    match region {
        None => Raster::from_tile_fn(w, h, dst_fmt, None, mon, tile),
        Some((pick, base)) => Raster::from_tiles_picked(w, h, dst_fmt, None, mon, pick, base, tile),
    }
}

/// [`resize_smooth`] が値のほかに使う作業領域の見積もり（バイト）：(描く間に 1 つだけ持つ分, 出力のタイル 1 枚の仕事ごとの分)。
/// 大きさが `in_w × in_h`（画素 `in_bpp` バイト）の入力を `out_w × out_h` にする。最近傍は (0, 0)（タイルの行だけ）。
/// - 1 つだけ：縦横の重みの表（出力の座標ごとに `Taps` の 32 バイトと、重み 4 バイト × 幅。幅は 2R·max(1, ⌈in/out⌉) + 2 以下）。
/// - タイルごと：縦に畳む acc（TILE² × 4 チャンネル × f32 = 1 MiB）、横に畳んだ 1 行（TILE × 16）、読んだ元の行と線形光の行
///   （列の幅 × (画素のバイト数 + 16)。列の幅は (TILE + 2R)·⌈in/out⌉ + 2 以下で、入力の幅で頭打ち）。
///
/// 段 10 の実測（4096² の乱数の Rgba8 → 2048² の Lanczos3、16 スレッド。ピークから入力と出力の値を引いたもの）：20 MiB
/// （見積もりは 16 スレッドで 17 MiB。差はプールのスレッドの立ち上げなど、操作に依らない分）。
pub fn smooth_work(
    in_w: u64,
    in_h: u64,
    in_bpp: u64,
    out_w: u64,
    out_h: u64,
    filter: Filter,
) -> (u64, u64) {
    if filter == Filter::Nearest {
        return (0, 0);
    }
    let two_r = filter.two_r();
    let ratio = |inn: u64, out: u64| inn.div_ceil(out.max(1)).max(1);
    let table = |inn: u64, out: u64| {
        let width = two_r.saturating_mul(ratio(inn, out)).saturating_add(2);
        out.saturating_mul(width.saturating_mul(4).saturating_add(32))
    };
    let shared = table(in_w, out_w).saturating_add(table(in_h, out_h));
    let t = u64::from(crate::value::TILE);
    let cols = t
        .saturating_add(two_r)
        .saturating_mul(ratio(in_w, out_w))
        .saturating_add(2)
        .min(in_w);
    let per_tile = (t * t * 16)
        .saturating_add(t * 16)
        .saturating_add(cols.saturating_mul(in_bpp.saturating_add(16)));
    (shared, per_tile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    /// 段 10：軸ごとの逆写し（source_span）は、実際の重み・最近傍の位置をすべて含み、前へ写す（dest_span）は、区間を読む
    /// 出力の座標をすべて含む（拡大・縮小・非整数・同じ大きさ・1 画素）。
    #[test]
    fn axis_spans_cover_the_taps() {
        let sizes = [
            (1u32, 7u32),
            (7, 1),
            (517, 333),
            (333, 517),
            (300, 300),
            (256, 513),
            (4096, 1024),
            (10, 30),
        ];
        for (inn, out) in sizes {
            for filter in [
                Filter::Nearest,
                Filter::Box,
                Filter::Bilinear,
                Filter::Lanczos3,
            ] {
                // 出力の座標ごとの実際の読み元
                let reads: Vec<(u32, u32)> = match filter {
                    Filter::Nearest => (0..out)
                        .map(|x| {
                            let i = nearest_index(x, inn, out);
                            (i, i + 1)
                        })
                        .collect(),
                    _ => taps(inn, out, filter)
                        .iter()
                        .map(|t| (t.start, t.end()))
                        .collect(),
                };
                let mut r = crate::determinism::rng(u64::from(inn) * 31 + u64::from(out));
                for _ in 0..40 {
                    let x0 = rand_chacha::rand_core::Rng::next_u32(&mut r) % out;
                    let x1 = x0 + 1 + rand_chacha::rand_core::Rng::next_u32(&mut r) % (out - x0);
                    let (s0, s1) = source_span(inn, out, filter, x0, x1);
                    for (x, (a, b)) in reads.iter().enumerate().take(x1 as usize).skip(x0 as usize)
                    {
                        assert!(s0 <= *a && *b <= s1, "{inn}→{out} {filter:?} x={x}");
                    }
                    let a = rand_chacha::rand_core::Rng::next_u32(&mut r) % inn;
                    let b = a + 1 + rand_chacha::rand_core::Rng::next_u32(&mut r) % (inn - a);
                    let (d0, d1) = dest_span(inn, out, filter, a, b);
                    for (x, (ra, rb)) in reads.iter().enumerate() {
                        if *ra < b && *rb > a {
                            assert!(
                                d0 as usize <= x && x < d1 as usize,
                                "{inn}→{out} {filter:?} [{a},{b}) x={x}"
                            );
                        }
                    }
                }
                assert_eq!(source_span(inn, out, filter, 5, 5), (0, 0));
                assert_eq!(dest_span(inn, out, filter, inn, inn + 3), (0, 0));
            }
        }
    }

    /// 作業領域の見積もりは、実際に持つ重みの表と、タイルの読み元の列の幅以上。
    #[test]
    fn smooth_work_bounds_the_tables_and_columns() {
        for (inn, out) in [(4096u32, 1024u32), (100, 333), (7, 1), (1, 50), (640, 640)] {
            for filter in [Filter::Box, Filter::Bilinear, Filter::Lanczos3] {
                let tx = taps(inn, out, filter);
                let held: u64 = tx.iter().map(|k| 32 + 4 * k.weights.len() as u64).sum();
                let (shared, per_tile) =
                    smooth_work(u64::from(inn), 1, 4, u64::from(out), 1, filter);
                assert!(shared >= held, "{inn}→{out} {filter:?}");
                // タイル 1 枚の読み元の列の幅
                let t = crate::value::TILE as usize;
                for xs in tx.chunks(t) {
                    let lo = xs.iter().map(|k| k.start).min().unwrap();
                    let hi = xs.iter().map(Taps::end).max().unwrap();
                    let cols = u64::from(hi - lo);
                    assert!(per_tile >= 1 << 20);
                    assert!(per_tile - (t * t * 16 + t * 16) as u64 >= cols * (4 + 16));
                }
            }
        }
        assert_eq!(smooth_work(10, 10, 4, 5, 5, Filter::Nearest), (0, 0));
        // 極端な大きさでもあふれない
        let (s, p) = smooth_work(u64::MAX, u64::MAX, 8, 1, 1, Filter::Lanczos3);
        assert!(s > 0 && p > 0);
    }

    #[test]
    fn nearest_round_trips_integer_scales() {
        for (inn, k) in [(5u32, 2u32), (7, 3), (1, 8), (300, 4)] {
            let up: Vec<u32> = (0..inn * k)
                .map(|x| nearest_index(x, inn, inn * k))
                .collect();
            let down: Vec<u32> = (0..inn)
                .map(|x| up[nearest_index(x, inn * k, inn) as usize])
                .collect();
            assert_eq!(down, (0..inn).collect::<Vec<_>>(), "{inn} {k}");
        }
        assert_eq!(nearest_index(0, 4, 2), 1);
        assert_eq!(nearest_index(1, 4, 2), 3);
    }

    #[test]
    fn weights_are_normalized_and_box_averages_blocks() {
        for filter in [Filter::Box, Filter::Bilinear, Filter::Lanczos3] {
            for (inn, out) in [(10u32, 3u32), (3, 10), (17, 17), (1, 5), (5, 1)] {
                for k in taps(inn, out, filter) {
                    let sum: f32 = k.weights.iter().sum();
                    assert!((sum - 1.0).abs() < 1e-5, "{filter:?} {inn}->{out}: {sum}");
                    assert!(k.end() <= inn);
                }
            }
        }
        let b = taps(8, 2, Filter::Box);
        assert_eq!(
            b[0],
            Taps {
                start: 0,
                weights: vec![0.25; 4]
            }
        );
        assert_eq!(
            b[1],
            Taps {
                start: 4,
                weights: vec![0.25; 4]
            }
        );
    }

    #[test]
    fn encode_picks_the_nearest_code() {
        let t = tables(false);
        for v in 0..=255u32 {
            assert_eq!(encode(t.dec[v as usize], &t.enc), v);
        }
        let t16 = tables(true);
        for v in (0..=65_535u32).step_by(251) {
            assert_eq!(encode(t16.dec[v as usize], &t16.enc), v);
        }
    }

    /// 同じ大きさ・一様な色は変わらない。タイルの分け方によらない（大きな画像で、タイルの境目をまたぐ）。
    #[test]
    fn uniform_images_stay_uniform() {
        for filter in [Filter::Box, Filter::Bilinear, Filter::Lanczos3] {
            let px =
                Raster::filled(300, 270, PixelFormat::Rgba8, None, &[200, 100, 50, 255]).unwrap();
            let out = resize_smooth(&px, 517, 130, filter, &NoMonitor).unwrap();
            for y in [0, 64, 129] {
                for x in [0, 255, 256, 516] {
                    assert_eq!(
                        out.pixel(x, y),
                        Some(&[200u8, 100, 50, 255][..]),
                        "{filter:?}"
                    );
                }
            }
            let same = resize_smooth(&px, 300, 270, filter, &NoMonitor).unwrap();
            assert_eq!(same, px);
        }
    }

    /// 透明な画素の色は混ざらない（乗算済みで畳む）。
    #[test]
    fn transparent_colors_do_not_bleed() {
        let data = [255, 0, 0, 255, 0, 255, 0, 0];
        let px = Raster::from_rows(2, 1, PixelFormat::Rgba8, None, &data).unwrap();
        let out = resize_smooth(&px, 1, 1, Filter::Box, &NoMonitor).unwrap();
        assert_eq!(out.pixel(0, 0), Some(&[255u8, 0, 0, 128][..]));
    }
}
