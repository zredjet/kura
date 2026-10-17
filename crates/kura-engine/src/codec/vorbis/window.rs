//! Vorbis の窓（`lib/window.c`）。
//!
//! # 移す C
//!
//! - `_vorbis_window_get`（window.c:2133–2135）：`vwin[n]`（n は `ov_ilog(blocksize) - 7`、64〜8192 の 8 通り）。表は
//!   `vorbis_tables::VWIN64`〜`VWIN8192`（window.c の float の定数をそのまま。**計算で作らない**：照合用の C は表の値を使う）。
//! - `_vorbis_apply_window`（window.c:2137–2261）の SSE でない枝（2247–2259）：
//!   `lW = W ? lW : 0`、`nW = W ? nW : 0`。`leftbegin = n/4 - ln/4`、`leftend = leftbegin + ln/2`、
//!   `rightbegin = n/2 + n/4 - rn/4`、`rightend = rightbegin + rn/2`（long の整数の割り算）。
//!   `[0, leftbegin)` を 0、`[leftbegin, leftend)` に `windowLW[p]` を掛ける（p は 0 から増える）、
//!   `[rightbegin, rightend)` に `windowNW[p]` を掛ける（p は `rn/2 - 1` から減る）、残りを 0。掛け算は f32 で 1 回。
//!
//! # 移さないもの
//!
//! SSE の枝（2158–2246）。
//!
//! # 型の対応
//!
//! `long *blocksizes` → `&[i64; 2]`、`int *winno` → `&[i32; 2]`（private_state の `window[2]`）、`float *d` → `&mut [f32]`。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use vorbis_tables::{VWIN64, VWIN128, VWIN256, VWIN512, VWIN1024, VWIN2048, VWIN4096, VWIN8192};

/// window.c:2122–2131 の `vwin[8]`。
static VWIN: [&[f32]; 8] = [
    &VWIN64, &VWIN128, &VWIN256, &VWIN512, &VWIN1024, &VWIN2048, &VWIN4096, &VWIN8192,
];

/// `_vorbis_window_get`（window.c:2133–2135）：窓の左半分の表（長さ 32 << n）。範囲の外（C では範囲の外の読み取り）は空。
pub(crate) fn vorbis_window_get(n: i32) -> &'static [f32] {
    usize::try_from(n)
        .ok()
        .and_then(|i| VWIN.get(i))
        .copied()
        .unwrap_or(&[])
}

/// `_vorbis_apply_window`（window.c:2137–2261。SSE でない枝 2247–2259）。
///
/// `W`・`lW`・`nW` は 0 か 1（C は `blocksizes[W]` の添字に使う）。窓の表の長さ・`d` の長さが食い違う（C では範囲の外の
/// 読み書き。`_vds_shared_init` が確かめた大きさでは起きない）ときは、届く所までだけ掛ける（panic しない）。
pub(crate) fn vorbis_apply_window(
    d: &mut [f32],
    winno: &[i32; 2],
    blocksizes: &[i64; 2],
    lw: i64,
    w: i64,
    nw: i64,
) {
    let lw = if w != 0 { lw } else { 0 };
    let nw = if w != 0 { nw } else { 0 };
    let (lw, w, nw) = (
        usize::from(lw != 0),
        usize::from(w != 0),
        usize::from(nw != 0),
    );

    let window_lw = vorbis_window_get(winno[lw]);
    let window_nw = vorbis_window_get(winno[nw]);

    let n = blocksizes[w];
    let ln = blocksizes[lw];
    let rn = blocksizes[nw];

    let leftbegin = n / 4 - ln / 4;
    let leftend = leftbegin + ln / 2;

    let rightbegin = n / 2 + n / 4 - rn / 4;
    let rightend = rightbegin + rn / 2;

    // 添字を d の中に止める（正しい大きさでは何も変えない）
    let len = i64::try_from(d.len()).unwrap_or(i64::MAX);
    let clamp = |x: i64| usize::try_from(x.clamp(0, len)).unwrap_or(0);
    let (leftbegin, leftend) = (clamp(leftbegin), clamp(leftend));
    let (rightbegin, rightend, n) = (clamp(rightbegin), clamp(rightend), clamp(n));
    let half = usize::try_from(rn / 2).unwrap_or(0).min(window_nw.len());

    // for(i=0;i<leftbegin;i++) d[i]=0.f;
    for x in &mut d[..leftbegin] {
        *x = 0.0;
    }
    // for(p=0;i<leftend;i++,p++) d[i]*=windowLW[p];
    for (x, wv) in d[leftbegin..leftend.max(leftbegin)]
        .iter_mut()
        .zip(window_lw)
    {
        *x *= *wv;
    }
    // for(i=rightbegin,p=rn/2-1;i<rightend;i++,p--) d[i]*=windowNW[p];
    for (x, wv) in d[rightbegin..rightend.max(rightbegin)]
        .iter_mut()
        .zip(window_nw[..half].iter().rev())
    {
        *x *= *wv;
    }
    // for(;i<n;i++) d[i]=0.f;（i は rightend から）
    for x in &mut d[rightend..n.max(rightend)] {
        *x = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 窓の表は 8 通り（32 << n 個）で、0 から 1 へ単調に増え、`w[i]² + w[len-1-i]²` が 1（Princen-Bradley）。
    #[test]
    fn window_tables_are_power_complementary() {
        for n in 0..8 {
            let w = vorbis_window_get(n);
            assert_eq!(w.len(), 32 << n);
            assert!(w.windows(2).all(|p| p[0] <= p[1]));
            assert!(w[0] > 0.0 && w[w.len() - 1] <= 1.0);
            for i in 0..w.len() {
                let s = w[i] * w[i] + w[w.len() - 1 - i] * w[w.len() - 1 - i];
                assert!((s - 1.0).abs() < 1e-5, "n={n} i={i} {s}");
            }
        }
        assert!(vorbis_window_get(-1).is_empty());
        assert!(vorbis_window_get(8).is_empty());
    }

    /// 長い窓の両隣が短いとき（transition）の形：外側は 0、短い窓の傾きが真ん中に来る。短い窓では前後を見ない。
    #[test]
    fn apply_window_shapes() {
        let bs = [256_i64, 2048];
        let winno = [2, 5];
        // 長・前後とも長
        let mut d = vec![1.0_f32; 2048];
        vorbis_apply_window(&mut d, &winno, &bs, 1, 1, 1);
        let wl = vorbis_window_get(5);
        assert_eq!(d[0].to_bits(), wl[0].to_bits());
        assert_eq!(d[1023].to_bits(), wl[1023].to_bits());
        assert_eq!(d[1024].to_bits(), wl[1023].to_bits());
        assert_eq!(d[2047].to_bits(), wl[0].to_bits());
        // 長・前が短い・後ろが短い
        let mut d = vec![1.0_f32; 2048];
        vorbis_apply_window(&mut d, &winno, &bs, 0, 1, 0);
        let ws = vorbis_window_get(2);
        let lb = 512 - 64;
        assert!(d[..lb].iter().all(|&x| x == 0.0));
        assert_eq!(d[lb].to_bits(), ws[0].to_bits());
        assert_eq!(d[lb + 127].to_bits(), ws[127].to_bits());
        assert!(d[lb + 128..1536 - 64].iter().all(|&x| x == 1.0));
        assert_eq!(d[1536 - 64].to_bits(), ws[127].to_bits());
        assert_eq!(d[1536 + 63].to_bits(), ws[0].to_bits());
        assert!(d[1536 + 64..].iter().all(|&x| x == 0.0));
        // 短い窓は lW・nW を見ない（0 として扱う）
        let mut a = vec![0.5_f32; 256];
        let mut b = a.clone();
        vorbis_apply_window(&mut a, &winno, &bs, 1, 0, 1);
        vorbis_apply_window(&mut b, &winno, &bs, 0, 0, 0);
        assert_eq!(a, b);
        assert_eq!(a[0].to_bits(), (0.5 * ws[0]).to_bits());
        // 短すぎる d でも panic しない
        let mut short = vec![1.0_f32; 10];
        vorbis_apply_window(&mut short, &winno, &bs, 1, 1, 1);
    }
}
