//! 2 次（と 1 次）の IIR の段の係数（M17 段 2。docs/editing.md §18.5、ADR-231 (6)・ADR-234）。式は凍結する（変えるなら
//! 使う操作の版を上げる）。超越関数は `crate::math` だけ、平方根は IEEE の `sqrt`。
//!
//! 差分の式は u[n] = b0·x[n] + b1·x[n−1] + b2·x[n−2] − a1·u[n−1] − a2·u[n−2]（1 次の段は b2 = a2 = 0）。周波数 ω は
//! ラジアン / 標本（ω0 = 2π·f0 / fs）。
//!
//! - 極はインパルス不変（Vicanek 2016 の式 12）：q = 1/(2Q)、q ≤ 1 なら a1 = −2e^{−qω0}·cos(√(1−q²)·ω0)、q > 1 なら
//!   cosh、a2 = e^{−2qω0}。
//! - ローパス・ハイパス・バンドパス・ピーキング（ベル）は、Vicanek "Matched Second Order Digital Filters"（2016）の
//!   4.1〜4.4 の閉じた式（振幅を直流・中心の値と極値で合わせる）。ベルの極は Q·√G で求める（原型の分母が s ω0/(√G Q)）。
//!   バンドパスの B1・B2 は 4.3 と同じ条件（中心で 1・中心で極値）を閉じた形 B2 = (A0/φ1² − 16a2)/4・
//!   B1 = A1 − A0·(φ0/φ1)² で書き、極の対から桁落ちしない形で求める（低い周波数・高い Q で 4.3 の R1 − R2·φ1 が
//!   桁落ちする）。B1 < 0（中心の極値を作れない）なら B1 = 0 として中心の利得だけを合わせる。
//! - ノッチ（論文にない）：同じ極と、ちょうど中心にある零点（b ∝ [1, −2cos ω0, 1]）。直流の利得を 1 にそろえる。
//! - シェルフ（論文にない）：アナログの原型は RBJ と同じ 2 次のシェルフ（A = 10^{dB/40}、肩の Q）。極はインパルス不変
//!   （極の周波数は ω0·√A か ω0/√A。`SHELF_POLE_MAX` を超えれば止めて、極の Q を (上限/原型の極)² 倍にする。止めないと
//!   インパルス不変の写しで折り返し、止めるだけでは鋭い共振が残って帯の中で 10 dB 以上持ち上がる）、分子は直流の値を
//!   合わせたうえで、決まった 48 点の対数の格子（10 Hz〜0.95 × ナイキスト）の相対誤差の最小二乗（未知数は φ1・φ1² の
//!   係数で、2 列の QR（修正 Gram–Schmidt）で解く。φ1・φ2 のままの正規方程式は、レートが高く周波数が低いと 2 列がほぼ
//!   平行になって崩れる）。根が負になる設定は「直流・ナイキスト・中心の 3 点で合わせる」に落とし、それでも負なら 0 で
//!   止める。
//! - チルト：1 次の原型 √g·(s + ω0/√g)/(s + ω0·√g)（中心で 0 dB、低域 −T/2・高域 +T/2）。極はインパルス不変、
//!   零点は直流とナイキストの振幅で合わせる。
//! - 1 次のローパス・ハイパス（カットの奇数次）：極 e^{−ω0}、直流とナイキストの振幅で合わせる。
//! - 振幅の式（式 25）：|H|² = (B0φ0 + B1φ1 + B2φ2)/(A0φ0 + A1φ1 + A2φ2)。φ0 = 1 − sin²(ω/2)、φ1 = sin²(ω/2)、
//!   φ2 = 4φ0φ1、A0 = (1+a1+a2)²、A1 = (1−a1+a2)²、A2 = −4a2、B0 = (b0+b1+b2)²、B1 = (b0−b1+b2)²、B2 = −4b0b2。
//!   B から b へは最小位相の解（式 29）。分母を A の形で足すと、極が 1 に近い（低い周波数）とき A1φ1 と A2φ2 が打ち
//!   消し合うので、シェルフ・バンドパスは極の対の積の形（`Pair`）で求める。
//! - Butterworth の N 次の 2 次の段の Q：Q_k = 1/(2cos θ_k)、θ_k = (2k − 1)π/(2N)（偶数次）・kπ/N（奇数次。実の極が
//!   1 つ残る）。

use crate::math;

/// 段の係数（f64）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Section {
    /// 何もしない段。
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// 周波数 `w`（ラジアン / 標本）での振幅の 2 乗（H(e^{iω}) を複素数で。式 25 の形は、極が 1 に近いと分母が打ち
    /// 消し合って崩れるので、曲線には使わない）。
    pub fn power(&self, w: f64) -> f64 {
        let (c1, s1) = (math::cos(w), math::sin(w));
        let (c2, s2) = (math::cos(2.0 * w), math::sin(2.0 * w));
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = self.b1 * s1 + self.b2 * s2;
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = self.a1 * s1 + self.a2 * s2;
        let den = dr * dr + di * di;
        if den > 0.0 {
            (nr * nr + ni * ni) / den
        } else {
            0.0
        }
    }

    /// 周波数 `w` での利得（dB。下は `MIN_DB` で止める：ノッチの中心などで −∞ にしない）。
    pub fn db(&self, w: f64) -> f64 {
        math::max(10.0 * math::log10(self.power(w)), MIN_DB)
    }
}

/// 曲線の利得の下限（dB）。
pub const MIN_DB: f64 = -300.0;

fn sq(x: f64) -> f64 {
    x * x
}

/// 0 未満を 0 にした平方根（式の中の根が丸めで負になったとき）。
fn root(x: f64) -> f64 {
    math::max(x, 0.0).sqrt()
}

/// cosh（`math::exp` から）。
fn cosh(x: f64) -> f64 {
    0.5 * (math::exp(x) + math::exp(-x))
}

/// φ0・φ1・φ2（式 26）。
pub fn phis(w: f64) -> (f64, f64, f64) {
    let s = sq(math::sin(0.5 * w));
    let p0 = 1.0 - s;
    (p0, s, 4.0 * p0 * s)
}

/// A0・A1・A2（式 27）。
#[cfg(test)]
fn pole_terms(a1: f64, a2: f64) -> (f64, f64, f64) {
    (sq(1.0 + a1 + a2), sq(1.0 - a1 + a2), -4.0 * a2)
}

/// 極（インパルス不変。式 12）：中心 `w0`（ラジアン / 標本）・共振 `q_res` の 2 次の極の (a1, a2)。
pub fn poles(w0: f64, q_res: f64) -> (f64, f64) {
    let q = 0.5 / q_res;
    let e = math::exp(-q * w0);
    let a1 = if q <= 1.0 {
        -2.0 * e * math::cos((1.0 - q * q).sqrt() * w0)
    } else {
        -2.0 * e * cosh((q * q - 1.0).sqrt() * w0)
    };
    (a1, math::exp(-2.0 * q * w0))
}

/// 極の対（`poles` と同じ a1・a2）と、桁落ちしない形の量（1 − r は expm1 で）。
struct Pair {
    a1: f64,
    a2: f64,
    roots: Roots,
}

enum Roots {
    /// 複素の対 r·e^{±iθ}（q ≤ 1）。`omr` = 1 − r、`gap` = (ω0 − θ)/2（θ = ω0√(1−q²)）。
    Complex {
        r: f64,
        omr: f64,
        theta: f64,
        gap: f64,
    },
    /// 実の対 r_i = e^{−ω0·e_i}（q > 1。e = q ∓ √(q²−1)）。`omr` = 1 − r_i。
    Real { r: [f64; 2], omr: [f64; 2] },
}

impl Pair {
    /// 中心 `w0`・共振 `q_res` の対。
    fn new(w0: f64, q_res: f64) -> Self {
        let (a1, a2) = poles(w0, q_res);
        let q = 0.5 / q_res;
        let roots = if q <= 1.0 {
            let c = (1.0 - q * q).sqrt();
            Roots::Complex {
                r: math::exp(-q * w0),
                omr: -math::expm1(-q * w0),
                theta: c * w0,
                gap: 0.5 * w0 * q * q / (1.0 + c),
            }
        } else {
            let s = (q * q - 1.0).sqrt();
            let e = [1.0 / (q + s), q + s];
            Roots::Real {
                r: [math::exp(-w0 * e[0]), math::exp(-w0 * e[1])],
                omr: [-math::expm1(-w0 * e[0]), -math::expm1(-w0 * e[1])],
            }
        };
        Self { a1, a2, roots }
    }

    /// 1 + a1 + a2（= |1 − p|² の積）。
    fn at_dc(&self) -> f64 {
        match self.roots {
            Roots::Complex { r, omr, theta, .. } => sq(omr) + 4.0 * r * sq(math::sin(0.5 * theta)),
            Roots::Real { omr, .. } => omr[0] * omr[1],
        }
    }

    /// 1 − a1 + a2（= |1 + p|² の積）。
    fn at_nyquist(&self) -> f64 {
        match self.roots {
            Roots::Complex { r, omr, theta, .. } => sq(omr) + 4.0 * r * sq(math::cos(0.5 * theta)),
            Roots::Real { r, .. } => (1.0 + r[0]) * (1.0 + r[1]),
        }
    }

    /// 分母の振幅の 2 乗 |A(e^{iω})|²（式 25 の分母）。
    fn den(&self, w: f64) -> f64 {
        match self.roots {
            Roots::Complex { r, omr, theta, .. } => {
                let o = sq(omr);
                (o + 4.0 * r * sq(math::sin(0.5 * (theta - w))))
                    * (o + 4.0 * r * sq(math::sin(0.5 * (theta + w))))
            }
            Roots::Real { r, omr } => {
                let p1 = sq(math::sin(0.5 * w));
                (sq(omr[0]) + 4.0 * r[0] * p1) * (sq(omr[1]) + 4.0 * r[1] * p1)
            }
        }
    }
}

/// B0・B1・B2 から最小位相の b0・b1・b2（式 29）。
fn numerator(bb0: f64, bb1: f64, bb2: f64) -> (f64, f64, f64) {
    let (r0, r1) = (root(bb0), root(bb1));
    let w = 0.5 * (r0 + r1);
    from_e(r0, r1, w * w + bb2)
}

/// √B0・√B1 と E = W² + B2 = (b0 − b2)²（W = (√B0 + √B1)/2 = b0 + b2）から最小位相の b0・b1・b2。E < 0（作れない B2）は
/// 0 に止める（零点が単位円の上に来て、直流 √B0・ナイキスト √B1 は保つ）。
fn from_e(r0: f64, r1: f64, e: f64) -> (f64, f64, f64) {
    let w = 0.5 * (r0 + r1);
    let se = root(e);
    (0.5 * (w + se), 0.5 * (r0 - r1), 0.5 * (w - se))
}

/// ローパス（4.1）。分母の量は極の対の積の形（`Pair`）で。
pub fn lowpass(w0: f64, q: f64) -> Section {
    let pair = Pair::new(w0, q);
    let (a1, a2) = (pair.a1, pair.a2);
    let (p0, p1, _) = phis(w0);
    let s0 = pair.at_dc();
    let r1 = pair.den(w0) * q * q;
    let bb0 = s0 * s0;
    let bb1 = (r1 - bb0 * p0) / p1;
    let b0 = 0.5 * (s0 + root(bb1));
    Section {
        b0,
        b1: s0 - b0,
        b2: 0.0,
        a1,
        a2,
    }
}

/// ハイパス（4.2）。分母の量は極の対の積の形（`Pair`）で。
pub fn highpass(w0: f64, q: f64) -> Section {
    let pair = Pair::new(w0, q);
    let (a1, a2) = (pair.a1, pair.a2);
    let (_, p1, _) = phis(w0);
    let b0 = root(pair.den(w0)) * q / (4.0 * p1);
    Section {
        b0,
        b1: -2.0 * b0,
        b2: b0,
        a1,
        a2,
    }
}

/// バンドパス（4.3 の条件：中心の利得 1・中心で極値）。B2 = (A0/φ1² − 16a2)/4 = (X − 4√a2)(X + 4√a2)/4
/// （X = (1+a1+a2)/φ1）、B1 = A1 − A0·(φ0/φ1)² = D·((1−a1+a2) + (1+a1+a2)·φ0/φ1)（D = (1−a1+a2) − (1+a1+a2)·φ0/φ1）。
/// 複素の対では X − 4r = ((1−r)² − 4r·S)/φ1・D = (4r·S − (1−r)²·cos ω0)/φ1（S = sin((ω0−θ)/2)·sin((ω0+θ)/2)）で、
/// 打ち消し合う大きな項を作らない。B1 < 0 なら B1 = 0・B2 = |A(e^{iω0})|²/φ2（中心の利得 1 だけ）。
pub fn bandpass(w0: f64, q: f64) -> Section {
    let pair = Pair::new(w0, q);
    let (a1, a2) = (pair.a1, pair.a2);
    let (p0, p1, p2) = phis(w0);
    let (dc, ny) = (pair.at_dc(), pair.at_nyquist());
    let x = dc / p1;
    let (xm, xp, d) = match pair.roots {
        Roots::Complex { r, omr, theta, gap } => {
            let s = math::sin(gap) * math::sin(0.5 * (w0 + theta));
            let o = sq(omr);
            (
                (o - 4.0 * r * s) / p1,
                x + 4.0 * r,
                (4.0 * r * s - o * math::cos(w0)) / p1,
            )
        }
        Roots::Real { .. } => {
            let rg = math::exp(-0.5 / q * w0);
            (x - 4.0 * rg, x + 4.0 * rg, ny - dc * p0 / p1)
        }
    };
    let (bb1, bb2) = {
        let bb1 = d * (ny + dc * p0 / p1);
        if bb1 >= 0.0 {
            (bb1, 0.25 * xm * xp)
        } else {
            (0.0, pair.den(w0) / p2)
        }
    };
    let b1 = -0.5 * root(bb1);
    let b0 = 0.5 * (root(bb2 + b1 * b1) - b1);
    Section {
        b0,
        b1,
        b2: -b0 - b1,
        a1,
        a2,
    }
}

/// ピーキング（ベル。4.4 の条件：直流 1・中心で G・中心で極値）：中心の利得 `db`。4.4 の R1・R2 を閉じた形に解いて
/// B0 = A0、B1 = G²·A1 + (1 − G²)·A0·(φ0/φ1)²、B2 = (A0·(G² − 1)/φ1² − 16a2·G²)/4（A0・A1 は極の対の積の形で）。
///
/// 低い周波数の狭いベルは零点が単位円に近く、式 29 の W² + B2 = (b0 − b2)² が大きな項の小さな差になる（192 kHz・10 Hz・
/// Q 36・+30 dB で約 3e-9）。複素の極の対では、それを打ち消しのない形で求める（d0 = 1+a1+a2、n0 = 1−a1+a2、X = d0/φ1、
/// m = n0 + X·φ0、D・X − 4r はバンドパスと同じ）：
/// 4E = G²·(2·d0·D·m/(√B1 + X·φ0) + 2(1−r)²·m + 8r·sin²(θ/2)·(X − 4r))。
pub fn peaking(w0: f64, q: f64, db: f64) -> Section {
    let g = math::db_to_gain(db);
    let pair = Pair::new(w0, q * g.sqrt());
    let (a1, a2) = (pair.a1, pair.a2);
    let (p0, p1, _) = phis(w0);
    let g2 = g * g;
    let (d0, n0) = (pair.at_dc(), pair.at_nyquist());
    let x = d0 / p1;
    let r1 = root(g2 * sq(n0) + (1.0 - g2) * sq(x * p0));
    let e = match pair.roots {
        Roots::Complex { r, omr, theta, gap } => {
            let s = math::sin(gap) * math::sin(0.5 * (w0 + theta));
            let o = sq(omr);
            let xm = (o - 4.0 * r * s) / p1;
            let d = (4.0 * r * s - o * math::cos(w0)) / p1;
            let m = n0 + x * p0;
            0.25 * g2
                * (2.0 * d0 * d * m / (r1 + x * p0)
                    + 2.0 * o * m
                    + 8.0 * r * sq(math::sin(0.5 * theta)) * xm)
        }
        Roots::Real { .. } => {
            let bb2 = 0.25 * (sq(x) * (g2 - 1.0) - 16.0 * a2 * g2);
            sq(0.5 * (d0 + r1)) + bb2
        }
    };
    let (b0, b1, b2) = from_e(d0, r1, e);
    Section { b0, b1, b2, a1, a2 }
}

/// ノッチ：極はインパルス不変、零点はちょうど中心（b ∝ [1, −2cos ω0, 1]）、直流の利得 1。
pub fn notch(w0: f64, q: f64) -> Section {
    let (a1, a2) = poles(w0, q);
    let c = math::cos(w0);
    let dc = 2.0 - 2.0 * c;
    let k = if dc > 0.0 { (1.0 + a1 + a2) / dc } else { 0.0 };
    Section {
        b0: k,
        b1: -2.0 * c * k,
        b2: k,
        a1,
        a2,
    }
}

/// シェルフの最小二乗の格子の点の数（凍結）。
pub const SHELF_GRID: usize = 48;
/// シェルフの最小二乗の格子の下端（Hz。凍結）。
pub const SHELF_GRID_LOW_HZ: f64 = 10.0;
/// シェルフの最小二乗の格子の上端（ナイキストに対する比。凍結）。
pub const SHELF_GRID_HIGH: f64 = 0.95;

/// アナログのシェルフの原型の振幅の 2 乗（ω0 で正規化した x = ω/ω0）。
fn shelf_power(x: f64, a: f64, q: f64, high: bool) -> f64 {
    let sa = a.sqrt();
    let mid = sq(sa * x / q);
    let (num, den) = if high {
        (sq(1.0 - a * x * x) + mid, sq(a - x * x) + mid)
    } else {
        (sq(a - x * x) + mid, sq(1.0 - a * x * x) + mid)
    };
    a * a * num / den
}

/// B から b を作れるか（√ の中が負にならない）。
fn realizable(bb0: f64, bb1: f64, bb2: f64) -> bool {
    if bb0 < 0.0 || bb1 < 0.0 {
        return false;
    }
    let w = 0.5 * (bb0.sqrt() + bb1.sqrt());
    w * w + bb2 >= 0.0
}

/// シェルフの極の周波数の上限（ラジアン / 標本。凍結）。超えたら止めて、極の Q を (上限/原型の極)² 倍にする。
pub const SHELF_POLE_MAX: f64 = SHELF_POLE_MAX_RATIO * std::f64::consts::PI;
/// 同じ（π に対する比。凍結）。
pub const SHELF_POLE_MAX_RATIO: f64 = 0.9;

/// 2 列の最小二乗（行は (u, v, y)。y ≈ c1·u + c2·v の (c1, c2)）を、修正 Gram–Schmidt の QR で解く。列が 0 なら None。
fn least_squares2(rows: &[(f64, f64, f64)]) -> Option<(f64, f64)> {
    let mut nu = 0.0;
    for r in rows {
        nu += r.0 * r.0;
    }
    let nu = nu.sqrt();
    if nu.is_nan() || nu <= 0.0 {
        return None;
    }
    let mut r12 = 0.0;
    let mut z1 = 0.0;
    for r in rows {
        r12 += r.0 / nu * r.1;
        z1 += r.0 / nu * r.2;
    }
    let mut nv = 0.0;
    for r in rows {
        nv += sq(r.1 - r12 * (r.0 / nu));
    }
    let nv = nv.sqrt();
    if nv.is_nan() || nv <= 0.0 {
        return None;
    }
    let mut z2 = 0.0;
    for r in rows {
        z2 += (r.1 - r12 * (r.0 / nu)) / nv * r.2;
    }
    let c2 = z2 / nv;
    Some(((z1 - r12 * c2) / nu, c2))
}

/// ローシェルフ（`high` が偽）・ハイシェルフ（真）：中心 `w0`・肩の `q`・利得 `db`・サンプルレート `rate`（格子の下端に使う）。
pub fn shelf(w0: f64, q: f64, db: f64, rate: f64, high: bool) -> Section {
    let a = math::pow(10.0, db / 40.0);
    let sa = a.sqrt();
    // 原型の極の周波数（ハイシェルフの持ち上げ・ローシェルフの下げは ω0 より上）。上限を超えたら止め、極の Q を
    // (上限 / 原型の極)² 倍にする（ナイキストの外の極は帯の中では広い傾きとして効くので、鋭い共振を作らない）
    let raw = if high { w0 * sa } else { w0 / sa };
    let wp = math::min(raw, SHELF_POLE_MAX);
    let pair = Pair::new(wp, q * sq(wp / raw));
    let (a1, a2) = (pair.a1, pair.a2);
    let target = |w: f64| shelf_power(w / w0, a, q, high);
    let bb0 = sq(pair.at_dc()) * target(0.0);
    // 直流を合わせたうえで、分子の B1φ1 + B2φ2 = c1·φ1 + c2·φ1²（c1 = B1 + 4B2、c2 = −4B2）を相対誤差の最小二乗で
    // （決まった格子・重み 1/t）
    let lo = 2.0 * std::f64::consts::PI * SHELF_GRID_LOW_HZ / rate;
    let hi = std::f64::consts::PI * SHELF_GRID_HIGH;
    let mut rows = [(0.0, 0.0, 0.0); SHELF_GRID];
    if lo < hi {
        for (k, row) in rows.iter_mut().enumerate() {
            let w = lo * math::pow(hi / lo, k as f64 / (SHELF_GRID - 1) as f64);
            let (p0, p1, _) = phis(w);
            let t = target(w) * pair.den(w);
            if t.is_nan() || t <= 0.0 {
                continue;
            }
            *row = (p1 / t, p1 * p1 / t, (t - bb0 * p0) / t);
        }
    }
    let (mut bb1, mut bb2) = match least_squares2(&rows) {
        Some((c1, c2)) => (c1 + c2, -0.25 * c2),
        None => (-1.0, 0.0),
    };
    if !realizable(bb0, bb1, bb2) {
        // 直流・ナイキスト・中心の 3 点で合わせる
        let (p0, p1, p2) = phis(w0);
        bb1 = sq(pair.at_nyquist()) * target(std::f64::consts::PI);
        let r = pair.den(w0) * target(w0);
        bb2 = if p2 > 0.0 {
            (r - bb0 * p0 - bb1 * p1) / p2
        } else {
            0.0
        };
    }
    let (b0, b1, b2) = numerator(bb0, bb1, bb2);
    Section { b0, b1, b2, a1, a2 }
}

/// 1 次の段の極（インパルス不変）：e^{−ω}。
fn first_pole(w: f64) -> f64 {
    math::exp(-w)
}

/// 1 次の段を、直流の振幅 `dc`・ナイキストの振幅 `ny` で合わせる（極 p）。
fn first_order(p: f64, dc: f64, ny: f64) -> Section {
    let a1 = -p;
    let r0 = (1.0 + a1) * dc;
    let r1 = (1.0 - a1) * ny;
    Section {
        b0: 0.5 * (r0 + r1),
        b1: 0.5 * (r0 - r1),
        b2: 0.0,
        a1,
        a2: 0.0,
    }
}

/// 1 次のローパス（原型 1/(1 + s/ω0)）。
pub fn first_lowpass(w0: f64) -> Section {
    let x = std::f64::consts::PI / w0;
    first_order(first_pole(w0), 1.0, 1.0 / (1.0 + x * x).sqrt())
}

/// 1 次のハイパス（原型 (s/ω0)/(1 + s/ω0)）。
pub fn first_highpass(w0: f64) -> Section {
    let x = std::f64::consts::PI / w0;
    first_order(first_pole(w0), 0.0, x / (1.0 + x * x).sqrt())
}

/// チルト（1 次。中心 `w0` で 0 dB、低域 −db/2・高域 +db/2）。
pub fn tilt(w0: f64, db: f64) -> Section {
    let g = math::db_to_gain(db);
    let sg = g.sqrt();
    let pi = std::f64::consts::PI;
    // 原型 √g·(s + ω0/√g)/(s + ω0·√g) のナイキストの振幅
    let ny = sg * ((pi * pi + w0 * w0 / g) / (pi * pi + w0 * w0 * g)).sqrt();
    first_order(first_pole(w0 * sg), 1.0 / sg, ny)
}

/// Butterworth の N 次の 2 次の段の Q（k = 1..⌊N/2⌋）：θ_k = (2k − 1 + N mod 2)π/(2N)（奇数次は kπ/N）。
pub fn butterworth_q(order: u32, k: u32) -> f64 {
    let n = f64::from(order);
    let odd = f64::from(order % 2);
    0.5 / math::cos((2.0 * f64::from(k) - 1.0 + odd) * std::f64::consts::PI / (2.0 * n))
}

/// カット（`high` が偽はローカット＝ハイパス、真はハイカット＝ローパス）：傾き `slope`（6 の倍数の dB/oct）の Butterworth を、
/// 2 次の段（k の順）と、奇数次なら最後に 1 次の段で重ねる。
pub fn cut(w0: f64, slope: u32, high: bool) -> Vec<Section> {
    let order = (slope / 6).max(1);
    let mut out = Vec::with_capacity(order as usize / 2 + 1);
    for k in 1..=order / 2 {
        let q = butterworth_q(order, k);
        out.push(if high {
            lowpass(w0, q)
        } else {
            highpass(w0, q)
        });
    }
    if order % 2 == 1 {
        out.push(if high {
            first_lowpass(w0)
        } else {
            first_highpass(w0)
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    /// 式 25 の形の振幅の 2 乗（設計が使う A・B の関係を確かめる）。
    fn power25(s: &Section, w: f64) -> f64 {
        let (p0, p1, p2) = phis(w);
        let (a0, a1, a2) = pole_terms(s.a1, s.a2);
        let bb0 = sq(s.b0 + s.b1 + s.b2);
        let bb1 = sq(s.b0 - s.b1 + s.b2);
        let bb2 = -4.0 * s.b0 * s.b2;
        (bb0 * p0 + bb1 * p1 + bb2 * p2) / (a0 * p0 + a1 * p1 + a2 * p2)
    }

    fn w(f: f64, fs: f64) -> f64 {
        2.0 * PI * f / fs
    }

    /// 型。
    #[derive(Debug, Clone, Copy)]
    enum Kind {
        Bell(f64),
        LowShelf(f64),
        HighShelf(f64),
        LowPass,
        HighPass,
        BandPass,
        Notch,
        Tilt(f64),
        /// カット（傾き、ハイカットか）
        Cut(u32, bool),
    }

    /// 設計した段。
    fn design(k: Kind, w0: f64, q: f64, fs: f64) -> Vec<Section> {
        match k {
            Kind::Bell(g) => vec![peaking(w0, q, g)],
            Kind::LowShelf(g) => vec![shelf(w0, q, g, fs, false)],
            Kind::HighShelf(g) => vec![shelf(w0, q, g, fs, true)],
            Kind::LowPass => vec![lowpass(w0, q)],
            Kind::HighPass => vec![highpass(w0, q)],
            Kind::BandPass => vec![bandpass(w0, q)],
            Kind::Notch => vec![notch(w0, q)],
            Kind::Tilt(g) => vec![tilt(w0, g)],
            Kind::Cut(slope, high) => cut(w0, slope, high),
        }
    }

    /// アナログの原型の振幅の 2 乗（x = ω/ω0）。
    fn analog(k: Kind, x: f64, q: f64) -> f64 {
        let d = sq(1.0 - x * x) + sq(x / q);
        match k {
            Kind::Bell(g) => {
                let gg = math::db_to_gain(g);
                (sq(1.0 - x * x) + sq(x * gg.sqrt() / q))
                    / (sq(1.0 - x * x) + sq(x / (q * gg.sqrt())))
            }
            Kind::LowShelf(g) => shelf_power(x, math::pow(10.0, g / 40.0), q, false),
            Kind::HighShelf(g) => shelf_power(x, math::pow(10.0, g / 40.0), q, true),
            Kind::LowPass => 1.0 / d,
            Kind::HighPass => sq(x * x) / d,
            Kind::BandPass => sq(x / q) / d,
            Kind::Notch => sq(1.0 - x * x) / d,
            Kind::Tilt(g) => {
                let gg = math::db_to_gain(g);
                gg * (x * x + 1.0 / gg) / (x * x + gg)
            }
            Kind::Cut(slope, high) => {
                let n = f64::from((slope / 6).max(1));
                let xn = math::pow(x, 2.0 * n);
                if high {
                    1.0 / (1.0 + xn)
                } else {
                    xn / (1.0 + xn)
                }
            }
        }
    }

    fn db_of(s: &[Section], ww: f64) -> f64 {
        s.iter().map(|x| x.db(ww)).sum()
    }

    /// 式 25 の振幅は、複素数で求めた振幅と合う（どの型・端の設定でも。48 kHz）。
    #[test]
    fn power_matches_the_complex_response() {
        let fs = 48_000.0;
        let mut sections = Vec::new();
        for f in [10.0, 100.0, 1_000.0, 10_000.0, 23_000.0] {
            for q in [0.1, 0.707, 4.0, 40.0] {
                let w0 = w(f, fs);
                sections.push(lowpass(w0, q));
                sections.push(highpass(w0, q));
                sections.push(bandpass(w0, q));
                sections.push(notch(w0, q));
                for g in [-30.0, -6.0, 6.0, 30.0] {
                    sections.push(peaking(w0, q, g));
                    if q <= 4.0 {
                        sections.push(shelf(w0, q, g, fs, false));
                        sections.push(shelf(w0, q, g, fs, true));
                    }
                    sections.push(tilt(w0, g));
                }
            }
            sections.push(first_lowpass(w(f, fs)));
            sections.push(first_highpass(w(f, fs)));
        }
        for s in &sections {
            for k in 1..200 {
                let ww = PI * k as f64 / 200.0;
                let (a, b) = (power25(s, ww), s.power(ww));
                assert!(
                    (a - b).abs() <= 1e-9 * math::max(b, 1e-12) + 1e-15,
                    "{s:?} ω {ww}: {a} と {b}"
                );
            }
        }
    }

    /// 曲線の利得は −∞ にならない（ノッチの中心）。
    #[test]
    fn db_has_a_floor() {
        let w0 = w(20.0, 48_000.0);
        let n = notch(w0, 4.0);
        assert!(n.power(w0) < 1e-20);
        assert!(n.db(w0) >= MIN_DB && n.db(w0).is_finite());
    }

    /// 極は単位円の中（安定）で、零点も単位円の中か上（最小位相）。シェルフは極の周波数が上限で止まる向き
    /// （ハイシェルフの持ち上げ・ローシェルフの下げ）も。
    #[test]
    fn sections_are_stable_and_minimum_phase() {
        for fs in [44_100.0, 192_000.0, 768_000.0] {
            for f in [10.0, 50.0, 1_000.0, 15_000.0, 0.49 * fs] {
                for q in [0.1, 0.5, 2.0, 40.0] {
                    let w0 = w(f, fs);
                    let sq4 = math::min(q, 4.0);
                    for s in [
                        lowpass(w0, q),
                        highpass(w0, q),
                        bandpass(w0, q),
                        notch(w0, q),
                        peaking(w0, q, -24.0),
                        peaking(w0, q, 24.0),
                        shelf(w0, sq4, 12.0, fs, false),
                        shelf(w0, sq4, -12.0, fs, true),
                        shelf(w0, sq4, -30.0, fs, false),
                        shelf(w0, sq4, 30.0, fs, true),
                        tilt(w0, 9.0),
                    ] {
                        assert!(
                            s.a2.abs() < 1.0 && s.a1.abs() < 1.0 + s.a2 + 1e-12,
                            "{fs} {f} {q} {s:?}"
                        );
                        assert!(s.b0 >= s.b2.abs() - 1e-12, "{fs} {f} {q} {s:?}");
                        assert!(
                            s.b0 + s.b2 >= s.b1.abs() - 1e-9 * math::max(s.b0.abs(), 1.0),
                            "{fs} {f} {q} {s:?}"
                        );
                        for c in [s.b0, s.b1, s.b2, s.a1, s.a2] {
                            assert!(c.is_finite(), "{fs} {f} {q} {s:?}");
                        }
                    }
                }
            }
        }
    }

    /// 合わせた点（44.1〜768 kHz、10 Hz から）：ベルは直流 0 dB・中心で利得、ローパスは直流 0 dB・中心で Q、ハイパスは
    /// 中心で Q、バンドパスは中心で 0 dB、ノッチは中心で深い谷・直流 0 dB、シェルフ・チルトは直流の値、カットは切れ目で
    /// −3.01 dB（Butterworth）。
    #[test]
    fn matched_points_hold() {
        for fs in [
            44_100.0, 48_000.0, 96_000.0, 192_000.0, 384_000.0, 768_000.0,
        ] {
            for f in [10.0, 11.0, 30.0, 1_000.0, 8_000.0] {
                let w0 = w(f, fs);
                for q in [0.1, 0.5, 1.0, 3.0, 36.4, 40.0] {
                    let at = format!("{fs} Hz・{f} Hz・Q {q}");
                    // 0 dB のベルは何もしない（零点が極に重なる。E = (1 − a2)² の閉じた形の確かめ）
                    let id = peaking(w0, q, 0.0);
                    for (got, want) in [(id.b0, 1.0), (id.b1, id.a1), (id.b2, id.a2)] {
                        assert!((got - want).abs() < 1e-12, "{at}: {id:?}");
                    }
                    for g in [-30.0, -0.1, 0.1, 9.0, 30.0] {
                        let b = peaking(w0, q, g);
                        assert!(b.db(0.0).abs() < 1e-6, "{at}");
                        assert!((b.db(w0) - g).abs() < 1e-4, "{at}・{g} dB: {}", b.db(w0));
                    }
                    let lp = lowpass(w0, q);
                    assert!(lp.db(0.0).abs() < 1e-6, "{at}");
                    assert!((lp.power(w0) / (q * q) - 1.0).abs() < 1e-4, "{at}");
                    let hp = highpass(w0, q);
                    assert!((hp.power(w0) / (q * q) - 1.0).abs() < 1e-4, "{at}");
                    let bp = bandpass(w0, q);
                    assert!(bp.db(w0).abs() < 1e-3, "{at}: {}", bp.db(w0));
                    let n = notch(w0, q);
                    assert!(n.db(0.0).abs() < 1e-6 && n.db(w0) < -100.0, "{at}");
                    if q <= 4.0 {
                        let ls = shelf(w0, q, 12.0, fs, false);
                        assert!((ls.db(0.0) - 12.0).abs() < 1e-6, "{at}");
                        let hs = shelf(w0, q, -12.0, fs, true);
                        assert!(hs.db(0.0).abs() < 1e-6, "{at}");
                    }
                }
                let t = tilt(w0, 12.0);
                assert!((t.db(0.0) + 6.0).abs() < 1e-6, "{fs} {f}");
                for slope in [6, 12, 18, 24, 36, 48, 72, 96] {
                    // 奇数次の 1 次の段は直流とナイキストで合わせるので、切れ目はナイキストよりずっと下でだけ
                    if (slope / 6) % 2 == 1 && f > 0.025 * fs {
                        continue;
                    }
                    for high in [false, true] {
                        let got = db_of(&cut(w0, slope, high), w0);
                        assert!(
                            (got + 3.0103).abs() < 0.01,
                            "{fs} Hz・{f} Hz・{slope} dB/oct・{high}: {got}"
                        );
                    }
                }
            }
        }
    }

    /// ナイキストよりずっと下（中心が 0.025 × レート以下で、0.05 × レートまで）では、どの型もアナログの原型に合う
    /// （44.1〜768 kHz、10 Hz から。原型が −60 dB より深い所は見ない）。
    #[test]
    fn designs_follow_the_analog_prototypes_well_below_nyquist() {
        let kinds = [
            Kind::Bell(-30.0),
            Kind::Bell(-1.0),
            Kind::Bell(12.0),
            Kind::Bell(30.0),
            Kind::LowShelf(-30.0),
            Kind::LowShelf(-1.0),
            Kind::LowShelf(6.0),
            Kind::LowShelf(30.0),
            Kind::HighShelf(-30.0),
            Kind::HighShelf(-1.0),
            Kind::HighShelf(6.0),
            Kind::HighShelf(30.0),
            Kind::LowPass,
            Kind::HighPass,
            Kind::BandPass,
            Kind::Notch,
            Kind::Tilt(-12.0),
            Kind::Tilt(30.0),
            Kind::Cut(6, false),
            Kind::Cut(18, false),
            Kind::Cut(18, true),
            Kind::Cut(48, false),
            Kind::Cut(96, true),
        ];
        let mut worst = std::collections::BTreeMap::new();
        for fs in [
            44_100.0, 48_000.0, 96_000.0, 192_000.0, 384_000.0, 768_000.0,
        ] {
            for f0 in [10.0, 20.0, 50.0, 200.0, 1_000.0] {
                let w0 = w(f0, fs);
                for k in kinds {
                    let qs: &[f64] = match k {
                        Kind::Cut(..) | Kind::Tilt(_) => &[std::f64::consts::FRAC_1_SQRT_2],
                        Kind::LowShelf(_) | Kind::HighShelf(_) => {
                            &[0.1, std::f64::consts::FRAC_1_SQRT_2, 2.0, 4.0]
                        }
                        _ => &[0.1, std::f64::consts::FRAC_1_SQRT_2, 4.0, 40.0],
                    };
                    for &q in qs {
                        let s = design(k, w0, q, fs);
                        let (lo, hi) = (math::max(f0 / 10.0, 5.0), math::min(f0 * 10.0, 0.05 * fs));
                        for i in 0..=60 {
                            let f = lo * math::pow(hi / lo, f64::from(i) / 60.0);
                            let want = 10.0 * math::log10(analog(k, f / f0, q));
                            if want < -60.0 {
                                continue;
                            }
                            let got = db_of(&s, w(f, fs));
                            let e = (got - want).abs();
                            let key = format!("{k:?}");
                            let entry = worst.entry(key).or_insert((0.0, String::new()));
                            if e > entry.0 {
                                *entry = (
                                    e,
                                    format!(
                                        "{fs} Hz・{f0} Hz・Q {q}・{f:.1} Hz：{got:.3} と {want:.3}"
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
        let mut bad = Vec::new();
        for (k, (e, at)) in &worst {
            eprintln!("{k}: 最大 {e:.4} dB（{at}）");
            // 照らしの上限：±30 dB のシェルフ（Q 0.1）は 0.1 dB、+30 dB のチルト（1 次。直流とナイキストで合わせる）は 0.2 dB
            let bound = if k.contains("Shelf") {
                0.1
            } else if k.contains("Tilt") {
                0.2
            } else {
                0.05
            };
            if *e > bound {
                bad.push(k.clone());
            }
        }
        assert!(bad.is_empty(), "{bad:?}");
    }

    /// シェルフ（48 kHz、0.9 × ナイキストまで）：中心が 0.1 × レート以下なら原型と 1.1 dB 以内。それより上（原型の肩や極が
    /// ナイキストを超える）では合わせきれないが、原型より 3.5 dB を超えて持ち上げない（鋭い共振・折り返しを作らない）。
    /// 44.1〜96 kHz・中心 81 点・Q 7 つ・利得 10 個で測った最大：0.1 × レート以下 1.04 dB、持ち上げすぎ +3.45 dB（Q 4・
    /// −30 dB のローシェルフ・中心 0.23 × レート）、届かない分 −10.8 dB（+30 dB・Q 2 のハイシェルフ・中心 0.49 × レート）。
    #[test]
    fn shelves_near_nyquist_stay_sane() {
        let fs = 48_000.0;
        for f0 in [
            1_000.0, 1_881.0, 4_800.0, 9_600.0, 11_110.0, 15_000.0, 20_000.0, 23_520.0,
        ] {
            let w0 = w(f0, fs);
            for q in [0.1, std::f64::consts::FRAC_1_SQRT_2, 2.0, 4.0] {
                for g in [-30.0, -12.0, -6.0, 6.0, 12.0, 30.0] {
                    for high in [false, true] {
                        let s = shelf(w0, q, g, fs, high);
                        let a = math::pow(10.0, g / 40.0);
                        for i in 0..400 {
                            let f = 20.0 * math::pow(0.9 * 24_000.0 / 20.0, f64::from(i) / 399.0);
                            let want = 10.0 * math::log10(shelf_power(f / f0, a, q, high));
                            let d = s.db(w(f, fs)) - want;
                            let at =
                                format!("{f0} Hz・Q {q}・{g} dB・{high}・{f:.0} Hz：{d:+.2} dB");
                            if f0 <= 0.1 * fs {
                                assert!(d.abs() <= 1.1, "{at}");
                            }
                            assert!(d <= 3.5, "{at}");
                        }
                    }
                }
            }
        }
    }

    /// カットの段の数と、Butterworth の Q（N = 4 で 0.5412・1.3066、N = 3 で 1.0、N = 6 で 0.5176・std::f64::consts::FRAC_1_SQRT_2・1.9319）。
    #[test]
    fn cuts_stack_butterworth_sections() {
        assert!((butterworth_q(4, 1) - 0.541_196).abs() < 1e-6);
        assert!((butterworth_q(4, 2) - 1.306_563).abs() < 1e-6);
        assert!((butterworth_q(2, 1) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert!((butterworth_q(3, 1) - 1.0).abs() < 1e-12);
        assert!((butterworth_q(6, 1) - 0.517_638).abs() < 1e-6);
        assert!((butterworth_q(6, 2) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
        assert!((butterworth_q(6, 3) - 1.931_852).abs() < 1e-6);
        let w0 = w(1_000.0, 48_000.0);
        for (slope, n) in [
            (6, 1),
            (12, 1),
            (18, 2),
            (24, 2),
            (36, 3),
            (48, 4),
            (72, 6),
            (96, 8),
        ] {
            assert_eq!(cut(w0, slope, false).len(), n, "{slope}");
        }
        // 奇数次の最後は 1 次の段
        assert_eq!(cut(w0, 18, true)[1].a2, 0.0);
        // ローカットは直流で深く、ハイカットはナイキストの近くで深い
        let low = db_of(&cut(w0, 48, false), w(10.0, 48_000.0));
        assert!(low < -150.0, "{low}");
        let high = db_of(&cut(w0, 24, true), w(20_000.0, 48_000.0));
        assert!(high < -60.0, "{high}");
    }
}
