//! フィルタバンク（ISO/IEC 14496-3 4.6.11）：サイン窓・KBD 窓、逆 MDCT（長い窓 2,048・短い窓 256）、窓の並び 4 つと
//! 重ね合わせ。逆 MDCT は大きさ M の DCT-IV を M/2 点の複素 FFT で求め（前と後ろの回転）、ISO の式
//! `x[n] = (2/N) Σ X[k] cos(2π/N (n + n0)(k + 1/2))` を 32,768 で割って f32 の標本にする。三角関数・ベッセル関数は作るときに
//! f64 の `crate::math` で求めて f32 に丸める（平方根は IEEE 754 で正しく丸めると決まっているので std。値は環境によらない）。

use std::f64::consts::PI;

/// 窓の並び。
const LONG_START: u8 = 1;
const EIGHT_SHORT: u8 = 2;
const LONG_STOP: u8 = 3;

/// 複素 FFT（基数 2 の時間間引き。`n` は 4 以上の 2 のべき）。最初の 2 段（回転が 1 と −i だけ）は 4 点ずつまとめ、残りの
/// 段は段ごとに連続した回転の表（実部と虚部を別の列）で、境界の確かめのない形にする（各要素の演算の順は決まっていて、
/// ベクトル化しても値は同じ）。
struct Fft {
    /// ビットを逆にした並べ替えで入れ替える組（i < j）。
    swaps: Vec<(usize, usize)>,
    /// 段（長さ 8, 16, …, n）ごとの exp(−2πik/len)（k < len/2）の実部と虚部を順につないだもの。
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let swaps = (0..n)
            .filter_map(|i| {
                let j = i.reverse_bits() >> (usize::BITS - bits);
                (j > i).then_some((i, j))
            })
            .collect();
        let (mut tw_re, mut tw_im) = (Vec::new(), Vec::new());
        let mut len = 8;
        while len <= n {
            for k in 0..len / 2 {
                let a = -2.0 * PI * k as f64 / len as f64;
                tw_re.push(crate::math::cos(a) as f32);
                tw_im.push(crate::math::sin(a) as f32);
            }
            len *= 2;
        }
        Self {
            swaps,
            tw_re,
            tw_im,
        }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        for &(i, j) in &self.swaps {
            re.swap(i, j);
            im.swap(i, j);
        }
        // 長さ 2 と 4 の段：回転は 1 と −i
        for (r, i) in re
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(im.as_chunks_mut::<4>().0)
        {
            let (s0r, s0i) = (r[0] + r[1], i[0] + i[1]);
            let (d0r, d0i) = (r[0] - r[1], i[0] - i[1]);
            let (s1r, s1i) = (r[2] + r[3], i[2] + i[3]);
            let (d1r, d1i) = (r[2] - r[3], i[2] - i[3]);
            // d1 × (−i) = (d1i, −d1r)
            r[0] = s0r + s1r;
            i[0] = s0i + s1i;
            r[2] = s0r - s1r;
            i[2] = s0i - s1i;
            r[1] = d0r + d1i;
            i[1] = d0i - d1r;
            r[3] = d0r - d1i;
            i[3] = d0i + d1r;
        }
        // 残りの段（長さ 8, 16, …, n）
        let mut len = 8;
        let mut at = 0;
        while len <= n {
            let half = len / 2;
            one_stage(
                re,
                im,
                half,
                (&self.tw_re[at..at + half], &self.tw_im[at..at + half]),
            );
            at += half;
            len *= 2;
        }
    }
}

/// 基数 2 の 1 段（ブロックの長さ 2·half、回転 `w` は half 個）。
fn one_stage(re: &mut [f32], im: &mut [f32], half: usize, w: (&[f32], &[f32])) {
    let (wr, wi) = (&w.0[..half], &w.1[..half]);
    for (r, i) in re
        .chunks_exact_mut(2 * half)
        .zip(im.chunks_exact_mut(2 * half))
    {
        let (ar, br) = r.split_at_mut(half);
        let (ai, bi) = i.split_at_mut(half);
        // どれも長さ half にそろえて、境界の確かめを外す
        let (br, ai, bi) = (&mut br[..half], &mut ai[..half], &mut bi[..half]);
        for k in 0..half {
            let tr = br[k] * wr[k] - bi[k] * wi[k];
            let ti = br[k] * wi[k] + bi[k] * wr[k];
            br[k] = ar[k] - tr;
            bi[k] = ai[k] - ti;
            ar[k] += tr;
            ai[k] += ti;
        }
    }
}

/// 大きさ `m` の逆 MDCT（`m` 個の係数から 2m 個の標本）。
struct Imdct {
    m: usize,
    fft: Fft,
    /// 前の回転 exp(−iπ(n + 1/4)/m) と後ろの回転 exp(−iπk/m)（n, k < m/2）の実部と虚部。
    pre_c: Vec<f32>,
    pre_s: Vec<f32>,
    post_c: Vec<f32>,
    post_s: Vec<f32>,
    scale: f32,
    re: Vec<f32>,
    im: Vec<f32>,
    /// DCT-IV の出力の偶数番目 u[2k] と、後ろからの奇数番目 u[m − 1 − 2k]。
    even: Vec<f32>,
    odd: Vec<f32>,
    u: Vec<f32>,
}

impl Imdct {
    fn new(m: usize) -> Self {
        let h = m / 2;
        let cos = |a: f64| crate::math::cos(a) as f32;
        let sin = |a: f64| crate::math::sin(a) as f32;
        let pre = |n: usize| -PI * (n as f64 + 0.25) / m as f64;
        let post = |k: usize| -PI * k as f64 / m as f64;
        Self {
            m,
            fft: Fft::new(h),
            pre_c: (0..h).map(|n| cos(pre(n))).collect(),
            pre_s: (0..h).map(|n| sin(pre(n))).collect(),
            post_c: (0..h).map(|k| cos(post(k))).collect(),
            post_s: (0..h).map(|k| sin(post(k))).collect(),
            // (2/N)/32768、N = 2m
            scale: (1.0 / (m as f64 * 32_768.0)) as f32,
            re: vec![0.0; h],
            im: vec![0.0; h],
            even: vec![0.0; h],
            odd: vec![0.0; h],
            u: vec![0.0; m],
        }
    }

    fn run(&mut self, x: &[f32], y: &mut [f32]) {
        let m = self.m;
        let h = m / 2;
        let (x, y) = (&x[..m], &mut y[..2 * m]);
        // t[n] = (x[2n] + i·x[m − 1 − 2n]) · exp(−iπ(n + 1/4)/m)
        let pairs = x.as_chunks::<2>().0;
        let front = pairs.iter().map(|p| p[0]);
        let back = pairs.iter().rev().map(|p| p[1]);
        for ((((re, im), (a, b)), c), s) in self
            .re
            .iter_mut()
            .zip(self.im.iter_mut())
            .zip(front.zip(back))
            .zip(&self.pre_c)
            .zip(&self.pre_s)
        {
            *re = a * c - b * s;
            *im = a * s + b * c;
        }
        self.fft.run(&mut self.re, &mut self.im);
        for (((((zr, zi), c), s), e), o) in self
            .re
            .iter()
            .zip(&self.im)
            .zip(&self.post_c)
            .zip(&self.post_s)
            .zip(self.even.iter_mut())
            .zip(self.odd.iter_mut())
        {
            *e = zr * c - zi * s;
            *o = -(zr * s + zi * c);
        }
        // u に並べ直す（u[2k] = even[k]、u[m − 1 − 2k] = odd[k]）
        let u = &mut self.u[..m];
        for ((p, e), o) in u
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&self.even)
            .zip(self.odd.iter().rev())
        {
            p[0] = *e;
            p[1] = *o;
        }
        let sc = self.scale;
        let (a, b) = y.split_at_mut(h);
        let (b, c) = b.split_at_mut(2 * h);
        for (y, v) in a.iter_mut().zip(&u[h..]) {
            *y = v * sc;
        }
        for (y, v) in b.iter_mut().zip(u.iter().rev()) {
            *y = -v * sc;
        }
        for (y, v) in c.iter_mut().zip(&u[..h]) {
            *y = -v * sc;
        }
    }
}

/// 0 次の第 1 種変形ベッセル関数（級数を 60 項。KBD 窓の大きさの引数で十分に収まる）。
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x / 2.0;
    for k in 1..60 {
        term *= q / k as f64;
        sum += term * term;
    }
    sum
}

/// 窓の上がる側（長さ m。下がる側は逆の順）：サイン窓と KBD 窓。
fn windows(m: usize, alpha: f64) -> (Vec<f32>, Vec<f32>) {
    let sine = (0..m)
        .map(|n| crate::math::sin(PI / (2 * m) as f64 * (n as f64 + 0.5)) as f32)
        .collect();
    let half = m as f64 / 2.0;
    let w: Vec<f64> = (0..=m)
        .map(|p| {
            let t = (p as f64 - half) / half;
            bessel_i0(PI * alpha * crate::math::max(1.0 - t * t, 0.0).sqrt())
        })
        .collect();
    let total: f64 = w.iter().sum();
    let mut acc = 0.0;
    let kbd = (0..m)
        .map(|n| {
            acc += w[n];
            (acc / total).sqrt() as f32
        })
        .collect();
    (sine, kbd)
}

/// フィルタバンク。
pub(crate) struct Filterbank {
    /// 窓の上がる側（サイン・KBD）。
    long: [Vec<f32>; 2],
    short: [Vec<f32>; 2],
    /// 下がる側（上がる側を逆の順にしたもの）。
    long_fall: [Vec<f32>; 2],
    short_fall: [Vec<f32>; 2],
    imdct_long: Imdct,
    imdct_short: Imdct,
    buf: Vec<f32>,
    tmp: Vec<f32>,
}

impl Filterbank {
    pub fn new() -> Self {
        let (ls, lk) = windows(1024, 4.0);
        let (ss, sk) = windows(128, 6.0);
        let fall = |w: &[f32]| w.iter().rev().copied().collect::<Vec<f32>>();
        Self {
            long_fall: [fall(&ls), fall(&lk)],
            short_fall: [fall(&ss), fall(&sk)],
            long: [ls, lk],
            short: [ss, sk],
            imdct_long: Imdct::new(1024),
            imdct_short: Imdct::new(128),
            buf: vec![0.0; 2048],
            tmp: vec![0.0; 256],
        }
    }

    /// スペクトル（1,024）から 1,024 フレームを `out` に書き、後ろ半分を `overlap` に残す。`shape` は今の窓の形
    /// （0 サイン・1 KBD）、`prev` は前のフレームの形（上がる側に使う）。
    pub fn synthesize(
        &mut self,
        spec: &[f32; 1024],
        seq: u8,
        shape: u8,
        prev: u8,
        overlap: &mut [f32],
        out: &mut [f32],
    ) {
        let (cur, prev) = (usize::from(shape & 1), usize::from(prev & 1));
        let buf = &mut self.buf[..2048];
        let (out, overlap) = (&mut out[..1024], &mut overlap[..1024]);
        if seq == EIGHT_SHORT {
            buf.fill(0.0);
            for w in 0..8 {
                self.imdct_short
                    .run(&spec[w * 128..w * 128 + 128], &mut self.tmp);
                let left = &self.short[if w == 0 { prev } else { cur }][..128];
                let right = &self.short_fall[cur][..128];
                let at = 448 + 128 * w;
                let (t0, t1) = self.tmp[..256].split_at(128);
                for (b, (t, l)) in buf[at..at + 128].iter_mut().zip(t0.iter().zip(left)) {
                    *b += t * l;
                }
                for (b, (t, r)) in buf[at + 128..at + 256].iter_mut().zip(t1.iter().zip(right)) {
                    *b += t * r;
                }
            }
            for ((o, b), v) in out.iter_mut().zip(&buf[..1024]).zip(overlap.iter()) {
                *o = b + v;
            }
            overlap.copy_from_slice(&buf[1024..]);
            return;
        }
        self.imdct_long.run(spec, buf);
        let (first, second) = buf.split_at_mut(1024);
        // 前の半分：窓の上がる側を掛けて、前のフレームの残りと足す
        match seq {
            LONG_STOP => {
                // 0〜447 は 0、448〜575 は短い窓の上がる側、576〜1023 は 1
                let short_l = &self.short[prev][..128];
                for ((o, v), (n, b)) in out
                    .iter_mut()
                    .zip(overlap.iter())
                    .zip(first.iter().enumerate())
                {
                    *o = match n {
                        0..448 => 0.0 + v,
                        448..576 => b * short_l[n - 448] + v,
                        _ => b + v,
                    };
                }
            }
            _ => {
                let long_l = &self.long[prev][..1024];
                for ((o, v), (b, w)) in out
                    .iter_mut()
                    .zip(overlap.iter())
                    .zip(first.iter().zip(long_l))
                {
                    *o = b * w + v;
                }
            }
        }
        // 後ろの半分：窓の下がる側を掛けて残す
        match seq {
            LONG_START => {
                // 1024〜1471 は 1、1472〜1599 は短い窓の下がる側、1600〜2047 は 0
                let short_r = &self.short_fall[cur][..128];
                for (n, (v, b)) in overlap.iter_mut().zip(second.iter()).enumerate() {
                    *v = match n {
                        0..448 => *b,
                        448..576 => b * short_r[n - 448],
                        _ => 0.0,
                    };
                }
            }
            _ => {
                let long_r = &self.long_fall[cur][..1024];
                for (v, (b, w)) in overlap.iter_mut().zip(second.iter().zip(long_r)) {
                    *v = b * w;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 速い逆 MDCT が、定義の式（f64 の直接の和）と f32 の丸めの範囲で同じ。
    #[test]
    fn imdct_matches_the_definition() {
        for m in [128usize, 1024] {
            let mut rng = crate::determinism::rng(m as u64);
            use rand_chacha::rand_core::Rng;
            let x: Vec<f32> = (0..m)
                .map(|_| (rng.next_u32() as i32 as f32) / 2.0e5)
                .collect();
            let mut y = vec![0f32; 2 * m];
            Imdct::new(m).run(&x, &mut y);
            let n = 2 * m;
            let n0 = (m as f64 + 1.0) / 2.0;
            let mut max_err = 0f64;
            let mut max_val = 0f64;
            for (i, yi) in y.iter().enumerate() {
                let mut s = 0f64;
                for (k, xk) in x.iter().enumerate() {
                    s += f64::from(*xk)
                        * crate::math::cos(
                            2.0 * PI / n as f64 * (i as f64 + n0) * (k as f64 + 0.5),
                        );
                }
                let want = s * 2.0 / n as f64 / 32_768.0;
                max_err = crate::math::max(max_err, (want - f64::from(*yi)).abs());
                max_val = crate::math::max(max_val, want.abs());
            }
            assert!(max_err <= max_val * 1e-5, "{m}: {max_err} / {max_val}");
        }
    }

    /// 窓は上がる側と下がる側で Princen-Bradley の条件（w[n]² + w[m−1−n]² = 1）を満たす。
    #[test]
    fn windows_are_power_complementary() {
        for (m, alpha) in [(1024usize, 4.0), (128, 6.0)] {
            let (s, k) = windows(m, alpha);
            for w in [&s, &k] {
                for n in 0..m {
                    let e = w[n] * w[n] + w[m - 1 - n] * w[m - 1 - n];
                    assert!((e - 1.0).abs() < 1e-5, "{m} {n} {e}");
                }
            }
        }
    }
}
