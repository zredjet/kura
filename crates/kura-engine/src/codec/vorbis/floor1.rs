//! floor1 の look・当てはめ・符号化（`lib/floor1.c`）。
//!
//! # 移す C
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `floor1_look` | 183–258 | ポストの数、位置の並べ替え（209 の `qsort`。位置は重ならないので安定な並べ替えで同じ結果：mod.rs の規則 8）、`forward_index`・`reverse_index`・`sorted_index`、`quant_q`（mult 1〜4 → 256・128・86・64）、デコードの近傍 `loneighbor`・`hineighbor` |
//! | `render_point` | 260–274 | `y & 0x7fff`、整数の割り算（0 へ切り捨て） |
//! | `vorbis_dBquant` | 285–299（#else 293–297） | `int i = *x*7.3142857f+1023.5f`（float の積和 → int の切り捨て）を 0〜1023 に |
//! | `render_line0` | 397–424 | 整数の Bresenham（`base = dy/adx`、`err`）。`n > x1` なら `n = x1` |
//! | `accumulate_fit` | 427–475 | 各点 `vorbis_dBquant(flr+i)` が 0 でなければ、`mdct[i]+twofitatten >= flr[i]` で a・b に分けて x・y・x²・y²・xy の和（int） |
//! | `fit_line` | 477–535 | 重み `(bn+an)*twofitweight/(an+1)+1.`（float の積・割り算のあとに double の 1. を足す）、和は double、端の拘束点、`denom > 0` なら `*y0 = rint(a+b*x0)`（double → int）を 0〜1023 に |
//! | `inspect_error` | 537–586 | `maxover`・`maxunder`・`maxerr`（float）と int の比較（`y+maxover<val` は float）。`mse/n > maxerr` は int を float に |
//! | `post_Y` | 588–595 | |
//! | `floor1_fit` | 597–750 | 1 本の直線から、`postlist` の順に分割して当て直す（貪欲）。`ly == -1 \|\| hy == -1` の `exit(1)`（666）は `Err(Internal)`。fit が 1 つもなければ（nonzero == 0）出力なし（C の NULL → `None`） |
//! | `floor1_encode` | 774–974 | `post` を mult で 1024 段 → quant_q 段に、予測との差を折り返して `out`、`1`（使う）・Y0・Y1（`ov_ilog(quant_q-1)` ビット）、partition ごとに class の book で cval・subclass の book で値（`vorbis_book_encode`）。最後に `render_line0` で整数の `ilogmask`（n/2 個。`ly` を末尾まで延ばす）。`post` が NULL（`None`）なら `0` を書いて `ilogmask` を 0 に |
//!
//! - `FLOOR1_fromdB_LOOKUP`（301–366）は `vorbis_tables::FLOOR1_FROMDB_LOOKUP`（エンコーダは psy.rs の
//!   `_vp_couple_quantize_normalize` で使う）。
//! - `look->phrasebits`・`postbits`・`frames`（統計）は出力に効かないので持たない。
//! - `TRAIN_FLOOR1` の枝は移さない。
//!
//! # 移さないもの
//!
//! `floor1_interpolate_fit`（752–771。ビットレートの管理）、`render_line`（368–395。デコーダ）、`floor1_pack`（info.rs）、
//! `floor1_unpack`・`floor1_inverse1`・`floor1_inverse2`（デコーダ）、`floor1_free_*`。
//!
//! # 型の対応
//!
//! - `vorbis_look_floor1`（codec_internal.h:147–163）→ [`VorbisLookFloor1`]（`vi` は `VorbisInfoFloor1` の写し）。
//! - `lsfit_acc`（floor1.c:35–52）→ [`LsfitAcc`]（int）。
//! - `floor1_fit` の戻り（`int *`、posts 個、`0x8000` の旗つき）→ `Option<Vec<i32>>`。
//!
//! # 移植の注記
//!
//! - `floor1_look` は C が通す設定（partitions 0〜31、class 0〜15、dim 0〜8、subs 0〜3、mult 1〜4、ポストの数 ≤ 65、
//!   位置が重ならない）だけを受け付け、ほかは `Err`（C は調べない。重なる位置は 0 で割る）。そのため、あとの関数の
//!   `/ adx` は 0 で割らない（それでも 0 なら panic せずに 0 として扱う）。
//! - int の和・積のあふれ（C は未定義）は 2 の補数で折り返す（`accumulate_fit` の `xa += i` は long の和を int に戻す）。
//! - 配列の長さ（`logmdct`・`logmask` は `look.n` 以上、`post` は posts 以上、`ilogmask` は pcmend/2 以上）は入口で確かめ、
//!   足りなければ `Err`。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::{VIF_CLASS, VIF_PARTS, VIF_POSIT, VorbisInfoFloor1, ov_ilog};

use super::bitpack::OggpackBuffer;
use super::codebook::{Codebook, vorbis_book_encode};
use crate::error::EngineError;

/// `vorbis_look_floor1`（codec_internal.h:147–163）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VorbisLookFloor1 {
    pub sorted_index: [i32; VIF_POSIT + 2],
    pub forward_index: [i32; VIF_POSIT + 2],
    pub reverse_index: [i32; VIF_POSIT + 2],
    pub hineighbor: [i32; VIF_POSIT],
    pub loneighbor: [i32; VIF_POSIT],
    pub posts: i32,
    pub n: i32,
    pub quant_q: i32,
    pub vi: VorbisInfoFloor1,
}

/// `lsfit_acc`（floor1.c:35–52）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LsfitAcc {
    pub x0: i32,
    pub x1: i32,
    pub xa: i32,
    pub ya: i32,
    pub x2a: i32,
    pub y2a: i32,
    pub xya: i32,
    pub an: i32,
    pub xb: i32,
    pub yb: i32,
    pub x2b: i32,
    pub y2b: i32,
    pub xyb: i32,
    pub bn: i32,
}

fn bad_setup(why: &str) -> EngineError {
    EngineError::Internal(format!("floor1 の設定が正しくない：{why}"))
}

/// `floor1_look`（floor1.c:183–258）。
pub(crate) fn floor1_look(info: &VorbisInfoFloor1) -> Result<VorbisLookFloor1, EngineError> {
    let mut look = VorbisLookFloor1 {
        sorted_index: [0; VIF_POSIT + 2],
        forward_index: [0; VIF_POSIT + 2],
        reverse_index: [0; VIF_POSIT + 2],
        hineighbor: [0; VIF_POSIT],
        loneighbor: [0; VIF_POSIT],
        posts: 0,
        n: info.postlist[1],
        quant_q: 0,
        vi: *info,
    };
    if look.n <= 0 {
        return Err(bad_setup("postlist[1] が正でない"));
    }

    // we drop each position value in-between already decoded values, and use linear interpolation to
    // predict each new value past the edges.
    let partitions = usize::try_from(info.partitions)
        .ok()
        .filter(|&p| p <= VIF_PARTS)
        .ok_or_else(|| bad_setup("partitions が範囲の外"))?;
    let mut n = 0usize;
    for &class in &info.partitionclass[..partitions] {
        let class = usize::try_from(class)
            .ok()
            .filter(|&c| c < VIF_CLASS)
            .ok_or_else(|| bad_setup("partitionclass が範囲の外"))?;
        let dim = info.class_dim[class];
        if !(0..=8).contains(&dim) || !(0..=3).contains(&info.class_subs[class]) {
            return Err(bad_setup("class_dim か class_subs が範囲の外"));
        }
        n += dim as usize;
    }
    n += 2;
    if n > VIF_POSIT + 2 {
        return Err(bad_setup("ポストが多すぎる"));
    }
    look.posts = n as i32;

    // also store a sorted position index（C の qsort。位置は重ならないので安定な並べ替えで同じ順）
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| info.postlist[i]);
    if order
        .windows(2)
        .any(|w| info.postlist[w[0]] == info.postlist[w[1]])
    {
        return Err(bad_setup("ポストの位置が重なる"));
    }

    // points from sort order back to range number
    for (i, &o) in order.iter().enumerate() {
        look.forward_index[i] = o as i32;
    }
    // points from range order to sorted position
    for (i, &o) in order.iter().enumerate() {
        look.reverse_index[o] = i as i32;
    }
    // we actually need the post values too
    for (i, &o) in order.iter().enumerate() {
        look.sorted_index[i] = info.postlist[o];
    }

    // quantize values to multiplier spec
    look.quant_q = match info.mult {
        1 => 256, // 1024 -> 256
        2 => 128, // 1024 -> 128
        3 => 86,  // 1024 -> 86
        4 => 64,  // 1024 -> 64
        _ => return Err(bad_setup("mult が 1〜4 でない")),
    };

    // discover our neighbors for decode where we don't use fit flags (that would push the neighbors outward)
    for i in 0..n - 2 {
        let mut lo = 0;
        let mut hi = 1;
        let mut lx = 0;
        let mut hx = look.n;
        let currentx = info.postlist[i + 2];
        for (j, &x) in info.postlist[..i + 2].iter().enumerate() {
            if x > lx && x < currentx {
                lo = j;
                lx = x;
            }
            if x < hx && x > currentx {
                hi = j;
                hx = x;
            }
        }
        look.loneighbor[i] = lo as i32;
        look.hineighbor[i] = hi as i32;
    }

    Ok(look)
}

/// `render_point`（floor1.c:260–274）。
fn render_point(x0: i32, x1: i32, y0: i32, y1: i32, x: i32) -> i32 {
    let y0 = y0 & 0x7fff; // mask off flag
    let y1 = y1 & 0x7fff;

    let dy = y1.wrapping_sub(y0);
    let adx = x1.wrapping_sub(x0);
    let ady = dy.wrapping_abs();
    let err = ady.wrapping_mul(x.wrapping_sub(x0));

    // adx は 0 にならない（floor1_look が重なる位置を断る）。0 なら 0 とする
    let off = err.checked_div(adx).unwrap_or(0);
    if dy < 0 {
        return y0.wrapping_sub(off);
    }
    y0.wrapping_add(off)
}

/// `vorbis_dBquant`（floor1.c:285–299。#else 293–297）。
fn vorbis_db_quant(x: f32) -> i32 {
    // float の積と和（FMA にしない）を 0 へ切り捨て（NaN は 0、範囲の外は飽和。照合用の arm64 と同じ）
    let i = (x * 7.314_285_7_f32 + 1023.5_f32) as i32;
    if i > 1023 {
        return 1023;
    }
    if i < 0 {
        return 0;
    }
    i
}

/// `render_line0`（floor1.c:397–424）。
fn render_line0(n: i32, x0: i32, x1: i32, y0: i32, y1: i32, d: &mut [i32]) {
    let dy = y1.wrapping_sub(y0);
    let adx = x1.wrapping_sub(x0);
    let mut ady = dy.wrapping_abs();
    let base = dy.checked_div(adx).unwrap_or(0);
    let sy = if dy < 0 {
        base.wrapping_sub(1)
    } else {
        base.wrapping_add(1)
    };
    let mut x = x0;
    let mut y = y0;
    let mut err: i32 = 0;

    ady = ady.wrapping_sub(base.wrapping_mul(adx).wrapping_abs());

    let n = if n > x1 { x1 } else { n };

    let mut put = |x: i32, y: i32| {
        if let Some(slot) = usize::try_from(x).ok().and_then(|x| d.get_mut(x)) {
            *slot = y;
        }
    };
    if x < n {
        put(x, y);
    }
    // C の `while(++x<n)`
    while let Some(next) = x.checked_add(1).filter(|&v| v < n) {
        x = next;
        err = err.wrapping_add(ady);
        if err >= adx {
            err = err.wrapping_sub(adx);
            y = y.wrapping_add(sy);
        } else {
            y = y.wrapping_add(base);
        }
        put(x, y);
    }
}

/// `accumulate_fit`（floor1.c:427–475）：a の点の数を返す。`flr`・`mdct` は `n` 個以上（呼ぶ側が確かめる）。
fn accumulate_fit(
    flr: &[f32],
    mdct: &[f32],
    x0: i32,
    x1: i32,
    a: &mut LsfitAcc,
    n: i32,
    info: &VorbisInfoFloor1,
) -> i32 {
    let (mut xa, mut ya, mut x2a, mut y2a, mut xya, mut na) = (0i32, 0i32, 0i32, 0i32, 0i32, 0i32);
    let (mut xb, mut yb, mut x2b, mut y2b, mut xyb, mut nb) = (0i32, 0i32, 0i32, 0i32, 0i32, 0i32);

    *a = LsfitAcc {
        x0,
        x1,
        ..LsfitAcc::default()
    };
    let x1 = if x1 >= n { n - 1 } else { x1 };

    for i in i64::from(x0)..=i64::from(x1) {
        let (Some(&f), Some(&m)) = (
            usize::try_from(i).ok().and_then(|i| flr.get(i)),
            usize::try_from(i).ok().and_then(|i| mdct.get(i)),
        ) else {
            continue;
        };
        let quantized = vorbis_db_quant(f);
        if quantized != 0 {
            let q = i64::from(quantized);
            // C は long の i との和・積を int に戻す（あふれは折り返し）
            if m + info.twofitatten >= f {
                xa = (i64::from(xa) + i) as i32;
                ya = ya.wrapping_add(quantized);
                x2a = (i64::from(x2a) + i * i) as i32;
                y2a = y2a.wrapping_add(quantized.wrapping_mul(quantized));
                xya = (i64::from(xya) + i * q) as i32;
                na += 1;
            } else {
                xb = (i64::from(xb) + i) as i32;
                yb = yb.wrapping_add(quantized);
                x2b = (i64::from(x2b) + i * i) as i32;
                y2b = y2b.wrapping_add(quantized.wrapping_mul(quantized));
                xyb = (i64::from(xyb) + i * q) as i32;
                nb += 1;
            }
        }
    }

    a.xa = xa;
    a.ya = ya;
    a.x2a = x2a;
    a.y2a = y2a;
    a.xya = xya;
    a.an = na;

    a.xb = xb;
    a.yb = yb;
    a.x2b = x2b;
    a.y2b = y2b;
    a.xyb = xyb;
    a.bn = nb;

    na
}

/// `fit_line`（floor1.c:477–535）：`y0`・`y1` は入出力（負なら拘束しない）。当てはめられなければ 1。
fn fit_line(a: &[LsfitAcc], y0: &mut i32, y1: &mut i32, info: &VorbisInfoFloor1) -> i32 {
    let (Some(first), Some(last)) = (a.first(), a.last()) else {
        // C は fits ≥ 1 でしか呼ばない
        *y0 = 0;
        *y1 = 0;
        return 1;
    };
    let (mut xb, mut yb, mut x2b, mut y2b, mut xyb, mut bn) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    let x0 = first.x0;
    let x1 = last.x1;

    for ai in a {
        // (int * float) / int は float、そのあとに double の 1. を足す
        let weight = f64::from(
            ai.bn.wrapping_add(ai.an) as f32 * info.twofitweight / ai.an.wrapping_add(1) as f32,
        ) + 1.;

        xb += f64::from(ai.xb) + f64::from(ai.xa) * weight;
        yb += f64::from(ai.yb) + f64::from(ai.ya) * weight;
        x2b += f64::from(ai.x2b) + f64::from(ai.x2a) * weight;
        y2b += f64::from(ai.y2b) + f64::from(ai.y2a) * weight;
        xyb += f64::from(ai.xyb) + f64::from(ai.xya) * weight;
        bn += f64::from(ai.bn) + f64::from(ai.an) * weight;
    }

    if *y0 >= 0 {
        xb += f64::from(x0);
        yb += f64::from(*y0);
        x2b += f64::from(x0.wrapping_mul(x0));
        y2b += f64::from(y0.wrapping_mul(*y0));
        xyb += f64::from(y0.wrapping_mul(x0));
        bn += 1.;
    }

    if *y1 >= 0 {
        xb += f64::from(x1);
        yb += f64::from(*y1);
        x2b += f64::from(x1.wrapping_mul(x1));
        y2b += f64::from(y1.wrapping_mul(*y1));
        xyb += f64::from(y1.wrapping_mul(x1));
        bn += 1.;
    }

    // C も y2b を当てはめに使わない（和だけ取る）
    let _ = y2b;
    let denom = bn * x2b - xb * xb;

    if denom > 0. {
        let a = (yb * x2b - xyb * xb) / denom;
        let b = (bn * xyb - xb * yb) / denom;
        // rint（既定の丸めの向き）の double を int に（NaN は 0、範囲の外は飽和。照合用の arm64 と同じ）
        *y0 = (a + b * f64::from(x0)).round_ties_even() as i32;
        *y1 = (a + b * f64::from(x1)).round_ties_even() as i32;

        // limit to our range!
        if *y0 > 1023 {
            *y0 = 1023;
        }
        if *y1 > 1023 {
            *y1 = 1023;
        }
        if *y0 < 0 {
            *y0 = 0;
        }
        if *y1 < 0 {
            *y1 = 0;
        }
        0
    } else {
        *y0 = 0;
        *y1 = 0;
        1
    }
}

/// `inspect_error`（floor1.c:537–586）：誤差が閾値を超えれば 1。`mask`・`mdct` は `x1` 個以上（呼ぶ側が確かめる）。
fn inspect_error(
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    mask: &[f32],
    mdct: &[f32],
    info: &VorbisInfoFloor1,
) -> i32 {
    let at = |v: &[f32], x: i32| {
        usize::try_from(x)
            .ok()
            .and_then(|x| v.get(x))
            .copied()
            .unwrap_or(0.0)
    };
    let dy = y1.wrapping_sub(y0);
    let adx = x1.wrapping_sub(x0);
    let mut ady = dy.wrapping_abs();
    let base = dy.checked_div(adx).unwrap_or(0);
    let sy = if dy < 0 {
        base.wrapping_sub(1)
    } else {
        base.wrapping_add(1)
    };
    let mut x = x0;
    let mut y = y0;
    let mut err: i32 = 0;
    let mut val = vorbis_db_quant(at(mask, x));
    let mut n: i32 = 0;

    ady = ady.wrapping_sub(base.wrapping_mul(adx).wrapping_abs());

    let mut mse = y.wrapping_sub(val);
    mse = mse.wrapping_mul(mse);
    n += 1;
    if at(mdct, x) + info.twofitatten >= at(mask, x) {
        // int + float は float
        if y as f32 + info.maxover < val as f32 {
            return 1;
        }
        if y as f32 - info.maxunder > val as f32 {
            return 1;
        }
    }

    // C の `while(++x<x1)`
    while let Some(next) = x.checked_add(1).filter(|&v| v < x1) {
        x = next;
        err = err.wrapping_add(ady);
        if err >= adx {
            err = err.wrapping_sub(adx);
            y = y.wrapping_add(sy);
        } else {
            y = y.wrapping_add(base);
        }

        val = vorbis_db_quant(at(mask, x));
        let d = y.wrapping_sub(val);
        mse = mse.wrapping_add(d.wrapping_mul(d));
        n += 1;
        if at(mdct, x) + info.twofitatten >= at(mask, x) && val != 0 {
            if y as f32 + info.maxover < val as f32 {
                return 1;
            }
            if y as f32 - info.maxunder > val as f32 {
                return 1;
            }
        }
    }

    if info.maxover * info.maxover / n as f32 > info.maxerr {
        return 0;
    }
    if info.maxunder * info.maxunder / n as f32 > info.maxerr {
        return 0;
    }
    // int / int（0 へ切り捨て）を float にして比べる
    if mse.wrapping_div(n) as f32 > info.maxerr {
        return 1;
    }
    0
}

/// `post_Y`（floor1.c:588–595）。
fn post_y(a: &[i32], b: &[i32], pos: usize) -> i32 {
    let (Some(&ap), Some(&bp)) = (a.get(pos), b.get(pos)) else {
        return -200;
    };
    if ap < 0 {
        return bp;
    }
    if bp < 0 {
        return ap;
    }
    ap.wrapping_add(bp) >> 1
}

fn posts_of(look: &VorbisLookFloor1) -> Result<usize, EngineError> {
    usize::try_from(look.posts)
        .ok()
        .filter(|p| (2..=VIF_POSIT + 2).contains(p))
        .ok_or_else(|| bad_setup("ポストの数が範囲の外"))
}

/// `floor1_fit`（floor1.c:597–750）：ポストの値（`0x8000` は「予測と同じ・使わない」）。当てはめる点がなければ `None`。
pub(crate) fn floor1_fit(
    look: &VorbisLookFloor1,
    logmdct: &[f32],
    logmask: &[f32],
) -> Result<Option<Vec<i32>>, EngineError> {
    let info = &look.vi;
    let n = look.n;
    let posts = posts_of(look)?;
    let need = usize::try_from(n).map_err(|_| bad_setup("n が負"))?;
    if logmdct.len() < need || logmask.len() < need {
        return Err(EngineError::Internal(format!(
            "floor1_fit：logmdct・logmask が {need} 個より短い"
        )));
    }
    let broken = || EngineError::Internal("floor1_fit：look の添字が範囲の外".into());
    let pos = |v: i32| usize::try_from(v).ok().filter(|&v| v < posts);

    let mut nonzero: i64 = 0;
    let mut fits = [LsfitAcc::default(); VIF_POSIT + 1];
    let mut fit_value_a = [-200i32; VIF_POSIT + 2]; // index by range list position
    let mut fit_value_b = [-200i32; VIF_POSIT + 2]; // index by range list position

    let mut loneighbor = [0i32; VIF_POSIT + 2]; // sorted index of range list position (+2)
    let mut hineighbor = [1i32; VIF_POSIT + 2];
    let mut memo = [-1i32; VIF_POSIT + 2]; // no neighbor yet

    // quantize the relevant floor points and collect them into line fit structures (one per minimal
    // division) at the same time（C の posts == 0 の枝は posts ≥ 2 なので通らない）
    for (i, fit) in fits.iter_mut().enumerate().take(posts - 1) {
        nonzero += i64::from(accumulate_fit(
            logmask,
            logmdct,
            look.sorted_index[i],
            look.sorted_index[i + 1],
            fit,
            n,
            info,
        ));
    }

    if nonzero == 0 {
        return Ok(None);
    }

    // start by fitting the implicit base case....
    let mut y0 = -200;
    let mut y1 = -200;
    fit_line(&fits[..posts - 1], &mut y0, &mut y1, info);

    fit_value_a[0] = y0;
    fit_value_b[0] = y0;
    fit_value_b[1] = y1;
    fit_value_a[1] = y1;

    // Non degenerate case: start progressive splitting.  This is a greedy, non-optimal algorithm, but simple
    // and close enough to the best answer.
    for i in 2..posts {
        let sortpos = pos(look.reverse_index[i]).ok_or_else(broken)?;
        let ln = pos(loneighbor[sortpos]).ok_or_else(broken)?;
        let hn = pos(hineighbor[sortpos]).ok_or_else(broken)?;

        // eliminate repeat searches of a particular range with a memo
        if memo[ln] != hn as i32 {
            // haven't performed this error search yet
            let lsortpos = pos(look.reverse_index[ln]).ok_or_else(broken)?;
            let hsortpos = pos(look.reverse_index[hn]).ok_or_else(broken)?;
            memo[ln] = hn as i32;

            // A note: we want to bound/minimize *local*, not global, error
            let lx = info.postlist[ln];
            let hx = info.postlist[hn];
            let ly = post_y(&fit_value_a, &fit_value_b, ln);
            let hy = post_y(&fit_value_a, &fit_value_b, hn);

            if ly == -1 || hy == -1 {
                // C は exit(1)（floor1.c:666）
                return Err(EngineError::Internal(
                    "floor1_fit：近傍のポストの値が -1（C は exit(1)）".into(),
                ));
            }

            if inspect_error(lx, hx, ly, hy, logmask, logmdct, info) != 0 {
                // outside error bounds/begin search area.  Split it.
                let mut ly0 = -200;
                let mut ly1 = -200;
                let mut hy0 = -200;
                let mut hy1 = -200;
                let lo_fits = fits.get(lsortpos..sortpos).ok_or_else(broken)?;
                let hi_fits = fits.get(sortpos..hsortpos).ok_or_else(broken)?;
                let ret0 = fit_line(lo_fits, &mut ly0, &mut ly1, info);
                let ret1 = fit_line(hi_fits, &mut hy0, &mut hy1, info);

                if ret0 != 0 {
                    ly0 = ly;
                    ly1 = hy0;
                }
                if ret1 != 0 {
                    hy0 = ly1;
                    hy1 = hy;
                }

                if ret0 != 0 && ret1 != 0 {
                    fit_value_a[i] = -200;
                    fit_value_b[i] = -200;
                } else {
                    // store new edge values
                    fit_value_b[ln] = ly0;
                    if ln == 0 {
                        fit_value_a[ln] = ly0;
                    }
                    fit_value_a[i] = ly1;
                    fit_value_b[i] = hy0;
                    fit_value_a[hn] = hy1;
                    if hn == 1 {
                        fit_value_b[hn] = hy1;
                    }

                    if ly1 >= 0 || hy0 >= 0 {
                        // store new neighbor values
                        for j in (0..sortpos).rev() {
                            if hineighbor[j] == hn as i32 {
                                hineighbor[j] = i as i32;
                            } else {
                                break;
                            }
                        }
                        for lo in loneighbor.iter_mut().take(posts).skip(sortpos + 1) {
                            if *lo == ln as i32 {
                                *lo = i as i32;
                            } else {
                                break;
                            }
                        }
                    }
                }
            } else {
                fit_value_a[i] = -200;
                fit_value_b[i] = -200;
            }
        }
    }

    let mut output = vec![0i32; posts];

    output[0] = post_y(&fit_value_a, &fit_value_b, 0);
    output[1] = post_y(&fit_value_a, &fit_value_b, 1);

    // fill in posts marked as not using a fit; we will zero back out to 'unused' when encoding them so long
    // as curve interpolation doesn't force them into use
    for i in 2..posts {
        let ln = pos(look.loneighbor[i - 2]).ok_or_else(broken)?;
        let hn = pos(look.hineighbor[i - 2]).ok_or_else(broken)?;
        let x0 = info.postlist[ln];
        let x1 = info.postlist[hn];
        let y0 = output[ln];
        let y1 = output[hn];

        let predicted = render_point(x0, x1, y0, y1, info.postlist[i]);
        let vx = post_y(&fit_value_a, &fit_value_b, i);

        if vx >= 0 && predicted != vx {
            output[i] = vx;
        } else {
            output[i] = predicted | 0x8000;
        }
    }

    Ok(Some(output))
}

/// `floor1_encode`（floor1.c:787–853）の前半：`post` を mult の段に落とし、予測との差を折り返した値（C の `out`。
/// 書く値）を作る。`post` は C と同じく書き換わる（予測と同じ点に `0x8000`）。
fn floor1_encode_out(
    look: &VorbisLookFloor1,
    post: &mut [i32],
) -> Result<[i32; VIF_POSIT + 2], EngineError> {
    let info = &look.vi;
    let posts = posts_of(look)?;
    if post.len() < posts {
        return Err(EngineError::Internal(format!(
            "floor1_encode：post が {posts} 個より短い"
        )));
    }
    let broken = || EngineError::Internal("floor1_encode：look の添字が範囲の外".into());
    let pos = |v: i32| usize::try_from(v).ok().filter(|&v| v < posts);
    let mut out = [0i32; VIF_POSIT + 2];

    // quantize values to multiplier spec
    for p in post.iter_mut().take(posts) {
        let mut val = *p & 0x7fff;
        match info.mult {
            1 => val >>= 2, // 1024 -> 256
            2 => val >>= 3, // 1024 -> 128
            3 => val /= 12, // 1024 -> 86
            4 => val >>= 4, // 1024 -> 64
            _ => {}
        }
        *p = val | (*p & 0x8000);
    }

    out[0] = post[0];
    out[1] = post[1];

    // find prediction values for each post and subtract them
    for i in 2..posts {
        let ln = pos(look.loneighbor[i - 2]).ok_or_else(broken)?;
        let hn = pos(look.hineighbor[i - 2]).ok_or_else(broken)?;
        let x0 = info.postlist[ln];
        let x1 = info.postlist[hn];
        let y0 = post[ln];
        let y1 = post[hn];

        let predicted = render_point(x0, x1, y0, y1, info.postlist[i]);

        if (post[i] & 0x8000) != 0 || predicted == post[i] {
            post[i] = predicted | 0x8000; // in case there was roundoff jitter in interpolation
            out[i] = 0;
        } else {
            let headroom = if look.quant_q.wrapping_sub(predicted) < predicted {
                look.quant_q.wrapping_sub(predicted)
            } else {
                predicted
            };

            let mut val = post[i].wrapping_sub(predicted);

            // at this point the 'deviation' value is in the range +/- max range, but the real, unique range can
            // always be mapped to only [0-maxrange).  So we want to wrap the deviation into this limited range,
            // but do it in the way that least screws an essentially gaussian probability distribution.
            if val < 0 {
                if val < headroom.wrapping_neg() {
                    val = headroom.wrapping_sub(val).wrapping_sub(1);
                } else {
                    // 負の数の左シフト（C の floor1.c:842）は 2 の補数のビット列のまま
                    val = (-1i32).wrapping_sub(val << 1);
                }
            } else if val >= headroom {
                val = val.wrapping_add(headroom);
            } else {
                val <<= 1;
            }

            out[i] = val;
            post[ln] &= 0x7fff;
            post[hn] &= 0x7fff;
        }
    }
    Ok(out)
}

/// `floor1_encode`（floor1.c:774–974）：floor を書き、`ilogmask`（n/2 個）を作る。使う（1）か使わない（0）を返す。
/// `post` は書き換える（C と同じ）。`books` は `CodecSetupInfo::fullbooks`、`pcmend` はブロックの大きさ。
pub(crate) fn floor1_encode(
    opb: &mut OggpackBuffer,
    books: &[Codebook],
    look: &VorbisLookFloor1,
    post: Option<&mut [i32]>,
    ilogmask: &mut [i32],
    pcmend: i32,
) -> Result<i32, EngineError> {
    let info = &look.vi;
    let half = usize::try_from(pcmend / 2)
        .map_err(|_| EngineError::Internal("floor1_encode：pcmend が負".into()))?;
    if ilogmask.len() < half {
        return Err(EngineError::Internal(format!(
            "floor1_encode：ilogmask が {half} 個より短い"
        )));
    }
    let Some(post) = post else {
        opb.oggpack_write(0, 1)?;
        ilogmask[..half].fill(0);
        return Ok(0);
    };
    let posts = posts_of(look)?;
    let out = floor1_encode_out(look, post)?;
    let book_of = |b: i32| {
        usize::try_from(b)
            .ok()
            .and_then(|b| books.get(b))
            .ok_or_else(|| EngineError::Internal(format!("floor1_encode：book {b} がない")))
    };

    // we have everything we need. pack it out
    // mark nontrivial floor
    opb.oggpack_write(1, 1)?;

    // beginning/end post（C は int を unsigned long にしてから下位のビットを書く）
    let ybits = ov_ilog(look.quant_q.wrapping_sub(1) as u32) as u32;
    opb.oggpack_write(out[0] as u64, ybits)?;
    opb.oggpack_write(out[1] as u64, ybits)?;

    // partition by partition
    let mut j = 2usize;
    let partitions = usize::try_from(info.partitions)
        .ok()
        .filter(|&p| p <= VIF_PARTS)
        .ok_or_else(|| bad_setup("partitions が範囲の外"))?;
    for &class in &info.partitionclass[..partitions] {
        let class = usize::try_from(class)
            .ok()
            .filter(|&c| c < VIF_CLASS)
            .ok_or_else(|| bad_setup("partitionclass が範囲の外"))?;
        let cdim = usize::try_from(info.class_dim[class])
            .ok()
            .filter(|&d| d <= 8)
            .ok_or_else(|| bad_setup("class_dim が範囲の外"))?;
        let csubbits = info.class_subs[class];
        if !(0..=3).contains(&csubbits) {
            return Err(bad_setup("class_subs が範囲の外"));
        }
        let csub = 1usize << csubbits;
        let mut bookas = [0usize; 8];
        let mut cval: i32 = 0;
        let mut cshift = 0;
        let vals = out
            .get(j..j + cdim)
            .filter(|_| j + cdim <= posts)
            .ok_or_else(|| bad_setup("partition がポストの数を超える"))?;

        // generate the partition's first stage cascade value
        if csubbits != 0 {
            let mut maxval = [0i32; 8];
            for (k, m) in maxval.iter_mut().enumerate().take(csub) {
                let booknum = info.class_subbook[class][k];
                if booknum < 0 {
                    *m = 1;
                } else {
                    // C は static の本の entries（long → int）
                    *m = book_of(booknum)?.c.entries as i32;
                }
            }
            for (k, &val) in vals.iter().enumerate() {
                for (l, &m) in maxval.iter().enumerate().take(csub) {
                    if val < m {
                        bookas[k] = l;
                        break;
                    }
                }
                cval |= (bookas[k] as i32) << cshift;
                cshift += csubbits;
            }
            // write it
            vorbis_book_encode(book_of(info.class_book[class])?, cval, opb)?;
        }

        // write post values
        for (k, &val) in vals.iter().enumerate() {
            let book = info.class_subbook[class][bookas[k]];
            if book >= 0 {
                let book = book_of(book)?;
                // hack to allow training with 'bad' books
                if i64::from(val) < book.entries {
                    vorbis_book_encode(book, val, opb)?;
                }
            }
        }
        j += cdim;
    }

    // generate quantized floor equivalent to what we'd unpack in decode: render the lines
    let mut hx = 0;
    let mut lx = 0;
    let mut ly = post[0].wrapping_mul(info.mult);
    let n = pcmend / 2;

    for jj in 1..posts {
        let current = usize::try_from(look.forward_index[jj])
            .ok()
            .filter(|&c| c < posts)
            .ok_or_else(|| bad_setup("forward_index が範囲の外"))?;
        let mut hy = post[current] & 0x7fff;
        if hy == post[current] {
            hy = hy.wrapping_mul(info.mult);
            hx = info.postlist[current];

            render_line0(n, lx, hx, ly, hy, ilogmask);

            lx = hx;
            ly = hy;
        }
    }
    // be certain
    if let Ok(from) = usize::try_from(hx) {
        for v in ilogmask.iter_mut().take(half).skip(from) {
            *v = ly;
        }
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vorbis_tables::FLOOR;

    /// すべてのテンプレートの floor で look が作れ、並べ替え・逆引き・近傍が正しい。
    #[test]
    fn look_for_every_template_floor() {
        for (k, f) in FLOOR.iter().enumerate() {
            let look = floor1_look(f).unwrap();
            let posts = look.posts as usize;
            assert_eq!(look.n, f.postlist[1], "floor {k}");
            for i in 0..posts {
                let fi = look.forward_index[i] as usize;
                assert_eq!(look.reverse_index[fi] as usize, i, "floor {k}");
                assert_eq!(look.sorted_index[i], f.postlist[fi], "floor {k}");
            }
            assert!(look.sorted_index[..posts].windows(2).all(|w| w[0] < w[1]));
            assert_eq!(look.sorted_index[0], 0);
            assert_eq!(look.sorted_index[posts - 1], look.n);
            for i in 0..posts - 2 {
                let x = f.postlist[i + 2];
                let (lo, hi) = (look.loneighbor[i] as usize, look.hineighbor[i] as usize);
                assert!(lo < i + 2 && hi < i + 2, "floor {k}");
                assert!(f.postlist[lo] < x && x < f.postlist[hi], "floor {k}");
                // 前のポストのうち、x の下で最も近い・上で最も近い
                for &p in &f.postlist[..i + 2] {
                    assert!(!(p > f.postlist[lo] && p < x), "floor {k}");
                    assert!(!(p < f.postlist[hi] && p > x), "floor {k}");
                }
            }
            assert_eq!(
                look.quant_q,
                [256, 128, 86, 64][f.mult as usize - 1],
                "floor {k}"
            );
        }
    }

    /// 既知の値：44.1 kHz の短い窓の floor（`FLOOR[5]`、照合用の `oracle_setup 44100 2 5 --look` の値）。
    #[test]
    fn look_known_values() {
        let look = floor1_look(&FLOOR[5]).unwrap();
        assert_eq!((look.posts, look.n, look.quant_q), (19, 128, 128));
        assert_eq!(
            look.sorted_index[..19],
            [
                0, 2, 4, 6, 8, 10, 12, 14, 16, 19, 23, 28, 33, 39, 46, 58, 70, 90, 128
            ]
        );
        assert_eq!(
            look.forward_index[..19],
            [
                0, 10, 4, 11, 5, 12, 2, 13, 6, 14, 7, 15, 8, 16, 3, 17, 9, 18, 1
            ]
        );
        assert_eq!(
            look.reverse_index[..19],
            [
                0, 18, 6, 14, 2, 4, 8, 10, 12, 16, 1, 3, 5, 7, 9, 11, 13, 15, 17
            ]
        );
        assert_eq!(
            look.hineighbor[..17],
            [1, 1, 2, 2, 3, 3, 3, 1, 4, 5, 2, 6, 7, 8, 3, 9, 1]
        );
        assert_eq!(
            look.loneighbor[..17],
            [0, 2, 0, 4, 2, 6, 7, 3, 0, 4, 5, 2, 6, 7, 8, 3, 9]
        );
        // 残りは 0（C の calloc）
        assert!(look.sorted_index[19..].iter().all(|&v| v == 0));
        assert!(look.hineighbor[17..].iter().all(|&v| v == 0));
    }

    /// 設定が壊れていれば Err（C は調べない）。
    #[test]
    fn look_rejects_broken_setups() {
        let mut f = FLOOR[5];
        f.mult = 5;
        assert!(floor1_look(&f).is_err());
        let mut f = FLOOR[5];
        f.postlist[3] = f.postlist[4];
        assert!(floor1_look(&f).is_err());
        let mut f = FLOOR[5];
        f.partitions = 32;
        assert!(floor1_look(&f).is_err());
        let mut f = FLOOR[5];
        f.class_dim[0] = 9;
        assert!(floor1_look(&f).is_err());
    }

    /// 小さな部品の既知の値。
    #[test]
    fn helpers_known_values() {
        assert_eq!(vorbis_db_quant(0.0), 1023);
        assert_eq!(vorbis_db_quant(-140.0), 0);
        assert_eq!(vorbis_db_quant(-70.0), 511); // -70*7.3142857+1023.5 = 511.5 → 511
        assert_eq!(vorbis_db_quant(f32::NAN), 0);
        assert_eq!(vorbis_db_quant(f32::INFINITY), 1023);
        assert_eq!(vorbis_db_quant(f32::NEG_INFINITY), 0);
        assert_eq!(render_point(0, 10, 0, 100, 5), 50);
        assert_eq!(render_point(0, 3, 0, 10, 1), 3);
        assert_eq!(render_point(0, 3, 10, 0, 1), 7);
        assert_eq!(render_point(0, 3, 10 | 0x8000, 0, 1), 7);
        assert_eq!(render_point(5, 5, 1, 2, 5), 1); // 0 で割らない
        assert_eq!(post_y(&[-200, 4], &[10, 6], 0), 10);
        assert_eq!(post_y(&[-200, 4], &[10, -200], 1), 4);
        assert_eq!(post_y(&[-200, 4], &[10, 7], 1), 5);
        let mut d = [0; 8];
        render_line0(8, 0, 4, 0, 6, &mut d);
        assert_eq!(d, [0, 1, 3, 4, 0, 0, 0, 0]);
        render_line0(8, 4, 8, 6, 0, &mut d);
        assert_eq!(d, [0, 1, 3, 4, 6, 5, 3, 2]);
        // n で止める
        let mut d = [9; 4];
        render_line0(2, 0, 4, 0, 4, &mut d);
        assert_eq!(d, [0, 1, 9, 9]);
    }

    /// 決まった形の入力で、当てはめ → 符号化が決定的で、形（posts 個・旗・0〜1023・ilogmask の範囲）が正しい。
    /// 無音（mask が −140 dB 以下）は当てはめない（None）で、0 を 1 ビット書いて ilogmask は 0。
    #[test]
    fn fit_and_encode_shapes() {
        use vorbis_tables::FLOOR_BOOKS;
        for (k, f) in FLOOR.iter().enumerate() {
            if f.partitions == 0 {
                continue;
            }
            // floor の book の並びを全体の番号として使う（class_book・class_subbook はその並びの添字）
            let books: Vec<Codebook> = FLOOR_BOOKS[k]
                .iter()
                .map(|b| super::super::codebook::vorbis_book_init_encode(b.unwrap()).unwrap())
                .collect();
            let look = floor1_look(f).unwrap();
            let n = look.n as usize;
            let mask: Vec<f32> = (0..n)
                .map(|i| -40.0 - 60.0 * (i as f32 / n as f32) + 8.0 * ((i % 7) as f32 - 3.0))
                .collect();
            let mdct: Vec<f32> = mask
                .iter()
                .enumerate()
                .map(|(i, m)| m + if i % 3 == 0 { 25.0 } else { -30.0 })
                .collect();
            let fit = floor1_fit(&look, &mdct, &mask).unwrap().unwrap();
            assert_eq!(fit.len(), look.posts as usize);
            assert!(fit[..2].iter().all(|&v| (0..=1023).contains(&v)));
            assert!(fit.iter().all(|&v| (0..=1023).contains(&(v & 0x7fff))));
            assert_eq!(floor1_fit(&look, &mdct, &mask).unwrap().unwrap(), fit);

            let pcmend = 2 * n as i32;
            let mut post = fit.clone();
            let mut ilog = vec![-1; n];
            let mut opb = OggpackBuffer::oggpack_writeinit();
            assert_eq!(
                floor1_encode(&mut opb, &books, &look, Some(&mut post), &mut ilog, pcmend).unwrap(),
                1
            );
            assert!(opb.oggpack_bits() > 1 + 2 * 7);
            assert!(ilog.iter().all(|&v| (0..=255).contains(&v)));
            assert_eq!(opb.oggpack_get_buffer()[0] & 1, 1);
            // もう一度（同じ入力）で同じビット列
            let mut post2 = fit.clone();
            let mut ilog2 = vec![0; n];
            let mut opb2 = OggpackBuffer::oggpack_writeinit();
            floor1_encode(
                &mut opb2,
                &books,
                &look,
                Some(&mut post2),
                &mut ilog2,
                pcmend,
            )
            .unwrap();
            assert_eq!(opb2, opb);
            assert_eq!((post2, ilog2), (post, ilog));

            // 無音
            let silent = vec![-200.0f32; n];
            assert_eq!(floor1_fit(&look, &silent, &silent).unwrap(), None);
            let mut ilog = vec![7; n];
            let mut opb = OggpackBuffer::oggpack_writeinit();
            assert_eq!(
                floor1_encode(&mut opb, &books, &look, None, &mut ilog, pcmend).unwrap(),
                0
            );
            assert_eq!(opb.oggpack_bits(), 1);
            assert!(ilog.iter().all(|&v| v == 0));
            // 長さが足りなければ Err（panic しない）
            assert!(floor1_fit(&look, &mdct[..n - 1], &mask).is_err());
            let mut short = vec![0; n - 1];
            assert!(floor1_encode(&mut opb, &books, &look, None, &mut short, pcmend).is_err());
        }
    }
}

/// 照合（照合用の C がある手元だけ。`KURA_VORBIS_ORACLE`）。
#[cfg(test)]
mod oracle_tests {
    use super::*;
    use crate::codec::vorbis::codebook::testutil::{cases, floor_info, fullbooks, int, ints};
    use crate::codec::vorbis::oracle;

    /// (a) `floor1_look` が照合用の `--look` の `look.floor` とビット一致（全レート × 1・2・3・6・8 ch × q −1・0・3・5・10）。
    #[test]
    fn look_matches_oracle() {
        if oracle::root().is_none() {
            return;
        }
        let mut checked = 0;
        for rate in [
            8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
        ] {
            for ch in [1, 2, 3, 6, 8] {
                for q in [-1, 0, 3, 5, 10] {
                    let setup = oracle::setup(rate, ch, q, true).unwrap();
                    let floors = setup["floors"].as_array().unwrap();
                    let looks = setup["look"]["floor"].as_array().unwrap();
                    assert_eq!(floors.len(), looks.len());
                    for (f, l) in floors.iter().zip(looks) {
                        let look = floor1_look(&floor_info(f)).unwrap();
                        let posts = look.posts as usize;
                        let ctx = format!("{rate} Hz {ch} ch q{q}");
                        assert_eq!(i64::from(look.posts), int(&l["posts"]), "{ctx}");
                        assert_eq!(i64::from(look.n), int(&l["n"]), "{ctx}");
                        assert_eq!(i64::from(look.quant_q), int(&l["quant_q"]), "{ctx}");
                        let v = |a: &[i32]| a.iter().map(|&x| i64::from(x)).collect::<Vec<_>>();
                        assert_eq!(
                            v(&look.sorted_index[..posts]),
                            ints(&l["sorted_index"]),
                            "{ctx}"
                        );
                        assert_eq!(
                            v(&look.forward_index[..posts]),
                            ints(&l["forward_index"]),
                            "{ctx}"
                        );
                        assert_eq!(
                            v(&look.reverse_index[..posts]),
                            ints(&l["reverse_index"]),
                            "{ctx}"
                        );
                        assert_eq!(
                            v(&look.hineighbor[..posts - 2]),
                            ints(&l["hineighbor"]),
                            "{ctx}"
                        );
                        assert_eq!(
                            v(&look.loneighbor[..posts - 2]),
                            ints(&l["loneighbor"]),
                            "{ctx}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        // 8・11.025 kHz と 16・22.05 kHz の q ≤ 0 は窓が 1 種類（floor が 1 つの設定もある）
        assert!(checked > 9 * 5 * 5, "{checked}");
        eprintln!("floor look：{checked} 個の floor が一致");
    }

    /// (b) ダンプ（レベル 2）の各ブロック・各チャンネルで、LMDC・MASK から `floor1_fit` が FFIT と、FFIT から
    /// `floor1_encode` が FOUT・FPST・NZ・ILOG と一致する。
    #[test]
    fn fit_and_encode_match_oracle() {
        if oracle::root().is_none() {
            return;
        }
        let mut blocks = 0usize;
        let mut fitted = 0usize;
        let mut empty = 0usize;
        let mut windows = [0usize; 2];
        for case in cases() {
            let ctx = format!("{case:?}");
            let input = oracle::signal(case.signal, case.rate, case.ch, case.frames, 3);
            let (_, dump) = oracle::encode(&input, case.rate, case.q, Some(2)).unwrap();
            let dump = dump.unwrap();
            let setup = oracle::setup(case.rate, case.ch, case.q, false).unwrap();
            let books = fullbooks(&setup);
            let looks: Vec<VorbisLookFloor1> = setup["floors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| floor1_look(&floor_info(f)).unwrap())
                .collect();
            let modes = setup["modes"].as_array().unwrap();
            let maps = setup["maps"].as_array().unwrap();
            for b in 0..dump.blocks.len() {
                let info = dump.block_info(b);
                let w = info[1] as usize;
                let n = info[8] as usize;
                windows[w] += 1;
                blocks += 1;
                let map = &maps[int(&modes[w]["mapping"]) as usize];
                let chmux = ints(&map["chmuxlist"]);
                let floorsub = ints(&map["floorsubmap"]);
                for c in 0..case.ch {
                    let at = format!("{ctx} ブロック {b} ch {c}");
                    let look = &looks[floorsub[chmux[c] as usize] as usize];
                    let lmdc = dump.get(b, "LMDC", c as u16).unwrap().f32s();
                    let mask = dump.get(b, "MASK", c as u16).unwrap().f32s();
                    let want = dump.get(b, "FFIT", c as u16).unwrap().i32s();
                    let fit = floor1_fit(look, &lmdc, &mask).unwrap();
                    match &fit {
                        None => {
                            assert!(want.is_empty(), "{at}：FFIT");
                            empty += 1;
                        }
                        Some(p) => {
                            assert_eq!(p, &want, "{at}：FFIT");
                            fitted += 1;
                        }
                    }
                    // 符号化
                    let mut post = fit.clone();
                    if let Some(p) = post.as_mut() {
                        let mut q = p.clone();
                        let out = floor1_encode_out(look, &mut q).unwrap();
                        let want = dump.get(b, "FOUT", c as u16).unwrap().i32s();
                        assert_eq!(out[..want.len()], want[..], "{at}：FOUT");
                    } else {
                        assert!(dump.get(b, "FOUT", c as u16).is_none(), "{at}：FOUT");
                    }
                    let mut ilog = vec![0x5555; n / 2];
                    let mut opb = OggpackBuffer::oggpack_writeinit();
                    let nz = floor1_encode(
                        &mut opb,
                        &books,
                        look,
                        post.as_deref_mut(),
                        &mut ilog,
                        n as i32,
                    )
                    .unwrap();
                    assert_eq!(
                        nz,
                        dump.get(b, "NZ  ", c as u16).unwrap().i32s()[0],
                        "{at}：NZ"
                    );
                    let fpst = dump.get(b, "FPST", c as u16).unwrap().i32s();
                    assert_eq!(post.unwrap_or_default(), fpst, "{at}：FPST");
                    assert_eq!(
                        ilog,
                        dump.get(b, "ILOG", c as u16).unwrap().i32s(),
                        "{at}：ILOG"
                    );
                }
            }
        }
        eprintln!(
            "floor fit・encode：{blocks} ブロック（短 {} 長 {}）、当てはめ {fitted}・なし {empty} が一致",
            windows[0], windows[1]
        );
        // 短い窓・長い窓、当てはめあり・なしの両方に届いた
        assert!(windows[0] > 0 && windows[1] > 0, "{windows:?}");
        assert!(fitted > 0 && empty > 0, "{fitted} {empty}");
        assert!(blocks > 1000, "{blocks}");
    }
}
