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
//! - ノッチ（論文にない）：同じ極と、ちょうど中心にある零点（b ∝ [1, −2cos ω0, 1]）。直流の利得を 1 にそろえる。
//! - シェルフ（論文にない）：アナログの原型は RBJ と同じ 2 次のシェルフ（A = 10^{dB/40}、肩の Q）。極はインパルス不変、
//!   分子は直流の値を合わせたうえで、決まった 48 点の対数の格子（10 Hz〜0.95 × ナイキスト）の相対誤差の最小二乗。根が
//!   負になる設定は「直流・ナイキスト・中心の 3 点で合わせる」に落とし、それでも負なら 0 で止める。
//! - チルト：1 次の原型 √g·(s + ω0/√g)/(s + ω0·√g)（中心で 0 dB、低域 −T/2・高域 +T/2）。極はインパルス不変、
//!   零点は直流とナイキストの振幅で合わせる。
//! - 1 次のローパス・ハイパス（カットの奇数次）：極 e^{−ω0}、直流とナイキストの振幅で合わせる。
//! - 振幅の式（式 25）：|H|² = (B0φ0 + B1φ1 + B2φ2)/(A0φ0 + A1φ1 + A2φ2)。φ0 = 1 − sin²(ω/2)、φ1 = sin²(ω/2)、
//!   φ2 = 4φ0φ1、A0 = (1+a1+a2)²、A1 = (1−a1+a2)²、A2 = −4a2、B0 = (b0+b1+b2)²、B1 = (b0−b1+b2)²、B2 = −4b0b2。
//!   B から b へは最小位相の解（式 29）。

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

    /// 周波数 `w`（ラジアン / 標本）での振幅の 2 乗（式 25）。
    pub fn power(&self, w: f64) -> f64 {
        let (p0, p1, p2) = phis(w);
        let (a0, a1, a2) = pole_terms(self.a1, self.a2);
        let bb0 = sq(self.b0 + self.b1 + self.b2);
        let bb1 = sq(self.b0 - self.b1 + self.b2);
        let bb2 = -4.0 * self.b0 * self.b2;
        let num = bb0 * p0 + bb1 * p1 + bb2 * p2;
        let den = a0 * p0 + a1 * p1 + a2 * p2;
        if den > 0.0 {
            math::max(num, 0.0) / den
        } else {
            0.0
        }
    }

    /// 周波数 `w` での利得（dB）。
    pub fn db(&self, w: f64) -> f64 {
        10.0 * math::log10(self.power(w))
    }
}

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

/// B0・B1・B2 から最小位相の b0・b1・b2（式 29）。
fn numerator(bb0: f64, bb1: f64, bb2: f64) -> (f64, f64, f64) {
    let (r0, r1) = (root(bb0), root(bb1));
    let w = 0.5 * (r0 + r1);
    let b0 = 0.5 * (w + root(w * w + bb2));
    let b1 = 0.5 * (r0 - r1);
    let b2 = if b0 > 0.0 { -bb2 / (4.0 * b0) } else { 0.0 };
    (b0, b1, b2)
}

/// ローパス（4.1）。
pub fn lowpass(w0: f64, q: f64) -> Section {
    let (a1, a2) = poles(w0, q);
    let (aa0, aa1, aa2) = pole_terms(a1, a2);
    let (p0, p1, p2) = phis(w0);
    let r1 = (aa0 * p0 + aa1 * p1 + aa2 * p2) * q * q;
    let bb0 = aa0;
    let bb1 = (r1 - bb0 * p0) / p1;
    let s0 = 1.0 + a1 + a2;
    let b0 = 0.5 * (s0 + root(bb1));
    Section {
        b0,
        b1: s0 - b0,
        b2: 0.0,
        a1,
        a2,
    }
}

/// ハイパス（4.2）。
pub fn highpass(w0: f64, q: f64) -> Section {
    let (a1, a2) = poles(w0, q);
    let (aa0, aa1, aa2) = pole_terms(a1, a2);
    let (p0, p1, p2) = phis(w0);
    let b0 = root(aa0 * p0 + aa1 * p1 + aa2 * p2) * q / (4.0 * p1);
    Section {
        b0,
        b1: -2.0 * b0,
        b2: b0,
        a1,
        a2,
    }
}

/// バンドパス（4.3。中心の利得 1）。
pub fn bandpass(w0: f64, q: f64) -> Section {
    let (a1, a2) = poles(w0, q);
    let (aa0, aa1, aa2) = pole_terms(a1, a2);
    let (p0, p1, p2) = phis(w0);
    let r1 = aa0 * p0 + aa1 * p1 + aa2 * p2;
    let r2 = -aa0 + aa1 + 4.0 * (p0 - p1) * aa2;
    let bb2 = (r1 - r2 * p1) / (4.0 * p1 * p1);
    let bb1 = r2 + 4.0 * (p1 - p0) * bb2;
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

/// ピーキング（ベル。4.4）：中心の利得 `db`。
pub fn peaking(w0: f64, q: f64, db: f64) -> Section {
    let g = math::db_to_gain(db);
    let (a1, a2) = poles(w0, q * g.sqrt());
    let (aa0, aa1, aa2) = pole_terms(a1, a2);
    let (p0, p1, p2) = phis(w0);
    let g2 = g * g;
    let r1 = (aa0 * p0 + aa1 * p1 + aa2 * p2) * g2;
    let r2 = (-aa0 + aa1 + 4.0 * (p0 - p1) * aa2) * g2;
    let bb0 = aa0;
    let bb2 = (r1 - r2 * p1 - bb0) / (4.0 * p1 * p1);
    let bb1 = r2 + bb0 + 4.0 * (p1 - p0) * bb2;
    let (b0, b1, b2) = numerator(bb0, bb1, bb2);
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

/// ローシェルフ（`high` が偽）・ハイシェルフ（真）：中心 `w0`・肩の `q`・利得 `db`・サンプルレート `rate`（格子の下端に使う）。
pub fn shelf(w0: f64, q: f64, db: f64, rate: f64, high: bool) -> Section {
    let a = math::pow(10.0, db / 40.0);
    let sa = a.sqrt();
    let wp = if high { w0 * sa } else { w0 / sa };
    let (a1, a2) = poles(wp, q);
    let (aa0, aa1, aa2) = pole_terms(a1, a2);
    let target = |w: f64| shelf_power(w / w0, a, q, high);
    let bb0 = aa0 * target(0.0);
    // 直流を合わせたうえで、B1・B2 を相対誤差の最小二乗で（決まった格子・重み 1/t²）
    let lo = 2.0 * std::f64::consts::PI * SHELF_GRID_LOW_HZ / rate;
    let hi = std::f64::consts::PI * SHELF_GRID_HIGH;
    let (mut s11, mut s12, mut s22, mut t1, mut t2) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for k in 0..SHELF_GRID {
        let w = lo * math::pow(hi / lo, k as f64 / (SHELF_GRID - 1) as f64);
        let (p0, p1, p2) = phis(w);
        let t = target(w) * (aa0 * p0 + aa1 * p1 + aa2 * p2);
        if t.is_nan() || t <= 0.0 {
            continue;
        }
        let r = t - bb0 * p0;
        let wt = 1.0 / (t * t);
        s11 += wt * p1 * p1;
        s12 += wt * p1 * p2;
        s22 += wt * p2 * p2;
        t1 += wt * p1 * r;
        t2 += wt * p2 * r;
    }
    let det = s11 * s22 - s12 * s12;
    let (mut bb1, mut bb2) = if det != 0.0 && lo < hi {
        ((t1 * s22 - t2 * s12) / det, (s11 * t2 - s12 * t1) / det)
    } else {
        (-1.0, 0.0)
    };
    if !realizable(bb0, bb1, bb2) {
        // 直流・ナイキスト・中心の 3 点で合わせる
        let (p0, p1, p2) = phis(w0);
        bb1 = aa1 * target(std::f64::consts::PI);
        let r = (aa0 * p0 + aa1 * p1 + aa2 * p2) * target(w0);
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

/// Butterworth の N 次の 2 次の段の Q（k = 1..⌊N/2⌋）。
pub fn butterworth_q(order: u32, k: u32) -> f64 {
    let n = f64::from(order);
    0.5 / math::cos((2.0 * f64::from(k) - 1.0) * std::f64::consts::PI / (2.0 * n))
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

    /// 素朴な参照：複素数で H(e^{−iω}) を求めた振幅の 2 乗。
    fn direct_power(s: &Section, w: f64) -> f64 {
        let (c1, s1) = (math::cos(w), -math::sin(w));
        let (c2, s2) = (math::cos(2.0 * w), -math::sin(2.0 * w));
        let nr = s.b0 + s.b1 * c1 + s.b2 * c2;
        let ni = s.b1 * s1 + s.b2 * s2;
        let dr = 1.0 + s.a1 * c1 + s.a2 * c2;
        let di = s.a1 * s1 + s.a2 * s2;
        (nr * nr + ni * ni) / (dr * dr + di * di)
    }

    fn w(f: f64, fs: f64) -> f64 {
        2.0 * PI * f / fs
    }

    /// 式 25 の振幅（`power`）は、複素数で求めた振幅と合う（どの型・端の設定でも）。
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
                let (a, b) = (s.power(ww), direct_power(s, ww));
                assert!(
                    (a - b).abs() <= 1e-9 * math::max(b, 1e-12) + 1e-15,
                    "{s:?} ω {ww}: {a} と {b}"
                );
            }
        }
    }

    /// 極は単位円の中（安定）で、零点も単位円の中か上（最小位相）。
    #[test]
    fn sections_are_stable_and_minimum_phase() {
        let fs = 44_100.0;
        for f in [10.0, 50.0, 1_000.0, 15_000.0, 21_600.0] {
            for q in [0.1, 0.5, 2.0, 40.0] {
                let w0 = w(f, fs);
                for s in [
                    lowpass(w0, q),
                    highpass(w0, q),
                    bandpass(w0, q),
                    notch(w0, q),
                    peaking(w0, q, -24.0),
                    peaking(w0, q, 24.0),
                    shelf(w0, math::min(q, 4.0), 12.0, fs, false),
                    shelf(w0, math::min(q, 4.0), -12.0, fs, true),
                    tilt(w0, 9.0),
                ] {
                    assert!(s.a2.abs() < 1.0 && s.a1.abs() < 1.0 + s.a2 + 1e-12, "{s:?}");
                    assert!(s.b0 >= s.b2.abs() - 1e-12, "{s:?}");
                    assert!(
                        s.b0 + s.b2 >= s.b1.abs() - 1e-9 * math::max(s.b0.abs(), 1.0),
                        "{s:?}"
                    );
                }
            }
        }
    }

    /// 合わせた点：ベルは直流 0 dB・中心で利得、ローパスは直流 0 dB・中心で Q、ハイパスは中心で Q、バンドパスは中心で 0 dB、
    /// ノッチは中心で深い谷・直流 0 dB、シェルフ・チルトは直流の値。
    #[test]
    fn matched_points_hold() {
        let fs = 48_000.0;
        for f in [30.0, 1_000.0, 8_000.0] {
            let w0 = w(f, fs);
            for q in [0.5, 1.0, 3.0] {
                let b = peaking(w0, q, 9.0);
                assert!(
                    b.db(1e-9).abs() < 1e-6 && (b.db(w0) - 9.0).abs() < 1e-6,
                    "{f} {q}"
                );
                let lp = lowpass(w0, q);
                assert!(lp.db(1e-9).abs() < 1e-6);
                assert!((lp.power(w0) - q * q).abs() < 1e-6 * q * q, "{f} {q}");
                let hp = highpass(w0, q);
                assert!((hp.power(w0) - q * q).abs() < 1e-6 * q * q, "{f} {q}");
                let bp = bandpass(w0, q);
                assert!(bp.db(w0).abs() < 1e-6, "{f} {q}");
                let n = notch(w0, q);
                assert!(n.db(1e-9).abs() < 1e-9 && n.db(w0) < -100.0, "{f} {q}");
                let ls = shelf(w0, q, 12.0, fs, false);
                assert!((ls.db(1e-9) - 12.0).abs() < 1e-6, "{f} {q}");
                let hs = shelf(w0, q, -12.0, fs, true);
                assert!(hs.db(1e-9).abs() < 1e-6, "{f} {q}");
            }
            let t = tilt(w0, 12.0);
            assert!((t.db(1e-9) + 6.0).abs() < 1e-6, "{f}");
        }
    }

    /// カットの段の数と、Butterworth の Q（N = 4 で 0.5412・1.3066）。
    #[test]
    fn cuts_stack_butterworth_sections() {
        assert!((butterworth_q(4, 1) - 0.541_196).abs() < 1e-6);
        assert!((butterworth_q(4, 2) - 1.306_563).abs() < 1e-6);
        assert!((butterworth_q(2, 1) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
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
        let low: f64 = cut(w0, 48, false)
            .iter()
            .map(|s| s.db(w(10.0, 48_000.0)))
            .sum();
        assert!(low < -150.0, "{low}");
        let high: f64 = cut(w0, 24, true)
            .iter()
            .map(|s| s.db(w(20_000.0, 48_000.0)))
            .sum();
        assert!(high < -60.0, "{high}");
    }
}
