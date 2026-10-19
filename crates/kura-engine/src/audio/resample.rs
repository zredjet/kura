//! サンプルレート変換の多相 FIR（docs/editing.md §8.3。ADR-138：自前で書く）。式はここで凍結する（変えるなら
//! `audio.resample` の版を上げる）。
//!
//! - g = gcd(from, to)、L = to / g（相の数）、M = from / g。出力 n の中心の入力の位置 nM/L を、整数の商 i と余り p（相）に
//!   分ける（u128 で正確に）。
//! - 出力 y[n] = Σ_{k=0..K} c[p][k] · x[i − (h − 1) + k]。入力の範囲の外は 0。積和は f64 で k の順に `acc + c·x`
//!   （`mul_add` は使わない）、最後に 1 回だけ f32 に丸める。出力どうしに状態がないので、区間の切り方によらず同じ値（E18）。
//! - 係数：K = 256 · from / min(from, to) を偶数に切り上げ、h = K / 2。遮断 fc = ρ · s（ρ = 0.963、s = 1（上げる）か
//!   to / from（下げる））。τ = (p + L·(h − 1 − k)) / L（分子は整数で正確、丸めは割り算の 1 回）、x = τ / h、
//!   窓 w = I0(β·√(1 − x²)) / I0(β)（β = 14.8、|x| ≥ 1 なら 0。I0 は級数）、g = sinc(fc·τ)·w（sinc(u) = sin(πu) / (πu)、
//!   `math::sin`）。相ごとに Σ_k g（k の順）で割る（直流がちょうど通る）。阻止域は入力・出力の低いほうのナイキストから
//!   −142 dB 以下、−0.1 dB の点は 0.942·ナイキスト（調べた値。ADR-148）。
//! - 出力の長さは round_half_even(frames · to / from)（`TimeMap::Rate` の位置の写し方と同じ）。

use crate::error::EngineError;
use crate::math;
use crate::monitor::Monitor;

/// 低いほうのレートの標本で数えた核の幅。
pub const KERNEL_WIDTH: u64 = 256;
/// Kaiser 窓の β。
pub const BETA: f64 = 14.8;
/// 遮断（低いほうのナイキストに対する比。−6 dB の点）。
pub const RHO: f64 = 0.963;

/// 設計したフィルタ（相 × タップの係数の表）。
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// 相の数。
    pub l: u64,
    /// 入力の歩み。
    pub m: u64,
    /// タップの数（偶数）。
    pub k: usize,
    /// 係数（相 p のタップ k は `coeffs[p * k + k]`）。
    pub coeffs: Vec<f64>,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// (L, M)。
pub fn ratio(from: u32, to: u32) -> (u64, u64) {
    let g = gcd(u64::from(from), u64::from(to)).max(1);
    (u64::from(to) / g, u64::from(from) / g)
}

/// タップの数 K（偶数）。
pub fn taps(from: u32, to: u32) -> u64 {
    let low = u64::from(from.min(to)).max(1);
    let k = (KERNEL_WIDTH * u64::from(from)).div_ceil(low);
    k + (k & 1)
}

/// 係数の表の要素の数（L · K）。
pub fn table_len(from: u32, to: u32) -> u64 {
    ratio(from, to).0.saturating_mul(taps(from, to))
}

/// 出力の長さ round_half_even(frames · to / from)。
pub fn out_frames(frames: u64, from: u32, to: u32) -> Option<u64> {
    let v = crate::geom::div_round_half_even(
        i128::from(frames).checked_mul(i128::from(to))?,
        i128::from(from),
    )?;
    u64::try_from(v).ok()
}

/// 出力の区間 `[n0, n1)` が読む入力の区間（段 10。範囲の逆写しに使う）。`(l, m)` は [`ratio`]、`h` はタップの数の半分。
/// 出力 n は入力 `[⌊nM/L⌋ − h + 1, ⌊nM/L⌋ + h + 1)` を読む（[`Filter::output`]）。0 より前は切る。空なら `(0, 0)`。
pub fn source_window(l: u64, m: u64, h: u64, n0: u64, n1: u64) -> (u64, u64) {
    if n0 >= n1 || l == 0 {
        return (0, 0);
    }
    let center = |n: u64| (u128::from(n) * u128::from(m) / u128::from(l)) as u64;
    let lo = center(n0).saturating_sub(h.saturating_sub(1));
    let hi = center(n1 - 1).saturating_add(h).saturating_add(1);
    (lo, hi)
}

/// 入力の区間 `[a, b)` を読む出力の区間（段 10。変わった区間を前へ写すのに使う。保守的）。空なら `(0, 0)`。
pub fn dest_window(l: u64, m: u64, h: u64, a: u64, b: u64) -> (u64, u64) {
    if a >= b || m == 0 {
        return (0, 0);
    }
    // 出力 n が読むのは ⌊nM/L⌋ ≥ a − h かつ ⌊nM/L⌋ < b + h − 1 のとき：n ≥ ⌈(a − h)·L/M⌉、n < ⌈(b + h − 1)·L/M⌉
    let up = |x: u64| {
        let v = (u128::from(x) * u128::from(l)).div_ceil(u128::from(m));
        u64::try_from(v).unwrap_or(u64::MAX)
    };
    let first = up(a.saturating_sub(h));
    let end = up(b.saturating_add(h).saturating_sub(1));
    if first < end { (first, end) } else { (0, 0) }
}

/// 0 次の第 1 種変形ベッセル関数 I0（級数 Σ ((z/2)^k / k!)²。足しても変わらなくなるまで）。
fn i0(z: f64) -> f64 {
    let q = z * z / 4.0;
    let (mut sum, mut term) = (1.0f64, 1.0f64);
    let mut k = 1.0f64;
    loop {
        term = term * q / (k * k);
        if sum + term == sum {
            return sum;
        }
        sum += term;
        k += 1.0;
    }
}

fn sinc(u: f64) -> f64 {
    if u == 0.0 {
        1.0
    } else {
        let a = std::f64::consts::PI * u;
        math::sin(a) / a
    }
}

/// from → to のフィルタを設計する。相ごとに中止を確かめる。
pub fn design(from: u32, to: u32, mon: &dyn Monitor) -> Result<Filter, EngineError> {
    let (l, m) = ratio(from, to);
    let k = taps(from, to);
    let len = usize::try_from(l.saturating_mul(k))
        .map_err(|_| EngineError::Limit("変換のフィルタが大きすぎます".into()))?;
    let mut coeffs = Vec::new();
    coeffs
        .try_reserve_exact(len)
        .map_err(|_| EngineError::Limit("変換のフィルタが大きすぎます".into()))?;
    let h = k / 2;
    let s = if to >= from {
        1.0
    } else {
        f64::from(to) / f64::from(from)
    };
    let fc = RHO * s;
    let i0_beta = i0(BETA);
    let mut g = vec![0f64; k as usize];
    for p in 0..l {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        for (kk, gk) in g.iter_mut().enumerate() {
            // τ = (p + L·(h − 1 − k)) / L。分子は整数で正確に
            let num = i128::from(p) + i128::from(l) * (i128::from(h) - 1 - kk as i128);
            let tau = num as f64 / l as f64;
            let x = tau / h as f64;
            let w = if x.abs() >= 1.0 {
                0.0
            } else {
                i0(BETA * (1.0 - x * x).sqrt()) / i0_beta
            };
            *gk = sinc(fc * tau) * w;
        }
        let sum = g.iter().fold(0.0f64, |acc, v| acc + v);
        coeffs.extend(g.iter().map(|v| v / sum));
    }
    Ok(Filter {
        l,
        m,
        k: k as usize,
        coeffs,
    })
}

impl Filter {
    /// 出力 n の値（`plane` は入力のチャンネル）。
    pub fn output(&self, plane: &[f32], n: u64) -> f32 {
        let num = u128::from(n) * u128::from(self.m);
        let i = num / u128::from(self.l);
        let p = (num % u128::from(self.l)) as usize;
        let h = (self.k / 2) as i128;
        let base = i as i128 - (h - 1);
        let row = &self.coeffs[p * self.k..(p + 1) * self.k];
        let mut acc = 0f64;
        // 範囲の外の入力（0）は足しても値が変わらない（acc は +0 から始まるので −0 にならない）ので飛ばす
        let lo = (-base).clamp(0, self.k as i128) as usize;
        let hi = (plane.len() as i128 - base).clamp(0, self.k as i128) as usize;
        for kk in lo..hi.max(lo) {
            let x = plane[(base + kk as i128) as usize];
            acc += row[kk] * f64::from(x);
        }
        acc as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    /// 段 10：出力の区間が読む入力の窓（source_window）は `Filter::output` が読む位置をすべて含み、入力の区間を読む出力の
    /// 区間（dest_window）は、その区間を読む出力をすべて含む。
    #[test]
    fn windows_cover_the_filter() {
        for (from, to) in [
            (44_100u32, 48_000u32),
            (48_000, 44_100),
            (8_000, 44_100),
            (96_000, 8_000),
        ] {
            let (l, m) = ratio(from, to);
            let h = taps(from, to) / 2;
            // 出力 n が読む入力の範囲（Filter::output と同じ式）
            let read = |n: u64| {
                let i = (u128::from(n) * u128::from(m) / u128::from(l)) as i128;
                (i - (h as i128 - 1), i - (h as i128 - 1) + 2 * h as i128)
            };
            for (n0, n1) in [(0u64, 1u64), (5, 9), (1_000, 1_300), (77, 78)] {
                let (a, b) = source_window(l, m, h, n0, n1);
                for n in n0..n1 {
                    let (lo, hi) = read(n);
                    assert!(
                        lo.max(0) >= a as i128 && hi <= b as i128,
                        "{from}→{to} n={n}"
                    );
                }
            }
            for (a, b) in [(0u64, 1u64), (500, 520), (3_000, 3_001)] {
                let (d0, d1) = dest_window(l, m, h, a, b);
                for n in 0..5_000u64 {
                    let (lo, hi) = read(n);
                    if lo < b as i128 && hi > a as i128 {
                        assert!(n >= d0 && n < d1, "{from}→{to} [{a},{b}) n={n}");
                    }
                }
            }
        }
        assert_eq!(source_window(1, 1, 4, 3, 3), (0, 0));
        assert_eq!(dest_window(1, 1, 4, 3, 3), (0, 0));
    }

    #[test]
    fn ratios_taps_and_lengths() {
        assert_eq!(ratio(44_100, 48_000), (160, 147));
        assert_eq!(taps(44_100, 48_000), 256);
        assert_eq!(taps(48_000, 44_100), 280);
        assert_eq!(taps(96_000, 44_100), 558);
        assert_eq!(out_frames(44_100, 44_100, 48_000), Some(48_000));
        assert_eq!(out_frames(1, 96_000, 44_100), Some(0));
        assert!((i0(0.0) - 1.0).abs() < 1e-15);
        // I0(1) = 1.2660658777520082
        assert!((i0(1.0) - 1.266_065_877_752_008_2).abs() < 1e-15);
    }

    /// 直流はちょうど通る（相ごとの正規化）。
    #[test]
    fn dc_passes_exactly() {
        let f = design(44_100, 48_000, &NoMonitor).unwrap();
        for p in 0..f.l as usize {
            let s: f64 = f.coeffs[p * f.k..(p + 1) * f.k].iter().sum();
            assert!((s - 1.0).abs() < 1e-12);
        }
        let plane = vec![0.25f32; 4_000];
        for n in 300..4_000u64 {
            assert_eq!(f.output(&plane, n), 0.25);
        }
    }

    /// 係数の表を凍結する（ダイジェスト）。
    #[test]
    fn table_is_frozen() {
        // 係数は crate::math（libm を固定）だけで作るので、すべてのターゲットで同じ。変われば SRC の版を上げる
        let digest = |from, to| {
            let f = design(from, to, &NoMonitor).unwrap();
            let mut h = crate::digest::Hasher::new("kura-test-resample");
            for c in &f.coeffs {
                h.u64(c.to_bits());
            }
            h.finish().to_hex()
        };
        for (from, to, want) in [
            (
                44_100,
                48_000,
                "9bd931183b29d21def625310d0f0adcc4fd4084ebe8da85a37091ee7585520b8",
            ),
            (
                48_000,
                44_100,
                "2dbf50bcfc743fc3701964e62c2aedb90649a3ce687e7b8ff048b70af588795c",
            ),
            (
                8_000,
                22_050,
                "8434f62f0cb6b6cac6edb8d872913854f45674f77ce02dbf7cd2fdfb19475b07",
            ),
        ] {
            assert_eq!(digest(from, to), want, "{from} → {to}");
        }
        let f = design(44_100, 48_000, &NoMonitor).unwrap();
        // 同じものを 2 度作っても同じ
        let again = design(44_100, 48_000, &NoMonitor).unwrap();
        assert_eq!(f, again);
    }
}
