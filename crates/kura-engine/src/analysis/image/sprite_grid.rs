//! `image.sprite_grid` v1（**表示だけ**の推定。操作は申告しない。版を上げたら旧版の実装を消して `dropped` に載せてよい）：
//! スプライトシートの規則的な格子を、空の溝（透明か、四隅と同じ背景色）から推定する。
//!
//! 定義（凍結する）：
//! - 受けるのは 1 枚の画像だけ（フレームの列は受けない。フレームの外の扱いは持たない）。
//! - **空**：画像に透明な画素（アルファ 0。Indexed8 はパレットのアルファ 0）が 1 つでもあれば透明な画素。なければ、
//!   四隅の画素の生の値（Indexed8 はパレットの色）が同じならその値の画素（`background` は `color`。色は 8 ビットの色）。
//!   どちらでもなければ推定しない（`background` は `none`、`best` は null）。空でない画素を**中身**と呼ぶ。
//! - 軸ごとに分けて解く（縦も同じ）。`occ[x]` は列 x の中身の画素の数、`cut[x]`（1 ≤ x < 幅）は x − 1 と x のあいだを
//!   切ってよいか（列 x − 1 の中身と、列 x の同じ行・上下 1 行の中身が隣り合う行がない＝ 8 連結でまたがない）。
//! - 周期 P（2 ≤ P ≤ min(幅 − 1, `MAX_PERIOD`)。間隔のある 2 つのセルでは P が幅の半分を超える）ごとに、列を P で
//!   畳んだ中身 `F[j] = Σ occ[x ≡ j]` から 2 種類の格子を作る：
//!   (a) 詰めた格子：F が 0 の剰余の、輪で最も長い連なり（同じ長さなら始まりの小さいもの）を間隔 g、残りをセル
//!   c = P − g、セルの始まりの剰余を offset にする。(b) 周期の格子（間隔 0）：すべての x ≡ o で `cut[x]` が真の剰余 o
//!   ごとに (o, P, 0)。格子が成り立つのは、セルが 1 つ以上画像に入り（`sprite.split_grid` と同じ数え方）、中身が
//!   すべてセルの中にあり（余白・間隔・余りに中身がない）、間隔 0 なら内側の境目がすべて `cut` のとき。
//! - 選び方（整数の辞書順。↓ は大きいほど先）：周期ごとに（中身のあるセルの数 ↓、セルの数、当てはまり方（0 余白なし・
//!   1 前後の余白が同じ・2 ほか）、表し方（詰めた格子のセルが「いっぱい」＝中身がセルの両端の列に接するセルが中身の
//!   あるセルの半分以上なら詰めた格子を先に、でなければ周期の格子を先に）、offset）で 1 つ。周期をまたいで（中身のある
//!   セルの数 ↓、セルの数、整った余白か（当てはまり方が 0・1 なら先）、畳んだときの空の割合 g / P ↓、当てはまり方、
//!   offset、詰めた中身の幅 P − g、P）で並べる（`rank`）。中身のあるセルが 2 つ以上のものだけを使い、なければその軸は分けない
//!   （offset 0・セルは全体・間隔 0）。
//! - 2 軸を合わせる：両軸の最上位（中身のあるセル・セルの数・当てはまり方が最良と同じもの）に同じ周期があれば、
//!   正方形のセルを選ぶ。`alternatives` は、片方の軸だけを次の順位の格子に替えたもの（横・縦を交互に。`MAX_ALTERNATIVES`
//!   まで）。
//! - 溝から両軸とも分けられず、長い辺が短い辺のちょうど m 倍（m ≥ 2）で、長い辺の向きのどの列にも中身があれば
//!   （溝のない帯）、短い辺の正方形の格子を `source: "square"` の候補にする（確からしさは低い）。
//! - どの候補も、`sprite.split_grid` の `grid` にそのまま渡せて、中身の画素を 1 つも落とさない（中身はすべてセルの中）。
//!   `filled` はセル（2 次元）のうち中身のあるものの数（透明が空なら `split_grid` の `skipEmpty` で残るセルの数）。
//! - 不規則な配置（詰め込んだアトラス）・溝のないタイルセットは見つけない（null）。
//! - 整数だけ（`Numeric::Exact`）。走査はタイルごとに並列（隣の 1 画素を読む）、当てはめは周期ごとに並列。

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};

use crate::analysis::image::{filled_vec, raw_key, shape_pixels, tile_at, tile_count};
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate, NoParams};
use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::convert::for_each_rgba8_key;
use crate::imaging::pixels::{alpha_at_most, can_be_transparent};
use crate::imaging::quantize::unpack;
use crate::op::Numeric;
use crate::ops::sprite::split_grid::cells_on_axis;
use crate::ops::util::GridParam;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Raster, Value};

/// 当てはめる周期の上限（画素）。凍結する。
pub const MAX_PERIOD: u32 = 4_096;
/// `alternatives` の数の上限。凍結する。
pub const MAX_ALTERNATIVES: usize = 3;

/// 空とみなした画素。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum GridBackground {
    /// 透明な画素。
    Transparent,
    /// 四隅と同じ値の画素（色は 8 ビットの色）。
    Color { rgba: [u8; 4] },
    /// 空が決まらない（推定しない）。
    None,
}

/// 格子の当てはまり方（両軸のうち悪いほう）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GridFit {
    /// 余白なしで敷き詰める。
    Exact,
    /// 前後の余白が同じ。
    Symmetric,
    /// ほか。
    Loose,
}

/// 候補の根拠。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GridSource {
    /// 空の溝から。
    Gutter,
    /// 溝のない帯を短い辺の正方形で切る（確からしさは低い）。
    Square,
}

/// 格子の候補。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GridGuess {
    /// `sprite.split_grid` の `grid` にそのまま渡せる格子。
    pub grid: GridParam,
    pub cols: u32,
    pub rows: u32,
    /// 中身のあるセルの数。
    pub filled: u32,
    pub fit: GridFit,
    pub source: GridSource,
}

/// 結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpriteGridOutput {
    pub background: GridBackground,
    /// いちばんの候補（見つからなければ null）。
    pub best: Option<GridGuess>,
    /// ほかの候補（`MAX_ALTERNATIVES` まで。順位の順）。
    pub alternatives: Vec<GridGuess>,
}
crate::json_output!(SpriteGridOutput);

/// 空の判定。
enum Empty {
    /// アルファのバイトの位置と 16 ビットか（透明が空）。
    Alpha { at: usize, wide: bool },
    /// 生の値の鍵（`raw_key`）がこれと同じ画素が空。
    Raw(u64),
    /// Indexed8：番号ごとに空か。
    Table(Vec<bool>),
}

impl Empty {
    /// 画素を並べた `src` の中身の印（1 が中身）を `out` に書く（形式の分岐はループの外）。
    fn mask(&self, src: &[u8], bpp: usize, out: &mut Vec<u8>) {
        out.clear();
        match self {
            Self::Alpha { at, wide: false } => {
                out.extend(src.chunks_exact(bpp).map(|p| u8::from(p[*at] != 0)));
            }
            Self::Alpha { at, wide: true } => {
                out.extend(
                    src.chunks_exact(bpp)
                        .map(|p| u8::from((p[*at] | p[*at + 1]) != 0)),
                );
            }
            Self::Raw(bg) => out.extend(src.chunks_exact(bpp).map(|p| u8::from(raw_key(p) != *bg))),
            Self::Table(t) => out.extend(src.iter().map(|i| u8::from(!t[usize::from(*i)]))),
        }
    }
}

/// 透明な画素が 1 つでもあるか。
fn has_clear(px: &Raster, cx: &AnalyzeCx<'_>) -> Result<bool, EngineError> {
    let fmt = px.format();
    if !can_be_transparent(fmt) {
        return Ok(false);
    }
    let palette = px.palette().map(|p| p.colors.as_slice());
    let bpp = px.bpp();
    let parts = cx.par_blocks(tile_count(px), |i| {
        let (_, tile) = tile_at(px, i);
        Ok(tile
            .chunks_exact(bpp)
            .any(|p| alpha_at_most(fmt, palette, p, 0)))
    })?;
    Ok(parts.into_iter().any(|b| b))
}

/// 空の決め方と `background`。
fn find_empty(
    px: &Raster,
    cx: &AnalyzeCx<'_>,
) -> Result<Option<(Empty, GridBackground)>, EngineError> {
    let fmt = px.format();
    let palette = px.palette().map(|p| p.colors.as_slice());
    if has_clear(px, cx)? {
        let empty = match fmt {
            PixelFormat::GrayA8 => Empty::Alpha { at: 1, wide: false },
            PixelFormat::Rgba8 => Empty::Alpha { at: 3, wide: false },
            PixelFormat::GrayA16 => Empty::Alpha { at: 2, wide: true },
            PixelFormat::Rgba16 => Empty::Alpha { at: 6, wide: true },
            _ => {
                let pal = palette.unwrap_or_default();
                Empty::Table(
                    (0..256)
                        .map(|i| pal.get(i).is_some_and(|c| c[3] == 0))
                        .collect(),
                )
            }
        };
        return Ok(Some((empty, GridBackground::Transparent)));
    }
    let (w, h) = (px.width(), px.height());
    let bpp = px.bpp();
    let mut corners = Vec::with_capacity(4);
    for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        let mut b = vec![0u8; bpp];
        px.read_rect(IRect::new(i64::from(x), i64::from(y), 1, 1), &mut b)?;
        corners.push(b);
    }
    let mut key = 0u32;
    for_each_rgba8_key(fmt, palette, &corners[0], |k| {
        key = k;
        false
    })?;
    let rgba = unpack(key);
    let empty = if fmt == PixelFormat::Indexed8 {
        let pal = palette.unwrap_or_default();
        let color = |b: &Vec<u8>| b.first().and_then(|i| pal.get(usize::from(*i))).copied();
        let c0 = color(&corners[0]);
        if c0.is_none() || corners.iter().any(|c| color(c) != c0) {
            return Ok(None);
        }
        Empty::Table((0..256).map(|i| pal.get(i).copied() == c0).collect())
    } else {
        if corners.iter().any(|c| *c != corners[0]) {
            return Ok(None);
        }
        Empty::Raw(raw_key(&corners[0]))
    };
    Ok(Some((empty, GridBackground::Color { rgba })))
}

/// 軸ごとの中身の数と、切れない境目。
struct Axes {
    occ_x: Vec<u64>,
    occ_y: Vec<u64>,
    /// `blocked_x[x]`：x − 1 と x のあいだを切れない（8 連結で中身がまたぐ）。
    blocked_x: Vec<bool>,
    blocked_y: Vec<bool>,
}

/// タイルとその周りの 1 画素の中身の印（1 が中身）。周りに 1 画素ずつ足した (tw + 2) × (th + 2) の並びで、
/// 画像の外は 0。タイルの画素 (xx, yy) は `(yy + 1)·(tw + 2) + xx + 1`。
fn padded_mask(px: &Raster, r: IRect, empty: &Empty) -> Result<Vec<u8>, EngineError> {
    let (iw, ih) = (i64::from(px.width()), i64::from(px.height()));
    let (x0, y0) = ((r.x - 1).max(0), (r.y - 1).max(0));
    let (x1, y1) = ((r.right() + 1).min(iw), (r.bottom() + 1).min(ih));
    let (ew, eh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let bpp = px.bpp();
    let mut buf = vec![0u8; ew * eh * bpp];
    px.read_rect(IRect::new(x0, y0, ew as u32, eh as u32), &mut buf)?;
    let mut ext = Vec::with_capacity(ew * eh);
    empty.mask(&buf, bpp, &mut ext);
    let pw = r.w as usize + 2;
    let mut out = vec![0u8; pw * (r.h as usize + 2)];
    // 読んだ範囲の左上は、足した並びの (x0 − (r.x − 1), y0 − (r.y − 1))
    let (dx, dy) = ((x0 - (r.x - 1)) as usize, (y0 - (r.y - 1)) as usize);
    for (k, row) in ext.chunks_exact(ew).enumerate() {
        let at = (k + dy) * pw + dx;
        if let Some(dst) = out.get_mut(at..at + ew) {
            dst.copy_from_slice(row);
        }
    }
    Ok(out)
}

fn scan_axes(px: &Raster, empty: &Empty, cx: &AnalyzeCx<'_>) -> Result<Axes, EngineError> {
    let parts = cx.par_blocks(tile_count(px), |i| {
        let (r, _) = tile_at(px, i);
        let m = padded_mask(px, r, empty)?;
        let (tw, th) = (r.w as usize, r.h as usize);
        let pw = tw + 2;
        let mut ox = vec![0u64; tw];
        let mut oy = vec![0u64; th];
        let mut bx = vec![false; tw];
        let mut by = vec![false; th];
        for yy in 0..th {
            let (up, row, down) = (yy * pw, (yy + 1) * pw, (yy + 2) * pw);
            let (Some(above), Some(here), Some(below)) = (
                m.get(up..up + pw),
                m.get(row..row + pw),
                m.get(down..down + pw),
            ) else {
                continue;
            };
            let mut count = 0u64;
            let mut top_blocked = false;
            for xx in 0..tw {
                if here[xx + 1] == 0 {
                    continue;
                }
                count += 1;
                ox[xx] += 1;
                // 左の列（xx − 1）の同じ行・上下 1 行に中身があれば、xx の左の境目は切れない
                if !bx[xx] && (above[xx] | here[xx] | below[xx]) != 0 {
                    bx[xx] = true;
                }
                // 上の行（yy − 1）の左右 1 列に中身があれば、yy の上の境目は切れない
                top_blocked |= (above[xx] | above[xx + 1] | above[xx + 2]) != 0;
            }
            oy[yy] = count;
            by[yy] = top_blocked;
        }
        Ok((r, ox, oy, bx, by))
    })?;
    let (w, h) = (px.width() as usize, px.height() as usize);
    let mut axes = Axes {
        occ_x: filled_vec(w, 0)?,
        occ_y: filled_vec(h, 0)?,
        blocked_x: filled_vec(w, false)?,
        blocked_y: filled_vec(h, false)?,
    };
    for (r, ox, oy, bx, by) in parts {
        let (x0, y0) = (r.x as usize, r.y as usize);
        for (k, (o, b)) in ox.iter().zip(&bx).enumerate() {
            if let (Some(so), Some(sb)) =
                (axes.occ_x.get_mut(x0 + k), axes.blocked_x.get_mut(x0 + k))
            {
                *so += o;
                *sb |= b;
            }
        }
        for (k, (o, b)) in oy.iter().zip(&by).enumerate() {
            if let (Some(so), Some(sb)) =
                (axes.occ_y.get_mut(y0 + k), axes.blocked_y.get_mut(y0 + k))
            {
                *so += o;
                *sb |= b;
            }
        }
    }
    Ok(axes)
}

/// 1 つの軸の格子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AxisFit {
    offset: u32,
    cell: u32,
    gap: u32,
    /// セルの数。
    n: u32,
    /// 中身のあるセルの数（この軸に射影して）。
    filled: u32,
    /// 詰めた中身の幅（P − 最も長い空の連なり）。
    span: u32,
    /// 0 余白なし・1 前後の余白が同じ・2 ほか。
    class: u8,
    /// 同じ周期の中での表し方の好み（0 が先）。
    rep: u8,
}

impl AxisFit {
    fn period(&self) -> u32 {
        self.cell + self.gap
    }
}

/// 軸の中身の数の累積和（`sum(a, b)` は [a, b) の和）。
struct Prefix(Vec<u64>);

impl Prefix {
    fn new(occ: &[u64]) -> Result<Self, EngineError> {
        let mut p = filled_vec(occ.len() + 1, 0u64)?;
        for (i, v) in occ.iter().enumerate() {
            p[i + 1] = p[i] + v;
        }
        Ok(Self(p))
    }

    fn sum(&self, a: u64, b: u64) -> u64 {
        let at = |i: u64| {
            self.0
                .get(i as usize)
                .or(self.0.last())
                .copied()
                .unwrap_or(0)
        };
        at(b).saturating_sub(at(a))
    }

    fn total(&self) -> u64 {
        self.0.last().copied().unwrap_or(0)
    }
}

/// 格子 (o, c, g) を確かめる。成り立てば `AxisFit`。
fn eval_fit(
    pre: &Prefix,
    blocked: &[bool],
    len: u64,
    (o, c, g): (u32, u32, u32),
    span: u32,
    rep: u8,
) -> Option<AxisFit> {
    let n = cells_on_axis(len, o, c, g);
    if n == 0 {
        return None;
    }
    let p = u64::from(c) + u64::from(g);
    let (o64, c64) = (u64::from(o), u64::from(c));
    let last_end = o64 + (n - 1) * p + c64;
    // 余白・余りに中身があれば成り立たない（先に O(1) で）
    if pre.sum(0, o64) != 0 || pre.sum(last_end, len) != 0 {
        return None;
    }
    let (mut inside, mut filled) = (0u64, 0u32);
    for i in 0..n {
        let s = o64 + i * p;
        let v = pre.sum(s, s + c64);
        inside += v;
        filled += u32::from(v > 0);
    }
    if inside != pre.total() {
        return None;
    }
    if g == 0 {
        for i in 1..n {
            if blocked.get((o64 + i * p) as usize).copied().unwrap_or(true) {
                return None;
            }
        }
    }
    let end = len - last_end;
    let class = if o64 == 0 && end == 0 {
        0
    } else if end == o64 {
        1
    } else {
        2
    };
    Some(AxisFit {
        offset: o,
        cell: c,
        gap: g,
        n: n as u32,
        filled,
        span,
        class,
        rep,
    })
}

/// 周期 `p` の格子のうち、いちばんよいもの。
fn best_for_period(occ: &[u64], blocked: &[bool], pre: &Prefix, p: u32) -> Option<AxisFit> {
    let pu = p as usize;
    let len = occ.len() as u64;
    let mut fold = vec![0u64; pu];
    let mut res_blocked = vec![false; pu];
    let mut j = 0usize;
    for (x, (o, b)) in occ.iter().zip(blocked).enumerate() {
        fold[j] += o;
        if x >= 1 && *b {
            res_blocked[j] = true;
        }
        j += 1;
        if j == pu {
            j = 0;
        }
    }
    if fold.iter().all(|v| *v == 0) {
        return None;
    }
    // 詰めた格子：0 の剰余の、輪で最も長い連なり（同じ長さなら始まりの小さいもの）
    let (mut best_s, mut best_len) = (0usize, 0usize);
    for s in 0..pu {
        if fold[s] != 0 || fold[(s + pu - 1) % pu] == 0 {
            continue;
        }
        let mut l = 0;
        while l < pu && fold[(s + l) % pu] == 0 {
            l += 1;
        }
        if l > best_len {
            (best_s, best_len) = (s, l);
        }
    }
    let span = (pu - best_len) as u32;
    let mut fits = Vec::new();
    let mut tight_full = false;
    if best_len > 0 {
        let g = best_len as u32;
        let start = ((best_s + best_len) % pu) as u32;
        let c = p - g;
        let n = cells_on_axis(len, start, c, g);
        let (mut touch, mut filled) = (0u64, 0u64);
        for i in 0..n {
            let s0 = u64::from(start) + i * u64::from(p);
            let s1 = s0 + u64::from(c) - 1;
            if pre.sum(s0, s1 + 1) > 0 {
                filled += 1;
                let on = |x: u64| occ.get(x as usize).is_some_and(|v| *v > 0);
                if on(s0) && on(s1) {
                    touch += 1;
                }
            }
        }
        tight_full = filled > 0 && touch * 2 >= filled;
        fits.extend(eval_fit(
            pre,
            blocked,
            len,
            (start, c, g),
            span,
            u8::from(!tight_full),
        ));
    }
    for o in 0..p {
        if res_blocked[o as usize] {
            continue;
        }
        fits.extend(eval_fit(
            pre,
            blocked,
            len,
            (o, p, 0),
            span,
            u8::from(tight_full),
        ));
    }
    fits.into_iter()
        .min_by_key(|f| (Reverse(f.filled), f.n, f.class, f.rep, f.offset))
}

/// 周期をまたいだ順位：中身のあるセルの数 ↓、セルの数、整った余白か（当てはまり方が「余白なし」か「前後が同じ」なら
/// 先）、畳んだときの空の割合 (P − span) / P ↓（中身の位置が周期でそろっているほど大きい。整数の掛け算で比べる）、
/// 当てはまり方、offset、詰めた中身の幅、周期。周期ごとに 1 つなので全順序。
///
/// 整った余白を空の割合より先に見るのは、セルの少ない軸で「ずれた周期」（48 の格子に対する 47・49）が、空の割合で
/// たまたま勝つため。空の割合を当てはまり方の細かい順より先に見るのは、余白と間隔の釣り合いでたまたま画像を
/// ちょうど割り切る周期（間隔 2・余白 3 の 4 列で 15）より、中身がそろう本当の周期（14）を選ぶため。
fn rank(a: &AxisFit, b: &AxisFit) -> std::cmp::Ordering {
    let empty = |f: &AxisFit| u64::from(f.period().saturating_sub(f.span));
    let loose = |f: &AxisFit| f.class > 1;
    b.filled
        .cmp(&a.filled)
        .then(a.n.cmp(&b.n))
        .then(loose(a).cmp(&loose(b)))
        .then((empty(b) * u64::from(a.period())).cmp(&(empty(a) * u64::from(b.period()))))
        .then(a.class.cmp(&b.class))
        .then(a.offset.cmp(&b.offset))
        .then(a.span.cmp(&b.span))
        .then(a.period().cmp(&b.period()))
}

/// 1 つの軸の格子の候補を、順位の順に（周期ごとに 1 つ）。
fn fit_axis(
    occ: &[u64],
    blocked: &[bool],
    cx: &AnalyzeCx<'_>,
) -> Result<Vec<AxisFit>, EngineError> {
    let pre = Prefix::new(occ)?;
    if pre.total() == 0 {
        return Ok(Vec::new());
    }
    let p_hi = occ.len().saturating_sub(1).min(MAX_PERIOD as usize);
    if p_hi < 2 {
        return Ok(Vec::new());
    }
    let per_p = cx.par_blocks(p_hi - 1, |b| {
        Ok(best_for_period(occ, blocked, &pre, (b + 2) as u32))
    })?;
    let mut ranked: Vec<AxisFit> = per_p.into_iter().flatten().collect();
    ranked.sort_by(rank);
    Ok(ranked)
}

/// 2 軸を合わせる：両軸の最上位の段に同じ周期があれば、正方形のセルを選ぶ。
fn joint(lx: &[AxisFit], ly: &[AxisFit]) -> (Option<AxisFit>, Option<AxisFit>) {
    let tier = |v: &[AxisFit]| -> Vec<AxisFit> {
        let Some(b) = v.first() else {
            return Vec::new();
        };
        v.iter()
            .copied()
            .filter(|f| (f.filled, f.n, f.class) == (b.filled, b.n, b.class))
            .collect()
    };
    match (lx.first().copied(), ly.first().copied()) {
        (Some(x), Some(y)) => {
            if x.period() == y.period() {
                return (Some(x), Some(y));
            }
            if let Some(y2) = tier(ly).into_iter().find(|f| f.period() == x.period()) {
                return (Some(x), Some(y2));
            }
            if let Some(x2) = tier(lx).into_iter().find(|f| f.period() == y.period()) {
                return (Some(x2), Some(y));
            }
            (Some(x), Some(y))
        }
        (x, y) => (x, y),
    }
}

/// 両軸の格子から候補を作る（分けない軸は offset 0・セルは全体・間隔 0）。`filled` は後で数える。
fn guess(
    fx: Option<AxisFit>,
    fy: Option<AxisFit>,
    w: u32,
    h: u32,
    source: GridSource,
) -> GridGuess {
    let grid = GridParam {
        cell_w: fx.map_or(w, |f| f.cell),
        cell_h: fy.map_or(h, |f| f.cell),
        offset_x: fx.map_or(0, |f| f.offset),
        offset_y: fy.map_or(0, |f| f.offset),
        gap_x: fx.map_or(0, |f| f.gap),
        gap_y: fy.map_or(0, |f| f.gap),
    };
    let class = fx.map_or(0, |f| f.class).max(fy.map_or(0, |f| f.class));
    GridGuess {
        grid,
        cols: cells_on_axis(u64::from(w), grid.offset_x, grid.cell_w, grid.gap_x) as u32,
        rows: cells_on_axis(u64::from(h), grid.offset_y, grid.cell_h, grid.gap_y) as u32,
        filled: 0,
        fit: match class {
            0 => GridFit::Exact,
            1 => GridFit::Symmetric,
            _ => GridFit::Loose,
        },
        source,
    }
}

/// 軸の位置からセルの番号（セルの外は None）。
fn cell_index(v: u64, offset: u32, cell: u32, gap: u32, n: u32) -> Option<u32> {
    let v = v.checked_sub(u64::from(offset))?;
    let p = u64::from(cell) + u64::from(gap);
    let (i, at) = (v / p.max(1), v % p.max(1));
    (i < u64::from(n) && at < u64::from(cell)).then_some(i as u32)
}

/// 候補ごとに、中身のあるセルの数を数える（タイルごとに触れたセルの印を作り、全体の印に論理和で足す）。
fn count_filled(
    px: &Raster,
    empty: &Empty,
    guesses: &mut [GridGuess],
    cx: &AnalyzeCx<'_>,
) -> Result<(), EngineError> {
    if guesses.is_empty() {
        return Ok(());
    }
    let bpp = px.bpp();
    // 候補ごと・タイルごとの印：（左上のセル (i0, j0)、セルの数 (ni, nj)、印）
    type Window = Option<(u32, u32, u32, u32, Vec<u64>)>;
    let parts: Vec<Vec<Window>> = cx.par_blocks(tile_count(px), |t| {
        let (r, tile) = tile_at(px, t);
        let (tw, th) = (r.w as usize, r.h as usize);
        let mut full = Vec::with_capacity(tw * th);
        empty.mask(tile, bpp, &mut full);
        let mut out = Vec::with_capacity(guesses.len());
        for g in guesses.iter() {
            let gp = g.grid;
            let col: Vec<Option<u32>> = (0..tw)
                .map(|k| {
                    cell_index(
                        r.x as u64 + k as u64,
                        gp.offset_x,
                        gp.cell_w,
                        gp.gap_x,
                        g.cols,
                    )
                })
                .collect();
            let row: Vec<Option<u32>> = (0..th)
                .map(|k| {
                    cell_index(
                        r.y as u64 + k as u64,
                        gp.offset_y,
                        gp.cell_h,
                        gp.gap_y,
                        g.rows,
                    )
                })
                .collect();
            let range = |v: &[Option<u32>]| {
                let lo = v.iter().flatten().min().copied()?;
                let hi = v.iter().flatten().max().copied()?;
                Some((lo, hi - lo + 1))
            };
            let (Some((i0, ni)), Some((j0, nj))) = (range(&col), range(&row)) else {
                out.push(None);
                continue;
            };
            // 同じセルの列の連なり（セルの番号, 始まり, 終わり）
            let mut segs: Vec<(u32, usize, usize)> = Vec::new();
            for (xx, i) in col.iter().enumerate() {
                let Some(i) = *i else { continue };
                match segs.last_mut() {
                    Some((last, _, end)) if *last == i && *end == xx => *end = xx + 1,
                    _ => segs.push((i, xx, xx + 1)),
                }
            }
            let mut bits = vec![0u64; (ni as usize * nj as usize).div_ceil(64)];
            for (line, j) in full.chunks_exact(tw).zip(&row) {
                let Some(j) = *j else { continue };
                for &(i, a, b) in &segs {
                    let bit = ((j - j0) * ni + (i - i0)) as usize;
                    let Some(word) = bits.get_mut(bit / 64) else {
                        continue;
                    };
                    if *word >> (bit % 64) & 1 == 0
                        && line.get(a..b).is_some_and(|s| s.iter().any(|v| *v != 0))
                    {
                        *word |= 1 << (bit % 64);
                    }
                }
            }
            out.push(Some((i0, j0, ni, nj, bits)));
        }
        Ok(out)
    })?;
    for (k, g) in guesses.iter_mut().enumerate() {
        cx.check()?;
        let cells = g.cols as usize * g.rows as usize;
        let mut all = filled_vec(cells.div_ceil(64), 0u64)?;
        for part in &parts {
            let Some(Some((i0, j0, ni, nj, bits))) = part.get(k) else {
                continue;
            };
            for jj in 0..*nj {
                for ii in 0..*ni {
                    let b = (jj * ni + ii) as usize;
                    if bits.get(b / 64).is_some_and(|w| w >> (b % 64) & 1 == 1) {
                        let id = ((j0 + jj) as usize) * g.cols as usize + (i0 + ii) as usize;
                        if let Some(word) = all.get_mut(id / 64) {
                            *word |= 1 << (id % 64);
                        }
                    }
                }
            }
        }
        g.filled = all.iter().map(|w| w.count_ones()).sum();
    }
    Ok(())
}

/// 溝のない帯の正方形の格子（両軸とも溝から分けられないとき）。
fn square_strip(axes: &Axes, w: u32, h: u32) -> Option<GridGuess> {
    let (long, short, occ) = if w >= h {
        (w, h, &axes.occ_x)
    } else {
        (h, w, &axes.occ_y)
    };
    if short == 0 || long < short.saturating_mul(2) || long % short != 0 || occ.contains(&0) {
        return None;
    }
    let side = AxisFit {
        offset: 0,
        cell: short,
        gap: 0,
        n: long / short,
        filled: 0,
        span: short,
        class: 0,
        rep: 0,
    };
    Some(if w >= h {
        guess(Some(side), None, w, h, GridSource::Square)
    } else {
        guess(None, Some(side), w, h, GridSource::Square)
    })
}

/// スプライトシートの格子の推定。
pub struct SpriteGrid;

impl Analysis for SpriteGrid {
    const ID: &'static str = "image.sprite_grid";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Image];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = NoParams;
    type Output = SpriteGridOutput;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<SpriteGridOutput, EngineError> {
        cx.check()?;
        let Value::Image(img) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が 1 枚の画像ではありません",
                Self::ID
            )));
        };
        let px = &img.px;
        let (w, h) = (px.width(), px.height());
        let nothing = |background| SpriteGridOutput {
            background,
            best: None,
            alternatives: Vec::new(),
        };
        let found = find_empty(px, cx)?;
        // 透明を持たない形式は空を決めるのに画素を走査しないので、ここでも中止を確かめる
        cx.check()?;
        let Some((empty, background)) = found else {
            return Ok(nothing(GridBackground::None));
        };
        let axes = scan_axes(px, &empty, cx)?;
        // 中身のあるセルが 2 つ以上の格子だけ（1 つのスプライトを空のセルと並べて「分けた」とはしない）
        let lx: Vec<AxisFit> = fit_axis(&axes.occ_x, &axes.blocked_x, cx)?
            .into_iter()
            .filter(|f| f.filled >= 2)
            .collect();
        let ly: Vec<AxisFit> = fit_axis(&axes.occ_y, &axes.blocked_y, cx)?
            .into_iter()
            .filter(|f| f.filled >= 2)
            .collect();
        let mut guesses = Vec::new();
        if lx.is_empty() && ly.is_empty() {
            if axes.occ_x.iter().any(|v| *v > 0) {
                guesses.extend(square_strip(&axes, w, h));
            }
        } else {
            let (sx, sy) = joint(&lx, &ly);
            guesses.push(guess(sx, sy, w, h, GridSource::Gutter));
            let others = |l: &[AxisFit], s: Option<AxisFit>| -> Vec<AxisFit> {
                l.iter().copied().filter(|f| Some(*f) != s).collect()
            };
            let (ox, oy) = (others(&lx, sx), others(&ly, sy));
            let mut k = 0;
            while guesses.len() <= MAX_ALTERNATIVES && (k < ox.len() || k < oy.len()) {
                if let Some(fx) = ox.get(k) {
                    guesses.push(guess(Some(*fx), sy, w, h, GridSource::Gutter));
                }
                if guesses.len() <= MAX_ALTERNATIVES
                    && let Some(fy) = oy.get(k)
                {
                    guesses.push(guess(sx, Some(*fy), w, h, GridSource::Gutter));
                }
                k += 1;
            }
        }
        count_filled(px, &empty, &mut guesses, cx)?;
        let mut it = guesses.into_iter();
        Ok(SpriteGridOutput {
            background,
            best: it.next(),
            alternatives: it.collect(),
        })
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        let sheet = |cols,
                     rows,
                     cell: (u32, u32),
                     gap: (u32, u32),
                     margin: (u32, u32),
                     pad: (u32, u32),
                     background,
                     full,
                     empty: Vec<u32>,
                     seed| {
            InputGen::SpriteSheet {
                cols,
                rows,
                cell_w: cell.0,
                cell_h: cell.1,
                gap_x: gap.0,
                gap_y: gap.1,
                margin_x: margin.0,
                margin_y: margin.1,
                pad_right: pad.0,
                pad_bottom: pad.1,
                background,
                full,
                empty,
                seed,
            }
        };
        vec![
            ex(
                "sheet-gap-margin-empty-cell",
                sheet(
                    4,
                    3,
                    (12, 10),
                    (2, 1),
                    (3, 2),
                    (0, 0),
                    None,
                    false,
                    vec![5],
                    61,
                ),
            ),
            ex(
                "sheet-opaque-background",
                sheet(
                    5,
                    2,
                    (10, 10),
                    (4, 4),
                    (4, 4),
                    (0, 0),
                    Some([0, 128, 128, 255]),
                    false,
                    vec![],
                    62,
                ),
            ),
            ex(
                "tiles-gap-1-margin-1",
                sheet(6, 4, (8, 8), (1, 1), (1, 1), (0, 0), None, true, vec![], 63),
            ),
            ex(
                "strip-without-gutters-is-square",
                sheet(6, 1, (8, 6), (0, 0), (0, 1), (0, 1), None, true, vec![], 64),
            ),
            ex(
                "sheet-with-leftover-right",
                sheet(
                    5,
                    3,
                    (16, 16),
                    (0, 0),
                    (0, 0),
                    (9, 0),
                    None,
                    false,
                    vec![],
                    65,
                ),
            ),
            ex(
                "single-sprite-is-null",
                InputGen::Sprite {
                    w: 64,
                    h: 32,
                    rect: IRect::new(10, 5, 20, 12),
                },
            ),
            ex(
                "opaque-corners-differ-is-none",
                InputGen::Sheet {
                    cols: 4,
                    rows: 2,
                    cell: 16,
                },
            ),
            ex(
                "noise-has-no-grid",
                InputGen::Noise {
                    w: 30,
                    h: 20,
                    fmt: PixelFormat::Rgba8,
                    seed: 66,
                },
            ),
            ex(
                "large-sheet-across-tiles",
                sheet(
                    9,
                    4,
                    (40, 36),
                    (3, 3),
                    (2, 0),
                    (0, 5),
                    None,
                    false,
                    vec![0, 13],
                    67,
                ),
            ),
        ]
    }

    fn estimate(_p: &NoParams, input: &Shape) -> Estimate {
        let (pixels, _, w, h) = shape_pixels(input);
        let fit = |len: u64| len.saturating_mul(len.min(u64::from(MAX_PERIOD)));
        Estimate {
            // 背景 64 バイト ＋ 候補 4 つ（1 つ 256 バイトまで）
            bytes: 64 + 4 * 256 + 64,
            cost: pixels
                .saturating_mul(2)
                .saturating_add(fit(w))
                .saturating_add(fit(h)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::image::test_util::run;

    fn fit(occ: &[u64], blocked: &[bool]) -> Vec<AxisFit> {
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &crate::NoMonitor,
            limits: &limits,
        };
        fit_axis(occ, blocked, &cx).unwrap()
    }

    /// 中身が続く列のあいだは切れない（隣り合う列の両方に中身がある）とした `blocked`。
    fn contiguous(occ: &[u64]) -> Vec<bool> {
        (0..occ.len())
            .map(|x| x >= 1 && occ[x - 1] > 0 && occ[x] > 0)
            .collect()
    }

    #[test]
    fn one_axis_fits_gutters_and_periods() {
        // 長さ 19：セル 4・間隔 1・余白 0（位置 0..4, 5..9, 10..14, 15..19）。中身はセルいっぱい
        let mut occ = vec![0u64; 19];
        for s in [0, 5, 10, 15] {
            for v in &mut occ[s..s + 4] {
                *v = 3;
            }
        }
        let best = fit(&occ, &contiguous(&occ))[0];
        assert_eq!(
            (
                best.offset,
                best.cell,
                best.gap,
                best.n,
                best.filled,
                best.class
            ),
            (0, 4, 1, 4, 4, 0)
        );
        // 間隔 0 で、中身がセルの内側だけ（1..4）：周期 5 の周期の格子（余白なしの offset 0）
        let mut occ = vec![0u64; 20];
        for s in [0, 5, 10, 15] {
            for v in &mut occ[s + 1..s + 4] {
                *v = 1;
            }
        }
        let fits = fit(&occ, &contiguous(&occ));
        assert_eq!(
            (fits[0].offset, fits[0].cell, fits[0].gap, fits[0].filled),
            (0, 5, 0, 4)
        );
        // 切れない境目をまたぐ周期の格子は作らない（中身が隙間なく続く）
        let occ = vec![1u64; 20];
        let mut blocked = vec![true; 20];
        blocked[0] = false;
        assert!(fit(&occ, &blocked).iter().all(|f| f.n < 2));
        // 中身がない
        assert!(fit(&[0; 10], &[false; 10]).is_empty());
    }

    /// 値を直接確かめる（ゴールデンに凍結する前に）。
    #[test]
    fn guesses_the_generated_sheets() {
        // 余白は前後で同じ（TexturePacker の Border Padding のように）
        let sheet = |cols, rows, cell: u32, gap, margin, bg: Option<[u8; 4]>, full| {
            InputGen::SpriteSheet {
                cols,
                rows,
                cell_w: cell,
                cell_h: cell,
                gap_x: gap,
                gap_y: gap,
                margin_x: margin,
                margin_y: margin,
                pad_right: margin,
                pad_bottom: margin,
                background: bg,
                full,
                empty: vec![1],
                seed: 7,
            }
            .generate()
            .unwrap()
        };
        // RPG 風（間隔 0・中身はセルの内側で大きさがばらばら）：周期の格子でセルそのもの
        let out = run::<SpriteGrid>(&NoParams {}, &sheet(4, 3, 12, 0, 0, None, false));
        assert_eq!(out.background, GridBackground::Transparent);
        let best = out.best.unwrap();
        assert_eq!(
            best.grid,
            GridParam {
                cell_w: 12,
                cell_h: 12,
                offset_x: 0,
                offset_y: 0,
                gap_x: 0,
                gap_y: 0
            }
        );
        assert_eq!((best.cols, best.rows, best.filled), (4, 3, 11));
        assert_eq!(
            (best.source, best.fit),
            (GridSource::Gutter, GridFit::Exact)
        );
        // 間隔と余白があり、中身がセルの縁に届かない：周期 14 で同じスプライトに分ける（セルの縁は画素から決まらない
        // ので、周期の格子で間隔の中を切る）
        let out = run::<SpriteGrid>(&NoParams {}, &sheet(4, 3, 12, 2, 3, None, false));
        let best = out.best.unwrap();
        assert_eq!(
            (
                best.grid.cell_w + best.grid.gap_x,
                best.grid.cell_h + best.grid.gap_y
            ),
            (14, 14)
        );
        assert_eq!((best.cols, best.rows, best.filled), (4, 3, 11));
        // 余白が左上だけ（右下 0）でも、たまたま当てはまる周期 13 でなく 14
        let lopsided = InputGen::SpriteSheet {
            cols: 4,
            rows: 3,
            cell_w: 12,
            cell_h: 12,
            gap_x: 2,
            gap_y: 2,
            margin_x: 3,
            margin_y: 3,
            pad_right: 0,
            pad_bottom: 0,
            background: None,
            full: false,
            empty: vec![1],
            seed: 7,
        }
        .generate()
        .unwrap();
        let best = run::<SpriteGrid>(&NoParams {}, &lopsided).best.unwrap();
        assert_eq!(
            (
                best.grid.cell_w + best.grid.gap_x,
                best.grid.cell_h + best.grid.gap_y,
                best.filled
            ),
            (14, 14, 11)
        );
        // タイル（セルいっぱい）間隔 1・余白 1：詰めた格子
        let out = run::<SpriteGrid>(&NoParams {}, &sheet(5, 4, 8, 1, 1, None, true));
        let best = out.best.unwrap();
        assert_eq!(
            best.grid,
            GridParam {
                cell_w: 8,
                cell_h: 8,
                offset_x: 1,
                offset_y: 1,
                gap_x: 1,
                gap_y: 1
            }
        );
        assert_eq!((best.cols, best.rows, best.filled), (5, 4, 19));
        // 不透明な背景
        let out = run::<SpriteGrid>(
            &NoParams {},
            &sheet(4, 2, 10, 4, 4, Some([0, 128, 128, 255]), false),
        );
        assert_eq!(
            out.background,
            GridBackground::Color {
                rgba: [0, 128, 128, 255]
            }
        );
        let best = out.best.unwrap();
        assert_eq!((best.cols, best.rows), (4, 2));
        // 溝のない帯（横は隙間なく続き、上下に透明な行がある 30×6）は正方形
        let strip = InputGen::SpriteSheet {
            cols: 5,
            rows: 1,
            cell_w: 6,
            cell_h: 4,
            gap_x: 0,
            gap_y: 0,
            margin_x: 0,
            margin_y: 1,
            pad_right: 0,
            pad_bottom: 1,
            background: None,
            full: true,
            empty: vec![],
            seed: 1,
        }
        .generate()
        .unwrap();
        let out = run::<SpriteGrid>(&NoParams {}, &strip);
        let best = out.best.unwrap();
        assert_eq!(best.source, GridSource::Square);
        assert_eq!(
            (best.grid.cell_w, best.grid.cell_h, best.cols, best.rows),
            (6, 6, 5, 1)
        );
        assert_eq!(best.filled, 5);
        // 四隅が違う不透明な絵は推定しない
        let out = run::<SpriteGrid>(
            &NoParams {},
            &InputGen::Sheet {
                cols: 4,
                rows: 2,
                cell: 16,
            }
            .generate()
            .unwrap(),
        );
        assert_eq!(out.background, GridBackground::None);
        assert_eq!(out.best, None);
    }
}
