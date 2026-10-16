//! 整数の幾何（点・矩形・大きさ・比）。座標は画素の角を 0 とする連続の座標で、画素 (x, y) は [x, x+1) × [y, y+1)。

use serde::{Deserialize, Serialize};

/// 点。引数として読むときは知らない項目を拒む（文書の型は知らない項目を黙って捨てない。ADR-143 (4)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IPoint {
    pub x: i64,
    pub y: i64,
}

/// 大きさ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

/// 矩形（左上と大きさ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IRect {
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
}

impl IRect {
    /// 新しい矩形。
    pub fn new(x: i64, y: i64, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// 右端（含まない）。あふれるときは i64 の端で止める（範囲の判定では「外」になる）。
    pub fn right(&self) -> i64 {
        self.x.saturating_add(i64::from(self.w))
    }

    /// 下端（含まない）。あふれるときは i64 の端で止める。
    pub fn bottom(&self) -> i64 {
        self.y.saturating_add(i64::from(self.h))
    }

    /// 2 つの角から作る（順番は問わない）。
    pub fn from_corners(a: IPoint, b: IPoint) -> Option<Self> {
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        let w = u32::try_from(x1.checked_sub(x0)?).ok()?;
        let h = u32::try_from(y1.checked_sub(y0)?).ok()?;
        Some(Self { x: x0, y: y0, w, h })
    }

    /// `0..w × 0..h` の中に収まっていれば true（空の矩形は含めない）。
    pub fn inside(&self, w: u32, h: u32) -> bool {
        self.w > 0
            && self.h > 0
            && self.x >= 0
            && self.y >= 0
            && self.right() <= i64::from(w)
            && self.bottom() <= i64::from(h)
    }
}

/// 引数・メタデータの座標として受け付ける範囲（±2^31）。これより大きい値は範囲の外として断る。
/// 範囲の中の値どうしの和・差・小さな整数倍は i64 であふれない。
pub const COORD_LIMIT: i64 = 1 << 31;

/// 座標が受け付ける範囲の中なら true。
pub fn coord_ok(v: i64) -> bool {
    (-COORD_LIMIT..=COORD_LIMIT).contains(&v)
}

/// 整数の比（`num / den`）。大きさの引数に使い、浮動小数を使わずに丸めを一意にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ratio {
    pub num: u32,
    pub den: u32,
}

impl Ratio {
    /// `x × num / den` を最も近い整数に丸める（ちょうど半分なら偶数へ）。den が 0 なら None。
    pub fn apply_i64(&self, x: i64) -> Option<i64> {
        div_round_half_even(i128::from(x) * i128::from(self.num), i128::from(self.den))
    }
}

/// 長さの引数（docs/editing.md §5.4）。JSON は `{"px": n}`・`{"scale": {"num", "den"}}`・`"auto"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Length {
    /// 画素。
    Px(u32),
    /// 入力に対する比（「幅の 50%」は `Scale(1/2)`）。
    Scale(Ratio),
    /// もう一方の軸の倍率で縦横比を保つ。
    Auto,
}

/// `n / d` を最も近い整数に丸める（ちょうど半分なら偶数へ）。d が 0 なら None。
pub fn div_round_half_even(n: i128, d: i128) -> Option<i64> {
    if d == 0 {
        return None;
    }
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    let twice = 2 * r;
    let q = if twice > d || (twice == d && q % 2 != 0) {
        q + 1
    } else {
        q
    };
    i64::try_from(q).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_is_half_even() {
        assert_eq!(div_round_half_even(5, 2), Some(2));
        assert_eq!(div_round_half_even(7, 2), Some(4));
        assert_eq!(div_round_half_even(-5, 2), Some(-2));
        assert_eq!(div_round_half_even(10, 3), Some(3));
        assert_eq!(div_round_half_even(11, 3), Some(4));
        assert_eq!(div_round_half_even(1, 0), None);
        assert_eq!(Ratio { num: 1, den: 2 }.apply_i64(3), Some(2));
    }

    #[test]
    fn rect_inside() {
        assert!(IRect::new(0, 0, 4, 4).inside(4, 4));
        assert!(!IRect::new(1, 0, 4, 4).inside(4, 4));
        assert!(!IRect::new(-1, 0, 2, 2).inside(4, 4));
        assert!(!IRect::new(0, 0, 0, 2).inside(4, 4));
        let r = IRect::from_corners(IPoint { x: 4, y: 1 }, IPoint { x: 1, y: 3 }).unwrap();
        assert_eq!(r, IRect::new(1, 1, 3, 2));
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        let r = IRect::new(i64::MAX, i64::MAX, u32::MAX, u32::MAX);
        assert_eq!(r.right(), i64::MAX);
        assert!(!r.inside(10, 10));
        assert!(
            IRect::from_corners(IPoint { x: i64::MIN, y: 0 }, IPoint { x: i64::MAX, y: 0 })
                .is_none()
        );
        assert!(coord_ok(COORD_LIMIT) && !coord_ok(COORD_LIMIT + 1) && !coord_ok(i64::MIN));
    }
}
