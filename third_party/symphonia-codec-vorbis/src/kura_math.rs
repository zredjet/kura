// kura が足したファイル（KURA.md）。このクレートのほかのファイルと同じく MPL-2.0。
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! kura：f32 の超越関数を、OS の libm に頼らずに求める（floor 0 の cos・exp）。
//!
//! 元のコードは std の `f32::cos`・`f32::exp`（OS の libm）を呼ぶので、値が OS の版・CPU で 1 ulp 揺れうる。ここでは
//! 引数を f64 にして libm クレート（純 Rust。版を固定）の f64 の関数で計算し、f32 に丸める（ほぼ正しく丸めた値。
//! symphonia-codec-aac の写しの `kura_math` と同じ決め方）。f64 の計算（窓の sin・Bark の atan）は libm クレートの
//! f64 の関数をそのまま呼ぶ。

/// cos（f32 の引数を f64 で計算して丸める）。
#[inline(always)]
pub(crate) fn cosf(x: f32) -> f32 {
    libm::cos(f64::from(x)) as f32
}

/// e の x 乗（f32 の引数を f64 で計算して丸める）。
#[inline(always)]
pub(crate) fn expf(x: f32) -> f32 {
    libm::exp(f64::from(x)) as f32
}
