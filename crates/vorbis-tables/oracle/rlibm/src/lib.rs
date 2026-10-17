//! 照合用の C エンコーダ（aoTuV b6.03 の写し）が使う超越関数を、Rust の `libm` =0.2.16（arch なし）で出す。
//!
//! C 側は `kura_libm.h` を `-include` で読み、`exp(x)` を `kura_exp(x)` などに置き換える。
//! kura-engine の `crate::math` も同じ `libm` の同じ関数を呼ぶので、照合用のエンコーダと移植の
//! 初期化の表（MDCT・FFT の三角関数、窓、bark・オクターブ、ATH、トーンの曲線など）は同じビットになる。
//!
//! ソースが使う超越関数（`grep` で数えた。../README.md の「超越関数の差し替え」の節）：
//! exp 3・log 10・pow 4・atan 2・sin 10・cos 9・acos 2・ldexp 2・frexp 1（hypot は psytune.c だけ。
//! ビルドに入らないが、`FAST_HYPOT` の受け口として出しておく）。float 版（expf など）は使われていない。
//! sqrt・floor・ceil・fabs・rint は IEEE で一意なので差し替えない。

/// e の x 乗（`crate::math::exp`）。
#[no_mangle]
pub extern "C" fn kura_exp(x: f64) -> f64 {
    libm::exp(x)
}

/// 自然対数（`crate::math::ln`）。
#[no_mangle]
pub extern "C" fn kura_log(x: f64) -> f64 {
    libm::log(x)
}

/// x の y 乗（`crate::math::pow`）。
#[no_mangle]
pub extern "C" fn kura_pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

/// 逆正接（`crate::math::atan`）。
#[no_mangle]
pub extern "C" fn kura_atan(x: f64) -> f64 {
    libm::atan(x)
}

/// 正弦（`crate::math::sin`）。
#[no_mangle]
pub extern "C" fn kura_sin(x: f64) -> f64 {
    libm::sin(x)
}

/// 余弦（`crate::math::cos`）。
#[no_mangle]
pub extern "C" fn kura_cos(x: f64) -> f64 {
    libm::cos(x)
}

/// 逆余弦（lsp.c だけ。floor0 用でエンコーダの経路には入らない）。
#[no_mangle]
pub extern "C" fn kura_acos(x: f64) -> f64 {
    libm::acos(x)
}

/// sqrt(x²+y²)（psytune.c の FAST_HYPOT だけ。ビルドには入らない）。
#[no_mangle]
pub extern "C" fn kura_hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

/// x·2ⁿ（sharedbook.c の `_float32_pack`・`_float32_unpack`）。
#[no_mangle]
pub extern "C" fn kura_ldexp(x: f64, n: i32) -> f64 {
    libm::ldexp(x, n)
}

/// 仮数と指数に分ける（lsp.c だけ。floor0 用でエンコーダの経路には入らない）。
///
/// # Safety
/// `e` は書き込める `int` を指すこと（C の `frexp` と同じ約束）。
#[no_mangle]
pub unsafe extern "C" fn kura_frexp(x: f64, e: *mut i32) -> f64 {
    let (m, k) = libm::frexp(x);
    if !e.is_null() {
        // SAFETY: 呼び出し側（C の frexp の約束）が有効な int へのポインタを渡す
        unsafe { *e = k };
    }
    m
}
