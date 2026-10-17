//! 実数の FFT（`lib/smallft.c`。FFTPACK の写し）。トーンの解析に使う（mapping0.c:847 `drft_forward`）。
//!
//! # 移す C（SSE でない経路）
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `drfti1` | 5576–5644 | 因数分解（`ntryh = {4,2,3,5}`）と回転因子 `wa`。`tpi = 6.28318530717958648f`（float）、`argh = tpi/n`（float）、`argld = (float)ld*argh`、`arg = fi*argld`（float）、`wa = cos(arg)`・`sin(arg)`（double の関数に float を渡して float に。`crate::math`） |
//! | `fdrffti` | 5646–5650 | `wsave + n` に `drfti1` |
//! | `dradf2` | 5652–5705 | |
//! | `dradf4` | 5707–5807 | `hsqt2 = .70710678118654752f` |
//! | `dradfg` | 5809–6109 | 一般の因数。ブロックの大きさは 2 の累乗（64〜8192）なので因数は 4 と 2 だけで、**呼ばれない**。移さずに、呼ばれたら `Err` でよい |
//! | `drftf1` | 6111–6170 | 因数ごとに dradf4・dradf2（・dradfg）を選ぶ。`na` で入出力を入れ替え、最後に `c` へ写す |
//! | `drft_forward` | 6770–6800 | SSE でない経路は `drftf1(n, data, trigcache, trigcache + n, splitcache)`（6799）。`n == 1` は何もしない |
//! | `drft_init` | 6811–6819 | `trigcache` は 3n 個（0 で埋める）、`splitcache` は 32 個 |
//!
//! - FFTPACK の添字の計算（`t0`〜`t6` など）をそのまま写す。式の演算の順を変えない。
//! - 出力の形（`data` に実部・虚部が交互。`data[0]` は直流）は C のまま（mapping0.c:848–884 がそれを読む）。
//!
//! # 移さないもの
//!
//! SSE の経路（36–5574 の `cft1st`・`cftmdl`・`bitrv2`・`cftfsub`・`rftfsub`・`rdft`・`drftf256`〜`drftf4096`）。逆変換
//! （`dradb2`〜`drftb1`・`drft_backward`）。`drft_clear`。
//!
//! # 型の対応
//!
//! `drft_lookup`（smallft.h:22–26）→ [`DrftLookup`]。
//!
//! # 移植の補足
//!
//! - C の `drft_forward` は `trigcache` の先頭 n 個を作業領域（`drftf1` の `ch`）に使う（look を書き換える）。各段は `ch` を
//!   すべて書いてから読むので、前の中身は結果に効かない。Rust では look を書き換えず、呼ぶたびに n 個の作業領域を作る
//!   （look の表は `drft_init` のあとの C の表と同じ。照合の `look.drft` は初期化の直後の値）。
//! - `dradfg`（2・4 以外の因数）は移さない：`drft_forward` は因数が 2・4 だけでないと `Err`。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use crate::error::EngineError;
use crate::math;

/// `drfti1` の `ntryh`（smallft.c:5577）。
const NTRYH: [i32; 4] = [4, 2, 3, 5];

/// `splitcache`（`ifac`）の大きさ（smallft.c:6817）。
const IFAC_LEN: usize = 32;

/// `drft_lookup`（smallft.h:22–26）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DrftLookup {
    pub n: i32,
    /// 3n 個（`[n, 2n)` が回転因子 `wa`）。
    pub trigcache: Vec<f32>,
    /// 32 個（`ifac`：`[0]` = n、`[1]` = 因数の数、`[2..]` = 因数）。
    pub splitcache: Vec<i32>,
}

/// `drfti1`（smallft.c:5576–5644）：`wa` と `ifac` を作る。`n ≥ 2`（呼ぶ側が確かめる）。
#[allow(clippy::excessive_precision, clippy::approx_constant)]
fn drfti1(n: i32, wa: &mut [f32], ifac: &mut [i32]) {
    // static float tpi = 6.28318530717958648f;
    let tpi: f32 = 6.283_185_307_179_586_48_f32;
    let mut ntry = 0_i32;
    let mut j = -1_i32;
    let mut nl = n;
    let mut nf = 0_i32;

    'l101: loop {
        j += 1;
        if j < 4 {
            ntry = NTRYH[j as usize];
        } else {
            ntry += 2;
        }
        // L104
        loop {
            let nq = nl / ntry;
            let nr = nl - ntry * nq;
            if nr != 0 {
                continue 'l101;
            }
            nf += 1;
            // ifac は 32 個（2 の累乗の n では nf は 16 以下）
            if nf + 1 >= IFAC_LEN as i32 {
                return;
            }
            ifac[(nf + 1) as usize] = ntry;
            nl = nq;
            if ntry == 2 && nf != 1 {
                for i in 1..nf {
                    let ib = nf - i + 1;
                    ifac[(ib + 1) as usize] = ifac[ib as usize];
                }
                ifac[2] = 2;
            }
            // L107
            if nl != 1 {
                continue;
            }
            break 'l101;
        }
    }
    ifac[0] = n;
    ifac[1] = nf;
    let argh = tpi / n as f32;
    let mut is = 0_usize;
    let nfm1 = nf - 1;
    let mut l1 = 1_i32;

    if nfm1 == 0 {
        return;
    }

    for k1 in 0..nfm1 {
        let ip = ifac[(k1 + 2) as usize];
        let mut ld = 0_i32;
        let l2 = l1 * ip;
        let ido = n / l2;
        let ipm = ip - 1;

        for _ in 0..ipm {
            ld += l1;
            let mut i = is;
            let argld = ld as f32 * argh;
            let mut fi = 0.0_f32;
            let mut ii = 2;
            while ii < ido {
                fi += 1.0_f32;
                let arg = fi * argld;
                wa[i] = math::cos(f64::from(arg)) as f32;
                i += 1;
                wa[i] = math::sin(f64::from(arg)) as f32;
                i += 1;
                ii += 2;
            }
            is += ido as usize;
        }
        l1 = l2;
    }
}

/// `fdrffti`（smallft.c:5646–5650）。
fn fdrffti(n: i32, wsave: &mut [f32], ifac: &mut [i32]) {
    if n == 1 {
        return;
    }
    let nu = n as usize;
    drfti1(n, &mut wsave[nu..], ifac);
}

/// `dradf2`（smallft.c:5652–5705）。
fn dradf2(ido: i32, l1: i32, cc: &[f32], ch: &mut [f32], wa1: &[f32]) {
    let ido = ido as usize;
    let l1 = l1 as usize;

    let mut t1 = 0_usize;
    let t0 = l1 * ido;
    let mut t2 = t0;
    let t3 = ido << 1;
    for _ in 0..l1 {
        ch[t1 << 1] = cc[t1] + cc[t2];
        ch[(t1 << 1) + t3 - 1] = cc[t1] - cc[t2];
        t1 += ido;
        t2 += ido;
    }

    if ido < 2 {
        return;
    }
    if ido != 2 {
        t1 = 0;
        t2 = t0;
        for _ in 0..l1 {
            let mut t3 = t2;
            let mut t4 = (t1 << 1) + (ido << 1);
            let mut t5 = t1;
            let mut t6 = t1 + t1;
            let mut i = 2;
            while i < ido {
                t3 += 2;
                t4 -= 2;
                t5 += 2;
                t6 += 2;
                let tr2 = wa1[i - 2] * cc[t3 - 1] + wa1[i - 1] * cc[t3];
                let ti2 = wa1[i - 2] * cc[t3] - wa1[i - 1] * cc[t3 - 1];
                ch[t6] = cc[t5] + ti2;
                ch[t4] = ti2 - cc[t5];
                ch[t6 - 1] = cc[t5 - 1] + tr2;
                ch[t4 - 1] = cc[t5 - 1] - tr2;
                i += 2;
            }
            t1 += ido;
            t2 += ido;
        }

        if ido % 2 == 1 {
            return;
        }
    }

    // L105
    t1 = ido;
    t2 = t1 - 1;
    let mut t3 = t2;
    t2 += t0;
    for _ in 0..l1 {
        ch[t1] = -cc[t2];
        ch[t1 - 1] = cc[t3];
        t1 += ido << 1;
        t2 += ido;
        t3 += ido;
    }
}

/// `dradf4`（smallft.c:5707–5807）。
#[allow(clippy::excessive_precision, clippy::approx_constant)]
fn dradf4(ido: i32, l1: i32, cc: &[f32], ch: &mut [f32], wa1: &[f32], wa2: &[f32], wa3: &[f32]) {
    // static float hsqt2 = .70710678118654752f;
    let hsqt2: f32 = 0.707_106_781_186_547_52_f32;
    let ido = ido as usize;
    let l1 = l1 as usize;
    let t0 = l1 * ido;

    let mut t1 = t0;
    let mut t4 = t1 << 1;
    let mut t2 = t1 + (t1 << 1);
    let mut t3 = 0_usize;

    for _ in 0..l1 {
        let tr1 = cc[t1] + cc[t2];
        let tr2 = cc[t3] + cc[t4];

        let mut t5 = t3 << 2;
        ch[t5] = tr1 + tr2;
        ch[(ido << 2) + t5 - 1] = tr2 - tr1;
        t5 += ido << 1;
        ch[t5 - 1] = cc[t3] - cc[t4];
        ch[t5] = cc[t2] - cc[t1];

        t1 += ido;
        t2 += ido;
        t3 += ido;
        t4 += ido;
    }

    if ido < 2 {
        return;
    }
    if ido != 2 {
        t1 = 0;
        for _ in 0..l1 {
            t2 = t1;
            t4 = t1 << 2;
            let t6 = ido << 1;
            let mut t5 = t6 + t4;
            let mut i = 2;
            while i < ido {
                t2 += 2;
                t3 = t2;
                t4 += 2;
                t5 -= 2;

                t3 += t0;
                let cr2 = wa1[i - 2] * cc[t3 - 1] + wa1[i - 1] * cc[t3];
                let ci2 = wa1[i - 2] * cc[t3] - wa1[i - 1] * cc[t3 - 1];
                t3 += t0;
                let cr3 = wa2[i - 2] * cc[t3 - 1] + wa2[i - 1] * cc[t3];
                let ci3 = wa2[i - 2] * cc[t3] - wa2[i - 1] * cc[t3 - 1];
                t3 += t0;
                let cr4 = wa3[i - 2] * cc[t3 - 1] + wa3[i - 1] * cc[t3];
                let ci4 = wa3[i - 2] * cc[t3] - wa3[i - 1] * cc[t3 - 1];

                let tr1 = cr2 + cr4;
                let tr4 = cr4 - cr2;
                let ti1 = ci2 + ci4;
                let ti4 = ci2 - ci4;

                let ti2 = cc[t2] + ci3;
                let ti3 = cc[t2] - ci3;
                let tr2 = cc[t2 - 1] + cr3;
                let tr3 = cc[t2 - 1] - cr3;

                ch[t4 - 1] = tr1 + tr2;
                ch[t4] = ti1 + ti2;

                ch[t5 - 1] = tr3 - ti4;
                ch[t5] = tr4 - ti3;

                ch[t4 + t6 - 1] = ti4 + tr3;
                ch[t4 + t6] = tr4 + ti3;

                ch[t5 + t6 - 1] = tr2 - tr1;
                ch[t5 + t6] = ti1 - ti2;
                i += 2;
            }
            t1 += ido;
        }
        if ido & 1 != 0 {
            return;
        }
    }

    // L105
    t1 = t0 + ido - 1;
    t2 = t1 + (t0 << 1);
    let t3 = ido << 2;
    let mut t4 = ido;
    let t5 = ido << 1;
    let mut t6 = ido;

    for _ in 0..l1 {
        let ti1 = -hsqt2 * (cc[t1] + cc[t2]);
        let tr1 = hsqt2 * (cc[t1] - cc[t2]);

        ch[t4 - 1] = tr1 + cc[t6 - 1];
        ch[t4 + t5 - 1] = cc[t6 - 1] - tr1;

        ch[t4] = ti1 - cc[t1 + t0];
        ch[t4 + t5] = ti1 + cc[t1 + t0];

        t1 += ido;
        t2 += ido;
        t4 += t3;
        t6 += ido;
    }
}

/// `dradfg`（smallft.c:5809–6109）：2 の累乗の大きさでは呼ばれない（移さない）。
fn dradfg(
    _ido: i32,
    ip: i32,
    _l1: i32,
    _idl1: i32,
    _cc: &mut [f32],
    _ch: &mut [f32],
    _wa: &[f32],
) -> Result<(), EngineError> {
    Err(EngineError::Internal(format!(
        "smallft：因数 {ip} の FFT（dradfg）は移していません"
    )))
}

/// `wa + off - 1` の位置から（C のポインタ）。
fn wa_at(wa: &[f32], off: i32) -> Result<&[f32], EngineError> {
    usize::try_from(off - 1)
        .ok()
        .and_then(|o| wa.get(o..))
        .ok_or_else(|| EngineError::Internal("smallft：回転因子の位置が範囲の外".into()))
}

/// `drftf1`（smallft.c:6111–6170）。`ch` は作業領域（n 個）。
fn drftf1(
    n: i32,
    c: &mut [f32],
    ch: &mut [f32],
    wa: &[f32],
    ifac: &[i32],
) -> Result<(), EngineError> {
    let nf = ifac[1];
    let mut na = 1;
    let mut l2 = n;
    let mut iw = n;

    for k1 in 0..nf {
        let kh = nf - k1;
        let ip = ifac[(kh + 1) as usize];
        let l1 = l2 / ip;
        let ido = n / l2;
        let idl1 = ido * l1;
        iw -= (ip - 1) * ido;
        na = 1 - na;

        if ip == 4 {
            let ix2 = iw + ido;
            let ix3 = ix2 + ido;
            let (w1, w2, w3) = (wa_at(wa, iw)?, wa_at(wa, ix2)?, wa_at(wa, ix3)?);
            if na != 0 {
                dradf4(ido, l1, ch, c, w1, w2, w3);
            } else {
                dradf4(ido, l1, c, ch, w1, w2, w3);
            }
        } else if ip == 2 {
            let w1 = wa_at(wa, iw)?;
            if na != 0 {
                dradf2(ido, l1, ch, c, w1);
            } else {
                dradf2(ido, l1, c, ch, w1);
            }
        } else {
            if ido == 1 {
                na = 1 - na;
            }
            let w = wa_at(wa, iw)?;
            if na != 0 {
                dradfg(ido, ip, l1, idl1, ch, c, w)?;
                na = 0;
            } else {
                dradfg(ido, ip, l1, idl1, c, ch, w)?;
                na = 1;
            }
        }
        l2 = l1;
    }

    if na == 1 {
        return Ok(());
    }

    let n = n as usize;
    c[..n].copy_from_slice(&ch[..n]);
    Ok(())
}

/// `look` が `drft_init` の作った形で、因数が 2・4 だけか（そうでなければ添字が範囲の外に出うる）。
fn lookup_is_valid(l: &DrftLookup) -> bool {
    let Ok(n) = usize::try_from(l.n) else {
        return false;
    };
    if l.trigcache.len() != 3 * n || l.splitcache.len() != IFAC_LEN || l.splitcache[0] != l.n {
        return false;
    }
    let nf = l.splitcache[1];
    if !(1..IFAC_LEN as i32 - 1).contains(&nf) {
        return false;
    }
    let mut prod = 1_i64;
    for &f in &l.splitcache[2..(nf + 2) as usize] {
        if f != 2 && f != 4 {
            return false;
        }
        prod *= i64::from(f);
    }
    prod == i64::from(l.n)
}

/// `drft_forward`（smallft.c:6770–6800。SSE でない経路 6799）。`data` は n 個（その場で変換）。
pub(crate) fn drft_forward(l: &DrftLookup, data: &mut [f32]) -> Result<(), EngineError> {
    if l.n == 1 {
        return Ok(());
    }
    if !lookup_is_valid(l) {
        return Err(EngineError::Internal(format!(
            "smallft：FFT の look が正しくない（n = {}）",
            l.n
        )));
    }
    let n = l.n as usize;
    let Some(data) = data.get_mut(..n) else {
        return Err(EngineError::Internal(format!(
            "smallft：FFT の入力が {n} 個に足りない"
        )));
    };
    // C は trigcache の先頭 n 個を作業領域に使う（drftf1 の ch）。中身は読まずに書くので新しく作る。
    let mut ch = vec![0.0_f32; n];
    drftf1(l.n, data, &mut ch, &l.trigcache[n..2 * n], &l.splitcache)
}

/// `drft_init`（smallft.c:6811–6819）。`n < 1` は表を作らない（[`drft_forward`] は `Err`）。
pub(crate) fn drft_init(n: i32) -> DrftLookup {
    let Some(len) = usize::try_from(n)
        .ok()
        .filter(|&n| n >= 1)
        .and_then(|n| n.checked_mul(3))
    else {
        return DrftLookup {
            n,
            ..DrftLookup::default()
        };
    };
    let mut trigcache = vec![0.0_f32; len];
    let mut splitcache = vec![0_i32; IFAC_LEN];
    fdrffti(n, &mut trigcache, &mut splitcache);
    DrftLookup {
        n,
        trigcache,
        splitcache,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::rand_core::Rng;

    /// 因数分解は C と同じ並び（4 を先に、2 が出たら先頭へ）で、積が n。
    #[test]
    fn init_factors_like_fftpack() {
        let l = drft_init(512);
        assert_eq!(&l.splitcache[..7], &[512, 5, 2, 4, 4, 4, 4]);
        assert!(l.splitcache[7..].iter().all(|&v| v == 0));
        let l = drft_init(256);
        assert_eq!(&l.splitcache[..6], &[256, 4, 4, 4, 4, 4]);
        for log2n in 1..=13 {
            let n = 1 << log2n;
            let l = drft_init(n);
            assert_eq!(l.trigcache.len(), 3 * n as usize);
            assert!(lookup_is_valid(&l), "n={n}");
            // 作業領域（先頭 n 個）と後ろ n 個は 0 のまま
            assert!(l.trigcache[..n as usize].iter().all(|&v| v == 0.0));
            assert!(l.trigcache[2 * n as usize..].iter().all(|&v| v == 0.0));
        }
        // 3 の因数は表は作れるが、変換は Err（dradfg は移さない）
        let l = drft_init(48);
        assert_eq!(&l.splitcache[..5], &[48, 3, 4, 4, 3]);
        assert!(drft_forward(&l, &mut [0.0; 48]).is_err());
        // n = 1 は何もしない、0 以下は Err
        assert!(drft_forward(&drft_init(1), &mut [5.0]).is_ok());
        assert!(drft_forward(&drft_init(0), &mut []).is_err());
        // 入力が短い
        assert!(drft_forward(&drft_init(64), &mut [0.0; 63]).is_err());
    }

    /// 出力は FFTPACK の実数の形（[0] = 直流、[2k-1]・[2k] = k 番目の実部・虚部（e^{-i…}）、[n-1] = ナイキスト）で、
    /// 定義どおりの DFT と合う。
    #[test]
    fn forward_matches_the_definition() {
        let mut r = crate::determinism::rng(9);
        for log2n in [2, 3, 6, 9, 11] {
            let n = 1usize << log2n;
            let x: Vec<f32> = (0..n)
                .map(|_| (f64::from(r.next_u32()) / 4_294_967_296.0 * 2.0 - 1.0) as f32)
                .collect();
            let l = drft_init(n as i32);
            let mut y = x.clone();
            drft_forward(&l, &mut y).unwrap();
            let dft = |k: usize| {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, &v) in x.iter().enumerate() {
                    let a = 2.0 * std::f64::consts::PI * (i * k % n) as f64 / n as f64;
                    re += f64::from(v) * math::cos(a);
                    im -= f64::from(v) * math::sin(a);
                }
                (re, im)
            };
            let tol = 1e-4 * n as f64;
            assert!((f64::from(y[0]) - dft(0).0).abs() < tol);
            assert!((f64::from(y[n - 1]) - dft(n / 2).0).abs() < tol);
            for k in 1..n / 2 {
                let (re, im) = dft(k);
                assert!((f64::from(y[2 * k - 1]) - re).abs() < tol, "n={n} k={k}");
                assert!((f64::from(y[2 * k]) - im).abs() < tol, "n={n} k={k}");
            }
            // 決定的（同じ入力なら同じビット列）
            let mut y2 = x.clone();
            drft_forward(&l, &mut y2).unwrap();
            assert_eq!(
                y.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                y2.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
    }
}
