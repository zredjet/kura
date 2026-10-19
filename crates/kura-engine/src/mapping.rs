//! メタデータの写し方（docs/editing.md §6）。
//!
//! 操作は「座標をどう写すか」（`SpaceMap`）と「時刻をどう写すか」（`TimeMap`）を申告するだけでよい。
//! 格子・ピボット・スライス・ループ点・マーカーは、ここでまとめて写す。範囲からはみ出したものは警告を出して落とす。
//! 整数の計算はすべてあふれを確かめ、あふれたものは「範囲の外」として落とす（落ちない。docs/editing.md §13）。

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, Issue, IssueCode};
use crate::geom::{IPoint, IRect, Ratio, Size, div_round_half_even};
use crate::value::{AudioMeta, Grid, ImageMeta, LoopRange, Marker, Slice};

/// 整数の 1 次の写像：`x' = a·x + b·y + c`、`y' = d·x + e·y + f`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntAffine {
    pub a: i64,
    pub b: i64,
    pub c: i64,
    pub d: i64,
    pub e: i64,
    pub f: i64,
}

impl IntAffine {
    /// 平行移動。
    pub fn translate(dx: i64, dy: i64) -> Self {
        Self {
            a: 1,
            b: 0,
            c: dx,
            d: 0,
            e: 1,
            f: dy,
        }
    }

    /// 左右の反転（幅 w の画像）。
    pub fn flip_h(w: u32) -> Self {
        Self {
            a: -1,
            b: 0,
            c: i64::from(w),
            d: 0,
            e: 1,
            f: 0,
        }
    }

    /// 上下の反転（高さ h の画像）。
    pub fn flip_v(h: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 0,
            e: -1,
            f: i64::from(h),
        }
    }

    /// 時計回りに 90°（高さ h の画像）。
    pub fn rotate_cw(h: u32) -> Self {
        Self {
            a: 0,
            b: -1,
            c: i64::from(h),
            d: 1,
            e: 0,
            f: 0,
        }
    }

    /// 反時計回りに 90°（幅 w の画像）。
    pub fn rotate_ccw(w: u32) -> Self {
        Self {
            a: 0,
            b: 1,
            c: 0,
            d: -1,
            e: 0,
            f: i64::from(w),
        }
    }

    /// 整数倍の拡大。
    pub fn scale(kx: i64, ky: i64) -> Self {
        Self {
            a: kx,
            b: 0,
            c: 0,
            d: 0,
            e: ky,
            f: 0,
        }
    }

    /// 点を写す。あふれたら None（範囲の外として扱う）。
    pub fn point(&self, p: IPoint) -> Option<IPoint> {
        let x = self
            .a
            .checked_mul(p.x)?
            .checked_add(self.b.checked_mul(p.y)?)?
            .checked_add(self.c)?;
        let y = self
            .d
            .checked_mul(p.x)?
            .checked_add(self.e.checked_mul(p.y)?)?
            .checked_add(self.f)?;
        Some(IPoint { x, y })
    }

    /// 平行移動を除いた部分でベクトルを写す。あふれたら None。
    pub fn vector(&self, x: i64, y: i64) -> Option<(i64, i64)> {
        let vx = self.a.checked_mul(x)?.checked_add(self.b.checked_mul(y)?)?;
        let vy = self.d.checked_mul(x)?.checked_add(self.e.checked_mul(y)?)?;
        Some((vx, vy))
    }

    /// 軸に沿った写像（反転・90° の回転・正の整数倍とその組み合わせ）なら true。
    pub fn axis_aligned(&self) -> bool {
        (self.b == 0 && self.d == 0 && self.a != 0 && self.e != 0)
            || (self.a == 0 && self.e == 0 && self.b != 0 && self.d != 0)
    }

    /// 逆の写像（段 10。範囲の逆写しに使う）。軸に沿った単位の写像（平行移動・反転・90° の回転。係数が 0 か ±1）だけで、
    /// ほかの写像やあふれるときは None。
    pub fn inverse(&self) -> Option<Self> {
        let unit = |v: i64| (-1..=1).contains(&v);
        if !self.axis_aligned() || ![self.a, self.b, self.d, self.e].into_iter().all(unit) {
            return None;
        }
        // 係数の行列は直交で ±1 だけなので、逆は転置。平行移動は −(転置)·(c, f)
        let (a, b, d, e) = (self.a, self.d, self.b, self.e);
        let c = a
            .checked_mul(self.c)?
            .checked_add(b.checked_mul(self.f)?)?
            .checked_neg()?;
        let f = d
            .checked_mul(self.c)?
            .checked_add(e.checked_mul(self.f)?)?
            .checked_neg()?;
        Some(Self { a, b, c, d, e, f })
    }

    /// 矩形（画素の集合 `[x, x+w) × [y, y+h)`）を写した矩形。角を写して並べ直すので、軸に沿った写像では正確。
    /// 空の矩形・あふれるときは None。
    pub fn map_rect(&self, r: IRect) -> Option<IRect> {
        if r.w == 0 || r.h == 0 {
            return None;
        }
        let a = self.point(IPoint { x: r.x, y: r.y })?;
        let b = self.point(IPoint {
            x: r.x.checked_add(i64::from(r.w))?,
            y: r.y.checked_add(i64::from(r.h))?,
        })?;
        IRect::from_corners(a, b)
    }
}

/// 座標の写し方。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum SpaceMap {
    /// 変えない。
    Identity,
    /// 整数の 1 次の写像。`out` は出力の大きさ（はみ出しの判定に使う）。
    Affine { m: IntAffine, out: Size },
    /// 任意の倍率（`x' = round_half_even(x × num / den)`）。
    Scale { sx: Ratio, sy: Ratio, out: Size },
    /// 推論の段階では分からない（実行で決まる）。
    Unknown,
    /// 座標を持つメタデータを捨てる。
    Reset,
}

/// 音声の区間の写し方の 1 区間：`[src_start, src_end)` を `dst_start` から並べる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Seg {
    pub src_start: u64,
    pub src_end: u64,
    pub dst_start: u64,
}

/// 時刻の写し方。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum TimeMap {
    Identity,
    /// トリム・結合：区間ごとの平行移動と切り取り。
    Segments {
        segs: Vec<Seg>,
    },
    /// リサンプル：`pos' = round_half_even(pos × to / from)`。
    Rate {
        from: u32,
        to: u32,
    },
    Unknown,
    Reset,
}

fn dropped(what: &str) -> Issue {
    Issue::warning(
        IssueCode::MetadataDropped,
        format!("{what}が範囲の外に出たので消しました"),
    )
}

/// 文書の全体の規則の版 `rules` の写し方で、画像のメタデータを写す（§8.4）。規則の版を上げたら、古い版の写し方も残して、
/// ここで版ごとに選ぶ。実行器と、メタデータを自分で書く操作（`MetaAccess::Write`）の両方がこれを使う。
pub fn map_image_meta_for(
    rules: u32,
    meta: &ImageMeta,
    input: Size,
    map: &SpaceMap,
) -> Result<(ImageMeta, Vec<Issue>), EngineError> {
    match rules {
        1 => Ok(map_image_meta(meta, input, map)),
        // 文書の形の確かめで知らない版は断っているので、ここには来ない
        other => Err(EngineError::Internal(format!(
            "規則の版 {other} の写し方がありません"
        ))),
    }
}

/// 文書の全体の規則の版 `rules` の写し方で、音声のメタデータを写す。
pub fn map_audio_meta_for(
    rules: u32,
    meta: &AudioMeta,
    map: &TimeMap,
    out_frames: u64,
) -> Result<(AudioMeta, Vec<Issue>), EngineError> {
    match rules {
        1 => Ok(map_audio_meta(meta, map, out_frames)),
        other => Err(EngineError::Internal(format!(
            "規則の版 {other} の写し方がありません"
        ))),
    }
}

/// 規則 1 の画像のメタデータの写し方。`input` は入力の大きさ。返すのは写したものと警告。
pub fn map_image_meta(meta: &ImageMeta, input: Size, map: &SpaceMap) -> (ImageMeta, Vec<Issue>) {
    let mut out = meta.clone();
    let mut warnings = Vec::new();
    let has_spatial = meta.grid.is_some() || meta.pivot.is_some() || !meta.slices.is_empty();
    let (point, size): (Box<dyn Fn(IPoint) -> Option<IPoint>>, Size) = match map {
        SpaceMap::Identity => return (out, warnings),
        SpaceMap::Affine { m, out: size } => {
            let m = *m;
            (Box::new(move |p| m.point(p)), *size)
        }
        SpaceMap::Scale { sx, sy, out: size } => {
            let (sx, sy) = (*sx, *sy);
            (
                Box::new(move |p| {
                    Some(IPoint {
                        x: sx.apply_i64(p.x)?,
                        y: sy.apply_i64(p.y)?,
                    })
                }),
                *size,
            )
        }
        SpaceMap::Unknown | SpaceMap::Reset => {
            out.grid = None;
            out.pivot = None;
            out.slices.clear();
            if has_spatial {
                warnings.push(Issue::warning(
                    IssueCode::MetadataDropped,
                    "格子・ピボット・スライスを消しました",
                ));
            }
            return (out, warnings);
        }
    };
    let inside_point =
        |p: IPoint| p.x >= 0 && p.y >= 0 && p.x <= i64::from(size.w) && p.y <= i64::from(size.h);
    let map_rect = |r: IRect| -> Option<IRect> {
        let a = point(IPoint { x: r.x, y: r.y })?;
        let b = point(IPoint {
            x: r.right(),
            y: r.bottom(),
        })?;
        IRect::from_corners(a, b).filter(|m| m.inside(size.w, size.h))
    };

    if let Some(p) = meta.pivot {
        out.pivot = point(p).filter(|q| inside_point(*q));
        if out.pivot.is_none() {
            warnings.push(dropped("ピボット"));
        }
    }

    out.slices = Vec::with_capacity(meta.slices.len());
    for s in &meta.slices {
        let Some(rect) = map_rect(s.rect) else {
            warnings.push(dropped(&format!("スライス「{}」", s.name)));
            continue;
        };
        let center = s.center.and_then(&map_rect);
        if s.center.is_some() && center.is_none() {
            warnings.push(dropped(&format!("スライス「{}」の 9 分割", s.name)));
        }
        let pivot = s.pivot.and_then(|p| point(p).filter(|q| inside_point(*q)));
        if s.pivot.is_some() && pivot.is_none() {
            warnings.push(dropped(&format!("スライス「{}」のピボット", s.name)));
        }
        out.slices.push(Slice {
            name: s.name.clone(),
            rect,
            center,
            pivot,
        });
    }

    if let Some(g) = meta.grid {
        out.grid = map_grid(&g, input, map);
        if out.grid.is_none() {
            warnings.push(Issue::warning(
                IssueCode::MetadataDropped,
                "格子が写せなかったので消しました",
            ));
        }
    }
    (out, warnings)
}

/// 格子を写す。軸に沿った整数の写像か、割り切れる倍率のときだけ写せる。写せなければ None。
pub fn map_grid(g: &Grid, input: Size, map: &SpaceMap) -> Option<Grid> {
    if g.cell_w == 0 || g.cell_h == 0 {
        return None;
    }
    match map {
        SpaceMap::Identity => Some(*g),
        SpaceMap::Affine { m, out } => map_grid_affine(g, input, m, *out),
        SpaceMap::Scale { sx, sy, out } => {
            let exact = |v: u32, r: &Ratio| -> Option<u32> {
                let n = u64::from(v) * u64::from(r.num);
                if r.den == 0 || n % u64::from(r.den) != 0 {
                    return None;
                }
                u32::try_from(n / u64::from(r.den)).ok()
            };
            let scaled = Grid {
                cell_w: exact(g.cell_w, sx)?,
                cell_h: exact(g.cell_h, sy)?,
                offset_x: exact(g.offset_x, sx)?,
                offset_y: exact(g.offset_y, sy)?,
                gap_x: exact(g.gap_x, sx)?,
                gap_y: exact(g.gap_y, sy)?,
            };
            if scaled.cell_w == 0 || scaled.cell_h == 0 {
                return None;
            }
            fit_grid(
                i128::from(scaled.offset_x),
                i128::from(scaled.offset_y),
                scaled,
                *out,
            )
        }
        SpaceMap::Unknown | SpaceMap::Reset => None,
    }
}

fn map_grid_affine(g: &Grid, input: Size, m: &IntAffine, out: Size) -> Option<Grid> {
    if !m.axis_aligned() {
        return None;
    }
    // 格子の計算は i128 で行う（u32 の和や積があふれない）
    let (cw, ch) = (i128::from(g.cell_w), i128::from(g.cell_h));
    let (px, py) = (cw + i128::from(g.gap_x), ch + i128::from(g.gap_y));
    let (ox, oy) = (i128::from(g.offset_x), i128::from(g.offset_y));
    let full = |len: u32, off: i128, cell: i128, period: i128| -> i128 {
        let len = i128::from(len);
        if off + cell > len {
            0
        } else {
            (len - off - cell) / period + 1
        }
    };
    let (nx, ny) = (full(input.w, ox, cw, px), full(input.h, oy, ch, py));
    if nx == 0 || ny == 0 {
        return None;
    }
    let pt = |x: i128, y: i128| -> Option<IPoint> {
        m.point(IPoint {
            x: i64::try_from(x).ok()?,
            y: i64::try_from(y).ok()?,
        })
    };
    let cell = IRect::from_corners(pt(ox, oy)?, pt(ox + cw, oy + ch)?)?;
    let block = IRect::from_corners(
        pt(ox, oy)?,
        pt(ox + (nx - 1) * px + cw, oy + (ny - 1) * py + ch)?,
    )?;
    // 間隔のベクトルの、写した先での横・縦の成分
    let vx = m.vector(i64::try_from(px).ok()?, 0)?;
    let vy = m.vector(0, i64::try_from(py).ok()?)?;
    let period_x = i128::from(vx.0).abs().max(i128::from(vy.0).abs());
    let period_y = i128::from(vx.1).abs().max(i128::from(vy.1).abs());
    let gap_x = u32::try_from(period_x - i128::from(cell.w)).ok()?;
    let gap_y = u32::try_from(period_y - i128::from(cell.h)).ok()?;
    let shaped = Grid {
        cell_w: cell.w,
        cell_h: cell.h,
        offset_x: 0,
        offset_y: 0,
        gap_x,
        gap_y,
    };
    fit_grid(i128::from(block.x), i128::from(block.y), shaped, out)
}

/// 左上が負なら、セルの間隔の倍数だけ進めて出力の中に入れる。最初のセルが出力に収まらなければ None。
fn fit_grid(x: i128, y: i128, g: Grid, out: Size) -> Option<Grid> {
    let (px, py) = (
        i128::from(g.cell_w) + i128::from(g.gap_x),
        i128::from(g.cell_h) + i128::from(g.gap_y),
    );
    if px <= 0 || py <= 0 {
        return None;
    }
    // 負なら、0 以上になる最小の「v + k·p」へ進める
    let advance = |v: i128, p: i128| if v < 0 { v + ((-v) + p - 1) / p * p } else { v };
    let (x, y) = (advance(x, px), advance(y, py));
    if x + i128::from(g.cell_w) > i128::from(out.w) || y + i128::from(g.cell_h) > i128::from(out.h)
    {
        return None;
    }
    Some(Grid {
        offset_x: u32::try_from(x).ok()?,
        offset_y: u32::try_from(y).ok()?,
        ..g
    })
}

impl TimeMap {
    /// 位置（フレーム）を写す。区間の外・あふれるときは None。
    pub fn pos(&self, pos: u64) -> Option<u64> {
        match self {
            Self::Identity => Some(pos),
            Self::Segments { segs } => segs
                .iter()
                .find(|s| pos >= s.src_start && pos < s.src_end)
                .and_then(|s| s.dst_start.checked_add(pos - s.src_start)),
            Self::Rate { from, to } => {
                let v = div_round_half_even(i128::from(pos) * i128::from(*to), i128::from(*from))?;
                u64::try_from(v).ok()
            }
            Self::Unknown | Self::Reset => None,
        }
    }

    /// 区間の終わり（含まない位置）を写す。`Segments` では区間の終わりちょうども写す。
    fn end(&self, end: u64) -> Option<u64> {
        match self {
            Self::Segments { segs } => segs
                .iter()
                .find(|s| end > s.src_start && end <= s.src_end)
                .and_then(|s| s.dst_start.checked_add(end - s.src_start)),
            _ => self.pos(end),
        }
    }
}

/// 規則 1 の音声のメタデータの写し方。`out_frames` は出力の長さ（これより後ろに出たものは詰めるか落とす）。返すのは写したものと警告。
pub fn map_audio_meta(meta: &AudioMeta, map: &TimeMap, out_frames: u64) -> (AudioMeta, Vec<Issue>) {
    let mut out = meta.clone();
    let mut warnings = Vec::new();
    if matches!(map, TimeMap::Unknown | TimeMap::Reset) {
        if meta.loop_range.is_some() || !meta.markers.is_empty() {
            warnings.push(Issue::warning(
                IssueCode::MetadataDropped,
                "ループ点・マーカーを消しました",
            ));
        }
        out.loop_range = None;
        out.markers.clear();
        return (out, warnings);
    }

    // ループ区間：区間の写しで重なった部分を写し、出力の長さで詰める
    out.loop_range = None;
    if let Some(l) = meta.loop_range {
        let mapped = match map {
            TimeMap::Segments { segs } => segs.iter().find_map(|s| {
                let (a, b) = (l.start.max(s.src_start), l.end.min(s.src_end));
                if a >= b {
                    return None;
                }
                let start = s.dst_start.checked_add(a - s.src_start)?;
                let end = s.dst_start.checked_add(b - s.src_start)?;
                Some((LoopRange { start, end }, a != l.start || b != l.end))
            }),
            _ => map
                .pos(l.start)
                .zip(map.end(l.end))
                .map(|(start, end)| (LoopRange { start, end }, false)),
        };
        match mapped {
            Some((range, clipped)) if range.start < out_frames && range.start < range.end => {
                let end = range.end.min(out_frames);
                if clipped || end != range.end {
                    warnings.push(Issue::warning(
                        IssueCode::MetadataDropped,
                        "ループ点を範囲に合わせて詰めました",
                    ));
                }
                out.loop_range = Some(LoopRange {
                    start: range.start,
                    end,
                });
            }
            _ => warnings.push(dropped("ループ点")),
        }
    }

    let before = meta.markers.len();
    out.markers = meta
        .markers
        .iter()
        .filter_map(|mk| {
            map.pos(mk.pos)
                .filter(|p| *p < out_frames)
                .map(|pos| Marker {
                    pos,
                    name: mk.name.clone(),
                })
        })
        .collect();
    if out.markers.len() != before {
        warnings.push(dropped("マーカー"));
    }
    (out, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Marker;

    /// 逆の写像は、写してから戻すと元の点・矩形になる（平行移動・反転・回転）。倍率や斜めの写像は None。
    #[test]
    fn inverse_and_map_rect_round_trip() {
        let maps = [
            IntAffine::translate(-7, 12),
            IntAffine::flip_h(300),
            IntAffine::flip_v(41),
            IntAffine::rotate_cw(17),
            IntAffine::rotate_ccw(23),
            IntAffine {
                a: -1,
                b: 0,
                c: 30,
                d: 0,
                e: -1,
                f: 9,
            },
        ];
        let rects = [
            IRect::new(0, 0, 1, 1),
            IRect::new(5, 3, 10, 7),
            IRect::new(-4, 250, 300, 2),
        ];
        for m in maps {
            let inv = m.inverse().unwrap();
            for p in [IPoint { x: 0, y: 0 }, IPoint { x: 13, y: -5 }] {
                assert_eq!(inv.point(m.point(p).unwrap()), Some(p), "{m:?}");
            }
            for r in rects {
                let there = m.map_rect(r).unwrap();
                assert_eq!(
                    u64::from(there.w) * u64::from(there.h),
                    u64::from(r.w) * u64::from(r.h)
                );
                assert_eq!(inv.map_rect(there), Some(r), "{m:?} {r:?}");
            }
        }
        assert!(IntAffine::scale(2, 1).inverse().is_none());
        let skew = IntAffine {
            a: 1,
            b: 1,
            c: 0,
            d: 0,
            e: 1,
            f: 0,
        };
        assert!(skew.inverse().is_none());
        assert!(IntAffine::translate(i64::MIN, 0).inverse().is_none());
        assert!(
            IntAffine::translate(1, 1)
                .map_rect(IRect::new(0, 0, 0, 3))
                .is_none()
        );
    }

    fn grid() -> Grid {
        Grid {
            cell_w: 16,
            cell_h: 16,
            offset_x: 0,
            offset_y: 0,
            gap_x: 0,
            gap_y: 0,
        }
    }

    #[test]
    fn translate_moves_pivot_and_advances_grid() {
        let mut meta = ImageMeta {
            grid: Some(grid()),
            pivot: Some(IPoint { x: 20, y: 20 }),
            ..ImageMeta::default()
        };
        meta.slices.push(Slice {
            name: "hit".into(),
            rect: IRect::new(16, 16, 8, 8),
            center: None,
            pivot: None,
        });
        let map = SpaceMap::Affine {
            m: IntAffine::translate(-8, -8),
            out: Size { w: 48, h: 48 },
        };
        let (out, warnings) = map_image_meta(&meta, Size { w: 64, h: 64 }, &map);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(out.pivot, Some(IPoint { x: 12, y: 12 }));
        assert_eq!(out.slices[0].rect, IRect::new(8, 8, 8, 8));
        // 切り抜きで左上のセルが欠けたので、次のセルから
        assert_eq!(
            out.grid,
            Some(Grid {
                offset_x: 8,
                offset_y: 8,
                ..grid()
            })
        );
    }

    #[test]
    fn things_outside_are_dropped_with_warnings() {
        let meta = ImageMeta {
            pivot: Some(IPoint { x: 2, y: 2 }),
            slices: vec![Slice {
                name: "a".into(),
                rect: IRect::new(0, 0, 4, 4),
                center: None,
                pivot: None,
            }],
            ..ImageMeta::default()
        };
        let map = SpaceMap::Affine {
            m: IntAffine::translate(-10, -10),
            out: Size { w: 10, h: 10 },
        };
        let (out, warnings) = map_image_meta(&meta, Size { w: 20, h: 20 }, &map);
        assert_eq!(out.pivot, None);
        assert!(out.slices.is_empty());
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn flip_and_rotate_keep_the_grid() {
        let g = Grid {
            cell_w: 16,
            cell_h: 8,
            offset_x: 2,
            offset_y: 0,
            gap_x: 1,
            gap_y: 0,
        };
        // 幅 70：2 + 16 + 1 + 16 + 1 + 16 + 1 + 16 = 69 で 4 セル。右に 1 余る
        let input = Size { w: 70, h: 16 };
        let flipped = map_grid(
            &g,
            input,
            &SpaceMap::Affine {
                m: IntAffine::flip_h(70),
                out: input,
            },
        )
        .unwrap();
        assert_eq!(flipped, Grid { offset_x: 1, ..g });
        // 2 回反転すると元に戻る
        let back = map_grid(
            &flipped,
            input,
            &SpaceMap::Affine {
                m: IntAffine::flip_h(70),
                out: input,
            },
        )
        .unwrap();
        assert_eq!(back, g);
        let rotated = map_grid(
            &g,
            input,
            &SpaceMap::Affine {
                m: IntAffine::rotate_cw(16),
                out: Size { w: 16, h: 70 },
            },
        )
        .unwrap();
        assert_eq!(
            rotated,
            Grid {
                cell_w: 8,
                cell_h: 16,
                offset_x: 0,
                offset_y: 2,
                gap_x: 0,
                gap_y: 1
            }
        );
    }

    #[test]
    fn scale_keeps_grid_only_when_exact() {
        let g = grid();
        let half = Ratio { num: 1, den: 2 };
        let third = Ratio { num: 1, den: 3 };
        let out = Size { w: 32, h: 32 };
        assert_eq!(
            map_grid(
                &g,
                Size { w: 64, h: 64 },
                &SpaceMap::Scale {
                    sx: half,
                    sy: half,
                    out
                }
            )
            .map(|g| g.cell_w),
            Some(8)
        );
        assert_eq!(
            map_grid(
                &g,
                Size { w: 64, h: 64 },
                &SpaceMap::Scale {
                    sx: third,
                    sy: third,
                    out
                }
            ),
            None
        );
    }

    #[test]
    fn segments_move_and_clip_loop_and_markers() {
        let meta = AudioMeta {
            loop_range: Some(LoopRange {
                start: 100,
                end: 300,
            }),
            markers: vec![
                Marker {
                    pos: 50,
                    name: "a".into(),
                },
                Marker {
                    pos: 150,
                    name: "b".into(),
                },
            ],
            ..AudioMeta::default()
        };
        // 120..1000 を残すトリム
        let map = TimeMap::Segments {
            segs: vec![Seg {
                src_start: 120,
                src_end: 1000,
                dst_start: 0,
            }],
        };
        let (out, warnings) = map_audio_meta(&meta, &map, 880);
        assert_eq!(out.loop_range, Some(LoopRange { start: 0, end: 180 }));
        assert_eq!(
            out.markers,
            vec![Marker {
                pos: 30,
                name: "b".into()
            }]
        );
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn rate_rounds_half_even() {
        let map = TimeMap::Rate {
            from: 44_100,
            to: 48_000,
        };
        assert_eq!(map.pos(44_100), Some(48_000));
        assert_eq!(map.pos(0), Some(0));
        let meta = AudioMeta {
            loop_range: Some(LoopRange {
                start: 0,
                end: 44_100,
            }),
            ..AudioMeta::default()
        };
        assert_eq!(
            map_audio_meta(&meta, &map, 48_000).0.loop_range,
            Some(LoopRange {
                start: 0,
                end: 48_000
            })
        );
    }

    #[test]
    fn overflowing_grids_and_points_are_dropped_not_panicking() {
        let input = Size { w: 4, h: 4 };
        let flip = SpaceMap::Affine {
            m: IntAffine::flip_h(4),
            out: input,
        };
        let g = Grid {
            cell_w: 1,
            cell_h: 1,
            offset_x: 0,
            offset_y: 0,
            gap_x: u32::MAX,
            gap_y: u32::MAX,
        };
        // 周期が u32 を超えても落ちない（セルは 1 つだけ入る）
        assert!(map_grid(&g, input, &flip).is_some());
        let huge = Grid {
            cell_w: u32::MAX,
            cell_h: u32::MAX,
            offset_x: u32::MAX,
            offset_y: u32::MAX,
            gap_x: u32::MAX,
            gap_y: u32::MAX,
        };
        assert_eq!(map_grid(&huge, input, &flip), None);
        let meta = ImageMeta {
            pivot: Some(IPoint {
                x: i64::MIN,
                y: i64::MAX,
            }),
            slices: vec![Slice {
                name: "s".into(),
                rect: IRect::new(i64::MAX, 0, u32::MAX, 1),
                center: None,
                pivot: None,
            }],
            ..ImageMeta::default()
        };
        let (out, warnings) = map_image_meta(&meta, input, &flip);
        assert_eq!(out.pivot, None);
        assert!(out.slices.is_empty());
        assert_eq!(warnings.len(), 2);
        let scale = SpaceMap::Affine {
            m: IntAffine::scale(i64::MAX, i64::MAX),
            out: input,
        };
        assert_eq!(map_image_meta(&meta, input, &scale).0.pivot, None);
    }

    #[test]
    fn unknown_and_reset_drop_spatial_meta_but_keep_the_rest() {
        let meta = ImageMeta {
            grid: Some(grid()),
            dpi: Some((144, 144)),
            text: [("Software".to_string(), "Kura".to_string())].into(),
            ..ImageMeta::default()
        };
        for map in [SpaceMap::Unknown, SpaceMap::Reset] {
            let (out, warnings) = map_image_meta(&meta, Size { w: 64, h: 64 }, &map);
            assert_eq!(out.grid, None);
            assert_eq!(out.dpi, Some((144, 144)));
            assert_eq!(out.text, meta.text);
            assert_eq!(warnings.len(), 1);
        }
        let plain = ImageMeta::default();
        assert!(
            map_image_meta(&plain, Size { w: 1, h: 1 }, &SpaceMap::Reset)
                .1
                .is_empty()
        );
        let audio = AudioMeta {
            loop_range: Some(LoopRange { start: 1, end: 2 }),
            ..AudioMeta::default()
        };
        for map in [TimeMap::Unknown, TimeMap::Reset] {
            let (out, warnings) = map_audio_meta(&audio, &map, 10);
            assert_eq!(out.loop_range, None);
            assert_eq!(warnings.len(), 1);
        }
    }

    #[test]
    fn slice_center_and_pivot_are_mapped_or_dropped_with_warnings() {
        let meta = ImageMeta {
            slices: vec![Slice {
                name: "box".into(),
                rect: IRect::new(10, 10, 8, 8),
                center: Some(IRect::new(12, 12, 4, 4)),
                pivot: Some(IPoint { x: 2, y: 2 }),
            }],
            ..ImageMeta::default()
        };
        let map = SpaceMap::Affine {
            m: IntAffine::translate(-5, -5),
            out: Size { w: 20, h: 20 },
        };
        let (out, warnings) = map_image_meta(&meta, Size { w: 30, h: 30 }, &map);
        assert_eq!(out.slices[0].rect, IRect::new(5, 5, 8, 8));
        assert_eq!(out.slices[0].center, Some(IRect::new(7, 7, 4, 4)));
        assert_eq!(out.slices[0].pivot, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn rate_clips_to_the_output_length() {
        let meta = AudioMeta {
            loop_range: Some(LoopRange { start: 1, end: 3 }),
            markers: vec![Marker {
                pos: 3,
                name: "end".into(),
            }],
            ..AudioMeta::default()
        };
        // 3 フレームを半分のレートにすると、出力は 2 フレーム（0..2）
        let (out, warnings) = map_audio_meta(&meta, &TimeMap::Rate { from: 2, to: 1 }, 2);
        assert_eq!(out.loop_range, Some(LoopRange { start: 0, end: 2 }));
        assert!(out.markers.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let identity = map_audio_meta(&meta, &TimeMap::Identity, 2);
        assert_eq!(identity.0.loop_range, Some(LoopRange { start: 1, end: 2 }));
        assert_eq!(identity.1.len(), 2);
    }
}
