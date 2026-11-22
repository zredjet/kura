//! 線形予測（`lib/lpc.c`）。入力の前後の外挿（block.c の `_preextrapolate_helper`・`vorbis_analysis_wrote` の末尾）に使う。
//!
//! # 移す C
//!
//! - `vorbis_lpc_from_data`（lpc.c:60–130）：自己相関（`aut`、double。`d += (double)data[i]*data[i-j]` を i の順に）、
//!   Levinson-Durbin（double。`error < epsilon` で残りを 0 にして抜ける）、`g = .99` の減衰（`damp *= g`）、`lpci[j] = (float)lpc[j]`。
//!   戻り値 `error` は double を float で返す（呼ぶ側は使わない）。
//! - `vorbis_lpc_predict`（lpc.c:132–159）：`work[0..m] = prime`（`prime` が NULL なら 0）、`y = 0; y -= work[o++]*coeff[--p]`
//!   （float）、`data[i] = work[o] = y`。作業領域は m + n 個。
//!
//! 和の順・double と float の区別を C のまま写す（`(double)data[i]*data[i-j]` は double の積、`y -= work*coeff` は float）。
//!
//! # 型の対応
//!
//! `float *data` → `&[f32]`、`float *lpci` → `&mut [f32]`（m 個）、`float *prime` → `Option<&[f32]>`（m 個）。
//!
//! # 移植の補足
//!
//! - 長さが足りない（`data` が n 個ない・`lpci` が m 個ないなど。C では範囲の外の読み書き）ときは、届く所までで止める
//!   （panic しない）。block.rs は正しい長さで呼ぶ。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

/// `vorbis_lpc_from_data`（lpc.c:60–130）：n 個の data から m 個の係数。戻り値は残りの誤差（C の float）。
pub(crate) fn vorbis_lpc_from_data(data: &[f32], lpci: &mut [f32], n: i32, m: i32) -> f32 {
    let m = usize::try_from(m).unwrap_or(0);
    let n = usize::try_from(n).unwrap_or(0).min(data.len());
    let data = &data[..n];
    let mut aut = vec![0.0_f64; m + 1];

    // autocorrelation, p+1 lag coefficients
    // j=m+1; while(j--){ double d=0; for(i=j;i<n;i++) d+=(double)data[i]*data[i-j]; aut[j]=d; }
    for j in (0..=m).rev() {
        let mut d = 0.0_f64;
        for i in j..n {
            d += f64::from(data[i]) * f64::from(data[i - j]);
        }
        aut[j] = d;
    }

    lpc_from_autocorrelation(&aut, lpci)
}

/// `vorbis_lpc_from_data` の自己相関のあと（lpc.c:79–129）：`aut`（m + 1 個）から m 個の係数（Levinson-Durbin・減衰）。
/// 自己相関を別に（ずれごとに並列に。各ずれの和の順は C のまま）求める所（block.rs の前の外挿）が使う。
pub(crate) fn lpc_from_autocorrelation(aut: &[f64], lpci: &mut [f32]) -> f32 {
    let m = aut.len().saturating_sub(1);
    if aut.is_empty() {
        return 0.0;
    }
    let mut lpc = vec![0.0_f64; m];

    // Generate lpc coefficients from autocorr values

    // set our noise floor to about -100dB
    let mut error = aut[0] * (1.0 + 1e-10);
    let epsilon = 1e-9 * aut[0] + 1e-10;

    'done: {
        for i in 0..m {
            let mut r = -aut[i + 1];

            if error < epsilon {
                for v in &mut lpc[i..] {
                    *v = 0.0;
                }
                break 'done;
            }

            // Sum up this iteration's reflection coefficient
            for j in 0..i {
                r -= lpc[j] * aut[i - j];
            }
            r /= error;

            // Update LPC coefficients and total error
            lpc[i] = r;
            let mut j = 0;
            while j < i / 2 {
                let tmp = lpc[j];

                lpc[j] += r * lpc[i - 1 - j];
                lpc[i - 1 - j] += r * tmp;
                j += 1;
            }
            if i & 1 != 0 {
                lpc[j] += lpc[j] * r;
            }

            error *= 1.0 - r * r;
        }
    }

    // slightly damp the filter
    let g = 0.99_f64;
    let mut damp = g;
    for v in &mut lpc {
        *v *= damp;
        damp *= g;
    }

    for (o, v) in lpci.iter_mut().zip(&lpc) {
        *o = *v as f32;
    }

    // we need the error value to know how big an impulse to hit the filter with later
    error as f32
}

/// `vorbis_lpc_predict`（lpc.c:132–159）：係数 m 個・初期値 m 個から n 個を予測して `data` に書く。
pub(crate) fn vorbis_lpc_predict(
    coeff: &[f32],
    prime: Option<&[f32]>,
    m: i32,
    data: &mut [f32],
    n: i64,
) {
    let m = usize::try_from(m).unwrap_or(0).min(coeff.len());
    let n = usize::try_from(n).unwrap_or(0).min(data.len());
    let mut work = vec![0.0_f32; m + n];

    // if(!prime) work[i]=0.f; else work[i]=prime[i];
    if let Some(prime) = prime {
        for (w, p) in work.iter_mut().zip(prime.iter().take(m)) {
            *w = *p;
        }
    }

    for (i, d) in data.iter_mut().take(n).enumerate() {
        let mut y = 0.0_f32;
        let mut o = i;
        let mut p = m;
        for _ in 0..m {
            p -= 1;
            y -= work[o] * coeff[p];
            o += 1;
        }

        // data[i]=work[o]=y;
        *d = y;
        work[o] = y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正弦波は 2 次の予測でほぼ続けられる（減衰 0.99 の分だけ小さくなる）。
    #[test]
    fn predicts_a_sine() {
        let n = 1024;
        let w = 2.0 * std::f64::consts::PI * 0.01;
        let x: Vec<f32> = (0..n)
            .map(|i| crate::math::sin(w * i as f64) as f32)
            .collect();
        let mut lpc = [0.0_f32; 16];
        let err = vorbis_lpc_from_data(&x, &mut lpc, n as i32, 16);
        // 残りの誤差は自己相関の 0 番（およそ n/2 = 512）より十分小さい
        assert!((0.0..5.0).contains(&err), "{err}");
        let mut out = vec![0.0_f32; 64];
        vorbis_lpc_predict(&lpc, Some(&x[n - 16..]), 16, &mut out, 64);
        // 係数を 0.99^k で減衰させるので、続きは少しずつ小さく・ずれる：最初の数標本だけ近く、全体は発散しない
        for (k, &v) in out.iter().enumerate() {
            let want = crate::math::sin(w * (n + k) as f64) as f32;
            if k < 8 {
                assert!((v - want).abs() < 0.1, "k={k} {v} {want}");
            }
            assert!(v.abs() <= 1.0, "k={k} {v}");
        }
    }

    /// 無音（自己相関が 0）では係数がすべて 0 で、予測も 0。prime が None なら 0 から始める。
    #[test]
    fn silence_gives_zero() {
        let x = vec![0.0_f32; 100];
        let mut lpc = [1.0_f32; 32];
        let err = vorbis_lpc_from_data(&x, &mut lpc, 100, 32);
        assert_eq!(err, 0.0);
        assert!(lpc.iter().all(|&c| c == 0.0));
        let mut out = vec![9.0_f32; 10];
        vorbis_lpc_predict(&lpc, None, 32, &mut out, 10);
        assert!(out.iter().all(|&v| v == 0.0));
        // 長さの食い違いでも panic しない
        vorbis_lpc_from_data(&x, &mut lpc, 1000, 40);
        vorbis_lpc_predict(&lpc, Some(&x[..3]), 32, &mut out, 1000);
    }

    /// 凍結した値：1 次の自己回帰（x[i] = 0.5 x[i-1]）の係数（C の式を double でたどったもの）。
    #[test]
    fn first_order_ar_coefficients() {
        let mut v = 1.0_f32;
        let x: Vec<f32> = (0..64)
            .map(|_| {
                let r = v;
                v *= 0.5;
                r
            })
            .collect();
        let mut lpc = [0.0_f32; 2];
        vorbis_lpc_from_data(&x, &mut lpc, 64, 2);
        // a1 ≈ -0.5 × 0.99、a2 ≈ 0（減衰 0.99²）
        assert!((lpc[0] + 0.495).abs() < 1e-3, "{lpc:?}");
        assert!(lpc[1].abs() < 1e-3, "{lpc:?}");
        // 決定的
        let mut lpc2 = [0.0_f32; 2];
        vorbis_lpc_from_data(&x, &mut lpc2, 64, 2);
        assert_eq!(lpc[0].to_bits(), lpc2[0].to_bits());
        assert_eq!(lpc[1].to_bits(), lpc2[1].to_bits());
    }
}
