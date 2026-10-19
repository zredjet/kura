//! 範囲（docs/editing.md §12。段 10）：値の中の範囲（[`Area`]）、出力が入力のどこに依るか（[`Reach`]）、タイルの集合
//! （[`Tiles`]）、範囲だけを描いた値（[`PartialValue`]）。
//!
//! - 座標系：画像の画素は**キャンバスの座標**で持つ（1 枚の画像はその画像の座標、フレームの列は共通のキャンバスの座標）。
//!   トリムしたフレーム（位置 `offset`）の画素 (x, y) はキャンバスの (offset.x + x, offset.y + y)。範囲はフレームの番号
//!   （画像は 0）ごとの矩形の集合で持つ（docs/editing.md §5.2）。音声は全チャンネルに共通のフレームの区間。
//! - どれも保守的（大きめでよく、小さすぎてはいけない）。矩形が [`MAX_RECTS`] 個を超えたら外接矩形に畳む。
//! - タイルの集合は 1 辺を持つ：範囲の描画は値のタイル（[`TILE`]。並列の分け方）、変わった範囲の報告は [`VIEW_TILE`]
//!   （配信の単位。値は同じ 256 だが意味が別なので、別の定数にする）。
//! - 範囲だけを描いた値は、描かなかったタイルを無効にした値（`Raster` の無効なタイル。読むと記録され、`read_rect` は失敗する。
//!   docs/editing.md §5.2）。ダイジェストを取れず（`Value::digest` が断る）、ノードのキャッシュに入らない。外へは
//!   [`PartialValue`] で渡す。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::audio::resample::{dest_window, source_window};
use crate::error::EngineError;
use crate::geom::{IPoint, IRect, Size};
use crate::imaging::resample::{Filter, dest_span, source_span};
use crate::mapping::{IntAffine, Seg};
use crate::paint::Sample;
use crate::render::cache::Origin;
use crate::shape::{Dim, Shape};
use crate::value::{
    AudioMeta, Frame, Frames, Image, ImageMeta, Palette, PixelFormat, Raster, TILE, Value,
};

/// 配信と変わった範囲の報告の単位（タイルの 1 辺）。値は [`TILE`] と同じだが、意味が別（`TILE` は並列の分け方）。
pub const VIEW_TILE: u32 = 256;

/// 1 フレームの矩形の数の上限（超えたら外接矩形に畳む。保守的なまま、手間を一定に抑える）。
pub const MAX_RECTS: usize = 64;

/// 音声の区間の数の上限（超えたら全体を覆う 1 つの区間に畳む）。
pub const MAX_SPANS: usize = 64;

/// 範囲の推論で、フレームの数ごとに広げる上限（これより多いフレームの「全体」は `Area::All` のまま）。
const MAX_EXPAND_FRAMES: u64 = 1 << 16;

/// 2 つの矩形の重なり（なければ None）。
fn intersect(a: &IRect, b: &IRect) -> Option<IRect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    Some(IRect::new(
        x0,
        y0,
        u32::try_from(x1 - x0).unwrap_or(u32::MAX),
        u32::try_from(y1 - y0).unwrap_or(u32::MAX),
    ))
}

/// `outer` が `inner` を覆えば true。
fn covers(outer: &IRect, inner: &IRect) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && outer.right() >= inner.right()
        && outer.bottom() >= inner.bottom()
}

/// 2 つの矩形の外接矩形（大きさは u32 で頭打ち）。
fn hull(a: &IRect, b: &IRect) -> IRect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    IRect::new(
        x0,
        y0,
        u32::try_from(x1.saturating_sub(x0)).unwrap_or(u32::MAX),
        u32::try_from(y1.saturating_sub(y0)).unwrap_or(u32::MAX),
    )
}

/// 大きさの矩形 `[0, w) × [0, h)`。
fn size_rect(s: Size) -> IRect {
    IRect::new(0, 0, s.w, s.h)
}

/// 1 フレームの中の画素の範囲：矩形の集合（重なってよい）か、フレーム全体。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rects {
    whole: bool,
    list: Vec<IRect>,
}

impl Rects {
    /// 空の範囲。
    pub fn new() -> Self {
        Self::default()
    }

    /// フレーム全体。
    pub fn whole() -> Self {
        Self {
            whole: true,
            list: Vec::new(),
        }
    }

    /// 矩形 1 つ（空なら空の範囲）。
    pub fn one(r: IRect) -> Self {
        let mut s = Self::new();
        s.push(r);
        s
    }

    /// フレーム全体なら true。
    pub fn is_whole(&self) -> bool {
        self.whole
    }

    /// 空なら true。
    pub fn is_empty(&self) -> bool {
        !self.whole && self.list.is_empty()
    }

    /// 矩形の並び（フレーム全体なら空）。
    pub fn rects(&self) -> &[IRect] {
        &self.list
    }

    /// 矩形を足す（空の矩形は無視。ほかに覆われるものは足さない。[`MAX_RECTS`] 個を超えたら外接矩形に畳む。外接矩形の幅・
    /// 高さが u32 に収まらなければ、1 つの矩形では覆えないのでフレーム全体にする）。
    pub fn push(&mut self, r: IRect) {
        if self.whole || r.w == 0 || r.h == 0 || self.list.iter().any(|q| covers(q, &r)) {
            return;
        }
        self.list.retain(|q| !covers(&r, q));
        self.list.push(r);
        if self.list.len() > MAX_RECTS {
            match self.bounds() {
                Some(b) if self.list.iter().all(|q| covers(&b, q)) => self.list = vec![b],
                _ => *self = Self::whole(),
            }
        }
    }

    /// 和をとる。
    pub fn union_with(&mut self, other: &Rects) {
        if other.whole {
            *self = Self::whole();
            return;
        }
        for r in &other.list {
            self.push(*r);
        }
    }

    /// 矩形の並びの外接矩形（空・フレーム全体は None）。幅・高さは u32 で頭打ちになる（遠く離れた矩形の並びでは、すべてを
    /// 覆うとは限らない）。
    pub fn bounds(&self) -> Option<IRect> {
        let first = self.list.first()?;
        Some(self.list.iter().fold(*first, |acc, r| hull(&acc, r)))
    }

    /// 点（画素）を含めば true。
    pub fn contains(&self, x: i64, y: i64) -> bool {
        self.whole
            || self
                .list
                .iter()
                .any(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
    }

    /// 矩形と重なれば true。
    pub fn intersects(&self, r: &IRect) -> bool {
        self.whole || self.list.iter().any(|q| intersect(q, r).is_some())
    }

    /// `bounds` で切った矩形の並び（フレーム全体は `bounds` そのもの）。
    pub fn clipped(&self, bounds: &IRect) -> Vec<IRect> {
        if self.whole {
            return if bounds.w > 0 && bounds.h > 0 {
                vec![*bounds]
            } else {
                Vec::new()
            };
        }
        self.list
            .iter()
            .filter_map(|r| intersect(r, bounds))
            .collect()
    }
}

/// 音声のフレームの区間の集合（`[start, end)`。並べて併合する）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spans(Vec<(u64, u64)>);

impl Spans {
    /// 空の集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 区間 1 つ（空なら空の集合）。
    pub fn one(start: u64, end: u64) -> Self {
        let mut s = Self::new();
        s.push(start, end);
        s
    }

    /// 区間を足す（重なる・接する区間と併合する。[`MAX_SPANS`] 個を超えたら全体を覆う 1 つに畳む）。
    pub fn push(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let mut all = std::mem::take(&mut self.0);
        all.push((start, end));
        all.sort();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(all.len());
        for (a, b) in all {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        if merged.len() > MAX_SPANS
            && let (Some(first), Some(last)) = (merged.first(), merged.last())
        {
            merged = vec![(first.0, last.1)];
        }
        self.0 = merged;
    }

    /// 和をとる。
    pub fn union_with(&mut self, other: &Spans) {
        for (a, b) in &other.0 {
            self.push(*a, *b);
        }
    }

    /// 区間の並び（小さい順・重ならない）。
    pub fn spans(&self) -> &[(u64, u64)] {
        &self.0
    }

    /// 空なら true。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 位置を含めば true。
    pub fn contains(&self, i: u64) -> bool {
        self.0.iter().any(|(a, b)| i >= *a && i < *b)
    }
}

/// 値の中の範囲（保守的：大きめでよく、小さすぎてはいけない）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Area {
    /// どこもない。
    #[default]
    Empty,
    /// すべて（全フレーム・全区間）。
    All,
    /// 画像の画素（キャンバスの座標）。フレームの番号（画像は 0）ごとの範囲。空の範囲のフレームは持たない。
    Pixels(BTreeMap<u32, Rects>),
    /// 音声のフレームの区間（全チャンネル）。空の集合は持たない。
    Samples(Spans),
}

impl Area {
    /// フレーム `frame` の矩形 1 つ。
    pub fn rect(frame: u32, r: IRect) -> Self {
        Self::frame(frame, Rects::one(r))
    }

    /// フレーム `frame` の範囲。
    pub fn frame(frame: u32, rects: Rects) -> Self {
        Self::pixels(BTreeMap::from([(frame, rects)]))
    }

    /// フレーム `frame` の全体。
    pub fn whole_frame(frame: u32) -> Self {
        Self::frame(frame, Rects::whole())
    }

    /// フレームごとの範囲（空のフレームを除き、どこもなければ `Empty`）。
    pub fn pixels(mut map: BTreeMap<u32, Rects>) -> Self {
        map.retain(|_, r| !r.is_empty());
        if map.is_empty() {
            Self::Empty
        } else {
            Self::Pixels(map)
        }
    }

    /// 音声の区間 `[start, end)`。
    pub fn span(start: u64, end: u64) -> Self {
        Self::samples(Spans::one(start, end))
    }

    /// 音声の区間の集合（空なら `Empty`）。
    pub fn samples(s: Spans) -> Self {
        if s.is_empty() {
            Self::Empty
        } else {
            Self::Samples(s)
        }
    }

    /// どこもなければ true。
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// すべてなら true。
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }

    /// 和（画素と音声を混ぜると `All`）。
    pub fn union(&self, other: &Area) -> Area {
        match (self, other) {
            (Self::Empty, x) | (x, Self::Empty) => x.clone(),
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::Pixels(a), Self::Pixels(b)) => {
                let mut out = a.clone();
                for (f, r) in b {
                    out.entry(*f).or_default().union_with(r);
                }
                Self::pixels(out)
            }
            (Self::Samples(a), Self::Samples(b)) => {
                let mut s = a.clone();
                s.union_with(b);
                Self::samples(s)
            }
            _ => Self::All,
        }
    }

    /// 和をとって入れる。
    pub fn union_with(&mut self, other: &Area) {
        if !other.is_empty() {
            *self = self.union(other);
        }
    }

    /// フレーム `frame` の範囲（`All` はフレーム全体。音声・そのフレームがなければ None）。
    pub fn frame_rects(&self, frame: u32) -> Option<Rects> {
        match self {
            Self::All => Some(Rects::whole()),
            Self::Pixels(map) => map.get(&frame).cloned(),
            Self::Empty | Self::Samples(_) => None,
        }
    }

    /// 画素（キャンバスの座標）を含めば true。
    pub fn contains_pixel(&self, frame: u32, x: i64, y: i64) -> bool {
        match self {
            Self::All => true,
            Self::Pixels(map) => map.get(&frame).is_some_and(|r| r.contains(x, y)),
            Self::Empty | Self::Samples(_) => false,
        }
    }

    /// 音声の位置を含めば true。
    pub fn contains_sample(&self, i: u64) -> bool {
        match self {
            Self::All => true,
            Self::Samples(s) => s.contains(i),
            Self::Empty | Self::Pixels(_) => false,
        }
    }

    /// 形の分かる値について、`All` をフレームごとの全体・音声の全区間に開いたもの（逆写し・前へ写すときに大きさを使えるように）。
    /// フレームの数・長さが決まっていなければ `All` のまま。
    pub fn expand(&self, shape: &Shape) -> Area {
        if !self.is_all() {
            return self.clone();
        }
        match shape {
            Shape::Image { .. } => Self::whole_frame(0),
            Shape::Frames {
                count: Dim::Exact(n),
                ..
            } if *n <= MAX_EXPAND_FRAMES => {
                let n = u32::try_from(*n).unwrap_or(0);
                Self::pixels((0..n).map(|f| (f, Rects::whole())).collect())
            }
            Shape::Audio {
                frames: Dim::Exact(n),
                ..
            } => Self::span(0, *n),
            _ => Self::All,
        }
    }

    /// フレームごとに当てる操作（`Lift::PerFrame`）の、1 フレームの画像の範囲（フレーム 0）を、`count` 枚のフレームの列に
    /// 広げたもの（どのフレームにも同じ範囲）。数が決まっていなければ `All`。
    pub fn lift(&self, count: Dim) -> Area {
        let Self::Pixels(map) = self else {
            return self.clone();
        };
        let Some(r) = map.get(&0) else {
            return Self::Empty;
        };
        match count {
            Dim::Exact(n) if n <= MAX_EXPAND_FRAMES => {
                let n = u32::try_from(n).unwrap_or(0);
                Self::pixels((0..n).map(|f| (f, r.clone())).collect())
            }
            _ => Self::All,
        }
    }
}

/// タイルの集合（1 辺 `size` の格子で、大きさ `w × h` の画像を覆うもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tiles {
    size: u32,
    w: u32,
    h: u32,
    bits: Vec<u64>,
}

impl Tiles {
    /// 空の集合。
    pub fn none(w: u32, h: u32, size: u32) -> Self {
        let size = size.max(1);
        let n = w.div_ceil(size) as usize * h.div_ceil(size) as usize;
        Self {
            size,
            w,
            h,
            bits: vec![0; n.div_ceil(64)],
        }
    }

    /// すべてのタイル。
    pub fn all(w: u32, h: u32, size: u32) -> Self {
        let mut t = Self::none(w, h, size);
        for ty in 0..t.tiles_y() {
            for tx in 0..t.tiles_x() {
                t.insert(tx, ty);
            }
        }
        t
    }

    /// 範囲（キャンバスの座標）に重なるタイル。`origin` は画像（フレーム）のキャンバスの中の位置。
    pub fn covering(w: u32, h: u32, size: u32, rects: &Rects, origin: IPoint) -> Self {
        if rects.is_whole() {
            return Self::all(w, h, size);
        }
        let mut t = Self::none(w, h, size);
        let image = IRect::new(0, 0, w, h);
        for r in rects.rects() {
            let local = IRect::new(
                r.x.saturating_sub(origin.x),
                r.y.saturating_sub(origin.y),
                r.w,
                r.h,
            );
            let Some(c) = intersect(&local, &image) else {
                continue;
            };
            let s = i64::from(t.size);
            let (tx0, ty0) = ((c.x / s) as u32, (c.y / s) as u32);
            let (tx1, ty1) = (((c.right() - 1) / s) as u32, ((c.bottom() - 1) / s) as u32);
            for ty in ty0..=ty1 {
                for tx in tx0..=tx1 {
                    t.insert(tx, ty);
                }
            }
        }
        t
    }

    /// タイルの 1 辺。
    pub fn size(&self) -> u32 {
        self.size
    }

    /// 覆う画像の幅。
    pub fn width(&self) -> u32 {
        self.w
    }

    /// 覆う画像の高さ。
    pub fn height(&self) -> u32 {
        self.h
    }

    /// 横のタイルの数。
    pub fn tiles_x(&self) -> u32 {
        self.w.div_ceil(self.size)
    }

    /// 縦のタイルの数。
    pub fn tiles_y(&self) -> u32 {
        self.h.div_ceil(self.size)
    }

    fn index(&self, tx: u32, ty: u32) -> Option<usize> {
        (tx < self.tiles_x() && ty < self.tiles_y())
            .then(|| ty as usize * self.tiles_x() as usize + tx as usize)
    }

    /// タイルが入っていれば true。
    pub fn contains(&self, tx: u32, ty: u32) -> bool {
        self.index(tx, ty)
            .is_some_and(|i| (self.bits[i / 64] >> (i % 64)) & 1 == 1)
    }

    /// タイルを入れる（格子の外は無視）。
    pub fn insert(&mut self, tx: u32, ty: u32) {
        if let Some(i) = self.index(tx, ty) {
            self.bits[i / 64] |= 1 << (i % 64);
        }
    }

    /// タイルを外す。
    pub fn remove(&mut self, tx: u32, ty: u32) {
        if let Some(i) = self.index(tx, ty) {
            self.bits[i / 64] &= !(1 << (i % 64));
        }
    }

    /// 入っているタイルの数。
    pub fn count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// 空なら true。
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|w| *w == 0)
    }

    /// すべてのタイルが入っていれば true。
    pub fn is_full(&self) -> bool {
        self.count() == self.tiles_x() as usize * self.tiles_y() as usize
    }

    /// 同じ格子なら true。
    pub fn same_grid(&self, other: &Tiles) -> bool {
        (self.size, self.w, self.h) == (other.size, other.w, other.h)
    }

    /// 和（格子が違えば `Internal`）。
    pub fn union_with(&mut self, other: &Tiles) -> Result<(), EngineError> {
        self.combine(other, |a, b| a | b)
    }

    /// 差（格子が違えば `Internal`）。
    pub fn subtract(&mut self, other: &Tiles) -> Result<(), EngineError> {
        self.combine(other, |a, b| a & !b)
    }

    /// 共通部分（格子が違えば `Internal`）。
    pub fn intersect_with(&mut self, other: &Tiles) -> Result<(), EngineError> {
        self.combine(other, |a, b| a & b)
    }

    fn combine(&mut self, other: &Tiles, f: impl Fn(u64, u64) -> u64) -> Result<(), EngineError> {
        if !self.same_grid(other) {
            return Err(EngineError::Internal(
                "タイルの格子が違う集合を合わせようとしました".into(),
            ));
        }
        for (a, b) in self.bits.iter_mut().zip(&other.bits) {
            *a = f(*a, *b);
        }
        Ok(())
    }

    /// `other` に含まれていれば true（格子が違えば false）。
    pub fn is_subset_of(&self, other: &Tiles) -> bool {
        self.same_grid(other) && self.bits.iter().zip(&other.bits).all(|(a, b)| a & !b == 0)
    }

    /// 入っているタイル（行の順）。
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let tx_n = self.tiles_x();
        (0..self.tiles_y())
            .flat_map(move |ty| (0..tx_n).map(move |tx| (tx, ty)))
            .filter(|(tx, ty)| self.contains(*tx, *ty))
    }

    /// タイルの矩形（画像の座標。端のタイルは画像の中に切る）。
    pub fn tile_rect(&self, tx: u32, ty: u32) -> IRect {
        let (x, y) = (tx.saturating_mul(self.size), ty.saturating_mul(self.size));
        IRect::new(
            i64::from(x),
            i64::from(y),
            self.size.min(self.w.saturating_sub(x)),
            self.size.min(self.h.saturating_sub(y)),
        )
    }

    /// 入っているタイルを覆う矩形（キャンバスの座標。`origin` は画像の位置）。行ごとに続いたタイルを 1 つの矩形にする。
    pub fn to_rects(&self, origin: IPoint) -> Rects {
        if self.is_full() && !self.is_empty() {
            let mut r = Rects::new();
            r.push(IRect::new(origin.x, origin.y, self.w, self.h));
            return r;
        }
        let mut out = Rects::new();
        for ty in 0..self.tiles_y() {
            let mut tx = 0;
            while tx < self.tiles_x() {
                if !self.contains(tx, ty) {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < self.tiles_x() && self.contains(tx, ty) {
                    tx += 1;
                }
                let a = self.tile_rect(start, ty);
                let b = self.tile_rect(tx - 1, ty);
                let run = hull(&a, &b);
                out.push(IRect::new(
                    run.x.saturating_add(origin.x),
                    run.y.saturating_add(origin.y),
                    run.w,
                    run.h,
                ));
            }
        }
        out
    }
}

/// フレームの番号（画像は 0）ごとのタイルの集合（範囲の描画で描くタイル）。
pub type FrameTiles = BTreeMap<u32, Tiles>;

/// 出力が入力 0 のどこに依るか（操作の申告。`Op::reach`）。2 つ目からの入力には、いつも全体で依るとみなす。
#[derive(Debug, Clone, PartialEq)]
pub enum Reach {
    /// 全体に依る（既定。保守的）。
    Global,
    /// 同じフレームの同じ位置の画素だけに依る。
    Same,
    /// 軸に沿った単位の写像（平行移動・反転・90° の回転）：出力の点 p は入力の `m⁻¹(p)` に依る。`m` は入力から出力へ
    /// （`SpaceMap::Affine` と同じ向き）。入力の外に写る出力の画素（キャンバスの広げた部分）はどこにも依らない。
    Affine {
        m: IntAffine,
        in_size: Size,
        out_size: Size,
    },
    /// リサイズ：最近傍は `nearest_index`、平滑は重みの範囲（`imaging::resample::source_span`）。
    Resample {
        filter: Filter,
        in_size: Size,
        out_size: Size,
    },
    /// 音声：同じ位置（全チャンネル）。
    SameTime,
    /// 音声：区間ごとのずらし（トリム。`TimeMap::Segments` と同じ形）。
    Segments(Vec<Seg>),
    /// 音声：多相 FIR の窓（`audio::resample`。`(l, m)` は比、`h` はタップの数の半分）。
    Rate { l: u64, m: u64, h: u64 },
}

impl Reach {
    /// 逆写し：出力の範囲 `out` を描くのに要る入力 0 の範囲（範囲の描画）。`out` の `All` は、呼び手が `Area::expand` で
    /// 開いておくと狭く求まる（開いていなければ `All`）。
    pub fn backward(&self, out: &Area) -> Area {
        if out.is_empty() {
            return Area::Empty;
        }
        match self {
            Self::Global => Area::All,
            Self::Same | Self::SameTime => out.clone(),
            Self::Affine {
                m,
                in_size,
                out_size,
            } => match m.inverse() {
                Some(inv) => map_pixels(out, *out_size, *in_size, |r| inv.map_rect(r)),
                None => Area::All,
            },
            Self::Resample {
                filter,
                in_size,
                out_size,
            } => map_pixels(out, *out_size, *in_size, |r| {
                let (x0, x1) = axis(r.x, r.w)?;
                let (y0, y1) = axis(r.y, r.h)?;
                let (sx0, sx1) = source_span(in_size.w, out_size.w, *filter, x0, x1);
                let (sy0, sy1) = source_span(in_size.h, out_size.h, *filter, y0, y1);
                Some(span_rect(sx0, sx1, sy0, sy1))
            }),
            Self::Segments(segs) => map_spans(out, |a, b, s| {
                for seg in segs {
                    let len = seg.src_end.saturating_sub(seg.src_start);
                    let (d0, d1) = (seg.dst_start, seg.dst_start.saturating_add(len));
                    let (lo, hi) = (a.max(d0), b.min(d1));
                    if lo < hi {
                        s.push(
                            seg.src_start.saturating_add(lo - d0),
                            seg.src_start.saturating_add(hi - d0),
                        );
                    }
                }
            }),
            Self::Rate { l, m, h } => map_spans(out, |a, b, s| {
                let (lo, hi) = source_window(*l, *m, *h, a, b);
                s.push(lo, hi);
            }),
        }
    }

    /// 位置を写す申告（Affine・Resample・Segments・Rate）なら true（`All` を開いてから写すと狭く求まるもの）。
    fn maps_positions(&self) -> bool {
        !matches!(self, Self::Global | Self::Same | Self::SameTime)
    }

    /// [`Reach::backward`] の、出力の形 `out_shape` で `All` を開いてから写す版（位置を写さない申告は `All` のまま）。
    pub fn backward_in(&self, out: &Area, out_shape: &Shape) -> Area {
        if self.maps_positions() {
            self.backward(&out.expand(out_shape))
        } else {
            self.backward(out)
        }
    }

    /// [`Reach::forward`] の、入力 0 の形 `in_shape` で `All` を開いてから写す版（位置を写さない申告は `All` のまま）。
    pub fn forward_in(&self, d: &Area, in_shape: &Shape) -> Area {
        if self.maps_positions() {
            self.forward(&d.expand(in_shape))
        } else {
            self.forward(d)
        }
    }

    /// 前へ写す：入力 0 の範囲 `d` が変わったとき、出力の変わりうる範囲（変わった範囲の伝播）。`All` は `Area::expand` で
    /// 開いておくと狭く求まる。
    pub fn forward(&self, d: &Area) -> Area {
        if d.is_empty() {
            return Area::Empty;
        }
        match self {
            Self::Global => Area::All,
            Self::Same | Self::SameTime => d.clone(),
            Self::Affine {
                m,
                in_size,
                out_size,
            } => map_pixels(d, *in_size, *out_size, |r| m.map_rect(r)),
            Self::Resample {
                filter,
                in_size,
                out_size,
            } => map_pixels(d, *in_size, *out_size, |r| {
                let (x0, x1) = axis(r.x, r.w)?;
                let (y0, y1) = axis(r.y, r.h)?;
                let (dx0, dx1) = dest_span(in_size.w, out_size.w, *filter, x0, x1);
                let (dy0, dy1) = dest_span(in_size.h, out_size.h, *filter, y0, y1);
                Some(span_rect(dx0, dx1, dy0, dy1))
            }),
            Self::Segments(segs) => map_spans(d, |a, b, s| {
                for seg in segs {
                    let (lo, hi) = (a.max(seg.src_start), b.min(seg.src_end));
                    if lo < hi {
                        s.push(
                            seg.dst_start.saturating_add(lo - seg.src_start),
                            seg.dst_start.saturating_add(hi - seg.src_start),
                        );
                    }
                }
            }),
            Self::Rate { l, m, h } => map_spans(d, |a, b, s| {
                let (lo, hi) = dest_window(*l, *m, *h, a, b);
                s.push(lo, hi);
            }),
        }
    }
}

/// 矩形の軸の区間（0 以上に切った u32。空なら None）。
fn axis(start: i64, len: u32) -> Option<(u32, u32)> {
    let a = u32::try_from(start.max(0)).ok()?;
    let b = u32::try_from(start.saturating_add(i64::from(len)).max(0)).unwrap_or(u32::MAX);
    (a < b).then_some((a, b))
}

/// 軸の区間から矩形（空なら大きさ 0）。
fn span_rect(x0: u32, x1: u32, y0: u32, y1: u32) -> IRect {
    IRect::new(
        i64::from(x0),
        i64::from(y0),
        x1.saturating_sub(x0),
        y1.saturating_sub(y0),
    )
}

/// 画素の範囲を、フレームごと・矩形ごとに写す（`from` の大きさで切ってから `f` で写し、`to` の大きさで切る）。
/// `f` が None（写せない）なら `All`。フレーム全体は `from` の矩形として写す。
fn map_pixels(area: &Area, from: Size, to: Size, f: impl Fn(IRect) -> Option<IRect>) -> Area {
    let Area::Pixels(map) = area else {
        return match area {
            Area::Empty => Area::Empty,
            _ => Area::All,
        };
    };
    let (from_rect, to_rect) = (size_rect(from), size_rect(to));
    let mut out = BTreeMap::new();
    for (frame, rects) in map {
        let mut res = Rects::new();
        for r in rects.clipped(&from_rect) {
            let Some(m) = f(r) else {
                return Area::All;
            };
            if let Some(c) = intersect(&m, &to_rect) {
                res.push(c);
            }
        }
        out.insert(*frame, res);
    }
    Area::pixels(out)
}

/// 音声の区間を写す（`f` が写した区間を足す）。画素の範囲は `All`。
fn map_spans(area: &Area, f: impl Fn(u64, u64, &mut Spans)) -> Area {
    match area {
        Area::Samples(spans) => {
            let mut out = Spans::new();
            for (a, b) in spans.spans() {
                f(*a, *b, &mut out);
            }
            Area::samples(out)
        }
        Area::Empty => Area::Empty,
        _ => Area::All,
    }
}

/// 大きさの決まった 1 枚の画像の形なら、その大きさ（操作の `reach` の補助。決まっていなければ None で、`Global` にする）。
pub fn exact_image_size(s: &Shape) -> Option<Size> {
    match *s {
        Shape::Image {
            w: Dim::Exact(w),
            h: Dim::Exact(h),
            ..
        } => Some(Size {
            w: u32::try_from(w).ok()?,
            h: u32::try_from(h).ok()?,
        }),
        _ => None,
    }
}

/// 範囲の描画の前の値の画素（1 枚の画像の操作の補助。画像でなければ `Internal`）。
pub fn base_image(base: Option<&Value>) -> Result<Option<&Raster>, EngineError> {
    match base {
        None => Ok(None),
        Some(Value::Image(img)) => Ok(Some(&img.px)),
        Some(_) => Err(EngineError::Internal(
            "範囲の描画の前の値が画像ではありません".into(),
        )),
    }
}

/// 値の画像のフレーム（番号, 画素, キャンバスの中の位置）。1 枚の画像はフレーム 0・位置 0。音声は空。
pub fn frames_of(v: &Value) -> Vec<(u32, &Raster, IPoint)> {
    match v {
        Value::Image(img) => vec![(0, &img.px, IPoint { x: 0, y: 0 })],
        Value::Frames(fr) => fr
            .frames()
            .iter()
            .enumerate()
            .map(|(i, f)| (u32::try_from(i).unwrap_or(u32::MAX), &f.px, f.offset))
            .collect(),
        Value::Audio(_) => Vec::new(),
    }
}

/// 値の各フレームについて、範囲に重なるタイル（1 辺 `size`）。範囲に掛からないフレームも空の集合で入れる。
pub fn tiles_in(v: &Value, area: &Area, size: u32) -> FrameTiles {
    frames_of(v)
        .into_iter()
        .map(|(i, px, origin)| {
            let t = match area.frame_rects(i) {
                Some(r) => Tiles::covering(px.width(), px.height(), size, &r, origin),
                None => Tiles::none(px.width(), px.height(), size),
            };
            (i, t)
        })
        .collect()
}

/// フレーム `frame` の、範囲の描画で描くタイル（なければ空の集合）。格子は大きさ `w × h` の値のタイル（[`TILE`]）。
pub fn pick_of(tiles: &FrameTiles, frame: u32, w: u32, h: u32) -> Tiles {
    tiles
        .get(&frame)
        .cloned()
        .unwrap_or_else(|| Tiles::none(w, h, TILE))
}

/// 範囲の描画の補助：画像かフレームの列の画素を、フレームごとに `f(フレーム, 画素, 描くタイル, 前の値の画素)` で作り直す
/// （メタデータ・位置・表示時間・キャンバス・繰り返しはそのまま）。`base` は前の出力（同じ種類・同じフレームの数）。
pub fn map_rasters_region(
    src: &Value,
    tiles: &FrameTiles,
    base: Option<&Value>,
    mut f: impl FnMut(u32, &Raster, &Tiles, Option<&Raster>) -> Result<Raster, EngineError>,
) -> Result<Value, EngineError> {
    let mismatch = || EngineError::Internal("範囲の描画の前の値の形が違います".into());
    match src {
        Value::Image(img) => {
            let b = match base {
                Some(Value::Image(b)) => Some(&b.px),
                Some(_) => return Err(mismatch()),
                None => None,
            };
            let px = f(
                0,
                &img.px,
                &pick_of(tiles, 0, img.px.width(), img.px.height()),
                b,
            )?;
            Ok(Value::Image(Arc::new(Image {
                px,
                meta: img.meta.clone(),
            })))
        }
        Value::Frames(fr) => {
            let b = match base {
                Some(Value::Frames(b)) if b.frames().len() == fr.frames().len() => Some(b),
                Some(_) => return Err(mismatch()),
                None => None,
            };
            let mut out = Vec::with_capacity(fr.frames().len());
            for (i, frame) in fr.frames().iter().enumerate() {
                let n = u32::try_from(i).unwrap_or(u32::MAX);
                let bp = b.and_then(|b| b.frames().get(i)).map(|f| &f.px);
                out.push(Frame {
                    px: f(
                        n,
                        &frame.px,
                        &pick_of(tiles, n, frame.px.width(), frame.px.height()),
                        bp,
                    )?,
                    offset: frame.offset,
                    duration_ms: frame.duration_ms,
                });
            }
            Ok(Value::Frames(Arc::new(Frames::new(
                out,
                fr.canvas(),
                fr.meta.clone(),
                fr.loop_count,
            )?)))
        }
        Value::Audio(_) => Err(EngineError::Internal(
            "音声には画素の範囲の描画を当てられません".into(),
        )),
    }
}

/// 範囲の描画の補助：画素をそのまま使う操作（メタデータ・パレットだけを変えるもの）の、描くタイルの中身を入力から共有した
/// 値（ほかは前の値のタイルか無効なタイル。[`Raster::splice`]）。
pub fn splice_value(
    src: &Value,
    tiles: &FrameTiles,
    base: Option<&Value>,
) -> Result<Value, EngineError> {
    map_rasters_region(src, tiles, base, |_, px, pick, b| {
        Raster::splice(px, pick, b)
    })
}

/// 値の範囲 `area` に重なるタイルを無効にした値（中身は共有したまま。継ぎはぎの前の値から、変わった範囲を外すのに使う）。
pub fn invalidate(v: &Value, area: &Area) -> Result<Value, EngineError> {
    if area.is_empty() {
        return Ok(v.clone());
    }
    let tiles = tiles_in(v, area, TILE);
    map_rasters_region(v, &tiles, None, |_, px, pick, _| Ok(px.invalidate(pick)))
}

/// 範囲だけを描いた値（`Engine::render_area` の結果）。描かなかったタイルは無効で、読むと誤り（`Internal`）になる。
/// ダイジェストを取れず、保存・一括・解析に使わない（全体の値が要るときは `exact` で、由来が `Exact` のものだけを取り出す）。
#[derive(Debug, Clone)]
pub struct PartialValue {
    value: Value,
    origin: Origin,
}

impl PartialValue {
    /// 値と由来から作る（実行器だけ）。
    pub(crate) fn new(value: Value, origin: Origin) -> Self {
        Self { value, origin }
    }

    /// 全体を描いた値（`render`・`render_cached` の出力）から作る（継ぎはぎの前の値に使う）。無効なタイルがなければ由来は
    /// `Exact`、あれば `Partial`。
    pub fn full(value: Value) -> Self {
        let origin = if value.is_complete() {
            Origin::Exact
        } else {
            Origin::Partial
        };
        Self { value, origin }
    }

    /// 由来（`Exact` は等倍で全体を描いた値、`Patched` は前の値に継ぎはぎした値、`Partial` は範囲だけを描いた値、`Scaled` は
    /// プレビューの段の計画で描いた値。`Exact` のほかは配信にだけ使う）。
    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// 形。
    pub fn shape(&self) -> Shape {
        self.value.shape()
    }

    /// すべてのタイルが有効なら true（継ぎはぎの値は、すべて有効でも由来は `Patched`）。
    pub fn is_complete(&self) -> bool {
        self.value.is_complete()
    }

    /// 由来が `Exact`（等倍で全体を描いた値）のときだけ、その値（保存・解析・照合に使ってよいもの）。
    pub fn exact(&self) -> Option<&Value> {
        (self.origin == Origin::Exact && self.value.is_complete()).then_some(&self.value)
    }

    /// 実行器の中で、値そのもの（無効なタイルを含む）。
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }

    /// フレームの数（音声は 0）。
    pub fn frame_count(&self) -> usize {
        frames_of(&self.value).len()
    }

    // 配信の口（M12 が kura:// でタイル・メタデータを出すのに使う。読むだけで、保存の道に渡せる値は返さない）

    /// 画素の形式（フレームの列は全フレームで同じ）。音声は None。
    pub fn format(&self) -> Option<PixelFormat> {
        frames_of(&self.value).first().map(|(_, px, _)| px.format())
    }

    /// パレット（Indexed8 のとき。フレームの列は全フレームで同じ）。`tile`・`read_rect` が返すパレットの番号を色にするのに
    /// 使う。
    pub fn palette(&self) -> Option<&Palette> {
        frames_of(&self.value)
            .first()
            .and_then(|(_, px, _)| px.palette())
            .map(|p| p.as_ref())
    }

    /// 画像・フレームの列のメタデータ（格子・ピボット・スライス・タグなど。キャンバスの座標）。音声は None。
    pub fn image_meta(&self) -> Option<&ImageMeta> {
        match &self.value {
            Value::Image(img) => Some(&img.meta),
            Value::Frames(fr) => Some(&fr.meta),
            Value::Audio(_) => None,
        }
    }

    /// 音声のメタデータ（ループ点・マーカーなど）。画像は None。
    pub fn audio_meta(&self) -> Option<&AudioMeta> {
        match &self.value {
            Value::Audio(a) => Some(&a.meta),
            _ => None,
        }
    }

    /// フレームの列のフレーム `frame` の表示時間（ミリ秒）。1 枚の画像・音声・範囲の外は None。
    pub fn frame_duration_ms(&self, frame: u32) -> Option<u32> {
        match &self.value {
            Value::Frames(fr) => fr
                .frames()
                .get(usize::try_from(frame).ok()?)
                .map(|f| f.duration_ms),
            _ => None,
        }
    }

    /// フレームの列の繰り返しの回数（0 は無限）。フレームの列でなければ None。
    pub fn loop_count(&self) -> Option<u32> {
        match &self.value {
            Value::Frames(fr) => Some(fr.loop_count),
            _ => None,
        }
    }

    /// フレームのキャンバスの中の矩形（なければ None）。
    pub fn frame_rect(&self, frame: u32) -> Option<IRect> {
        frames_of(&self.value)
            .into_iter()
            .find(|(i, _, _)| *i == frame)
            .map(|(_, px, o)| IRect::new(o.x, o.y, px.width(), px.height()))
    }

    /// フレームの有効なタイル（1 辺 [`TILE`]。フレームの画像の座標）。音声・フレームがなければ None。
    pub fn valid(&self, frame: u32) -> Option<Tiles> {
        frames_of(&self.value)
            .into_iter()
            .find(|(i, _, _)| *i == frame)
            .map(|(_, px, _)| px.valid_tiles())
    }

    /// 範囲（キャンバスの座標）に重なるタイルがどれも有効なら true。音声は全体を描いた値だけ true。
    pub fn covers(&self, area: &Area) -> bool {
        if matches!(self.value, Value::Audio(_)) {
            return true;
        }
        if area.is_all() {
            return self.value.is_complete();
        }
        tiles_in(&self.value, area, TILE).iter().all(|(f, need)| {
            self.valid(*f)
                .is_some_and(|valid| need.is_subset_of(&valid))
        })
    }

    /// タイルの中身（フレームの画像の座標のタイル）。無効なタイル・範囲の外は誤り。
    pub fn tile(&self, frame: u32, tx: u32, ty: u32) -> Result<&[u8], EngineError> {
        let px = self.raster(frame)?;
        if tx >= px.tiles_x() || ty >= px.tiles_y() || px.is_hole(tx, ty) {
            return Err(EngineError::Internal(format!(
                "範囲だけを描いた値の、描いていないタイル（フレーム {frame}・{tx},{ty}）を読もうとしました"
            )));
        }
        Ok(px.tile(tx, ty))
    }

    /// 矩形（フレームの画像の座標）の画素を、行を詰めて `out` に写す。無効なタイルに掛かれば誤り。
    pub fn read_rect(&self, frame: u32, rect: IRect, out: &mut [u8]) -> Result<(), EngineError> {
        self.raster(frame)?.read_rect(rect, out)
    }

    /// スポイト：1 画素（キャンバスの座標）を拾う。画像の外なら None。無効なタイルの画素は誤り。
    pub fn sample(&self, frame: u32, x: i64, y: i64) -> Result<Option<Sample>, EngineError> {
        let Some(r) = self.frame_rect(frame) else {
            return Ok(None);
        };
        if x < r.x || y < r.y || x >= r.right() || y >= r.bottom() {
            return Ok(None);
        }
        let px = self.raster(frame)?;
        let (lx, ly) = ((x - r.x) as u32, (y - r.y) as u32);
        if px.is_hole(lx / TILE, ly / TILE) {
            return Err(EngineError::Internal(format!(
                "範囲だけを描いた値の、描いていない画素（フレーム {frame}・{x},{y}）を拾おうとしました"
            )));
        }
        Ok(crate::paint::sample(&self.value, frame, x, y))
    }

    /// 同じタイルの実体を共有していれば true（テスト用）。
    pub fn shares_tile(&self, other: &PartialValue, frame: u32, tx: u32, ty: u32) -> bool {
        match (self.raster(frame), other.raster(frame)) {
            (Ok(a), Ok(b)) => a.shares_tile(b, tx, ty),
            _ => false,
        }
    }

    fn raster(&self, frame: u32) -> Result<&Raster, EngineError> {
        frames_of(&self.value)
            .into_iter()
            .find(|(i, _, _)| *i == frame)
            .map(|(_, px, _)| px)
            .ok_or_else(|| EngineError::Internal(format!("フレーム {frame} はありません")))
    }
}

/// 継ぎはぎの前の値（`Engine::render_area` に渡す）：前の版の出力と、そこから変わりうる範囲（`Engine::delta` の推論）。
/// 変わりうる範囲の外のタイルは前の値を使い、中だけを描き直す（推論が保守的なら、全体を描いたものと同じ）。
#[derive(Debug, Clone, Copy)]
pub struct Base<'a> {
    /// 前の版のこの出力の値。
    pub value: &'a PartialValue,
    /// 前の版から変わりうる範囲（キャンバスの座標）。
    pub changed: &'a Area,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_merge_cover_and_fold() {
        let mut r = Rects::new();
        r.push(IRect::new(0, 0, 10, 10));
        r.push(IRect::new(2, 2, 3, 3)); // 覆われる
        r.push(IRect::new(0, 0, 0, 5)); // 空
        assert_eq!(r.rects().len(), 1);
        r.push(IRect::new(-5, -5, 20, 20)); // 前のものを覆う
        assert_eq!(r.rects(), &[IRect::new(-5, -5, 20, 20)]);
        let mut many = Rects::new();
        for i in 0..(MAX_RECTS as i64 + 1) {
            many.push(IRect::new(i * 10, 0, 2, 2));
        }
        assert_eq!(
            many.rects(),
            &[IRect::new(0, 0, 10 * MAX_RECTS as u32 + 2, 2)]
        );
        assert!(many.contains(5, 1) && !many.contains(5, 2));
        // 遠く離れた矩形を混ぜて畳むと外接矩形の幅・高さが u32 に収まらない：覆えないのでフレーム全体にする（段 10 の検証の
        // S3。docs/editing.md §12.5 の Rects の畳み）
        for far in [
            IRect::new(-(1 << 40), 0, 1, 1),
            IRect::new(0, i64::MAX - 1, 1, 1),
            IRect::new(i64::MIN, i64::MIN, 1, 1),
        ] {
            let mut r = Rects::new();
            r.push(far);
            for i in 0..MAX_RECTS as i64 {
                r.push(IRect::new(i * 10, 0, 2, 2));
            }
            assert!(r.is_whole(), "{far:?}: {:?}", r.rects());
            assert!(r.contains(5, 1) && r.contains(far.x, far.y));
        }
        let mut w = Rects::one(IRect::new(1, 1, 1, 1));
        w.union_with(&Rects::whole());
        assert!(w.is_whole() && w.contains(-100, 100));
    }

    #[test]
    fn spans_merge() {
        let mut s = Spans::new();
        s.push(10, 20);
        s.push(0, 5);
        s.push(5, 7); // 接する
        s.push(15, 30);
        s.push(3, 3); // 空
        assert_eq!(s.spans(), &[(0, 7), (10, 30)]);
        let mut many = Spans::new();
        for i in 0..(MAX_SPANS as u64 + 1) {
            many.push(i * 10, i * 10 + 1);
        }
        assert_eq!(many.spans(), &[(0, MAX_SPANS as u64 * 10 + 1)]);
    }

    #[test]
    fn area_union_expand_and_lift() {
        let a = Area::rect(0, IRect::new(0, 0, 4, 4));
        let b = Area::rect(1, IRect::new(2, 2, 4, 4));
        let u = a.union(&b);
        assert!(u.contains_pixel(0, 1, 1) && u.contains_pixel(1, 3, 3));
        assert!(!u.contains_pixel(1, 1, 1));
        assert_eq!(a.union(&Area::Empty), a);
        assert!(a.union(&Area::span(0, 3)).is_all());
        assert!(Area::rect(0, IRect::new(0, 0, 0, 1)).is_empty());
        let frames = Shape::Frames {
            count: Dim::Exact(3),
            w: Dim::Exact(9),
            h: Dim::Exact(9),
            fmt: PixelFormat::Rgba8,
            full: crate::shape::Fullness::Full,
        };
        let e = Area::All.expand(&frames);
        assert_eq!(e.frame_rects(2), Some(Rects::whole()));
        assert_eq!(e.frame_rects(3), None);
        let audio = Shape::Audio {
            rate: 8000,
            channels: 1,
            frames: Dim::Exact(100),
        };
        assert_eq!(Area::All.expand(&audio), Area::span(0, 100));
        let lifted = a.lift(Dim::Exact(2));
        assert!(lifted.contains_pixel(1, 3, 3) && !lifted.contains_pixel(2, 3, 3));
        assert!(a.lift(Dim::AtMost(4)).is_all());
    }

    #[test]
    fn tiles_cover_rects_with_offsets() {
        let r = Rects::one(IRect::new(250, 0, 10, 1));
        let t = Tiles::covering(600, 300, 256, &r, IPoint { x: 0, y: 0 });
        assert_eq!(t.iter().collect::<Vec<_>>(), vec![(0, 0), (1, 0)]);
        // フレームの位置を引く
        let t = Tiles::covering(600, 300, 256, &r, IPoint { x: 250, y: 0 });
        assert_eq!(t.iter().collect::<Vec<_>>(), vec![(0, 0)]);
        let t = Tiles::covering(
            600,
            300,
            256,
            &Rects::one(IRect::new(-9, -9, 5, 5)),
            IPoint { x: 0, y: 0 },
        );
        assert!(t.is_empty());
        let all = Tiles::all(600, 300, 256);
        assert_eq!(all.count(), 6);
        assert!(all.is_full());
        let mut d = all.clone();
        d.subtract(&t).unwrap();
        assert!(d.is_full());
        d.remove(2, 1);
        assert!(!d.is_full() && d.is_subset_of(&all) && !all.is_subset_of(&d));
        assert_eq!(all.tile_rect(2, 1), IRect::new(512, 256, 88, 44));
        assert!(d.union_with(&Tiles::none(5, 5, 256)).is_err());
        let rects = d.to_rects(IPoint { x: 1, y: 2 });
        assert!(
            rects.contains(1, 2) && rects.contains(511 + 1, 299 + 2) && !rects.contains(513, 259)
        );
    }

    #[test]
    fn reach_maps_rects_both_ways() {
        let size = |w, h| Size { w, h };
        // 切り抜き (10, 20) から：出力 (0, 0, 5, 5) は入力 (10, 20, 5, 5)
        let crop = Reach::Affine {
            m: IntAffine::translate(-10, -20),
            in_size: size(100, 100),
            out_size: size(50, 50),
        };
        let out = Area::rect(0, IRect::new(0, 0, 5, 5));
        assert_eq!(crop.backward(&out), Area::rect(0, IRect::new(10, 20, 5, 5)));
        assert_eq!(crop.forward(&Area::rect(0, IRect::new(10, 20, 5, 5))), out);
        // 入力の外に出る範囲（キャンバスを広げた部分）は依らない
        let canvas = Reach::Affine {
            m: IntAffine::translate(5, 5),
            in_size: size(10, 10),
            out_size: size(20, 20),
        };
        assert!(
            canvas
                .backward(&Area::rect(0, IRect::new(0, 0, 5, 5)))
                .is_empty()
        );
        assert_eq!(
            canvas.forward(&Area::whole_frame(0)),
            Area::rect(0, IRect::new(5, 5, 10, 10))
        );
        // 左右の反転
        let flip = Reach::Affine {
            m: IntAffine::flip_h(10),
            in_size: size(10, 4),
            out_size: size(10, 4),
        };
        assert_eq!(
            flip.backward(&Area::rect(0, IRect::new(0, 0, 3, 4))),
            Area::rect(0, IRect::new(7, 0, 3, 4))
        );
        // 最近傍の 2 倍
        let up = Reach::Resample {
            filter: Filter::Nearest,
            in_size: size(10, 10),
            out_size: size(20, 20),
        };
        assert_eq!(
            up.backward(&Area::rect(0, IRect::new(4, 4, 4, 4))),
            Area::rect(0, IRect::new(2, 2, 2, 2))
        );
        assert_eq!(
            up.forward(&Area::rect(0, IRect::new(2, 2, 2, 2))),
            Area::rect(0, IRect::new(4, 4, 4, 4))
        );
        assert!(Reach::Global.backward(&out).is_all());
        assert!(Reach::Global.forward(&Area::Empty).is_empty());
        assert_eq!(Reach::Same.backward(&out), out);
        // 音声のずらし
        let trim = Reach::Segments(vec![Seg {
            src_start: 100,
            src_end: 200,
            dst_start: 0,
        }]);
        assert_eq!(trim.backward(&Area::span(10, 20)), Area::span(110, 120));
        assert_eq!(trim.forward(&Area::span(150, 400)), Area::span(50, 100));
        assert!(trim.forward(&Area::span(0, 100)).is_empty());
    }

    #[test]
    fn spliced_and_invalidated_values_track_holes() {
        let px = Raster::filled(600, 300, PixelFormat::Gray8, None, &[7]).unwrap();
        let v = Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        }));
        let hole = invalidate(&v, &Area::rect(0, IRect::new(300, 10, 1, 1))).unwrap();
        assert!(!hole.is_complete());
        assert!(hole.digest().is_err());
        let p = PartialValue::full(hole.clone());
        assert_eq!(p.origin(), Origin::Partial);
        assert!(p.tile(0, 1, 0).is_err() && p.tile(0, 0, 0).is_ok());
        assert!(p.sample(0, 300, 10).is_err());
        assert_eq!(p.sample(0, 2, 2).unwrap().unwrap().raw, vec![7]);
        assert!(p.sample(0, 900, 2).unwrap().is_none());
        assert!(p.covers(&Area::rect(0, IRect::new(0, 0, 256, 256))));
        assert!(!p.covers(&Area::rect(0, IRect::new(0, 0, 257, 1))));
        // 無効なタイルを選んで共有しようとすると誤り（読んだことになる）
        let Value::Image(img) = &hole else { panic!() };
        let mut pick = Tiles::none(600, 300, TILE);
        pick.insert(1, 0);
        assert!(Raster::splice(&img.px, &pick, None).is_err());
        assert!(img.px.holes_read());
        // 有効なタイルを選ぶと共有し、ほかは無効（前の値があればそのタイル）
        let mut pick = Tiles::none(600, 300, TILE);
        pick.insert(0, 0);
        let only = splice_value(&v, &BTreeMap::from([(0, pick.clone())]), None).unwrap();
        let only = PartialValue::full(only);
        assert!(only.shares_tile(&PartialValue::full(v.clone()), 0, 0, 0));
        assert_eq!(only.valid(0).unwrap(), pick);
        let full = splice_value(&v, &BTreeMap::new(), Some(&v)).unwrap();
        assert!(full.is_complete());
    }
}
