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
//! | `mdct_backward` | 1276–1280・1537–1627（`#else` の側） | デコーダ（M16 段 2。ADR-217）。C の呼び手（mapping0.c:1572）は入力と出力に同じ配列を渡すので、同じ配列の上で C と同じ順に読み書きする（[`mdct_backward`]） |
//!
//! - `MULT_NORM(x)`・`FLOAT_CONV(x)` は何もしない（mdct.h:48–49）。`HALVE(x)` は `x*.5f`。
//! - 式の中の演算の順・どれを先に掛けるかを変えない（例：1824 行 `r1*T[1] + r0*T[0]`）。FMA にしない。
//! - envelope.c は入力と出力に同じ配列を渡す（`mdct_forward(&ve->mdct, vec, vec)`）。C は入力をすべて作業領域に読んでから
//!   出力に書くので、Rust では入力を写してから呼ぶか、`in`・`out` を分けて同じ値を作る。
//!
//! # 移さないもの
//!
//! SSE の枝と `mdct_butterfly_first_backward`・`mdct_butterflies_backward`（93–403・667–799・
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
    /// 蝶の段ごとの `T`（[`butterfly_trig`]。C は表を持たない）。
    pub bfly: Vec<Vec<[f32; 8]>>,
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
    let bfly = butterfly_trig(&t, n2, log2n);
    MdctLookup {
        n,
        log2n,
        trig: t,
        bitrev,
        // lookup->scale=FLOAT_CONV(4.f/n);
        scale: 4.0_f32 / n as f32,
        bfly,
    }
}

/// 蝶の段（[`mdct_butterflies`] の `mdct_butterfly_first` と `mdct_butterfly_generic`）が組ごとに読む `T` の写し。段 `i`
/// （0 が first）は `trigint = 4 << i` で、`n/2 >> i` 個の点を 16 個ずつの組 `k` に分け（`2^i` 個の区切りで同じ `T`）、組の
/// 中の 4 組（C の順で `j` = 0〜3、組の中の位置 `a = 6 − 2j`）は `T[(4k + j)·trigint]`・`T[(4k + j)·trigint + 1]` を読む。
/// 写しは組ごとに `[T0(a=0), T0(a=2), T0(a=4), T0(a=6), T1(a=0), T1(a=2), T1(a=4), T1(a=6)]`（値はそのまま）。
fn butterfly_trig(t: &[f32], n2: usize, log2n: i32) -> Vec<Vec<[f32; 8]>> {
    // C：stages = log2n − 5 から、first は --stages > 0 のとき、generic は i = 1 から --stages > 0 の間
    let count = (log2n - 6).max(0) as usize;
    (0..count)
        .map(|i| {
            let trigint = 4usize << i;
            (0..(n2 >> i) / 16)
                .map(|k| {
                    let at = |j: usize, o: usize| {
                        t.get((4 * k + j) * trigint + o).copied().unwrap_or(0.0)
                    };
                    [
                        at(3, 0),
                        at(2, 0),
                        at(1, 0),
                        at(0, 0),
                        at(3, 1),
                        at(2, 1),
                        at(1, 1),
                        at(0, 1),
                    ]
                })
                .collect()
        })
        .collect()
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
        && init.bfly.len() == (init.log2n - 6).max(0) as usize
        && init
            .bfly
            .iter()
            .enumerate()
            .all(|(i, b)| b.len() == ((nu / 2) >> i) / 16)
}

/// `mdct_butterfly_8`（mdct.c:407–454。#else 432–452）。
#[inline(always)]
fn mdct_butterfly_8(x: &mut [f32; 8]) {
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

/// `mdct_butterfly_16`（mdct.c:457–528。#else 495–523）。
#[inline(always)]
fn mdct_butterfly_16(x: &mut [f32; 16]) {
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

    let [a, b] = x.as_chunks_mut::<8>().0 else {
        return;
    };
    mdct_butterfly_8(a);
    mdct_butterfly_8(b);
}

/// `mdct_butterfly_32`（mdct.c:531–664。#else 602–658）。
#[inline(always)]
fn mdct_butterfly_32(x: &mut [f32; 32]) {
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

    let [a, b] = x.as_chunks_mut::<16>().0 else {
        return;
    };
    mdct_butterfly_16(a);
    mdct_butterfly_16(b);
}

/// `mdct_butterfly_first`・`generic` の 1 回分（C の `x1`・`x2` の 8 個ずつ）：組の中の 4 組（位置 `a` = 6・4・2・0）を、
/// それぞれ `T[0]`・`T[1]`（`t` は [`butterfly_trig`] の形で、`a = 2l` の組が `t[l]`・`t[4 + l]`）で回す。C の 1 組は
/// `r0 = x1[a] − x2[a]; r1 = x1[a+1] − x2[a+1]; x1[a] += x2[a]; x1[a+1] += x2[a+1];
/// x2[a] = r1·T[1] + r0·T[0]; x2[a+1] = r1·T[0] − r0·T[1]`。組どうしは別の要素を読み書きするので、差と和を先に 8 個まとめて
/// 求めても、どの要素の式と順も C と同じ（偶数番の差を `e`、奇数番の差を `o` に並べ、4 組を同時に求める）。
#[inline(always)]
fn butterfly_8x2(x1: &mut [f32; 8], x2: &mut [f32; 8], t: &[f32; 8]) {
    let mut e = [0.0_f32; 4];
    let mut o = [0.0_f32; 4];
    for l in 0..4 {
        e[l] = x1[2 * l] - x2[2 * l];
        o[l] = x1[2 * l + 1] - x2[2 * l + 1];
    }
    for i in 0..8 {
        x1[i] += x2[i];
    }
    for l in 0..4 {
        x2[2 * l] = o[l] * t[4 + l] + e[l] * t[l];
        x2[2 * l + 1] = o[l] * t[l] - e[l] * t[4 + l];
    }
}

/// `mdct_butterfly_first`（mdct.c:801–896。#else 854–894）と `mdct_butterfly_generic`（mdct.c:899–1080。#else 1032–1078）の
/// 1 つの区切り（`x` は `points` 個）。C の `x1` は後ろ半分を、`x2` は前半分を、どちらも終わりから 8 個ずつ下り、組 `k`
/// ごとに `t[k]`（[`butterfly_trig`]）を使う。
fn mdct_butterfly_stage(x: &mut [f32], t: &[[f32; 8]]) {
    let points = x.len();
    let (lo, hi) = x.split_at_mut(points >> 1);
    let (lo, hi) = (lo.as_chunks_mut::<8>().0, hi.as_chunks_mut::<8>().0);
    for ((x1, x2), t) in hi.iter_mut().rev().zip(lo.iter_mut().rev()).zip(t) {
        butterfly_8x2(x1, x2, t);
    }
}

/// `mdct_butterflies`（mdct.c:1105–1135）。段 `i` の区切りは `points >> i` 個ずつ（`2^i` 個）。
fn mdct_butterflies(init: &MdctLookup, x: &mut [f32], points: i32) {
    let points = points as usize;
    let Some(x) = x.get_mut(..points) else {
        return;
    };
    for (i, t) in init.bfly.iter().enumerate() {
        for part in x.chunks_exact_mut(points >> i) {
            mdct_butterfly_stage(part, t);
        }
    }
    for c in x.as_chunks_mut::<32>().0 {
        mdct_butterfly_32(c);
    }
}

/// `mdct_bitreverse`（mdct.c:1155–1274。#else 1228–1272）。`x` は作業領域 `w` の全体（n 個）：後ろ半分を読み、前半分に書く
/// （C の `w0` は前から、`w1` は前半分の終わりから 4 個ずつ進み、出会うまで `n/16` 回。読む位置は `bitrev`）。
fn mdct_bitreverse(init: &MdctLookup, x: &mut [f32]) {
    let n = init.n as usize;
    let (w, src) = x[..n].split_at_mut(n >> 1);
    let (w0, w1) = w.split_at_mut(n >> 2);
    let bit = init.bitrev.as_chunks::<4>().0;
    let t = init.trig[n..].as_chunks::<4>().0;
    let at = |i: i32| -> [f32; 2] {
        let i = i as usize;
        [src[i], src[i + 1]]
    };
    for (((w0, w1), b), t) in w0
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(w1.as_chunks_mut::<4>().0.iter_mut().rev())
        .zip(bit)
        .zip(t)
    {
        let (a0, a1) = (at(b[0]), at(b[1]));
        let r0 = a0[1] - a1[1];
        let r1 = a0[0] + a1[0];
        let r2 = r1 * t[0] + r0 * t[1];
        let r3 = r1 * t[1] - r0 * t[0];

        let r0 = (a0[1] + a1[1]) * 0.5_f32;
        let r1 = (a0[0] - a1[0]) * 0.5_f32;

        w0[0] = r0 + r2;
        w1[2] = r0 - r2;
        w0[1] = r1 + r3;
        w1[3] = r3 - r1;

        let (a0, a1) = (at(b[2]), at(b[3]));
        let r0 = a0[1] - a1[1];
        let r1 = a0[0] + a1[0];
        let r2 = r1 * t[2] + r0 * t[3];
        let r3 = r1 * t[3] - r0 * t[2];

        let r0 = (a0[1] + a1[1]) * 0.5_f32;
        let r1 = (a0[0] - a1[0]) * 0.5_f32;

        w0[2] = r0 + r2;
        w1[0] = r0 - r2;
        w0[3] = r1 + r3;
        w1[1] = r3 - r1;
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

/// `mdct_backward`（mdct.c:1276–1280・1537–1627 の SSE でない経路）。入力（前半 `n/2` 個）と出力（`n` 個）は同じ配列
/// （C の呼び手 `mapping0_inverse` の `mdct_backward(b->transform[vb->W][0],pcm,pcm)`）。長さが足りないときは何もしない。
///
/// C の 6 つのループは、どれも読む範囲と書く範囲が重ならない（下の各ループの注）ので、範囲を分けて借り、C と同じ式で
/// 同じ要素を求める（読む値はどれも、そのループより前に書いたもの）。
pub(crate) fn mdct_backward(init: &MdctLookup, x: &mut [f32]) {
    if !lookup_is_valid(init) {
        return;
    }
    let n = init.n as usize;
    let n2 = n >> 1;
    let n4 = n >> 2;
    let Some(x) = x.get_mut(..n) else {
        return;
    };
    let t = &init.trig;

    // rotate：C の 2 つのループは、どちらも前半を 8 個の組で終わりから読み（1 つ目は iX = n2−7−8k の奇数番、2 つ目は
    // iX = n2−8−8k の偶数番。同じ k で同じ組）、後半に書く（1 つ目は [n2, n2+n4) を終わりから、2 つ目は [n2+n4, n) を頭から。
    // T は 1 つ目が n4 から上へ、2 つ目が n4 から下へ）。前半は書かないので、1 回のループにまとめても値は同じ
    {
        let (lo, hi) = x.split_at_mut(n2);
        let (out1, out2) = hi.split_at_mut(n4);
        let (t2, t1) = t[..n2].split_at(n4);
        for (((c, o1), t1), (o2, t2)) in lo
            .as_chunks::<8>()
            .0
            .iter()
            .rev()
            .zip(out1.as_chunks_mut::<4>().0.iter_mut().rev())
            .zip(t1.as_chunks::<4>().0)
            .zip(
                out2.as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(t2.as_chunks::<4>().0.iter().rev()),
            )
        {
            // iX = 組の頭 + 1：C の 4 つの式 `oX[0] = -iX[2]·T[3] − iX[0]·T[2]`・`oX[1] = iX[0]·T[3] − iX[2]·T[2]`・
            // `oX[2] = -iX[6]·T[1] − iX[4]·T[0]`・`oX[3] = iX[4]·T[1] − iX[6]·T[0]` を、どれも `p·q − r·s` の形に並べる
            let p = [-c[3], c[1], -c[7], c[5]];
            let q = [t1[3], t1[3], t1[1], t1[1]];
            let r = [c[1], c[3], c[5], c[7]];
            let s = [t1[2], t1[2], t1[0], t1[0]];
            for l in 0..4 {
                o1[l] = p[l] * q[l] - r[l] * s[l];
            }
            // iX = 組の頭：`oX[0] = iX[4]·T[3] + iX[6]·T[2]`・`oX[1] = iX[4]·T[2] − iX[6]·T[3]`・
            // `oX[2] = iX[0]·T[1] + iX[2]·T[0]`・`oX[3] = iX[0]·T[0] − iX[2]·T[1]`（左の積と右の積を先に求め、偶数番は和、
            // 奇数番は差）
            let m1 = [c[4] * t2[3], c[4] * t2[2], c[0] * t2[1], c[0] * t2[0]];
            let m2 = [c[6] * t2[2], c[6] * t2[3], c[2] * t2[0], c[2] * t2[1]];
            *o2 = [m1[0] + m2[0], m1[1] - m2[1], m1[2] + m2[2], m1[3] - m2[3]];
        }
    }

    mdct_butterflies(init, &mut x[n2..n], n2 as i32);
    mdct_bitreverse(init, x);

    let (lo, hi) = x.split_at_mut(n2);
    let (ha, hb) = hi.split_at_mut(n4);

    // roatate + window：前半を 8 個ずつ頭から読み（T は n2 から 8 個ずつ）、oX1 は [n2, n2+n4) を終わりから、oX2 は
    // [n2+n4, n) を頭から 4 個ずつ書く（iX と oX1 が出会うまで n/16 回）
    for (((s, t), o1), o2) in lo
        .as_chunks::<8>()
        .0
        .iter()
        .zip(t[n2..n].as_chunks::<8>().0)
        .zip(ha.as_chunks_mut::<4>().0.iter_mut().rev())
        .zip(hb.as_chunks_mut::<4>().0)
    {
        // C の `oX1[3−l] = iX[2l]·T[2l+1] − iX[2l+1]·T[2l]`・`oX2[l] = -(iX[2l]·T[2l] + iX[2l+1]·T[2l+1])`（l = 0〜3）
        let mut a = [0.0_f32; 4];
        let mut b = [0.0_f32; 4];
        for l in 0..4 {
            a[l] = s[2 * l] * t[2 * l + 1] - s[2 * l + 1] * t[2 * l];
            b[l] = -(s[2 * l] * t[2 * l] + s[2 * l + 1] * t[2 * l + 1]);
        }
        *o1 = [a[3], a[2], a[1], a[0]];
        *o2 = b;
    }

    // iX = [n2, n2+n4) を終わりから読み、oX1 = [0, n4) に同じ位置で写し（`oX1[j] = iX[j]`）、oX2 = [n4, n2) に頭から
    // 符号を変えて逆の順に書く（`oX2[0] = -oX1[3]` …。`oX2[m] = -iX[n4−1−m]`）
    let (l1, l2) = lo.split_at_mut(n4);
    l1.copy_from_slice(ha);
    for (o, s) in l2
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(ha.as_chunks::<4>().0.iter().rev())
    {
        *o = [-s[3], -s[2], -s[1], -s[0]];
    }

    // iX = [n2+n4, n) を頭から読み、oX1 = [n2, n2+n4) に終わりから書く（`oX1[0] = iX[3]` …。逆の順の写し）
    for (o, s) in ha
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .rev()
        .zip(hb.as_chunks::<4>().0)
    {
        *o = [s[3], s[2], s[1], s[0]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 書き直す前の形（C のポインタをそのまま同じ配列の添字にし、`do { … } while` を数えたループにしたもの。M3・M16 段 2 の
    /// 照合を通った実装）。速い形が同じビットを出すことを [`fast_forms_match_the_index_forms`] が照らす。
    mod index_form {
        use super::super::{C_PI1_8, C_PI2_8, C_PI3_8, MdctLookup, lookup_is_valid};

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

        pub(super) fn mdct_forward_with(
            init: &MdctLookup,
            input: &[f32],
            out: &mut [f32],
            w: &mut [f32],
        ) {
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

        /// `mdct_backward`（mdct.c:1276–1280・1537–1627 の SSE でない経路）。入力（前半 `n/2` 個）と出力（`n` 個）は同じ配列
        /// （C の呼び手 `mapping0_inverse` の `mdct_backward(b->transform[vb->W][0],pcm,pcm)`）。長さが足りないときは何もしない。
        pub(super) fn mdct_backward(init: &MdctLookup, x: &mut [f32]) {
            if !lookup_is_valid(init) {
                return;
            }
            let n = init.n as usize;
            let n2 = n >> 1;
            let n4 = n >> 2;
            if x.len() < n {
                return;
            }
            let t = &init.trig;

            // rotate
            let mut ix = n2 as isize - 7;
            let mut ox = n2 + n4;
            let mut ti = n4;
            loop {
                ox -= 4;
                let i = ix as usize;
                x[ox] = -x[i + 2] * t[ti + 3] - x[i] * t[ti + 2];
                x[ox + 1] = x[i] * t[ti + 3] - x[i + 2] * t[ti + 2];
                x[ox + 2] = -x[i + 6] * t[ti + 1] - x[i + 4] * t[ti];
                x[ox + 3] = x[i + 4] * t[ti + 1] - x[i + 6] * t[ti];
                ix -= 8;
                ti += 4;
                if ix < 0 {
                    break;
                }
            }
            let mut ix = n2 as isize - 8;
            let mut ox = n2 + n4;
            let mut ti = n4;
            loop {
                ti -= 4;
                let i = ix as usize;
                x[ox] = x[i + 4] * t[ti + 3] + x[i + 6] * t[ti + 2];
                x[ox + 1] = x[i + 4] * t[ti + 2] - x[i + 6] * t[ti + 3];
                x[ox + 2] = x[i] * t[ti + 1] + x[i + 2] * t[ti];
                x[ox + 3] = x[i] * t[ti] - x[i + 2] * t[ti + 1];
                ix -= 8;
                ox += 4;
                if ix < 0 {
                    break;
                }
            }

            mdct_butterflies(init, &mut x[n2..n], n2 as i32);
            mdct_bitreverse(init, x);

            // roatate + window
            let mut ox1 = n2 + n4;
            let mut ox2 = n2 + n4;
            let mut ix = 0usize;
            let mut ti = n2;
            loop {
                ox1 -= 4;
                x[ox1 + 3] = x[ix] * t[ti + 1] - x[ix + 1] * t[ti];
                x[ox2] = -(x[ix] * t[ti] + x[ix + 1] * t[ti + 1]);
                x[ox1 + 2] = x[ix + 2] * t[ti + 3] - x[ix + 3] * t[ti + 2];
                x[ox2 + 1] = -(x[ix + 2] * t[ti + 2] + x[ix + 3] * t[ti + 3]);
                x[ox1 + 1] = x[ix + 4] * t[ti + 5] - x[ix + 5] * t[ti + 4];
                x[ox2 + 2] = -(x[ix + 4] * t[ti + 4] + x[ix + 5] * t[ti + 5]);
                x[ox1] = x[ix + 6] * t[ti + 7] - x[ix + 7] * t[ti + 6];
                x[ox2 + 3] = -(x[ix + 6] * t[ti + 6] + x[ix + 7] * t[ti + 7]);
                ox2 += 4;
                ix += 8;
                ti += 8;
                if ix >= ox1 {
                    break;
                }
            }

            let mut ix = n2 + n4;
            let mut ox1 = n4;
            let mut ox2 = ox1;
            loop {
                ox1 -= 4;
                ix -= 4;
                x[ox1 + 3] = x[ix + 3];
                x[ox2] = -x[ox1 + 3];
                x[ox1 + 2] = x[ix + 2];
                x[ox2 + 1] = -x[ox1 + 2];
                x[ox1 + 1] = x[ix + 1];
                x[ox2 + 2] = -x[ox1 + 1];
                x[ox1] = x[ix];
                x[ox2 + 3] = -x[ox1];
                ox2 += 4;
                if ox2 >= ix {
                    break;
                }
            }

            let mut ix = n2 + n4;
            let mut ox1 = n2 + n4;
            let ox2 = n2;
            loop {
                ox1 -= 4;
                x[ox1] = x[ix + 3];
                x[ox1 + 1] = x[ix + 2];
                x[ox1 + 2] = x[ix + 1];
                x[ox1 + 3] = x[ix];
                ix += 4;
                if ox1 <= ox2 {
                    break;
                }
            }
        }
    }

    /// 速い形（範囲を分けて借りる形・蝶の段の表）が、書き直す前の添字の形と同じビットを出す（逆と順の MDCT、n = 64〜8192、
    /// ChaCha8 の値。大きさの違う値・0・符号の違う 0 を混ぜる）。
    #[test]
    fn fast_forms_match_the_index_forms() {
        use rand_chacha::rand_core::Rng;
        let mut rng = crate::determinism::rng(43);
        let value = |rng: &mut rand_chacha::ChaCha8Rng| match rng.next_u32() % 16 {
            0 => 0.0_f32,
            1 => -0.0,
            2 => f32::from_bits(rng.next_u32() & 0x807f_ffff),
            _ => f32::from_bits((rng.next_u32() & 0x83ff_ffff) | 0x3c00_0000),
        };
        for log2n in 6..=13 {
            let n = 1usize << log2n;
            let l = mdct_init(n as i32);
            for _ in 0..6 {
                let x: Vec<f32> = (0..n).map(|_| value(&mut rng)).collect();
                let (mut a, mut b) = (x.clone(), x.clone());
                mdct_backward(&l, &mut a);
                index_form::mdct_backward(&l, &mut b);
                let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
                assert_eq!(bits(&a), bits(&b), "逆 n={n}");
                let (mut oa, mut ob) = (vec![0.0_f32; n / 2], vec![0.0_f32; n / 2]);
                let (mut wa, mut wb) = (vec![0.0_f32; n], vec![0.0_f32; n]);
                mdct_forward_with(&l, &x, &mut oa, &mut wa);
                index_form::mdct_forward_with(&l, &x, &mut ob, &mut wb);
                assert_eq!(bits(&oa), bits(&ob), "順 n={n}");
            }
        }
    }

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
