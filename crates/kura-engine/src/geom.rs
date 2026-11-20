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

/// 相対の指定（[`Margin`]・[`CanvasLength`]）の比の分子・分母の上限（`image.resize` の比と同じ 2^16。極端な比で計算が
/// あふれないように）。
pub const RATIO_TERM_LIMIT: u32 = 1 << 16;

/// 余白 1 辺（`image.pad`。M14 段 1）。JSON は `{"px": n}`（負で削る。±2^31）か `{"scale": {"num", "den"}}`（その軸の入力の
/// 長さに対する比。分子は 0 も可）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Margin {
    /// 画素（負で削る）。
    Px(i64),
    /// その軸の入力の長さに対する比（`round_half_even(長さ·num/den)`）。
    Scale(Ratio),
}

impl Margin {
    /// 長さ `len` の軸に足す画素の数（負は削る）。比は `round_half_even(len·num/den)`。分母が 0 なら None。
    pub fn pixels(self, len: u64) -> Option<i64> {
        match self {
            Self::Px(n) => Some(n),
            Self::Scale(r) => {
                div_round_half_even(i128::from(len) * i128::from(r.num), i128::from(r.den))
            }
        }
    }
}

/// キャンバスの 1 辺の長さ（`image.fit_canvas`。M14 段 1）。JSON は `{"px": n}`・`{"scale": {"num", "den"}}`・`"keep"`・
/// `{"multipleOf": n}`・`"pow2"`・`"longest"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CanvasLength {
    /// 画素（1 以上）。
    Px(u32),
    /// その軸の入力の長さに対する比（`round_half_even(長さ·num/den)`。1 より小さくはしない）。
    Scale(Ratio),
    /// 入力のまま。
    Keep,
    /// 入力の長さを n の倍数に切り上げる（`⌈長さ/n⌉·n`。n は 1〜2^16）。
    MultipleOf(u32),
    /// 入力の長さ以上の最小の 2 の冪。
    Pow2,
    /// 入力の長い辺（両方の軸に使えば正方形）。
    Longest,
}

impl CanvasLength {
    /// 入力の大きさ（この軸 `len`・もう一方の軸 `other`）から求めた長さ。項の値が使えない（`MultipleOf(0)`・分母 0）か、
    /// あふれるなら None。範囲（1 以上・上限）は呼び手が確かめる（`Px(0)` は 0 を返す）。どれも `len`・`other` について単調
    /// （大きくすると小さくならない）。
    pub fn resolve(self, len: u64, other: u64) -> Option<u64> {
        match self {
            Self::Px(n) => Some(u64::from(n)),
            Self::Scale(r) => {
                let v =
                    div_round_half_even(i128::from(len) * i128::from(r.num), i128::from(r.den))?;
                Some(u64::try_from(v).ok()?.max(1))
            }
            Self::Keep => Some(len),
            Self::MultipleOf(n) => {
                let n = u64::from(n);
                if n == 0 {
                    return None;
                }
                len.div_ceil(n).checked_mul(n)
            }
            Self::Pow2 => len.max(1).checked_next_power_of_two(),
            Self::Longest => Some(len.max(other)),
        }
    }

    /// 長さが入力の大きさによらない（`Px`）なら true。
    pub fn is_fixed(self) -> bool {
        matches!(self, Self::Px(_))
    }

    /// もう一方の軸の長さにもよる（`Longest`）なら true。
    pub fn uses_other(self) -> bool {
        matches!(self, Self::Longest)
    }
}

/// 9 点の基準（`image.fit_canvas`・`meta.anchor_pivot`。M14 段 1。N → 1 のセルの揃え。段 6）。JSON は列挙の文字列（[`ANCHOR_NAMES`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// [`Anchor`] の JSON の名前（並びは [`Anchor::ALL`] と同じ。引数の記述の列挙の値に使う）。
pub const ANCHOR_NAMES: &[&str] = &[
    "topLeft",
    "top",
    "topRight",
    "left",
    "center",
    "right",
    "bottomLeft",
    "bottom",
    "bottomRight",
];

impl Anchor {
    /// すべての基準（左上から行の順）。
    pub const ALL: [Anchor; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// 横・縦の係数（左・上 0、中央 1、右・下 2。位置は ⌊長さ·係数/2⌋）。
    pub fn factors(self) -> (i64, i64) {
        match self {
            Self::TopLeft => (0, 0),
            Self::Top => (1, 0),
            Self::TopRight => (2, 0),
            Self::Left => (0, 1),
            Self::Center => (1, 1),
            Self::Right => (2, 1),
            Self::BottomLeft => (0, 2),
            Self::Bottom => (1, 2),
            Self::BottomRight => (2, 2),
        }
    }
}

/// 長さ `outer` の中に長さ `inner` を係数 `a`（[`Anchor::factors`] の 0・1・2）で置いたときの位置：⌊(outer − inner)·a / 2⌋
/// （負でもよい。床なので、割り切れない中央は中身が左・上へ 0.5px 寄る：広げる余りの 1px は右・下に付き、狭める余りの 1px は
/// 左・上から削る）。あふれるなら None。
pub fn anchor_offset(outer: i64, inner: i64, a: i64) -> Option<i64> {
    outer
        .checked_sub(inner)?
        .checked_mul(a)?
        .checked_div_euclid(2)
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

    /// 余白の JSON は `{"px": n}`（負も）・`{"scale": 比}`。画素の数は比を偶数への丸めで求める（分母 0 は None）。
    #[test]
    fn margins_read_and_resolve() {
        let px: Margin = serde_json::from_value(serde_json::json!({ "px": -3 })).unwrap();
        assert_eq!(px, Margin::Px(-3));
        let tenth: Margin =
            serde_json::from_value(serde_json::json!({ "scale": { "num": 1, "den": 10 } }))
                .unwrap();
        assert_eq!(
            serde_json::to_value(tenth).unwrap(),
            serde_json::json!({ "scale": { "num": 1, "den": 10 } })
        );
        assert_eq!(px.pixels(100), Some(-3));
        // 15/10 = 1.5 → 2、10/10 = 1、25/10 = 2.5 → 2（偶数へ）、5/10 = 0.5 → 0
        assert_eq!(tenth.pixels(15), Some(2));
        assert_eq!(tenth.pixels(10), Some(1));
        assert_eq!(tenth.pixels(25), Some(2));
        assert_eq!(tenth.pixels(5), Some(0));
        assert_eq!(Margin::Scale(Ratio { num: 0, den: 3 }).pixels(9), Some(0));
        assert_eq!(Margin::Scale(Ratio { num: 1, den: 0 }).pixels(9), None);
        // 知らない形・項目・2 つの形を一度に書いたものは読まない
        for bad in [
            serde_json::json!("px"),
            serde_json::json!({ "px": 1, "scale": { "num": 1, "den": 2 } }),
            serde_json::json!({ "scale": { "num": 1, "den": 2, "x": 0 } }),
            serde_json::json!({ "pct": 3 }),
            serde_json::json!({ "px": 1.5 }),
        ] {
            assert!(
                serde_json::from_value::<Margin>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    /// キャンバスの長さの 6 種：px・比（1 より小さくしない）・そのまま・倍数（切り上げ）・2 の冪・長い辺。
    #[test]
    fn canvas_lengths_read_and_resolve() {
        let read = |v: serde_json::Value| serde_json::from_value::<CanvasLength>(v).unwrap();
        let cases = [
            (serde_json::json!({ "px": 16 }), CanvasLength::Px(16)),
            (
                serde_json::json!({ "scale": { "num": 3, "den": 2 } }),
                CanvasLength::Scale(Ratio { num: 3, den: 2 }),
            ),
            (serde_json::json!("keep"), CanvasLength::Keep),
            (
                serde_json::json!({ "multipleOf": 8 }),
                CanvasLength::MultipleOf(8),
            ),
            (serde_json::json!("pow2"), CanvasLength::Pow2),
            (serde_json::json!("longest"), CanvasLength::Longest),
        ];
        for (json, want) in cases {
            assert_eq!(read(json.clone()), want);
            assert_eq!(serde_json::to_value(want).unwrap(), json);
        }
        assert_eq!(CanvasLength::Px(16).resolve(300, 2), Some(16));
        assert_eq!(CanvasLength::Px(0).resolve(300, 2), Some(0));
        // 7·3/2 = 10.5 → 10、5·3/2 = 7.5 → 8、1·1/4 = 0.25 → 0 → 1
        let s = CanvasLength::Scale(Ratio { num: 3, den: 2 });
        assert_eq!(s.resolve(7, 1), Some(10));
        assert_eq!(s.resolve(5, 1), Some(8));
        assert_eq!(
            CanvasLength::Scale(Ratio { num: 1, den: 4 }).resolve(1, 1),
            Some(1)
        );
        assert_eq!(
            CanvasLength::Scale(Ratio { num: 1, den: 0 }).resolve(1, 1),
            None
        );
        assert_eq!(CanvasLength::Keep.resolve(13, 99), Some(13));
        let m8 = CanvasLength::MultipleOf(8);
        assert_eq!(m8.resolve(13, 0), Some(16));
        assert_eq!(m8.resolve(16, 0), Some(16));
        assert_eq!(m8.resolve(1, 0), Some(8));
        assert_eq!(CanvasLength::MultipleOf(0).resolve(13, 0), None);
        assert_eq!(CanvasLength::MultipleOf(1).resolve(13, 0), Some(13));
        assert_eq!(CanvasLength::Pow2.resolve(300, 0), Some(512));
        assert_eq!(CanvasLength::Pow2.resolve(128, 0), Some(128));
        assert_eq!(CanvasLength::Pow2.resolve(1, 0), Some(1));
        assert_eq!(CanvasLength::Pow2.resolve(u64::MAX, 0), None);
        assert_eq!(CanvasLength::Longest.resolve(7, 12), Some(12));
        assert_eq!(CanvasLength::Longest.resolve(12, 7), Some(12));
        assert!(CanvasLength::Px(3).is_fixed() && !CanvasLength::Keep.is_fixed());
        assert!(CanvasLength::Longest.uses_other() && !CanvasLength::Pow2.uses_other());
        for bad in [
            serde_json::json!("auto"),
            serde_json::json!({ "multipleOf": -1 }),
            serde_json::json!({ "px": -1 }),
            serde_json::json!({ "px": 0, "keep": null }),
        ] {
            assert!(
                serde_json::from_value::<CanvasLength>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    /// 9 点の基準の名前と係数（左・上 0、中央 1、右・下 2）。
    #[test]
    fn anchors_have_names_and_factors() {
        assert_eq!(ANCHOR_NAMES.len(), Anchor::ALL.len());
        for (a, name) in Anchor::ALL.iter().zip(ANCHOR_NAMES) {
            assert_eq!(serde_json::to_value(a).unwrap(), serde_json::json!(name));
            let back: Anchor = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(back, *a);
        }
        let factors: Vec<(i64, i64)> = Anchor::ALL.iter().map(|a| a.factors()).collect();
        assert_eq!(
            factors,
            [
                (0, 0),
                (1, 0),
                (2, 0),
                (0, 1),
                (1, 1),
                (2, 1),
                (0, 2),
                (1, 2),
                (2, 2)
            ]
        );
        assert!(serde_json::from_value::<Anchor>(serde_json::json!("middle")).is_err());
    }

    /// 位置は ⌊(外 − 中)·a/2⌋：広げる余り（奇数）は右・下に、狭める余りは左・上から。偶数の差は真ん中。あふれは None。
    #[test]
    fn anchor_offsets_floor() {
        // 7 を 10 に：中央は 1（左 1・右 2）、右は 3、左は 0
        assert_eq!(anchor_offset(10, 7, 1), Some(1));
        assert_eq!(anchor_offset(10, 7, 2), Some(3));
        assert_eq!(anchor_offset(10, 7, 0), Some(0));
        // 偶数の差
        assert_eq!(anchor_offset(12, 8, 1), Some(2));
        // 20 を 15 に：中央は −3（左から 3・右から 2 削る）、右は −5
        assert_eq!(anchor_offset(15, 20, 1), Some(-3));
        assert_eq!(anchor_offset(15, 20, 2), Some(-5));
        assert_eq!(anchor_offset(16, 20, 1), Some(-2));
        assert_eq!(anchor_offset(16, 20, 0), Some(0));
        assert_eq!(anchor_offset(5, 5, 2), Some(0));
        assert_eq!(anchor_offset(i64::MAX, -1, 1), None);
        assert_eq!(anchor_offset(i64::MAX, 0, 2), None);
        assert_eq!(anchor_offset(i64::MAX, 0, 1), Some(i64::MAX / 2));
    }
}
