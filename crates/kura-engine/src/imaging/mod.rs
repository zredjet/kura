//! 画素の共通の計算（docs/editing.md §5.2）。コーデックと操作の両方が使う。
//!
//! - 整数か、f64 の四則演算と `sqrt` だけで計算し、どのターゲットでも同じ結果になる（超越関数を使わない）。
//! - 結果の式は凍結する。変えるなら、使うコーデック・操作の版を上げる（§8.4）。

pub mod blend;
pub mod composite;
pub mod convert;
pub mod orient;
pub mod quantize;
