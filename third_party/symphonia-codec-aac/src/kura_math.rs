// kura が足したファイル（KURA.md）。このクレートのほかのファイルと同じく MPL-2.0。
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! kura：f32 の超越関数を、OS の libm に頼らずに求める。
//!
//! 元のコードは std の `f32::sin`・`f32::powf`（OS の libm）を呼ぶので、表の値が OS の版・CPU で 1 ulp 揺れる（手元の
//! macOS 26 の arm64 と x86_64 で、POW43 の表の 59 個、サイン窓の 12 個が違った）。ここでは引数を f64 にして libm クレート
//! （純 Rust。版を固定）の f64 の関数で計算し、f32 に丸める。libm クレートの f32 の関数（`powf` は FreeBSD の e_powf.c）は
//! POW43 の表の 756 個で正しい丸めから 1 ulp ずれるので使わない。f64 で計算して丸めた値は、ほぼ正しく丸めた値になる。

/// sin（f32 の引数を f64 で計算して丸める）。
#[inline(always)]
pub(crate) fn sinf(x: f32) -> f32 {
    libm::sin(f64::from(x)) as f32
}

/// x の y 乗（f32 の引数を f64 で計算して丸める）。
#[inline(always)]
pub(crate) fn powf(x: f32, y: f32) -> f32 {
    libm::pow(f64::from(x), f64::from(y)) as f32
}
