//! ストロークの画素（docs/editing.md §10）。整数だけで、規則を凍結する（TS の `src/lib/paint.ts` と同じ規則）。
//!
//! 1. **線**：点と点のあいだをブレゼンハム（両端を含む）でつなぐ。`dx = |x1−x0|`・`dy = −|y1−y0|`・`err = dx + dy` から、
//!    点を置き、終点なら終わり、`e2 = 2·err` が `e2 ≥ dy` なら x を進め（err += dy）、`e2 ≤ dx` なら y を進める（err += dx）。
//!    向きは点の順（a から b）。続く区間は、前の区間の終点を重ねずにつなぎ、同じ点が続いたら 1 つにする。
//! 2. **ピクセルパーフェクト**：最後に残した点 p・いまの点 c・次の点 n が L 字（p と n が斜めに隣り合い、c がどちらとも
//!    縦か横で並ぶ）なら c を除く。最初と最後の点は除かない。
//! 3. **筆の形**：大きさ s（1〜255）の s×s のブロックを、左上が点から `⌊(s−1)/2⌋` だけ左上になるように置く。
//!    四角はブロック全部、円はブロックの中の (i, j) のうち `(2i+1−s)² + (2j+1−s)² ≤ s² − s` のもの。
//! 4. 線の各点に筆の形を押した画素の和集合を求め、各画素に 1 回だけ色を置く（重なっても濃くならず、順番に左右されない）。

use crate::error::EngineError;
use crate::geom::{IPoint, IRect};

/// 筆の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrushShape {
    Square,
    Circle,
}

/// 筆。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brush {
    pub shape: BrushShape,
    /// 1〜255。
    pub size: u8,
}

/// 筆の形の画素（点からのずれ。行の順）。
pub fn footprint(b: Brush) -> Vec<(i64, i64)> {
    let s = i64::from(b.size.max(1));
    let o = (s - 1) / 2;
    let mut out = Vec::new();
    for j in 0..s {
        for i in 0..s {
            let inside = match b.shape {
                BrushShape::Square => true,
                BrushShape::Circle => {
                    let (u, v) = (2 * i + 1 - s, 2 * j + 1 - s);
                    u * u + v * v <= s * s - s
                }
            };
            if inside {
                out.push((i - o, j - o));
            }
        }
    }
    out
}

/// 筆の形の画素の数（形を作らずに求める。円は大きさごとに 1 回だけ数えて覚える）。
pub fn footprint_len(b: Brush) -> u64 {
    let s = u64::from(b.size.max(1));
    match b.shape {
        BrushShape::Square => s * s,
        BrushShape::Circle => {
            static CIRCLE: std::sync::OnceLock<Vec<u64>> = std::sync::OnceLock::new();
            let table = CIRCLE.get_or_init(|| {
                (0..=255u8)
                    .map(|size| {
                        footprint(Brush {
                            shape: BrushShape::Circle,
                            size,
                        })
                        .len() as u64
                    })
                    .collect()
            });
            table[usize::from(b.size.max(1))]
        }
    }
}

/// 筆の形が点から広がる範囲（左上・右下のずれ。どちらの形もこのブロックに収まる）。
pub fn reach(b: Brush) -> (i64, i64) {
    let s = i64::from(b.size.max(1));
    let o = (s - 1) / 2;
    (-o, s - 1 - o)
}

/// 線の点の数（両端を含む）。
fn segment_len(a: IPoint, b: IPoint) -> u64 {
    a.x.abs_diff(b.x).max(a.y.abs_diff(b.y)) + 1
}

/// 線をつないだ道の点の数の上限（点を作らずに求める。手間の見積もりに使う）。
pub fn path_len_bound(points: &[IPoint]) -> u64 {
    match points {
        [] => 0,
        [_] => 1,
        _ => points
            .windows(2)
            .map(|w| segment_len(w[0], w[1]))
            .fold(0u64, u64::saturating_add),
    }
}

/// 線の点を順に返す（ブレゼンハム。両端を含む）。
struct Line {
    x: i64,
    y: i64,
    x1: i64,
    y1: i64,
    dx: i64,
    dy: i64,
    sx: i64,
    sy: i64,
    err: i64,
    done: bool,
}

impl Line {
    fn new(a: IPoint, b: IPoint) -> Self {
        let dx = (b.x - a.x).abs();
        let dy = -(b.y - a.y).abs();
        Self {
            x: a.x,
            y: a.y,
            x1: b.x,
            y1: b.y,
            dx,
            dy,
            sx: if a.x < b.x { 1 } else { -1 },
            sy: if a.y < b.y { 1 } else { -1 },
            err: dx + dy,
            done: false,
        }
    }
}

impl Iterator for Line {
    type Item = IPoint;

    fn next(&mut self) -> Option<IPoint> {
        if self.done {
            return None;
        }
        let p = IPoint {
            x: self.x,
            y: self.y,
        };
        if self.x == self.x1 && self.y == self.y1 {
            self.done = true;
            return Some(p);
        }
        let e2 = 2 * self.err;
        if e2 >= self.dy {
            self.err += self.dy;
            self.x += self.sx;
        }
        if e2 <= self.dx {
            self.err += self.dx;
            self.y += self.sy;
        }
        Some(p)
    }
}

/// 点の列をつないだ道（同じ点が続いたら 1 つ）。
fn raw_path(points: &[IPoint]) -> impl Iterator<Item = IPoint> + '_ {
    let first = points.first().copied();
    let segments = points
        .windows(2)
        .flat_map(|w| Line::new(w[0], w[1]).skip(1));
    let mut last: Option<IPoint> = None;
    first.into_iter().chain(segments).filter(move |p| {
        let keep = last != Some(*p);
        last = Some(*p);
        keep
    })
}

fn is_l(p: IPoint, c: IPoint, n: IPoint) -> bool {
    (p.x == c.x || p.y == c.y) && (n.x == c.x || n.y == c.y) && p.x != n.x && p.y != n.y
}

/// 道の点を順に `f` に渡す（ピクセルパーフェクトなら L 字の角を除く）。`f` がエラーを返したらそこで止める。
pub fn for_each_path_point<E>(
    points: &[IPoint],
    pixel_perfect: bool,
    mut f: impl FnMut(IPoint) -> Result<(), E>,
) -> Result<(), E> {
    if !pixel_perfect {
        for p in raw_path(points) {
            f(p)?;
        }
        return Ok(());
    }
    let mut it = raw_path(points).peekable();
    let mut last: Option<IPoint> = None;
    while let Some(c) = it.next() {
        if let (Some(p), Some(n)) = (last, it.peek().copied())
            && is_l(p, c, n)
        {
            continue;
        }
        f(c)?;
        last = Some(c);
    }
    Ok(())
}

/// 道の点の列（テスト・共有のベクタ用）。
pub fn path(points: &[IPoint], pixel_perfect: bool) -> Vec<IPoint> {
    let mut out = Vec::new();
    let _ = for_each_path_point::<std::convert::Infallible>(points, pixel_perfect, |p| {
        out.push(p);
        Ok(())
    });
    out
}

pub use crate::paint::mask::Mask;

/// 点の外接矩形に筆の広がりを足した矩形（`clip` で切ったもの。重ならなければ None）。
pub fn stroke_bounds(points: &[IPoint], brush: Brush, clip: IRect) -> Option<IRect> {
    let first = points.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    let (lo, hi) = reach(brush);
    let (bx0, by0) = (
        x0.saturating_add(lo).max(clip.x),
        y0.saturating_add(lo).max(clip.y),
    );
    let (bx1, by1) = (
        x1.saturating_add(hi + 1).min(clip.right()),
        y1.saturating_add(hi + 1).min(clip.bottom()),
    );
    (bx0 < bx1 && by0 < by1).then(|| IRect::new(bx0, by0, (bx1 - bx0) as u32, (by1 - by0) as u32))
}

/// ストロークの画素の集合（`clip` の中だけ）。`clip` と重ならなければ None。押した画素 65,536 個ごとに中止を確かめる。
pub fn stroke_mask(
    points: &[IPoint],
    brush: Brush,
    pixel_perfect: bool,
    clip: IRect,
    mon: &dyn crate::monitor::Monitor,
) -> Result<Option<Mask>, EngineError> {
    // 道は点の外接矩形の中に収まるので、筆の広がりを足した矩形と clip の重なりだけを見る
    let Some(rect) = stroke_bounds(points, brush, clip) else {
        return Ok(None);
    };
    let fp = footprint(brush);
    let mut mask = Mask::new(rect)?;
    // 押した画素の数で数えて、65,536 画素ごとに中止を確かめる（大きな筆でも 1 本の中で止まれるように）
    let mut pushed = 0u64;
    for_each_path_point(points, pixel_perfect, |p| {
        let before = pushed;
        pushed += fp.len() as u64;
        if before / 65_536 != pushed / 65_536 && mon.canceled() {
            return Err(EngineError::Canceled);
        }
        for (dx, dy) in &fp {
            mask.set(p.x + dx, p.y + dy);
        }
        Ok(())
    })?;
    Ok(Some(mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i64, y: i64) -> IPoint {
        IPoint { x, y }
    }

    #[test]
    fn lines_are_bresenham_with_both_ends() {
        assert_eq!(
            path(&[p(0, 0), p(3, 1)], false),
            [p(0, 0), p(1, 0), p(2, 1), p(3, 1)]
        );
        assert_eq!(path(&[p(2, 2)], false), [p(2, 2)]);
        assert_eq!(
            path(&[p(0, 0), p(0, 0), p(1, 1)], false),
            [p(0, 0), p(1, 1)]
        );
        // 向きは点の順
        assert_eq!(
            path(&[p(3, 1), p(0, 0)], false),
            [p(3, 1), p(2, 1), p(1, 0), p(0, 0)]
        );
        let l = path(&[p(-5, 7), p(9, -3)], false);
        assert_eq!(l.len() as u64, path_len_bound(&[p(-5, 7), p(9, -3)]));
        assert!(
            l.windows(2)
                .all(|w| w[0].x.abs_diff(w[1].x) <= 1 && w[0].y.abs_diff(w[1].y) <= 1)
        );
    }

    #[test]
    fn pixel_perfect_drops_l_corners() {
        // 右・下・右の階段は L 字の角が落ちて斜めになる
        let pts = [p(0, 0), p(1, 0), p(1, 1), p(2, 1), p(2, 2)];
        assert_eq!(path(&pts, false).len(), 5);
        assert_eq!(path(&pts, true), [p(0, 0), p(1, 1), p(2, 2)]);
        // まっすぐな線はそのまま
        assert_eq!(path(&[p(0, 0), p(4, 0)], true).len(), 5);
    }

    #[test]
    fn brush_shapes() {
        let circle = |s| {
            footprint(Brush {
                shape: BrushShape::Circle,
                size: s,
            })
        };
        assert_eq!(circle(1), [(0, 0)]);
        assert_eq!(circle(2).len(), 4);
        // 3 は十字
        assert_eq!(circle(3), [(0, -1), (-1, 0), (0, 0), (1, 0), (0, 1)]);
        // 4・5 は四隅を欠く
        assert_eq!(circle(4).len(), 12);
        assert_eq!(circle(5).len(), 21);
        assert_eq!(
            footprint(Brush {
                shape: BrushShape::Square,
                size: 4
            })
            .len(),
            16
        );
        assert_eq!(
            footprint(Brush {
                shape: BrushShape::Square,
                size: 2
            })[0],
            (0, 0)
        );
        assert_eq!(
            footprint(Brush {
                shape: BrushShape::Square,
                size: 3
            })[0],
            (-1, -1)
        );
    }

    /// 大きな筆の 1 本のストロークの中でも中止できる（押した画素の数で数える）。
    #[test]
    fn a_single_big_stroke_can_be_canceled() {
        let b = Brush {
            shape: BrushShape::Square,
            size: 255,
        };
        let flag = crate::monitor::CancelFlag::default();
        flag.cancel();
        let r = stroke_mask(
            &[p(0, 0), p(100, 0)],
            b,
            false,
            IRect::new(0, 0, 400, 300),
            &flag,
        );
        assert!(matches!(r, Err(EngineError::Canceled)));
        // 1 点だけ（65,536 画素に届かない）なら確かめずに終わる
        assert!(stroke_mask(&[p(0, 0)], b, false, IRect::new(0, 0, 400, 300), &flag).is_ok());
    }

    #[test]
    fn masks_clip_and_count_each_pixel_once() {
        let b = Brush {
            shape: BrushShape::Square,
            size: 3,
        };
        let m = stroke_mask(
            &[p(0, 0), p(5, 0)],
            b,
            false,
            IRect::new(0, 0, 4, 4),
            &crate::monitor::NoMonitor,
        )
        .unwrap()
        .unwrap();
        // x 0..4（切った範囲）× y 0..2
        assert_eq!(m.count(), 8);
        assert!(m.get(3, 1) && !m.get(3, 2) && !m.get(-1, 0));
        let mut seen = Vec::new();
        m.for_each(|x, y| {
            seen.push((x, y));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 8);
        assert!(seen.windows(2).all(|w| (w[0].1, w[0].0) < (w[1].1, w[1].0)));
        assert!(
            stroke_mask(
                &[p(50, 50)],
                b,
                false,
                IRect::new(0, 0, 4, 4),
                &crate::monitor::NoMonitor
            )
            .unwrap()
            .is_none()
        );
    }
}
