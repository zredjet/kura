//! 実数 FFT（M17 段 4。スペクトル `kura-spec/1` のため。docs/editing.md §18.4）。
//!
//! - 長さ N（2 の冪、4 以上）の実数の列を、長さ N/2 の複素の列に詰めて（偶数番 → 実部、奇数番 → 虚部）複素 FFT にかけ、
//!   分けて N/2 + 1 本のビン X_0〜X_{N/2} にする（ふつうの「実数の列を半分の長さの複素 FFT で」の形）。
//! - 複素 FFT は時間間引きの反復で、ビット逆順に並べたあと、基数 2 の段を 2 つずつ基数 4 の段にまとめて回す（段の数が
//!   奇数なら最後の 1 段は基数 2）。まとめても演算の式と順は基数 2 の 2 段と同じ。
//! - f64。回転の値は作るときに 1 度だけ `math::cos`・`sin` で求める（OS の libm を呼ばない）。同じ入力には同じビット列。
//! - Vorbis・AAC の FFT はコーデックの値と一致させるためのものなので共有しない（§18.4）。
//! - 逆変換（[`RealFft::inverse`]。M17 段 7 の直線位相の重なり保存）は、N/2 + 1 本のビンを同じ式の逆で長さ N/2 の複素の列に
//!   まとめ、共役をとって同じ複素 FFT にかけ（IDFT(Z) = conj(DFT(conj Z))/M）、偶数番・奇数番の標本に戻す。順変換と複素 FFT
//!   の式は変えない（`kura-spec/1` の値を保つ）。

use crate::error::EngineError;
use crate::math;

/// 長さ N の実数 FFT（作るときに回転の表とビット逆順の表を持つ。`Send`・`Sync`。値を持たないので共有してよい）。
#[derive(Debug, Clone)]
pub struct RealFft {
    n: usize,
    /// 複素の長さ M = N/2。
    m: usize,
    /// ビット逆順（長さ M）。
    rev: Vec<u32>,
    /// W_M^k = e^{−2πik/M}（k < M/2）。
    tw: Vec<(f64, f64)>,
    /// W_N^k = e^{−2πik/N}（k ≤ M。実数への分け方）。
    half: Vec<(f64, f64)>,
}

/// 作業の領域（呼び手がフレームごとに使い回す）。
#[derive(Debug, Clone, Default)]
pub struct FftWork {
    re: Vec<f64>,
    im: Vec<f64>,
}

fn twiddle(k: usize, len: usize) -> (f64, f64) {
    let a = -2.0 * std::f64::consts::PI * (k as f64) / (len as f64);
    (math::cos(a), math::sin(a))
}

impl RealFft {
    /// 表のバイト数（ビット逆順・回転の表）。
    pub fn resident_bytes(&self) -> u64 {
        (self.rev.len() * 4 + (self.tw.len() + self.half.len()) * 16) as u64
    }

    /// 長さ `n`（2 の冪、4 以上、2^24 以下）。
    pub fn new(n: usize) -> Result<Self, EngineError> {
        if n < 4 || !n.is_power_of_two() || n > 1 << 24 {
            return Err(EngineError::Internal(format!(
                "FFT の長さ {n} は 4 以上 2^24 以下の 2 の冪にします"
            )));
        }
        let m = n / 2;
        let bits = m.trailing_zeros();
        let rev = (0..m)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    ((i as u32).reverse_bits()) >> (32 - bits)
                }
            })
            .collect();
        let tw = (0..m / 2).map(|k| twiddle(k, m)).collect();
        let half = (0..=m).map(|k| twiddle(k, n)).collect();
        Ok(Self {
            n,
            m,
            rev,
            tw,
            half,
        })
    }

    /// 長さ N。
    pub fn len(&self) -> usize {
        self.n
    }

    /// 長さが 0 か（作れる長さは 4 以上なので常に偽）。
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// 実数の列 `x`（長さ N）の FFT X_k = Σ x_n e^{−2πikn/N}（k = 0〜N/2）を `out_re`・`out_im`（どちらも長さ N/2 + 1）に書く。
    pub fn forward(&self, x: &[f64], out_re: &mut [f64], out_im: &mut [f64], work: &mut FftWork) {
        let (n, m) = (self.n, self.m);
        debug_assert!(x.len() == n && out_re.len() == m + 1 && out_im.len() == m + 1);
        work.re.resize(m, 0.0);
        work.im.resize(m, 0.0);
        let (re, im) = (&mut work.re, &mut work.im);
        // 詰めてビット逆順に置く
        for i in 0..m {
            let j = self.rev[i] as usize;
            re[j] = x.get(2 * i).copied().unwrap_or(0.0);
            im[j] = x.get(2 * i + 1).copied().unwrap_or(0.0);
        }
        self.complex(re, im);
        // 分ける：E_k = (Z_k + conj Z_{M−k})/2、O_k = (Z_k − conj Z_{M−k})/(2i)、X_k = E_k + W_N^k O_k
        for k in 0..=m {
            let (zr, zi) = (re[k % m], im[k % m]);
            let (cr, ci) = (re[(m - k) % m], -im[(m - k) % m]);
            let (er, ei) = (0.5 * (zr + cr), 0.5 * (zi + ci));
            // (Z − conj)/(2i) = (dr + i di)/(2i) = (di − i dr)/2
            let (dr, di) = (zr - cr, zi - ci);
            let (or, oi) = (0.5 * di, -0.5 * dr);
            let (wr, wi) = self.half[k];
            out_re[k] = er + (wr * or - wi * oi);
            out_im[k] = ei + (wr * oi + wi * or);
        }
    }

    /// 半分のスペクトル X_0〜X_{N/2}（`in_re`・`in_im`。どちらも長さ N/2 + 1。実数の列のスペクトル）の逆変換
    /// x_n = (1/N) Σ_{k<N} X_k e^{2πikn/N} を `out`（長さ N）に書く（[`Self::forward`] の逆。X_0・X_{N/2} の虚部は 0 として
    /// 扱う形の式）。
    pub fn inverse(&self, in_re: &[f64], in_im: &[f64], out: &mut [f64], work: &mut FftWork) {
        let (n, m) = (self.n, self.m);
        debug_assert!(in_re.len() == m + 1 && in_im.len() == m + 1 && out.len() == n);
        work.re.resize(m, 0.0);
        work.im.resize(m, 0.0);
        let (re, im) = (&mut work.re, &mut work.im);
        // まとめる：E_k = (X_k + conj X_{M−k})/2、O_k = (X_k − conj X_{M−k})/2 · conj W_N^k、Z_k = E_k + i O_k。共役をとって
        // ビット逆順に置く
        for k in 0..m {
            let (xr, xi) = (in_re[k], in_im[k]);
            let (cr, ci) = (in_re[m - k], -in_im[m - k]);
            let (er, ei) = (0.5 * (xr + cr), 0.5 * (xi + ci));
            let (dr, di) = (0.5 * (xr - cr), 0.5 * (xi - ci));
            let (wr, wi) = self.half[k];
            let (or, oi) = (dr * wr + di * wi, di * wr - dr * wi);
            let j = self.rev[k] as usize;
            re[j] = er - oi;
            im[j] = -(ei + or);
        }
        self.complex(re, im);
        // 共役をとって 1/M を掛け、偶数番 ← 実部・奇数番 ← 虚部
        let s = 1.0 / m as f64;
        for i in 0..m {
            out[2 * i] = re[i] * s;
            out[2 * i + 1] = -im[i] * s;
        }
    }

    /// ビット逆順に並んだ長さ M の複素の列の FFT（その場で）。
    fn complex(&self, re: &mut [f64], im: &mut [f64]) {
        let m = self.m;
        let mut len = 1;
        while len < m {
            if len * 4 <= m {
                // 基数 2 の 2 段（span len と 2len）をまとめた基数 4 の段
                let step_a = m / (2 * len);
                let step_b = m / (4 * len);
                let mut base = 0;
                while base < m {
                    for j in 0..len {
                        let (i0, i1, i2, i3) = (
                            base + j,
                            base + j + len,
                            base + j + 2 * len,
                            base + j + 3 * len,
                        );
                        let (war, wai) = self.tw[j * step_a];
                        // 段 A：(i0, i1)・(i2, i3) を回転 W_{2len}^j で
                        let (t1r, t1i) = (re[i1] * war - im[i1] * wai, re[i1] * wai + im[i1] * war);
                        let (a0r, a0i) = (re[i0] + t1r, im[i0] + t1i);
                        let (a1r, a1i) = (re[i0] - t1r, im[i0] - t1i);
                        let (t3r, t3i) = (re[i3] * war - im[i3] * wai, re[i3] * wai + im[i3] * war);
                        let (a2r, a2i) = (re[i2] + t3r, im[i2] + t3i);
                        let (a3r, a3i) = (re[i2] - t3r, im[i2] - t3i);
                        // 段 B：(i0, i2) を W_{4len}^j、(i1, i3) を W_{4len}^{j+len} で
                        let (b0r, b0i) = self.tw[j * step_b];
                        let (b1r, b1i) = self.tw[(j + len) * step_b];
                        let (ur, ui) = (a2r * b0r - a2i * b0i, a2r * b0i + a2i * b0r);
                        let (vr, vi) = (a3r * b1r - a3i * b1i, a3r * b1i + a3i * b1r);
                        re[i0] = a0r + ur;
                        im[i0] = a0i + ui;
                        re[i2] = a0r - ur;
                        im[i2] = a0i - ui;
                        re[i1] = a1r + vr;
                        im[i1] = a1i + vi;
                        re[i3] = a1r - vr;
                        im[i3] = a1i - vi;
                    }
                    base += 4 * len;
                }
                len *= 4;
            } else {
                // 基数 2 の段（span len）
                let step = m / (2 * len);
                let mut base = 0;
                while base < m {
                    for j in 0..len {
                        let (i0, i1) = (base + j, base + j + len);
                        let (wr, wi) = self.tw[j * step];
                        let (tr, ti) = (re[i1] * wr - im[i1] * wi, re[i1] * wi + im[i1] * wr);
                        let (xr, xi) = (re[i0], im[i0]);
                        re[i0] = xr + tr;
                        im[i0] = xi + ti;
                        re[i1] = xr - tr;
                        im[i1] = xi - ti;
                    }
                    base += 2 * len;
                }
                len *= 2;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 定義どおりの DFT（f64。回転は `math`）。
    fn dft(x: &[f64]) -> Vec<(f64, f64)> {
        let n = x.len();
        (0..=n / 2)
            .map(|k| {
                let (mut r, mut i) = (0.0, 0.0);
                for (t, v) in x.iter().enumerate() {
                    let (c, s) = twiddle((k * t) % n, n);
                    r += v * c;
                    i += v * s;
                }
                (r, i)
            })
            .collect()
    }

    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            })
            .collect()
    }

    /// 定義どおりの DFT と合う（長さ 4〜4,096。誤差は振幅の和に比べて小さい）。大きな長さは逆の向きの照らし（パーセバル）で。
    #[test]
    fn matches_the_definition() {
        for bits in 2..=12 {
            let n = 1usize << bits;
            let fft = RealFft::new(n).unwrap();
            let x = noise(n, bits as u64);
            let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
            fft.forward(&x, &mut re, &mut im, &mut FftWork::default());
            let want = dft(&x);
            let scale: f64 = x.iter().map(|v| v.abs()).sum();
            for k in 0..=n / 2 {
                assert!(
                    (re[k] - want[k].0).abs() <= 1e-12 * scale
                        && (im[k] - want[k].1).abs() <= 1e-12 * scale,
                    "N {n} k {k}: ({}, {}) と {:?}",
                    re[k],
                    im[k],
                    want[k]
                );
            }
        }
    }

    /// パーセバル：Σ x² = (X_0² + X_{N/2}² + 2 Σ_{0<k<N/2} |X_k|²)/N（長さ 8,192〜65,536）。正弦はそのビンだけに出る。
    #[test]
    fn large_lengths_keep_energy_and_tones() {
        for bits in [13, 14, 15, 16] {
            let n = 1usize << bits;
            let fft = RealFft::new(n).unwrap();
            let x = noise(n, 77 + bits as u64);
            let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
            fft.forward(&x, &mut re, &mut im, &mut FftWork::default());
            let time: f64 = x.iter().map(|v| v * v).sum();
            let mut freq = re[0] * re[0] + re[n / 2] * re[n / 2];
            for k in 1..n / 2 {
                freq += 2.0 * (re[k] * re[k] + im[k] * im[k]);
            }
            freq /= n as f64;
            assert!(
                (time - freq).abs() <= 1e-9 * time,
                "N {n}: {time} と {freq}"
            );
            let bin = n / 8 + 3;
            let tone: Vec<f64> = (0..n)
                .map(|t| math::cos(2.0 * std::f64::consts::PI * (bin * t % n) as f64 / n as f64))
                .collect();
            fft.forward(&tone, &mut re, &mut im, &mut FftWork::default());
            for k in 0..=n / 2 {
                let p = (re[k] * re[k] + im[k] * im[k]).sqrt();
                let want = if k == bin { n as f64 / 2.0 } else { 0.0 };
                assert!((p - want).abs() < 1e-6 * n as f64, "N {n} k {k}: {p}");
            }
        }
    }

    /// 逆変換（M17 段 7）：順変換の逆で戻り（長さ 4〜2^17、相対 1e-12）、定義どおりの逆 DFT と合い（長さ 4〜4,096）、2 回で
    /// ビット単位で同じ。
    #[test]
    fn inverse_undoes_forward_and_matches_the_definition() {
        for bits in 2..=17 {
            let n = 1usize << bits;
            let fft = RealFft::new(n).unwrap();
            let x = noise(n, 300 + bits as u64);
            let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
            let mut work = FftWork::default();
            fft.forward(&x, &mut re, &mut im, &mut work);
            let mut back = vec![0.0; n];
            fft.inverse(&re, &im, &mut back, &mut work);
            let scale = x.iter().map(|v| v.abs()).fold(0.0, math::max);
            for (t, (a, b)) in back.iter().zip(&x).enumerate() {
                assert!(
                    (a - b).abs() <= 1e-12 * scale * (bits as f64),
                    "N {n} t {t}: {a} と {b}"
                );
            }
            if bits <= 12 {
                // 定義どおりの逆 DFT（実数の列のスペクトルなので、残りのビンは共役）
                for (t, got) in back.iter().enumerate() {
                    let mut v = 0.0;
                    for k in 0..n {
                        let (xr, xi) = if k <= n / 2 {
                            (re[k], im[k])
                        } else {
                            (re[n - k], -im[n - k])
                        };
                        let (c, s) = twiddle((k * t) % n, n);
                        // e^{+iθ} = c − i s（twiddle は e^{−iθ}）
                        v += xr * c + xi * s;
                    }
                    v /= n as f64;
                    assert!(
                        (got - v).abs() <= 1e-12 * scale * (bits as f64),
                        "N {n} t {t}: {got} と {v}"
                    );
                }
            }
            let mut again = vec![0.0; n];
            fft.inverse(&re, &im, &mut again, &mut FftWork::default());
            assert!(
                again
                    .iter()
                    .zip(&back)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
    }

    /// 2 回でビット単位で同じ（作り直しても・作業の領域を使い回しても）。長さの誤りは断る。
    #[test]
    fn repeatable_and_checked() {
        let x = noise(1024, 5);
        let mut work = FftWork::default();
        let run = |fft: &RealFft, work: &mut FftWork| {
            let (mut re, mut im) = (vec![0.0; 513], vec![0.0; 513]);
            fft.forward(&x, &mut re, &mut im, work);
            re.into_iter()
                .chain(im)
                .map(f64::to_bits)
                .collect::<Vec<_>>()
        };
        let a = run(&RealFft::new(1024).unwrap(), &mut work);
        let b = run(&RealFft::new(1024).unwrap(), &mut work);
        assert_eq!(a, b);
        for bad in [0, 2, 3, 6, 1000] {
            assert!(RealFft::new(bad).is_err(), "{bad}");
        }
    }
}
