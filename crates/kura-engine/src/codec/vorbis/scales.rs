//! dB・オクターブ・Bark の換算（`lib/scales.h`）。
//!
//! # 移す C
//!
//! `lib/scales.h`（`VORBIS_IEEE_FLOAT32` の側。31–53 行）：
//! - `unitnorm`（32–40）：符号だけ残した ±1.0（ビット操作：`(bits & 0x8000_0000) | 0x3f80_0000`）。
//! - `todB`（43–51）：`(float)((ix & 0x7fffffff) * 7.17711438e-7f - 764.6161886f)`。u32 → f32 の変換（丸め）、
//!   f32 の積、f32 の差。**libm を呼ばない**。`todB_nn` は同じ（53）。
//! - `toBARK`（78）：`13.1f*atan(.00074f*(n))+2.24f*atan((n)*(n)*1.85e-8f)+1e-4f*(n)`。マクロなので引数の型で演算の型が
//!   変わる（psy.c:463–469 の `rate/(2*n)*i` は long → `.00074f*(long)` は float、`atan` は double の関数、`13.1f*double` は
//!   double、`(n)*(n)` は long × long）。**呼ぶ側の式の型をそのまま写す**ため、Rust では引数の型ごとに関数を分けてよい。
//! - `toOC`（86）：`log(n)*1.442695f-5.965784f`（double）。`fromOC`（87）：`exp(((o)+5.965784f)*.693147f)`（double）。
//!
//! 超越関数は `crate::math`（`atan`・`ln`・`exp`）。
//!
//! # 移さないもの
//!
//! `fromdB`・`fromBARK`・`toMEL`・`fromMEL`（エンコーダの経路で使わない）。IEEE でない `todB`（56–65）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use crate::math;

/// `unitnorm`（scales.h:32–40）：x の符号の ±1.0（符号のビットだけ残し、`0x3f80_0000` を立てる。NaN の符号もそのまま）。
pub(crate) fn unitnorm(x: f32) -> f32 {
    f32::from_bits((x.to_bits() & 0x8000_0000) | 0x3f80_0000)
}

/// `todB`（scales.h:43–51）：振幅の dB の近似（ビット操作）。
///
/// `(float)(ix.i * 7.17711438e-7f - 764.6161886f)`：符号を落としたビット列（u32）を f32 に丸め、f32 で掛け、f32 で引く
/// （3 回の丸め。FMA にしない）。定数は C のリテラルの桁のまま書く。
#[allow(clippy::excessive_precision)]
pub(crate) fn todb(x: f32) -> f32 {
    let i = x.to_bits() & 0x7fff_ffff;
    let prod = i as f32 * 7.177_114_38e-7_f32;
    prod - 764.616_188_6_f32
}

/// `toBARK(n)`（scales.h:78）の、`n` が `long` の式（psy.c:463–469）。戻りは C の式の型（double）。
///
/// `13.1f*atan(.00074f*(n)) + 2.24f*atan((n)*(n)*1.85e-8f) + 1e-4f*(n)`：
/// - `.00074f*(n)`・`1e-4f*(n)` は long を float にして float で掛ける。
/// - `(n)*(n)` は long の積、それを float にして `1.85e-8f` と float で掛ける。
/// - `atan` は double の関数（float の引数を double にして渡す）。`13.1f*atan(..)` は double。
/// - 和は左から：double + double、それに float を double にして足す。
pub(crate) fn to_bark_long(n: i64) -> f64 {
    let a = 0.000_74_f32 * n as f32;
    let t1 = f64::from(13.1_f32) * math::atan(f64::from(a));
    let nn = n.wrapping_mul(n);
    let b = nn as f32 * 1.85e-8_f32;
    let t2 = f64::from(2.24_f32) * math::atan(f64::from(b));
    let c = 1e-4_f32 * n as f32;
    t1 + t2 + f64::from(c)
}

/// `toOC(n)`（scales.h:86）：`log(n)*1.442695f-5.965784f`（double）。`n` は double にしてから渡す（C の呼び出しの式の型で、
/// 渡す前の丸めが決まる）。定数は C の float のリテラル（std の `LOG2_E` とは別の値）。
#[allow(clippy::approx_constant)]
pub(crate) fn to_oc(n: f64) -> f64 {
    math::ln(n) * f64::from(1.442_695_f32) - f64::from(5.965_784_f32)
}

/// `fromOC(o)`（scales.h:87）：`exp(((o)+5.965784f)*.693147f)`（double）。定数は C の float のリテラル（std の `LN_2` とは別）。
#[allow(clippy::approx_constant)]
pub(crate) fn from_oc(o: f64) -> f64 {
    math::exp((o + f64::from(5.965_784_f32)) * f64::from(0.693_147_f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// todB は C のビット操作の式（u32 → f32、積、差の 3 回の丸め）と同じ。凍結した値（f32 のビット列。C の式を
    /// float の丸めで計算したもの）と、符号を見ないこと。
    #[test]
    fn todb_matches_the_bit_trick() {
        for (x, bits) in [
            (1.0_f32, 0xb880_0000_u32),
            (0.5, 0xc0c0_a900),
            (1e-3, 0xc270_3f00),
            (32768.0, 0x42b4_9e30),
            (0.0, 0xc43f_2770),
            (3.0e-20, 0xc3c3_5916),
        ] {
            assert_eq!(todb(x).to_bits(), bits, "{x}");
            assert_eq!(todb(-x).to_bits(), bits, "{x}");
        }
        // 4 倍（+2 オクターブ）でおよそ +12 dB
        let d = todb(4.0) - todb(1.0);
        assert!((d - 12.04).abs() < 0.1, "{d}");
        // 範囲：有限の入力では [-764.62, 776.6] に収まる
        assert!(todb(f32::MAX) < 800.0);
        assert!(todb(f32::MIN_POSITIVE) > -770.0);
    }

    /// unitnorm は符号の ±1（0・NaN の符号も写す）。
    #[test]
    fn unitnorm_keeps_only_the_sign() {
        assert_eq!(unitnorm(3.5), 1.0);
        assert_eq!(unitnorm(-1e-30), -1.0);
        assert_eq!(unitnorm(0.0), 1.0);
        assert_eq!(unitnorm(-0.0), -1.0);
        assert_eq!(unitnorm(f32::NAN).abs(), 1.0);
        assert_eq!(unitnorm(-f32::INFINITY), -1.0);
    }

    /// Bark・オクターブ：既知の点と単調性、fromOC は toOC の逆。
    #[test]
    fn bark_and_octave_scales() {
        assert_eq!(to_bark_long(0), 0.0);
        // 1 kHz はおよそ 8.5 Bark、22 kHz はおよそ 25 Bark
        let b1k = to_bark_long(1000);
        assert!((b1k - 8.5).abs() < 0.5, "{b1k}");
        let b22k = to_bark_long(22_050);
        assert!((b22k - 25.0).abs() < 1.5, "{b22k}");
        let mut prev = -1.0;
        for n in (0..96_000).step_by(997) {
            let b = to_bark_long(n);
            assert!(b > prev);
            prev = b;
        }
        // 2^5.965784 ≈ 62.5 Hz がオクターブ 0
        assert!(to_oc(62.5).abs() < 1e-4);
        assert!((to_oc(125.0) - 1.0).abs() < 1e-4);
        for o in [-2.0, 0.0, 1.5, 7.25] {
            let back = to_oc(from_oc(o));
            assert!((back - o).abs() < 1e-5, "{o} → {back}");
        }
    }
}
