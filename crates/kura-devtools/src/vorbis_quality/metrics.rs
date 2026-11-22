//! 品質の指標。**指標の定義はこの冒頭が正**（`vorbis-quality` の表と `results.jsonl` の数の意味）。libm（`=0.2.16`）と
//! 自前の基数 2 の FFT と順の和だけで求めるので、同じ入力なら どの機械でも同じ数字になる。
//!
//! 出どころ：M10 段 7 の品質の調査（作業の記録。リポジトリにはない）で、Python（numpy）と Rust の試作で確かめた定義を
//! ここに写した。決めたことの要約は `docs/decisions.md` の ADR-149 (13)。
//!
//! # 比べるもの
//!
//! - 元 `r`：16 ビットに丸めた試験の音（`signals`）か、読んだ入力のファイルの値。試す側 `t`：ogg を symphonia（kura の
//!   読み取り）で読んだ値（丸めない f32）。どちらもチャンネルごとの f64。
//! - 比べる前に、位置のずれ（[`lag`]：チャンネルで足した相互相関の絶対値が最大になる遅れ。±4,096 標本、先頭の 131,072 標本で
//!   探す）だけ `t` をずらし（[`align`]）、両方を共通の長さ（元の長さと、ずらした `t` の長さの短いほう）に切る
//!   （`mod.rs` の `measure`）。長さそのもの（symphonia・lewton が読んだフレームの数）は指標とは別に記録する。
//! - フレームは 2,048 点・1,024 標本の進み。信号の終わりまで覆い、最後の半端は 0 で埋める（[`frame_count`]。
//!   調査の Python は半端を捨てていた）。
//!
//! # NMR（PEAQ の基本版に近いもの。[`Model`]・[`Nmr`]）
//!
//! ITU-R BS.1387-1 の基本版の FFT の耳のモデルに近いもの。適合の試験はしていない（データの境界の規則・レベルの正規化などは
//! 省いた）。フレームごと・チャンネルごとに：
//!
//! 1. **窓**：Hann `0.5·(1 − cos(2πn/(N − 1)))·√(8/3)`（N = 2,048）。振幅スペクトル `|X(k)|`（k = 0〜N/2）。
//! 2. **聴取のレベル**：1019.5 Hz の満幅（振幅 1）の正弦の振幅スペクトルの最大が 92 dB SPL（`10^(92/20)`）になる係数 `fac`
//!    を掛ける（[`LISTENING_DB_SPL`]）。
//! 3. **外耳・中耳の重み**：`W(f) = 10^((−0.6·3.64·f^−0.8 + 6.5·e^(−0.6·(f − 3.3)²) − 10⁻³·f^3.6) / 20)`（f は kHz、
//!    1 Hz より下は 1 Hz）。`E(k) = |X(k)|·fac·W(f_k)`。
//! 4. **帯域**：80 Hz〜18 kHz を Bark（`z = 7·asinh(f/650)`）で 0.25 Bark ずつ 109 帯域に分ける（最後の帯域は 18 kHz で
//!    切る）。中心 `fc` は帯域の Bark の中央。ビンは幅 fs/N の区間と帯域の重なりの割合（0〜1）で配る。
//! 5. **元のエネルギーと雑音**：`Pe = Σ E_r(k)² + Pthres`、内部の雑音 `Pthres = 10^(0.4·0.364·(fc/1000)^−0.8)`。
//!    雑音 `Pn = Σ (E_r(k) − E_t(k))²`（振幅スペクトルの差。位相を見ない）。
//! 6. **周波数の広がり**：帯域 j のエネルギーを、下へ 27 dB/Bark、上へ `24 + 230/fc_j − 0.2·L_j` dB/Bark
//!    （`L_j = 10·log10 Pe_j`）の傾きで広げる。各帯域の寄与は、線形の重みの和で割ってから 0.4 乗で足し、和を 1/0.4 乗に
//!    戻して、すべて 1 の入力を広げたもので割る（BS.1387-1 の式の形）。結果を `Es` とする。
//! 7. **時間の広がり**：`E_k ← a·E_k + (1 − a)·Es_k`（はじめは 0）、`a = exp(−1024 / fs / τ)`、
//!    `τ = 0.008 + (100 / fc)·(0.030 − 0.008)` 秒。マスクの元は `max(E_k, Es_k)`。
//! 8. **マスク**：`M = max(E, Es) / 10^(m/10)`。m は帯域の番号 k について `k·0.25 ≤ 12` なら 3 dB、それより上は
//!    `0.25·k·0.25` dB。
//! 9. **フレームの NMR**：帯域の平均 `(1/109)·Σ Pn/M`（線形）。帯域の最大 `max Pn/M` が 1.5 dB を超えるフレームを
//!    「乱れたフレーム」と数える。
//!
//! 集計（dB は `10·log10`、0 以下は [`FLOOR_DB`] の −300 dB。負ほど良い）：
//!
//! - **全体**：チャンネルごとにフレームの NMR の線形の平均を dB にし、チャンネルで平均する（PEAQ の NMRtotB に近いもの）。
//! - **中央値・p95・最悪**：すべてのチャンネル・フレームのフレームの NMR（dB）の 50・95 百分位（numpy の `percentile` の
//!   既定の線形の補間）と最大。最悪にはそのフレームの始まりの秒（番号 × 1,024 / fs）とチャンネルを添える。
//! - **乱れたフレームの割合**：チャンネルごとの割合をチャンネルで平均（RelDistFramesB）。
//!
//! 周波数の広がりの `pow` は、傾きが等比になることを使って帯域ごとに数回に減らした（式は同じ。丸めの差だけ）。
//! NMR は低域通過（帯域の上限）をほとんど見ない（帯域の平均に薄まる）ので、上限を別に記録する。
//!
//! # スペクトルの指標（[`spectral`]）
//!
//! 周期の Hann `0.5 − 0.5·cos(2πn/N)`（2,048 点・半分の重なり）の電力スペクトルを、チャンネルで足して使う。
//!
//! - **帯域の上限**：全フレームを足した長い時間の電力スペクトルで、出力の [f, fs/2] のエネルギーが元の [f, fs/2] の
//!   1/100（−20 dB）を下回る最小のビンの周波数 f（= ビンの番号 × fs / 2,048）。ただし f > 1 kHz で、元の [f, fs/2] に
//!   全体の 10⁻⁶ より多いエネルギーがある範囲に限る。見つからなければ「なし」（低域通過が見つからない）。
//!   libvorbis は段ごとに決まった周波数で切る（aoTuV の `modes/psych_44.h` の `_psy_lowpass_44`。44.1 kHz で q0 約 14.7 kHz、
//!   q3 約 17.1 kHz、q6 以上は切らない）。
//! - **LSD**：50 Hz〜16 kHz のビンについて、元のフレームの電力（全ビンの和）が全フレームの最大の 10⁻⁶（−60 dB）より大きい
//!   フレームで、そのフレームの元の最大のビンの 10⁻⁸（−80 dB）より大きいビンの `10·log10(元 / 出力)`（出力は同じ −80 dB の
//!   床で下を切る）の二乗平均の平方根を求め、フレームで平均する（dB）。元が無音なら出さない。
//! - **帯域ごとの SNR**：0–1・1–4・4–8・8–12・12–16・16–20 kHz（[`SNR_BANDS`]）で、元の電力と、複素スペクトルの差（位相を
//!   含む波形の誤差）の電力の比。
//!
//! # そのほか
//!
//! - **SNR**（[`snr_db`]）：波形の誤差の全体の比。知覚符号化の品質は表さない（バグの検出だけに使う。チャンネルの取り違え・
//!   符号・1 標本のずれなら数 dB 以下になる）。
//! - **同じビットレートでの比較**：基準の族の（音声 kbps, NMR 全体）を率の順に並べ、NMR がそれまでより良い点だけを残した
//!   包絡を曲線にする（[`envelope`]）。ΔNMR は比べる側の点のビットレートで、包絡を log(kbps) に対して区分線形に補間した NMR
//!   との差（[`nmr_at_rate`]。正は悪い、範囲の外は出さない）。BD-rate は NMR を品質の軸にして、重なる NMR の範囲を 200 点に
//!   分けて log(rate) の差を平均した d から `(e^d − 1)·100` %（[`bd_rate`]。正は同じ NMR に多くのビットが要る）。

use libm::{asinh, cos, exp, log, log10, pow, sin, sinh};

/// フレームの長さ（標本）。
pub const N: usize = 2048;
/// フレームの進み（標本）。
pub const HOP: usize = 1024;
/// 聴取のレベルの仮定：1019.5 Hz の満幅の正弦が 92 dB SPL。
pub const LISTENING_DB_SPL: f64 = 92.0;
/// 位置のずれを探す範囲（標本）。
pub const MAX_LAG: usize = 4096;
/// 位置のずれを探すのに使う長さの上限（先頭から）。
const LAG_WINDOW: usize = 1 << 17;
/// dB に直すときの下限（0 を −∞ にせず −300 dB にする）。
pub const FLOOR_DB: f64 = -300.0;

const PI: f64 = std::f64::consts::PI;

/// `10·log10(x)`。0 以下は `FLOOR_DB`。
pub fn db(x: f64) -> f64 {
    if x > 1e-30 { 10.0 * log10(x) } else { FLOOR_DB }
}

/// 基数 2 の複素 FFT（その場、ビット反転のあと蝶）。回転因子は libm の sin・cos。
pub struct Fft {
    n: usize,
    tw: Vec<(f64, f64)>,
    rev: Vec<usize>,
}

impl Fft {
    /// `n` 点（2 の冪、2 以上）。
    pub fn new(n: usize) -> Result<Fft, String> {
        if n < 2 || !n.is_power_of_two() {
            return Err(format!("FFT の長さ {n} は 2 の冪ではありません"));
        }
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let tw = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                (cos(a), sin(a))
            })
            .collect();
        Ok(Fft { n, tw, rev })
    }

    /// 順方向（符号 −）。`re`・`im` の長さは `n`。
    pub fn forward(&self, re: &mut [f64], im: &mut [f64]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            let half = len / 2;
            for s in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = self.tw[k * step];
                    let (a, b) = (s + k, s + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }

    /// 逆方向（1/n で割る）。
    pub fn inverse(&self, re: &mut [f64], im: &mut [f64]) {
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.forward(re, im);
        let s = 1.0 / self.n as f64;
        for (r, i) in re.iter_mut().zip(im.iter_mut()) {
            *r *= s;
            *i = -*i * s;
        }
    }

    /// 窓をかけた実数の列（長さ `n`）の複素スペクトル（0〜n/2）。
    fn spectrum(&self, x: &[f64], w: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let mut re: Vec<f64> = x.iter().zip(w).map(|(a, b)| a * b).collect();
        let mut im = vec![0.0; self.n];
        self.forward(&mut re, &mut im);
        re.truncate(self.n / 2 + 1);
        im.truncate(self.n / 2 + 1);
        (re, im)
    }

    /// 窓をかけた実数の列の振幅スペクトル（0〜n/2）。
    fn mag(&self, x: &[f64], w: &[f64]) -> Vec<f64> {
        let (re, im) = self.spectrum(x, w);
        re.iter()
            .zip(&im)
            .map(|(r, i)| (r * r + i * i).sqrt())
            .collect()
    }
}

/// 信号の終わりまで覆うフレームの数（`len` が `N` 以下なら 1）。
pub fn frame_count(len: usize) -> usize {
    if len <= N {
        1
    } else {
        1 + (len - N).div_ceil(HOP)
    }
}

/// `n` 番目のフレーム（範囲の外は 0）。
fn frame(x: &[f64], n: usize) -> Vec<f64> {
    let start = n * HOP;
    let mut out = vec![0.0; N];
    if start < x.len() {
        let end = (start + N).min(x.len());
        out[..end - start].copy_from_slice(&x[start..end]);
    }
    out
}

/// NMR の結果（PEAQ の NMRtotB に近いもの）。
#[derive(Debug, Clone, PartialEq)]
pub struct Nmr {
    /// 全体：チャンネルごとに、フレームの NMR の線形の平均を dB にし、チャンネルで平均したもの。
    pub total_db: f64,
    /// フレームの NMR（dB。すべてのチャンネルとフレーム）の中央値。
    pub median_db: f64,
    /// 同じく 95 百分位（numpy の線形の補間と同じ）。
    pub p95_db: f64,
    /// 同じく最悪（最大）。
    pub max_db: f64,
    /// 最悪のフレームの始まり（秒）。
    pub worst_at_s: f64,
    /// 最悪のフレームのチャンネル。
    pub worst_ch: usize,
    /// 乱れたフレームの割合（帯域の最大の NMR が 1.5 dB を超えるフレーム。チャンネルで平均。RelDistFramesB）。
    pub rel_dist_frames: f64,
    /// 1 チャンネルあたりのフレームの数。
    pub frames: usize,
}

/// PEAQ の基本版の FFT の耳のモデル（1 つのサンプルレートについて前計算したもの）。
pub struct Model {
    fs: f64,
    fft: Fft,
    w: Vec<f64>,
    fac: f64,
    ear: Vec<f64>,
    /// 帯域ごとの（ビン, 重み）。
    g: Vec<Vec<(usize, f64)>>,
    pth: Vec<f64>,
    fc: Vec<f64>,
    a: Vec<f64>,
    gm: Vec<f64>,
    norm: Vec<f64>,
    dz: f64,
    /// 下の傾き `10^(−27·m·dz/10)`（m = 0..Z）。
    lower: Vec<f64>,
    /// 同じく 0.4 乗。
    lower04: Vec<f64>,
}

impl Model {
    /// サンプルレート `fs` のモデルを作る。
    pub fn new(fs: u32) -> Result<Model, String> {
        let fs = f64::from(fs);
        let fft = Fft::new(N)?;
        let w: Vec<f64> = (0..N)
            .map(|n| {
                0.5 * (1.0 - cos(2.0 * PI * n as f64 / (N as f64 - 1.0))) * (8.0f64 / 3.0).sqrt()
            })
            .collect();
        let sine: Vec<f64> = (0..N)
            .map(|n| sin(2.0 * PI * 1019.5 * n as f64 / fs))
            .collect();
        let xmax = fft.mag(&sine, &w).into_iter().fold(0.0, f64::max);
        let fac = pow(10.0, LISTENING_DB_SPL / 20.0) / xmax;
        let nb = N / 2 + 1;
        let df = fs / N as f64;
        let ear = (0..nb)
            .map(|k| {
                let f = (k as f64 * df).max(1.0) / 1000.0;
                pow(
                    10.0,
                    (-0.6 * 3.64 * pow(f, -0.8) + 6.5 * exp(-0.6 * (f - 3.3) * (f - 3.3))
                        - 1e-3 * pow(f, 3.6))
                        / 20.0,
                )
            })
            .collect();
        let dz = 0.25;
        let zl = 7.0 * asinh(80.0 / 650.0);
        let zu = 7.0 * asinh(18000.0 / 650.0);
        let z = ((zu - zl) / dz).ceil() as usize;
        let edge = |i: usize| 650.0 * sinh((zl + dz * i as f64).min(zu) / 7.0);
        let fc: Vec<f64> = (0..z)
            .map(|k| 650.0 * sinh((zl + dz * (k as f64 + 0.5)) / 7.0))
            .collect();
        let g = (0..z)
            .map(|k| {
                let (fl, fu) = (edge(k), edge(k + 1));
                (0..nb)
                    .filter_map(|b| {
                        let f = b as f64 * df;
                        let v =
                            ((fu.min(f + df / 2.0) - fl.max(f - df / 2.0)) / df).clamp(0.0, 1.0);
                        (v > 0.0).then_some((b, v))
                    })
                    .collect()
            })
            .collect();
        let pth = fc
            .iter()
            .map(|&f| pow(10.0, 0.4 * 0.364 * pow(f / 1000.0, -0.8)))
            .collect();
        let a = fc
            .iter()
            .map(|&f| {
                let tau = 0.008 + 100.0 / f * (0.030 - 0.008);
                exp(-(HOP as f64) / fs / tau)
            })
            .collect();
        let gm = (0..z)
            .map(|k| {
                let zz = k as f64 * dz;
                pow(10.0, if zz <= 12.0 { 3.0 } else { 0.25 * zz } / 10.0)
            })
            .collect();
        let lower = (0..=z)
            .map(|m| pow(10.0, -27.0 * m as f64 * dz / 10.0))
            .collect();
        let lower04 = (0..=z)
            .map(|m| pow(10.0, -27.0 * m as f64 * dz * 0.4 / 10.0))
            .collect();
        let mut m = Model {
            fs,
            fft,
            w,
            fac,
            ear,
            g,
            pth,
            fc,
            a,
            gm,
            norm: vec![1.0; z],
            dz,
            lower,
            lower04,
        };
        m.norm = m.spread(&vec![1.0; z]);
        Ok(m)
    }

    /// 周波数の広がり（BS.1387-1 の式 (2.1.6) の形。0.4 乗の和）。
    /// 下の傾きは 27 dB/Bark、上は 24 + 230/fc − 0.2L dB/Bark。どちらも Bark の差に比例するので、
    /// 帯域が 1 つ離れるごとに一定の比で減る（等比）。
    fn spread(&self, p: &[f64]) -> Vec<f64> {
        let z = p.len();
        let mut acc = vec![0.0; z];
        for (j, (&pj, &fc)) in p.iter().zip(&self.fc).enumerate() {
            let l = 10.0 * log10(pj);
            let su = 24.0 + 230.0 / fc - 0.2 * l;
            let ru = pow(10.0, -su * self.dz / 10.0);
            let ru04 = pow(10.0, -su * self.dz * 0.4 / 10.0);
            // 線形の広がりの和（k = 0..z の順）
            let mut s = 0.0;
            for k in 0..j {
                s += self.lower[j - k];
            }
            let mut e = 1.0;
            for _ in j..z {
                s += e;
                e *= ru;
            }
            let base = pow(pj / s, 0.4);
            for (k, v) in acc.iter_mut().enumerate().take(j) {
                *v += base * self.lower04[j - k];
            }
            let mut e04 = 1.0;
            for v in acc.iter_mut().skip(j) {
                *v += base * e04;
                e04 *= ru04;
            }
        }
        acc.iter()
            .zip(&self.norm)
            .map(|(v, n)| pow(*v, 1.0 / 0.4) / n)
            .collect()
    }

    fn bands(&self, spec: &[f64]) -> Vec<f64> {
        self.g
            .iter()
            .map(|row| row.iter().map(|&(b, v)| v * spec[b]).sum())
            .collect()
    }

    /// 1 チャンネル：フレームごとの NMR（帯域の平均の線形の比）と、乱れたフレームの数。
    fn channel(&self, r: &[f64], t: &[f64]) -> (Vec<f64>, usize) {
        let z = self.fc.len();
        let frames = frame_count(r.len().max(t.len()));
        let mut prev = vec![0.0; z];
        let mut out = Vec::with_capacity(frames);
        let mut dist = 0usize;
        for n in 0..frames {
            let fr = self.fft.mag(&frame(r, n), &self.w);
            let ft = self.fft.mag(&frame(t, n), &self.w);
            let er: Vec<f64> = fr
                .iter()
                .zip(&self.ear)
                .map(|(x, e)| x * self.fac * e)
                .collect();
            let et: Vec<f64> = ft
                .iter()
                .zip(&self.ear)
                .map(|(x, e)| x * self.fac * e)
                .collect();
            let pe: Vec<f64> = self
                .bands(&er.iter().map(|x| x * x).collect::<Vec<_>>())
                .iter()
                .zip(&self.pth)
                .map(|(a, b)| a + b)
                .collect();
            let pn = self.bands(
                &er.iter()
                    .zip(&et)
                    .map(|(a, b)| (a - b) * (a - b))
                    .collect::<Vec<_>>(),
            );
            let es = self.spread(&pe);
            let (mut loc, mut mx) = (0.0, 0.0f64);
            for k in 0..z {
                prev[k] = self.a[k] * prev[k] + (1.0 - self.a[k]) * es[k];
                let e = prev[k].max(es[k]);
                let ratio = pn[k] / (e / self.gm[k]);
                loc += ratio;
                mx = mx.max(ratio);
            }
            out.push(loc / z as f64);
            if db(mx) > 1.5 {
                dist += 1;
            }
        }
        (out, dist)
    }

    /// 元 `r` と試す側 `t`（チャンネルごと。同じ数・同じ長さにそろえたもの）の NMR。
    pub fn nmr(&self, r: &[Vec<f64>], t: &[Vec<f64>]) -> Nmr {
        let mut total = 0.0;
        let mut rdf = 0.0;
        let mut all: Vec<f64> = Vec::new();
        let (mut max_db, mut worst_at, mut worst_ch) = (f64::NEG_INFINITY, 0usize, 0usize);
        let mut frames = 0;
        let ch = r.len().max(1);
        for (c, (rc, tc)) in r.iter().zip(t).enumerate() {
            let (loc, dist) = self.channel(rc, tc);
            frames = loc.len();
            let mean = loc.iter().fold(0.0, |s, v| s + v) / loc.len() as f64;
            total += db(mean);
            rdf += dist as f64 / loc.len() as f64;
            for (n, v) in loc.iter().enumerate() {
                let d = db(*v);
                if d > max_db {
                    (max_db, worst_at, worst_ch) = (d, n, c);
                }
                all.push(d);
            }
        }
        all.sort_by(f64::total_cmp);
        Nmr {
            total_db: total / ch as f64,
            median_db: percentile(&all, 50.0),
            p95_db: percentile(&all, 95.0),
            max_db,
            worst_at_s: (worst_at * HOP) as f64 / self.fs,
            worst_ch,
            rel_dist_frames: rdf / ch as f64,
            frames,
        }
    }
}

/// 並べた列の百分位（numpy の `percentile` の既定（線形の補間）と同じ）。空なら NaN。
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

/// 中央値（並べ替えてから）。空なら NaN。
pub fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    percentile(&v, 50.0)
}

/// 帯域ごとの SNR の帯域（Hz）。
pub const SNR_BANDS: [(u32, u32); 6] = [
    (0, 1000),
    (1000, 4000),
    (4000, 8000),
    (8000, 12000),
    (12000, 16000),
    (16000, 20000),
];

/// スペクトルの指標。
#[derive(Debug, Clone, PartialEq)]
pub struct Spectral {
    /// 帯域の上限（Hz）。`None` は低域通過が見つからない（元にエネルギーがある範囲で、上が −20 dB を下回らない）。
    pub cutoff_hz: Option<f64>,
    /// LSD（dB）。元が無音なら `None`。
    pub lsd_db: Option<f64>,
    /// 帯域ごとの SNR（dB。位相を含む波形の誤差）。元のその帯域が無音なら `None`。
    pub band_snr_db: Vec<Option<f64>>,
}

/// 帯域の上限・LSD・帯域ごとの SNR（周期の Hann、チャンネルの電力を足したもの）。
pub fn spectral(r: &[Vec<f64>], t: &[Vec<f64>], fs: u32) -> Result<Spectral, String> {
    let fft = Fft::new(N)?;
    let w: Vec<f64> = (0..N)
        .map(|n| 0.5 - 0.5 * cos(2.0 * PI * n as f64 / N as f64))
        .collect();
    let nb = N / 2 + 1;
    let len = r.iter().chain(t).map(Vec::len).max().unwrap_or(0);
    let frames = frame_count(len);
    let mut pr = vec![vec![0.0; nb]; frames];
    let mut pd = vec![vec![0.0; nb]; frames];
    let mut pe = vec![vec![0.0; nb]; frames];
    for (rc, tc) in r.iter().zip(t) {
        for n in 0..frames {
            let (ar, ai) = fft.spectrum(&frame(rc, n), &w);
            let (br, bi) = fft.spectrum(&frame(tc, n), &w);
            for k in 0..nb {
                pr[n][k] += ar[k] * ar[k] + ai[k] * ai[k];
                pd[n][k] += br[k] * br[k] + bi[k] * bi[k];
                let (er, ei) = (ar[k] - br[k], ai[k] - bi[k]);
                pe[n][k] += er * er + ei * ei;
            }
        }
    }
    let fs = f64::from(fs);
    let freq = |k: usize| k as f64 * fs / N as f64;

    // 帯域の上限：[f, fs/2] の出力のエネルギーが元の −20 dB を下回る最小の f
    // （f > 1 kHz、元に全体の 1e-6 以上のエネルギーがある範囲で）
    let tot_r: Vec<f64> = (0..nb).map(|k| pr.iter().map(|f| f[k]).sum()).collect();
    let tot_d: Vec<f64> = (0..nb).map(|k| pd.iter().map(|f| f[k]).sum()).collect();
    let mut cr = vec![0.0; nb];
    let mut cd = vec![0.0; nb];
    let (mut sr, mut sd) = (0.0, 0.0);
    for k in (0..nb).rev() {
        sr += tot_r[k];
        sd += tot_d[k];
        cr[k] = sr;
        cd[k] = sd;
    }
    let cutoff_hz = (0..nb)
        .find(|&k| cr[k] > cr[0] * 1e-6 && freq(k) > 1000.0 && cd[k] < cr[k] * 0.01)
        .map(freq);

    // LSD：50 Hz〜16 kHz、元のフレームの電力が最大の −60 dB 以上のフレーム、フレームの最大の −80 dB 以上のビン
    let fr_e: Vec<f64> = pr.iter().map(|f| f.iter().sum()).collect();
    let fr_max = fr_e.iter().copied().fold(0.0, f64::max);
    let bins: Vec<usize> = (0..nb)
        .filter(|&k| (50.0..=16000.0).contains(&freq(k)))
        .collect();
    let mut lsd_sum = 0.0;
    let mut lsd_n = 0usize;
    for n in 0..frames {
        if fr_max <= 0.0 || fr_e[n] <= fr_max * 1e-6 {
            continue;
        }
        let amax = bins.iter().map(|&k| pr[n][k]).fold(0.0, f64::max);
        if amax <= 0.0 {
            continue;
        }
        let floor = amax * 1e-8;
        let (mut s, mut c) = (0.0, 0usize);
        for &k in &bins {
            let a = pr[n][k];
            if a > floor {
                let d = 10.0 * log10(a.max(floor) / pd[n][k].max(floor));
                s += d * d;
                c += 1;
            }
        }
        lsd_sum += (s / c.max(1) as f64).sqrt();
        lsd_n += 1;
    }
    let lsd_db = (lsd_n > 0).then(|| lsd_sum / lsd_n as f64);

    let band_snr_db = SNR_BANDS
        .iter()
        .map(|&(lo, hi)| {
            let (lo, hi) = (f64::from(lo), f64::from(hi));
            let (mut er, mut ee) = (0.0, 0.0);
            for k in (0..nb).filter(|&k| freq(k) >= lo && freq(k) < hi) {
                for n in 0..frames {
                    er += pr[n][k];
                    ee += pe[n][k];
                }
            }
            (er > 1e-9 && ee > 0.0).then(|| 10.0 * log10(er / ee))
        })
        .collect();
    Ok(Spectral {
        cutoff_hz,
        lsd_db,
        band_snr_db,
    })
}

/// 波形の SNR（dB。すべてのチャンネル）。誤差が 0 なら `FLOOR_DB` の符号を返した +300 dB。
pub fn snr_db(r: &[Vec<f64>], t: &[Vec<f64>]) -> f64 {
    let (mut s, mut e) = (0.0, 0.0);
    for (rc, tc) in r.iter().zip(t) {
        for (a, b) in rc.iter().zip(tc) {
            s += a * a;
            e += (a - b) * (a - b);
        }
    }
    if e <= 0.0 {
        -FLOOR_DB
    } else if s <= 0.0 {
        FLOOR_DB
    } else {
        10.0 * log10(s / e)
    }
}

/// 位置のずれ：相互相関（チャンネルで足したもの。先頭の `LAG_WINDOW` 標本）の絶対値が最大になる遅れ（±`MAX_LAG`）。
/// 正なら試す側が遅れている（`t[i + lag] ≈ r[i]`）。同点は 0 に近いほう、相関がすべて 0 なら 0。
pub fn lag(r: &[Vec<f64>], t: &[Vec<f64>]) -> Result<i64, String> {
    let len = r.first().map_or(0, |c| c.len().min(LAG_WINDOW));
    if len == 0 {
        return Ok(0);
    }
    let n = (len + MAX_LAG).next_power_of_two();
    let fft = Fft::new(n)?;
    let mut cre = vec![0.0; n];
    let mut cim = vec![0.0; n];
    for (rc, tc) in r.iter().zip(t) {
        let mut ar = vec![0.0; n];
        let mut ai = vec![0.0; n];
        ar[..len].copy_from_slice(&rc[..len]);
        let mut br = vec![0.0; n];
        let mut bi = vec![0.0; n];
        let tl = tc.len().min(len);
        br[..tl].copy_from_slice(&tc[..tl]);
        fft.forward(&mut ar, &mut ai);
        fft.forward(&mut br, &mut bi);
        for k in 0..n {
            // B · conj(A)
            cre[k] += br[k] * ar[k] + bi[k] * ai[k];
            cim[k] += bi[k] * ar[k] - br[k] * ai[k];
        }
    }
    fft.inverse(&mut cre, &mut cim);
    let at = |m: i64| -> f64 {
        let idx = if m < 0 { n as i64 + m } else { m } as usize;
        cre[idx].abs()
    };
    let mut best = (0i64, at(0));
    for d in 1..=MAX_LAG as i64 {
        for m in [-d, d] {
            let v = at(m);
            if v > best.1 {
                best = (m, v);
            }
        }
    }
    Ok(best.0)
}

/// 試す側を `lag` だけずらして元にそろえる（正なら先頭を捨て、負なら 0 を足す）。
pub fn align(t: &[Vec<f64>], lag: i64) -> Vec<Vec<f64>> {
    t.iter()
        .map(|c| {
            if lag >= 0 {
                c.get(lag as usize..).unwrap_or(&[]).to_vec()
            } else {
                let mut v = vec![0.0; lag.unsigned_abs() as usize];
                v.extend_from_slice(c);
                v
            }
        })
        .collect()
}

/// 区分線形の補間（`xs` は昇順）。範囲の外は `None`。
pub fn interp(xs: &[f64], ys: &[f64], x: f64) -> Option<f64> {
    let (first, last) = (*xs.first()?, *xs.last()?);
    if x < first || x > last {
        return None;
    }
    for i in 1..xs.len() {
        if x <= xs[i] {
            let (x0, x1, y0, y1) = (xs[i - 1], xs[i], ys[i - 1], ys[i]);
            return Some(if x1 > x0 {
                y0 + (y1 - y0) * (x - x0) / (x1 - x0)
            } else {
                y1
            });
        }
    }
    Some(ys[0])
}

/// 率と品質の曲線（kbps, NMR 全体 dB）。率の順に並べ、NMR がそれまでより良い（小さい）点だけを残した包絡。
pub fn envelope(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (kb, m) in pts {
        if out.last().is_none_or(|&(_, best)| m < best) {
            out.push((kb, m));
        }
    }
    out
}

/// 包絡の上で、ビットレート `kbps` のときの NMR（log(kbps) で区分線形）。範囲の外は `None`。
pub fn nmr_at_rate(env: &[(f64, f64)], kbps: f64) -> Option<f64> {
    if kbps <= 0.0 || env.iter().any(|p| p.0 <= 0.0) {
        return None;
    }
    let xs: Vec<f64> = env.iter().map(|p| log(p.0)).collect();
    let ys: Vec<f64> = env.iter().map(|p| p.1).collect();
    interp(&xs, &ys, log(kbps))
}

/// BD-rate（%）：NMR を品質の軸にして、重なる範囲で log(rate) を NMR の区分線形の関数として平均した差。
/// 正なら `test` は `reference` より同じ NMR に多くのビットが要る。重ならないか点が 2 つ未満なら `None`。
pub fn bd_rate(reference: &[(f64, f64)], test: &[(f64, f64)]) -> Option<f64> {
    if reference.len() < 2 || test.len() < 2 {
        return None;
    }
    // 包絡は率の順に NMR が減るので、NMR の昇順に並べ直す
    let curve = |c: &[(f64, f64)]| -> Option<(Vec<f64>, Vec<f64>)> {
        if c.iter().any(|p| p.0 <= 0.0) {
            return None;
        }
        let mut v: Vec<(f64, f64)> = c.iter().map(|p| (p.1, log(p.0))).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        Some(v.into_iter().unzip())
    };
    let (rx, ry) = curve(reference)?;
    let (tx, ty) = curve(test)?;
    let lo = rx[0].max(tx[0]);
    let hi = rx[rx.len() - 1].min(tx[tx.len() - 1]);
    if lo >= hi {
        return None;
    }
    const STEPS: usize = 200;
    let mut sum = 0.0;
    for i in 0..STEPS {
        let q = lo + (hi - lo) * i as f64 / (STEPS - 1) as f64;
        sum += interp(&tx, &ty, q)? - interp(&rx, &ry, q)?;
    }
    Some((exp(sum / STEPS as f64) - 1.0) * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::rand_core::Rng;

    fn noise(len: usize, seed: u64, stream: u64) -> Vec<f64> {
        let mut rng = kura_engine::determinism::rng_stream(seed, stream);
        (0..len)
            .map(|_| rng.next_u32() as f64 / 2_147_483_648.0 - 1.0)
            .collect()
    }

    /// 音楽に近いもの：和音と雑音の打撃。
    fn music(len: usize, fs: u32) -> Vec<Vec<f64>> {
        (0..2)
            .map(|c| {
                let n = noise(len, 7, c);
                (0..len)
                    .map(|i| {
                        let t = i as f64 / f64::from(fs);
                        let beat = (i % (fs as usize / 4)) as f64 / f64::from(fs);
                        0.2 * sin(2.0 * PI * 220.0 * t)
                            + 0.15 * sin(2.0 * PI * 330.0 * t)
                            + 0.1 * sin(2.0 * PI * 440.0 * (1.0 + 0.001 * c as f64) * t)
                            + 0.3 * n[i] * exp(-beat * 40.0)
                    })
                    .collect()
            })
            .collect()
    }

    fn add_noise(x: &[Vec<f64>], level_db: f64, seed: u64) -> Vec<Vec<f64>> {
        let g = pow(10.0, level_db / 20.0);
        x.iter()
            .enumerate()
            .map(|(c, ch)| {
                let n = noise(ch.len(), seed, c as u64);
                ch.iter().zip(n).map(|(a, b)| a + g * b).collect()
            })
            .collect()
    }

    /// FFT：正弦は そのビンに n/2、逆変換で元に戻る。
    #[test]
    fn fft_round_trip_and_bin() {
        let fft = Fft::new(64).unwrap();
        let x: Vec<f64> = (0..64)
            .map(|i| cos(2.0 * PI * 5.0 * i as f64 / 64.0))
            .collect();
        let mut re = x.clone();
        let mut im = vec![0.0; 64];
        fft.forward(&mut re, &mut im);
        assert!((re[5] - 32.0).abs() < 1e-9 && (re[59] - 32.0).abs() < 1e-9);
        assert!(
            re.iter()
                .enumerate()
                .all(|(k, v)| k == 5 || k == 59 || v.abs() < 1e-9)
        );
        fft.inverse(&mut re, &mut im);
        for (a, b) in re.iter().zip(&x) {
            assert!((a - b).abs() < 1e-12);
        }
        assert!(Fft::new(48).is_err());
    }

    /// 百分位は numpy の線形の補間と同じ。
    #[test]
    fn percentile_matches_numpy() {
        let v = [1.0, 2.0, 3.0, 4.0, 10.0];
        assert_eq!(percentile(&v, 50.0), 3.0);
        assert!((percentile(&v, 95.0) - 8.8).abs() < 1e-12); // numpy.percentile([1,2,3,4,10], 95) = 8.8
        assert_eq!(percentile(&v, 100.0), 10.0);
        assert_eq!(percentile(&[5.0], 95.0), 5.0);
        assert_eq!(median(&[3.0, 1.0, 2.0, 4.0]), 2.5);
    }

    /// 元と同じなら NMR は −∞ に近い（−300 dB に張り付く）、乱れたフレームなし、LSD 0、上限なし、SNR は +300 dB。
    #[test]
    fn identical_is_perfect() {
        let fs = 48_000;
        let x = music(fs as usize / 2, fs);
        let m = Model::new(fs).unwrap();
        let r = m.nmr(&x, &x);
        assert!(r.total_db <= -299.0, "{r:?}");
        assert!(r.max_db <= -299.0);
        assert_eq!(r.rel_dist_frames, 0.0);
        assert_eq!(r.frames, frame_count(x[0].len()));
        let s = spectral(&x, &x, fs).unwrap();
        assert_eq!(s.cutoff_hz, None);
        assert_eq!(s.lsd_db, Some(0.0));
        assert_eq!(snr_db(&x, &x), 300.0);
        assert_eq!(lag(&x, &x).unwrap(), 0);
    }

    /// 足した雑音が 20 dB 大きくなると、NMR もおよそ 20 dB 悪くなり、乱れたフレームが増える。
    #[test]
    fn nmr_tracks_added_noise() {
        let fs = 44_100;
        let x = music(fs as usize / 2, fs);
        let m = Model::new(fs).unwrap();
        let a = m.nmr(&x, &add_noise(&x, -80.0, 3));
        let b = m.nmr(&x, &add_noise(&x, -60.0, 3));
        let c = m.nmr(&x, &add_noise(&x, -40.0, 3));
        assert!(a.total_db < b.total_db && b.total_db < c.total_db);
        let d1 = b.total_db - a.total_db;
        let d2 = c.total_db - b.total_db;
        assert!((17.0..23.0).contains(&d1), "{d1}");
        assert!((15.0..25.0).contains(&d2), "{d2}");
        assert!(c.rel_dist_frames > 0.5, "{c:?}");
        assert!(a.rel_dist_frames <= b.rel_dist_frames && b.rel_dist_frames <= c.rel_dist_frames);
        assert!(a.median_db <= a.p95_db && a.p95_db <= a.max_db);
        // 白色雑音の SNR はおよそ信号と雑音の電力の比
        let snr = snr_db(&x, &add_noise(&x, -40.0, 3));
        let sig = x.iter().flatten().map(|v| v * v).sum::<f64>() / (2 * x[0].len()) as f64;
        let expect = 10.0 * log10(sig / (1e-4 / 3.0));
        assert!((snr - expect).abs() < 0.5, "{snr} {expect}");
    }

    /// 1 か所だけの乱れ：最悪のフレームの時刻がその位置を含む。
    #[test]
    fn worst_frame_locates_a_click() {
        let fs = 48_000;
        let x = music(fs as usize, fs);
        let mut y = x.clone();
        let at = 30_000usize;
        let burst = noise(200, 11, 1);
        for (i, b) in burst.iter().enumerate() {
            y[1][at + i] += 0.5 * b;
        }
        let r = Model::new(fs).unwrap().nmr(&x, &y);
        let start = r.worst_at_s * f64::from(fs);
        assert_eq!(r.worst_ch, 1);
        assert!(
            start <= at as f64 && at as f64 + 200.0 <= start + N as f64,
            "{r:?}"
        );
        assert!(r.max_db > r.p95_db);
    }

    /// 帯域を切った雑音：上限がその周波数。定義（上の帯域のエネルギーが元の 1% を下回る最小の f）から、白色雑音では
    /// 切った周波数 fc より (fs/2 − fc) の 1% だけ低く出る（±2 ビン）。
    #[test]
    fn cutoff_finds_lowpass() {
        let fs = 48_000;
        let len = 1 << 15;
        let fft = Fft::new(len).unwrap();
        let x: Vec<Vec<f64>> = (0..2).map(|c| noise(len, 5, c)).collect();
        for cut in [8_000.0, 15_000.0] {
            let y: Vec<Vec<f64>> = x
                .iter()
                .map(|c| {
                    let mut re = c.clone();
                    let mut im = vec![0.0; len];
                    fft.forward(&mut re, &mut im);
                    for k in 0..len {
                        let f = k.min(len - k) as f64 * f64::from(fs) / len as f64;
                        if f >= cut {
                            re[k] = 0.0;
                            im[k] = 0.0;
                        }
                    }
                    fft.inverse(&mut re, &mut im);
                    re
                })
                .collect();
            let s = spectral(&x, &y, fs).unwrap();
            let got = s.cutoff_hz.unwrap();
            let bin = f64::from(fs) / N as f64;
            let expect = cut - 0.01 * (f64::from(fs) / 2.0 - cut);
            assert!((got - expect).abs() <= 2.0 * bin, "{cut}: {got}");
            // 切った帯域の上の SNR は 0 dB 付近、下は高い
            let hi = s.band_snr_db[5].unwrap();
            assert!(hi.abs() < 1.0 || cut > 16_000.0, "{hi}");
        }
    }

    /// LSD：一様に −6.02 dB にすると LSD はおよそ 6.02 dB。
    #[test]
    fn lsd_of_gain_change() {
        let fs = 48_000;
        let x: Vec<Vec<f64>> = (0..2).map(|c| noise(20_000, 9, c)).collect();
        let y: Vec<Vec<f64>> = x
            .iter()
            .map(|c| c.iter().map(|v| v * 0.5).collect())
            .collect();
        let s = spectral(&x, &y, fs).unwrap();
        let lsd = s.lsd_db.unwrap();
        assert!((lsd - 20.0 * log10(2.0)).abs() < 0.05, "{lsd}");
        assert_eq!(s.cutoff_hz, None);
        assert!((snr_db(&x, &y) - 20.0 * log10(2.0)).abs() < 1e-9);
    }

    /// 位置のずれ：遅らせた・早めたものを見つけ、そろえると元に戻る。
    #[test]
    fn lag_finds_shift() {
        let fs = 48_000;
        let x = music(fs as usize / 2, fs);
        for d in [37i64, -250, 1] {
            let y: Vec<Vec<f64>> = align(&x, -d);
            assert_eq!(lag(&x, &y).unwrap(), d);
            let back = align(&y, d);
            assert_eq!(&back[0][300..1300], &x[0][300..1300]);
        }
        let silent = vec![vec![0.0; 3000]; 2];
        assert_eq!(lag(&silent, &silent).unwrap(), 0);
    }

    /// 曲線：包絡は率の順に良くなる点だけ、同じ曲線の BD-rate は 0、同じ NMR に 2 倍の率が要れば +100%。
    #[test]
    fn curves_and_bd_rate() {
        let pts = [(64.0, -10.0), (80.0, -12.0), (72.0, -9.0), (128.0, -16.0)];
        let env = envelope(&pts);
        assert_eq!(env, vec![(64.0, -10.0), (80.0, -12.0), (128.0, -16.0)]);
        assert_eq!(nmr_at_rate(&env, 64.0), Some(-10.0));
        assert_eq!(nmr_at_rate(&env, 32.0), None);
        let mid = nmr_at_rate(&env, (64.0f64 * 80.0).sqrt()).unwrap();
        assert!((mid + 11.0).abs() < 1e-9);
        assert!(bd_rate(&env, &env).unwrap().abs() < 1e-9);
        let double: Vec<(f64, f64)> = env.iter().map(|p| (p.0 * 2.0, p.1)).collect();
        assert!((bd_rate(&env, &double).unwrap() - 100.0).abs() < 1e-6);
        assert_eq!(bd_rate(&env, &[(10.0, 5.0), (20.0, 4.0)]), None);
    }

    /// 同じ入力なら同じ数字（ビットまで）。
    #[test]
    fn deterministic() {
        let fs = 44_100;
        let x = music(10_000, fs);
        let y = add_noise(&x, -50.0, 4);
        let a = Model::new(fs).unwrap().nmr(&x, &y);
        let b = Model::new(fs).unwrap().nmr(&x, &y);
        assert_eq!(a.total_db.to_bits(), b.total_db.to_bits());
        assert_eq!(a.p95_db.to_bits(), b.p95_db.to_bits());
    }
}
