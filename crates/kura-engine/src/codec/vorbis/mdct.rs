//! 順方向の MDCT（`lib/mdct.c`。`MDCT_INTEGERIZED` でない float の版）。
//!
//! # 移す C（SSE でない枝だけ）
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `mdct_init` | 54–92 | `log2n = rint(log((float)n)/log(2.f))`（`crate::math::ln`、round_ties_even）。`trig` は `n + n/4` 個：`cos((M_PI/n)*(4*i))` などを double で計算して float に（66–76 行。`M_PI` は double）。`bitrev` は `n/4` 個（78–91）。`scale = 4.f/n`（float） |
//! | `mdct_butterfly_8` | 407–454（#else 432–452） | |
//! | `mdct_butterfly_16` | 457–528（#else 495–523） | `cPI2_8` は float の定数（mdct.h:44–46） |
//! | `mdct_butterfly_32` | 531–664（#else 602–658） | `cPI1_8`・`cPI2_8`・`cPI3_8` |
//! | `mdct_butterfly_first` | 801–896（#else 854–894） | 引数は `(T, x, points)` |
//! | `mdct_butterfly_generic` | 899–1080（#else 1032–1078） | 引数は `(T, x, points, trigint)` |
//! | `mdct_butterflies` | 1105–1135 | `stages = log2n - 5` |
//! | `mdct_bitreverse` | 1155–1274（#else 1228–1272） | |
//! | `mdct_forward` | 1799–1869（`#else` の側） | 作業領域 `w` は n 個。`out[i] = (w[0]*T[0]+w[1]*T[1])*scale` の順で掛ける |
//!
//! - `MULT_NORM(x)`・`FLOAT_CONV(x)` は何もしない（mdct.h:48–49）。`HALVE(x)` は `x*.5f`。
//! - 式の中の演算の順・どれを先に掛けるかを変えない（例：1824 行 `r1*T[1] + r0*T[0]`）。FMA にしない。
//! - envelope.c は入力と出力に同じ配列を渡す（`mdct_forward(&ve->mdct, vec, vec)`）。C は入力をすべて作業領域に読んでから
//!   出力に書くので、Rust では入力を写してから呼ぶか、`in`・`out` を分けて同じ値を作る。
//!
//! # 移さないもの
//!
//! `mdct_backward`（デコーダ）、SSE の枝と `mdct_butterfly_first_backward`・`mdct_butterflies_backward`（93–403・667–799・
//! 1082–1103）、`mdct_clear`。
//!
//! # 型の対応
//!
//! `mdct_lookup`（mdct.h:55–73。SSE でない欄だけ）→ [`MdctLookup`]。`DATA_TYPE`・`REG_TYPE` は `f32`。
//!
//! # 移植の補足
//!
//! - C のポインタ（`x1`・`x2`・`T`・`w0`・`w1`）は同じ配列の添字にした。`do { … } while(x2>=x)` は回る数を数えた `for` に
//!   した（同じ順で同じ要素を読み書きする）。
//! - 長さ・表が `mdct_init` の作った形でない（C では範囲の外の読み書き）ときは、何もしない（panic しない）。
//!   エンコーダの中では `_vds_shared_init` が確かめた大きさ（2 の累乗、64〜8192）と envelope の 128 だけで呼ぶ。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use std::f64::consts::PI;

use crate::math;

/// mdct.h:44（`.38268343236508977175F`）。
#[allow(clippy::excessive_precision, clippy::approx_constant)]
const C_PI3_8: f32 = 0.382_683_432_365_089_771_75_f32;
/// mdct.h:45（`.70710678118654752441F`）。
#[allow(clippy::excessive_precision, clippy::approx_constant)]
const C_PI2_8: f32 = 0.707_106_781_186_547_524_41_f32;
/// mdct.h:46（`.92387953251128675613F`）。
#[allow(clippy::excessive_precision, clippy::approx_constant)]
const C_PI1_8: f32 = 0.923_879_532_511_286_756_13_f32;

/// `mdct_lookup`（mdct.h:55–73）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MdctLookup {
    pub n: i32,
    pub log2n: i32,
    /// `n + n/4` 個。
    pub trig: Vec<f32>,
    /// `n/4` 個。
    pub bitrev: Vec<i32>,
    /// `4.f/n`。
    pub scale: f32,
}

/// `mdct_init`（mdct.c:54–92）。`n` は 2 の累乗で 64 以上（C の前提。そうでなければ表を作らず、[`mdct_forward`] は何もしない）。
pub(crate) fn mdct_init(n: i32) -> MdctLookup {
    if n < 64 || n.count_ones() != 1 {
        return MdctLookup {
            n,
            ..MdctLookup::default()
        };
    }
    let nu = n as usize;
    let n2 = nu >> 1;
    // int log2n=lookup->log2n=rint(log((float)n)/log(2.f));
    let log2n =
        (math::ln(f64::from(n as f32)) / math::ln(f64::from(2.0_f32))).round_ties_even() as i32;

    let mut t = vec![0.0_f32; nu + nu / 4];
    let nf = f64::from(n);
    // trig lookups...
    for i in 0..nu / 4 {
        let a = (PI / nf) * f64::from(4 * i as i32);
        t[i * 2] = math::cos(a) as f32;
        t[i * 2 + 1] = (-math::sin(a)) as f32;
        let b = (PI / f64::from(2 * n)) * f64::from(2 * i as i32 + 1);
        t[n2 + i * 2] = math::cos(b) as f32;
        t[n2 + i * 2 + 1] = math::sin(b) as f32;
    }
    for i in 0..nu / 8 {
        let a = (PI / nf) * f64::from(4 * i as i32 + 2);
        t[nu + i * 2] = (math::cos(a) * 0.5) as f32;
        t[nu + i * 2 + 1] = (-math::sin(a) * 0.5) as f32;
    }

    // bitreverse lookup...
    let mut bitrev = vec![0_i32; nu / 4];
    let mask = (1_i32 << (log2n - 1)) - 1;
    let msb = 1_i32 << (log2n - 2);
    for i in 0..nu / 8 {
        let mut acc = 0_i32;
        let mut j = 0;
        while (msb >> j) != 0 {
            if (msb >> j) & (i as i32) != 0 {
                acc |= 1 << j;
            }
            j += 1;
        }
        bitrev[i * 2] = ((!acc) & mask) - 1;
        bitrev[i * 2 + 1] = acc;
    }
    MdctLookup {
        n,
        log2n,
        trig: t,
        bitrev,
        // lookup->scale=FLOAT_CONV(4.f/n);
        scale: 4.0_f32 / n as f32,
    }
}

/// `init` が `mdct_init` の作った形か（2 の累乗・表の長さ・bitrev の範囲）。
fn lookup_is_valid(init: &MdctLookup) -> bool {
    let n = init.n;
    if n < 64 || n.count_ones() != 1 || init.log2n != n.trailing_zeros() as i32 {
        return false;
    }
    let nu = n as usize;
    let half = n / 2;
    init.trig.len() == nu + nu / 4
        && init.bitrev.len() == nu / 4
        && init.bitrev.iter().all(|&b| (0..half - 1).contains(&b))
}

/// `mdct_butterfly_8`（mdct.c:407–454。#else 432–452）。`x` は 8 個。
fn mdct_butterfly_8(x: &mut [f32]) {
    let mut r0 = x[6] + x[2];
    let mut r1 = x[6] - x[2];
    let mut r2 = x[4] + x[0];
    let r3 = x[4] - x[0];

    x[6] = r0 + r2;
    x[4] = r0 - r2;

    r0 = x[5] - x[1];
    r2 = x[7] - x[3];
    x[0] = r1 + r0;
    x[2] = r1 - r0;

    r0 = x[5] + x[1];
    r1 = x[7] + x[3];
    x[3] = r2 + r3;
    x[1] = r2 - r3;
    x[7] = r1 + r0;
    x[5] = r1 - r0;
}

/// `mdct_butterfly_16`（mdct.c:457–528。#else 495–523）。`x` は 16 個。
fn mdct_butterfly_16(x: &mut [f32]) {
    let mut r0 = x[1] - x[9];
    let mut r1 = x[0] - x[8];

    x[8] += x[0];
    x[9] += x[1];
    x[0] = (r0 + r1) * C_PI2_8;
    x[1] = (r0 - r1) * C_PI2_8;

    r0 = x[3] - x[11];
    r1 = x[10] - x[2];
    x[10] += x[2];
    x[11] += x[3];
    x[2] = r0;
    x[3] = r1;

    r0 = x[12] - x[4];
    r1 = x[13] - x[5];
    x[12] += x[4];
    x[13] += x[5];
    x[4] = (r0 - r1) * C_PI2_8;
    x[5] = (r0 + r1) * C_PI2_8;

    r0 = x[14] - x[6];
    r1 = x[15] - x[7];
    x[14] += x[6];
    x[15] += x[7];
    x[6] = r0;
    x[7] = r1;

    mdct_butterfly_8(&mut x[..8]);
    mdct_butterfly_8(&mut x[8..16]);
}

/// `mdct_butterfly_32`（mdct.c:531–664。#else 602–658）。`x` は 32 個。
fn mdct_butterfly_32(x: &mut [f32]) {
    let mut r0 = x[30] - x[14];
    let mut r1 = x[31] - x[15];

    x[30] += x[14];
    x[31] += x[15];
    x[14] = r0;
    x[15] = r1;

    r0 = x[28] - x[12];
    r1 = x[29] - x[13];
    x[28] += x[12];
    x[29] += x[13];
    x[12] = r0 * C_PI1_8 - r1 * C_PI3_8;
    x[13] = r0 * C_PI3_8 + r1 * C_PI1_8;

    r0 = x[26] - x[10];
    r1 = x[27] - x[11];
    x[26] += x[10];
    x[27] += x[11];
    x[10] = (r0 - r1) * C_PI2_8;
    x[11] = (r0 + r1) * C_PI2_8;

    r0 = x[24] - x[8];
    r1 = x[25] - x[9];
    x[24] += x[8];
    x[25] += x[9];
    x[8] = r0 * C_PI3_8 - r1 * C_PI1_8;
    x[9] = r1 * C_PI3_8 + r0 * C_PI1_8;

    r0 = x[22] - x[6];
    r1 = x[7] - x[23];
    x[22] += x[6];
    x[23] += x[7];
    x[6] = r1;
    x[7] = r0;

    r0 = x[4] - x[20];
    r1 = x[5] - x[21];
    x[20] += x[4];
    x[21] += x[5];
    x[4] = r1 * C_PI1_8 + r0 * C_PI3_8;
    x[5] = r1 * C_PI3_8 - r0 * C_PI1_8;

    r0 = x[2] - x[18];
    r1 = x[3] - x[19];
    x[18] += x[2];
    x[19] += x[3];
    x[2] = (r1 + r0) * C_PI2_8;
    x[3] = (r1 - r0) * C_PI2_8;

    r0 = x[0] - x[16];
    r1 = x[1] - x[17];
    x[16] += x[0];
    x[17] += x[1];
    x[0] = r1 * C_PI3_8 + r0 * C_PI1_8;
    x[1] = r1 * C_PI1_8 - r0 * C_PI3_8;

    mdct_butterfly_16(&mut x[..16]);
    mdct_butterfly_16(&mut x[16..32]);
}

/// 1 組の 2 レジスタの蝶（`mdct_butterfly_first`・`generic` の中の 6 行）：`x1[a..=a+1]` と `x2[a..=a+1]` を
/// `T[t]`・`T[t+1]` で回す。
#[inline]
fn butterfly_pair(t: &[f32], ti: usize, x: &mut [f32], x1: usize, x2: usize, a: usize) {
    let r0 = x[x1 + a] - x[x2 + a];
    let r1 = x[x1 + a + 1] - x[x2 + a + 1];
    x[x1 + a] += x[x2 + a];
    x[x1 + a + 1] += x[x2 + a + 1];
    x[x2 + a] = r1 * t[ti + 1] + r0 * t[ti];
    x[x2 + a + 1] = r1 * t[ti] - r0 * t[ti + 1];
}

/// `mdct_butterfly_first`（mdct.c:801–896。#else 854–894）。
fn mdct_butterfly_first(t: &[f32], x: &mut [f32], points: i32) {
    let points = points as usize;
    // do{ … x1-=8; x2-=8; T+=16; }while(x2>=x);
    for k in 0..points / 16 {
        let x1 = points - 8 - 8 * k;
        let x2 = (points >> 1) - 8 - 8 * k;
        let ti = 16 * k;
        butterfly_pair(t, ti, x, x1, x2, 6);
        butterfly_pair(t, ti + 4, x, x1, x2, 4);
        butterfly_pair(t, ti + 8, x, x1, x2, 2);
        butterfly_pair(t, ti + 12, x, x1, x2, 0);
    }
}

/// `mdct_butterfly_generic`（mdct.c:899–1080。#else 1032–1078）。
fn mdct_butterfly_generic(t: &[f32], x: &mut [f32], points: i32, trigint: i32) {
    let points = points as usize;
    let trigint = trigint as usize;
    let mut ti = 0;
    for k in 0..points / 16 {
        let x1 = points - 8 - 8 * k;
        let x2 = (points >> 1) - 8 - 8 * k;
        butterfly_pair(t, ti, x, x1, x2, 6);
        ti += trigint;
        butterfly_pair(t, ti, x, x1, x2, 4);
        ti += trigint;
        butterfly_pair(t, ti, x, x1, x2, 2);
        ti += trigint;
        butterfly_pair(t, ti, x, x1, x2, 0);
        ti += trigint;
    }
}

/// `mdct_butterflies`（mdct.c:1105–1135）。
fn mdct_butterflies(init: &MdctLookup, x: &mut [f32], points: i32) {
    let t = &init.trig;
    let mut stages = init.log2n - 5;

    // if(--stages>0)
    stages -= 1;
    if stages > 0 {
        mdct_butterfly_first(t, x, points);
    }

    // for(i=1;--stages>0;i++)
    let mut i = 1;
    loop {
        stages -= 1;
        if stages <= 0 {
            break;
        }
        for j in 0..(1_i32 << i) {
            let off = ((points >> i) * j) as usize;
            mdct_butterfly_generic(t, &mut x[off..], points >> i, 4 << i);
        }
        i += 1;
    }

    for j in (0..points as usize).step_by(32) {
        mdct_butterfly_32(&mut x[j..j + 32]);
    }
}

/// `mdct_bitreverse`（mdct.c:1155–1274。#else 1228–1272）。`x` は作業領域 `w` の全体（n 個）：後ろ半分を読み、前半分に書く。
fn mdct_bitreverse(init: &MdctLookup, x: &mut [f32]) {
    let n = init.n as usize;
    let bit = &init.bitrev;
    let t = &init.trig[n..];
    let mut w0 = 0_usize;
    let mut w1 = n >> 1;
    let xb = n >> 1;
    let mut bi = 0_usize;
    let mut ti = 0_usize;

    loop {
        let mut x0 = xb + bit[bi] as usize;
        let mut x1 = xb + bit[bi + 1] as usize;

        let mut r0 = x[x0 + 1] - x[x1 + 1];
        let mut r1 = x[x0] + x[x1];
        let mut r2 = r1 * t[ti] + r0 * t[ti + 1];
        let mut r3 = r1 * t[ti + 1] - r0 * t[ti];

        w1 -= 4;

        r0 = (x[x0 + 1] + x[x1 + 1]) * 0.5_f32;
        r1 = (x[x0] - x[x1]) * 0.5_f32;

        x[w0] = r0 + r2;
        x[w1 + 2] = r0 - r2;
        x[w0 + 1] = r1 + r3;
        x[w1 + 3] = r3 - r1;

        x0 = xb + bit[bi + 2] as usize;
        x1 = xb + bit[bi + 3] as usize;

        r0 = x[x0 + 1] - x[x1 + 1];
        r1 = x[x0] + x[x1];
        r2 = r1 * t[ti + 2] + r0 * t[ti + 3];
        r3 = r1 * t[ti + 3] - r0 * t[ti + 2];

        r0 = (x[x0 + 1] + x[x1 + 1]) * 0.5_f32;
        r1 = (x[x0] - x[x1]) * 0.5_f32;

        x[w0 + 2] = r0 + r2;
        x[w1] = r0 - r2;
        x[w0 + 3] = r1 + r3;
        x[w1 + 1] = r3 - r1;

        ti += 4;
        bi += 4;
        w0 += 4;

        if w0 >= w1 {
            break;
        }
    }
}

/// `mdct_forward`（mdct.c:1799–1869）：`input` は n 個（窓を掛けた PCM）、`out` は n/2 個。
pub(crate) fn mdct_forward(init: &MdctLookup, input: &[f32], out: &mut [f32]) {
    let mut w = vec![0.0_f32; usize::try_from(init.n).unwrap_or(0)];
    mdct_forward_with(init, input, out, &mut w);
}

/// [`mdct_forward`] の作業領域（C の `alloca` の `w`。n 個）を呼ぶ側が持つ版（envelope が 64 標本ごとに呼ぶので）。
/// 作業領域の中身は読まない（すべて書いてから読む）。
pub(crate) fn mdct_forward_with(init: &MdctLookup, input: &[f32], out: &mut [f32], w: &mut [f32]) {
    if !lookup_is_valid(init) {
        return;
    }
    let n = init.n as usize;
    let n2 = n >> 1;
    let n4 = n >> 2;
    let n8 = n >> 3;
    if input.len() < n || out.len() < n2 || w.len() < n {
        return;
    }
    let input = &input[..n];
    let w = &mut w[..n];
    let trig = &init.trig;

    // window + rotate + step 1
    let mut x0 = n2 + n4;
    let mut x1 = x0 + 1;
    let mut ti = n2;
    let mut i = 0;

    while i < n8 {
        x0 -= 4;
        ti -= 2;
        let r0 = input[x0 + 2] + input[x1];
        let r1 = input[x0] + input[x1 + 2];
        w[n2 + i] = r1 * trig[ti + 1] + r0 * trig[ti];
        w[n2 + i + 1] = r1 * trig[ti] - r0 * trig[ti + 1];
        x1 += 4;
        i += 2;
    }

    x1 = 1;

    while i < n2 - n8 {
        ti -= 2;
        x0 -= 4;
        let r0 = input[x0 + 2] - input[x1];
        let r1 = input[x0] - input[x1 + 2];
        w[n2 + i] = r1 * trig[ti + 1] + r0 * trig[ti];
        w[n2 + i + 1] = r1 * trig[ti] - r0 * trig[ti + 1];
        x1 += 4;
        i += 2;
    }

    x0 = n;

    while i < n2 {
        ti -= 2;
        x0 -= 4;
        let r0 = -input[x0 + 2] - input[x1];
        let r1 = -input[x0] - input[x1 + 2];
        w[n2 + i] = r1 * trig[ti + 1] + r0 * trig[ti];
        w[n2 + i + 1] = r1 * trig[ti] - r0 * trig[ti + 1];
        x1 += 4;
        i += 2;
    }

    mdct_butterflies(init, &mut w[n2..], n2 as i32);
    mdct_bitreverse(init, w);

    // rotate + window
    let scale = init.scale;
    let mut ti = n2;
    let mut xo = n2;
    for i in 0..n4 {
        xo -= 1;
        let a = w[2 * i];
        let b = w[2 * i + 1];
        out[i] = (a * trig[ti] + b * trig[ti + 1]) * scale;
        out[xo] = (a * trig[ti + 1] - b * trig[ti]) * scale;
        ti += 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 定義どおりの MDCT（double。`X[k] = Σ x[i] cos(2π/N (i + 1/2 + N/4)(k + 1/2))`）。Vorbis の順方向は
    /// これに `-2/N`… の比例の違いがあるので、比を 1 つ決めて全体を比べる。
    fn naive(x: &[f32]) -> Vec<f64> {
        let n = x.len();
        let n0 = n as f64 / 4.0 + 0.5;
        (0..n / 2)
            .map(|k| {
                let mut s = 0.0;
                for (i, &v) in x.iter().enumerate() {
                    s += f64::from(v)
                        * math::cos(2.0 * PI / n as f64 * (i as f64 + n0) * (k as f64 + 0.5));
                }
                s
            })
            .collect()
    }

    /// 表の形（n=64〜8192）：trig の長さ・bitrev が 0〜n/2-2 の置換に近い形・scale。
    #[test]
    fn init_tables_have_the_c_shape() {
        for log2n in 6..=13 {
            let n = 1 << log2n;
            let l = mdct_init(n);
            assert_eq!(l.n, n);
            assert_eq!(l.log2n, log2n);
            assert_eq!(l.trig.len(), (n + n / 4) as usize);
            assert_eq!(l.bitrev.len(), (n / 4) as usize);
            assert_eq!(l.scale, 4.0 / n as f32);
            assert!(lookup_is_valid(&l));
            // T[0] = cos 0 = 1、T[1] = -sin 0 = -0
            assert_eq!(l.trig[0], 1.0);
            assert_eq!(l.trig[1].to_bits(), (-0.0_f32).to_bits());
            // 奇数番目の bitrev（acc）は i（< n/8）のビットを log2n-1 ビットで反転したもの：0〜n/2 の 4 の倍数を 1 度ずつ。
            // 偶数番目は `(~acc & (n/2-1)) - 1`。
            let mut odd: Vec<i32> = l.bitrev.iter().skip(1).step_by(2).copied().collect();
            for (k, pair) in l.bitrev.as_chunks::<2>().0.iter().enumerate() {
                assert_eq!(pair[0], ((!pair[1]) & (n / 2 - 1)) - 1, "n={n} k={k}");
            }
            odd.sort();
            assert_eq!(odd, (0..n / 8).map(|i| i * 4).collect::<Vec<_>>());
        }
        // 範囲の外は表を作らない（forward は何もしない）
        let bad = mdct_init(48);
        assert!(bad.trig.is_empty());
        let mut out = vec![7.0_f32; 24];
        mdct_forward(&bad, &[1.0; 48], &mut out);
        assert!(out.iter().all(|&v| v == 7.0));
    }

    /// 定義どおりの MDCT と比例する（Vorbis の順方向の正規化）。
    #[test]
    fn forward_matches_the_definition() {
        let mut r = crate::determinism::rng(5);
        use rand_chacha::rand_core::Rng;
        for log2n in [6, 7, 8, 11] {
            let n = 1usize << log2n;
            let x: Vec<f32> = (0..n)
                .map(|_| (f64::from(r.next_u32()) / 4_294_967_296.0 * 2.0 - 1.0) as f32)
                .collect();
            let l = mdct_init(n as i32);
            let mut out = vec![0.0_f32; n / 2];
            mdct_forward(&l, &x, &mut out);
            let want = naive(&x);
            // 比（Vorbis は -2/N 倍… の符号と大きさの違いがあるので最小二乗で 1 つ決める）
            let num: f64 = out.iter().zip(&want).map(|(a, b)| f64::from(*a) * b).sum();
            let den: f64 = want.iter().map(|b| b * b).sum();
            let k = num / den;
            assert!(k.abs() > 1e-6);
            for (a, b) in out.iter().zip(&want) {
                assert!((f64::from(*a) - k * b).abs() < 1e-4, "n={n}");
            }
            // 同じ入力なら同じビット列（作業領域を使い回しても）
            let mut out2 = vec![0.0_f32; n / 2];
            let mut w = vec![123.0_f32; n];
            mdct_forward_with(&l, &x, &mut out2, &mut w);
            assert_eq!(
                out.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                out2.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
    }
}
