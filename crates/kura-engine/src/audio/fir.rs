//! 直線位相の FIR（M17 段 7。docs/editing.md §18.6、ADR-239）。式と定数は `audio.eq` v1 の直線位相と一緒に凍結する（変えるなら
//! 使う操作の版を上げる）。超越関数は `crate::math` だけ、平方根は IEEE の `sqrt`。
//!
//! - **長さの梯子**（[`Ladder`]）：タップ数 L = 2^k + 1（k = [`MIN_ORDER`]〜[`MAX_ORDER`]）、重なり保存の FFT の長さ
//!   N = 2^(k+1)、出力のブロック B = 2^k、遅れ D = 2^(k−1) = (L − 1)/2。B は音声のチャンク（2^16）を割り切るので、ブロックは
//!   チャンクをまたがない。k は最も狭い帯域 bw（Hz）から、L ≥ [`LENGTH_FACTOR`]·fs/bw を満たす最小のもの（上限に届いたら
//!   `capped`。低い帯の形が目標から少しずれる）。
//! - **設計**（[`design`]）：目標の振幅（零位相。実数で、2×2 の行列の成分なら負もある）を、N の [`OVERSAMPLE`] 倍の格子の
//!   ビン 0〜len/2 で標本化して受け、偶の実の列の逆変換で零位相のインパルス h0 を作り、|m| ≤ D に Kaiser の窓
//!   （β = [`KAISER_BETA`]）を掛けて切る。h[m] と h[−m] は同じ値（ビットまで）。重なり保存に使うスペクトル H は、h を円周の
//!   0 を中心に長さ N に置いて順変換した実部（虚部は対称なので 0。捨てる）。
//! - **応答**（[`Fir::response_db`]）：h を円周の 0 を中心に設計の格子の長さ（N × [`OVERSAMPLE`]）に置いて順変換し、ビンの
//!   間を周波数で直線に補う（曲線は目標ではなく実際の FIR の応答。§18.6。2N の格子では上限に届いたノッチの裾で補いの差が
//!   0.03 dB を超えた）。

use crate::audio::fft::{FftWork, RealFft};
use crate::audio::resample::i0;
use crate::codec::try_vec;
use crate::error::EngineError;
use crate::math;

/// 梯子のいちばん短い段（L = 257）。
pub const MIN_ORDER: u32 = 8;
/// 梯子のいちばん長い段（L = 65,537。48 kHz で遅れ D は 0.68 秒）。
pub const MAX_ORDER: u32 = 16;
/// 帯域あたりの長さの係数 C（L ≥ C·fs/bw）。段 7 で測って決めた（48 kHz の代表のバンドで、上限に届かないものは目標との差が
/// 0.07 dB 以内。ADR-239）。
pub const LENGTH_FACTOR: f64 = 16.0;
/// Kaiser の窓の β。段 7 で測って決めた（小さいほど帯の形が鋭く、ノッチも深い。カットの深い所はどの β でも目標の ±4 dB 内。
/// ADR-239）。
pub const KAISER_BETA: f64 = 4.0;
/// 設計の格子の細かさ（N の何倍で目標を標本化するか。時間の折り返しを窓の外へ押し出す）。
pub const OVERSAMPLE: usize = 4;

/// 長さの梯子の段（モジュールの説明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ladder {
    pub order: u32,
}

impl Ladder {
    /// 段 `order`（[`MIN_ORDER`]〜[`MAX_ORDER`] に収める）。
    pub fn new(order: u32) -> Self {
        Self {
            order: order.clamp(MIN_ORDER, MAX_ORDER),
        }
    }

    /// 帯域 `bw`（Hz。最も狭いもの）とレート `fs` の段（L ≥ C·fs/bw の最小。上限に届いたら真も返す）。帯域が 0 以下・
    /// 有限でなければいちばん短い段。
    pub fn for_bandwidth(bw: f64, fs: f64) -> (Self, bool) {
        Self::for_bandwidth_with(bw, fs, LENGTH_FACTOR)
    }

    /// [`Self::for_bandwidth`] の係数 C を渡す形（測り）。
    fn for_bandwidth_with(bw: f64, fs: f64, factor: f64) -> (Self, bool) {
        if !(bw > 0.0 && bw.is_finite() && fs > 0.0 && fs.is_finite()) {
            return (Self::new(MIN_ORDER), false);
        }
        let need = factor * fs / bw;
        for order in MIN_ORDER..=MAX_ORDER {
            let l = Self::new(order).taps() as f64;
            if l >= need {
                return (Self::new(order), false);
            }
        }
        (Self::new(MAX_ORDER), true)
    }

    /// タップ数 L = 2^k + 1。
    pub fn taps(self) -> usize {
        (1usize << self.order) + 1
    }

    /// 重なり保存の FFT の長さ N = 2^(k+1)。
    pub fn fft_len(self) -> usize {
        1usize << (self.order + 1)
    }

    /// 出力のブロック B = 2^k。
    pub fn block(self) -> usize {
        1usize << self.order
    }

    /// 遅れ D = 2^(k−1)。
    pub fn delay(self) -> usize {
        1usize << (self.order - 1)
    }

    /// 設計の格子の長さ（N × [`OVERSAMPLE`]）。
    pub fn design_len(self) -> usize {
        self.fft_len() * OVERSAMPLE
    }
}

/// 設計の格子（ビン 0〜len/2 の ω_k = 2πk/len の cos・sin。目標を標本化する呼び手が使う）。
#[derive(Debug, Clone)]
pub struct DesignGrid {
    pub ladder: Ladder,
    len: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
}

impl DesignGrid {
    /// 段 `ladder` の格子を作る（cos・sin は `math` で 1 度だけ）。
    pub fn new(ladder: Ladder) -> Result<Self, EngineError> {
        let len = ladder.design_len();
        let bins = len / 2 + 1;
        let mut cos = try_vec(bins, 0f64)?;
        let mut sin = try_vec(bins, 0f64)?;
        for k in 0..bins {
            let w = 2.0 * std::f64::consts::PI * (k as f64) / (len as f64);
            cos[k] = math::cos(w);
            sin[k] = math::sin(w);
        }
        Ok(Self {
            ladder,
            len,
            cos,
            sin,
        })
    }

    /// ビンの数（len/2 + 1）。
    pub fn bins(&self) -> usize {
        self.len / 2 + 1
    }

    /// ビン `k` の ω（ラジアン / 標本）。
    pub fn omega(&self, k: usize) -> f64 {
        2.0 * std::f64::consts::PI * (k as f64) / (self.len as f64)
    }

    /// ビン `k` の (cos ω, sin ω, cos 2ω, sin 2ω)（2ω が π を超えたら 2π − 2ω の表から）。
    pub fn cos_sin(&self, k: usize) -> (f64, f64, f64, f64) {
        let half = self.len / 2;
        let j = 2 * k;
        let (c2, s2) = if j <= half {
            (self.cos[j], self.sin[j])
        } else {
            let j = self.len - j;
            (self.cos[j], -self.sin[j])
        };
        (self.cos[k], self.sin[k], c2, s2)
    }
}

/// 設計した FIR 1 本（零位相。遅れ D を中心に置いた形で当てる）。
#[derive(Debug, Clone, PartialEq)]
pub struct Fir {
    pub ladder: Ladder,
    /// h[0]〜h[D]（h[−m] = h[m]）。
    pub taps: Vec<f64>,
    /// 重なり保存に使う H（長さ N の順変換の実部。ビン 0〜N/2）。
    pub spectrum: Vec<f64>,
}

/// 設計に使う FFT（設計の格子と応答・重なり保存の長さ。作るのに時間が掛かるので、同じ段なら使い回す）。
#[derive(Debug, Clone)]
pub struct DesignFfts {
    pub ladder: Ladder,
    pub design: RealFft,
    pub block: RealFft,
}

impl DesignFfts {
    pub fn new(ladder: Ladder) -> Result<Self, EngineError> {
        Ok(Self {
            ladder,
            design: RealFft::new(ladder.design_len())?,
            block: RealFft::new(ladder.fft_len())?,
        })
    }

    /// 表のバイト数。
    pub fn resident_bytes(&self) -> u64 {
        self.design
            .resident_bytes()
            .saturating_add(self.block.resident_bytes())
    }
}

/// Kaiser の窓 w[m]（m = 0〜D。w[0] = 1）。
fn kaiser(d: usize, beta: f64) -> Result<Vec<f64>, EngineError> {
    let mut w = try_vec(d + 1, 0f64)?;
    let den = i0(beta);
    for (m, v) in w.iter_mut().enumerate() {
        let x = m as f64 / d as f64;
        *v = i0(beta * math::max(1.0 - x * x, 0.0).sqrt()) / den;
    }
    Ok(w)
}

/// 目標 `target`（設計の格子のビン 0〜len/2 の零位相の振幅）から FIR を設計する（モジュールの説明）。
pub fn design(target: &[f64], ffts: &DesignFfts) -> Result<Fir, EngineError> {
    design_with(target, ffts, KAISER_BETA)
}

/// [`design`] の β を渡す形（測り）。
fn design_with(target: &[f64], ffts: &DesignFfts, beta: f64) -> Result<Fir, EngineError> {
    let ladder = ffts.ladder;
    let len = ladder.design_len();
    if target.len() != len / 2 + 1 {
        return Err(EngineError::Internal(format!(
            "FIR の目標のビンの数 {} が格子 {} と違います",
            target.len(),
            len / 2 + 1
        )));
    }
    let mut work = FftWork::default();
    // 零位相のインパルス（偶の実の列の逆変換）
    let zeros = try_vec(len / 2 + 1, 0f64)?;
    let mut h0 = try_vec(len, 0f64)?;
    ffts.design.inverse(target, &zeros, &mut h0, &mut work);
    // 窓を掛けて |m| ≤ D に切る（正の側の値だけを使う：h[−m] と h[m] をビットまで同じにする）
    let d = ladder.delay();
    let w = kaiser(d, beta)?;
    let mut taps = try_vec(d + 1, 0f64)?;
    for m in 0..=d {
        taps[m] = h0[m] * w[m];
    }
    let spectrum = centered_spectrum(&taps, &ffts.block, &mut work)?;
    Ok(Fir {
        ladder,
        taps,
        spectrum,
    })
}

/// 円周の 0 を中心に `fft` の長さに置いた h の順変換の実部（ビン 0〜len/2）。
fn centered_spectrum(
    taps: &[f64],
    fft: &RealFft,
    work: &mut FftWork,
) -> Result<Vec<f64>, EngineError> {
    let n = fft.len();
    let mut x = try_vec(n, 0f64)?;
    for (m, &v) in taps.iter().enumerate() {
        if m >= n {
            break;
        }
        x[m] = v;
        if m > 0 {
            x[n - m] = v;
        }
    }
    let mut re = try_vec(n / 2 + 1, 0f64)?;
    let mut im = try_vec(n / 2 + 1, 0f64)?;
    fft.forward(&x, &mut re, &mut im, work);
    Ok(re)
}

impl Fir {
    /// 実際の応答（dB。`floor` で止める）を周波数 `freqs`（Hz）ごとに：設計の格子の長さに置いた順変換の振幅を、ビンの間で
    /// 周波数に直線に補う（ナイキストを超える周波数はナイキストの値）。
    pub fn response_db(
        &self,
        fs: f64,
        freqs: &[f64],
        ffts: &DesignFfts,
        floor: f64,
    ) -> Result<Vec<f64>, EngineError> {
        let mut work = FftWork::default();
        let spec = centered_spectrum(&self.taps, &ffts.design, &mut work)?;
        let n = ffts.design.len();
        let last = spec.len() - 1;
        let step = fs / n as f64;
        Ok(freqs
            .iter()
            .map(|&f| {
                let at = if step > 0.0 && f > 0.0 { f / step } else { 0.0 };
                let k = math::min(at, last as f64);
                let lo = k as usize;
                let hi = (lo + 1).min(last);
                let t = k - lo as f64;
                let v = spec[lo] + (spec[hi] - spec[lo]) * t;
                let p = v * v;
                if p > 0.0 {
                    math::max(10.0 * math::log10(p), floor)
                } else {
                    floor
                }
            })
            .collect())
    }

    /// 遅れ D（フレーム）。
    pub fn delay(&self) -> usize {
        self.ladder.delay()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::biquad::{self, Section};

    fn target_of(grid: &DesignGrid, sections: &[Section], gain: f64) -> Vec<f64> {
        (0..grid.bins())
            .map(|k| {
                let (c1, s1, c2, s2) = grid.cos_sin(k);
                sections
                    .iter()
                    .map(|s| s.power_cs(c1, s1, c2, s2).sqrt())
                    .product::<f64>()
                    * gain
            })
            .collect()
    }

    /// 梯子：L = 2^k + 1・N = 2^(k+1)・B = 2^k・D = 2^(k−1)、B は音声のチャンクを割り切る。帯域が狭いほど長く、上限で止まる。
    #[test]
    fn the_ladder_follows_the_bandwidth() {
        for order in MIN_ORDER..=MAX_ORDER {
            let l = Ladder::new(order);
            assert_eq!(l.taps(), (1 << order) + 1);
            assert_eq!(l.fft_len(), 2 * l.block());
            assert_eq!(l.taps() - 1, 2 * l.delay());
            assert_eq!(l.block() + l.taps() - 1, l.fft_len());
            assert_eq!(crate::value::AUDIO_CHUNK % l.block(), 0);
        }
        let fs = 48_000.0;
        let (wide, capped) = Ladder::for_bandwidth(10_000.0, fs);
        assert_eq!((wide.order, capped), (MIN_ORDER, false));
        let (narrow, _) = Ladder::for_bandwidth(50.0, fs);
        assert!(narrow > wide);
        assert!(narrow.taps() as f64 >= LENGTH_FACTOR * fs / 50.0);
        assert!(Ladder::new(narrow.order - 1).taps() as f64 > 0.0);
        let (tiny, capped) = Ladder::for_bandwidth(0.5, fs);
        assert_eq!((tiny.order, capped), (MAX_ORDER, true));
        assert_eq!(Ladder::for_bandwidth(f64::NAN, fs).0.order, MIN_ORDER);
    }

    /// 設計：h[0]〜h[D] で、平らな目標（利得だけ）は中心だけの FIR になり、H は利得。同じ目標から 2 回でビット単位で同じ。
    #[test]
    fn a_flat_target_gives_a_pure_gain() {
        let ladder = Ladder::new(MIN_ORDER);
        let ffts = DesignFfts::new(ladder).unwrap();
        let grid = DesignGrid::new(ladder).unwrap();
        let target = vec![0.5; grid.bins()];
        let fir = design(&target, &ffts).unwrap();
        assert_eq!(fir.taps.len(), ladder.delay() + 1);
        assert!((fir.taps[0] - 0.5).abs() < 1e-12);
        assert!(fir.taps[1..].iter().all(|v| v.abs() < 1e-12));
        assert!(fir.spectrum.iter().all(|v| (v - 0.5).abs() < 1e-12));
        let again = design(&target, &ffts).unwrap();
        assert!(
            fir.taps
                .iter()
                .zip(&again.taps)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }

    /// 応答の端：0 Hz・NaN の周波数は直流の値、ナイキストより上はナイキストの値、`floor` で止める（どれも有限）。
    #[test]
    fn the_response_handles_the_edges() {
        let ladder = Ladder::new(MIN_ORDER);
        let ffts = DesignFfts::new(ladder).unwrap();
        let grid = DesignGrid::new(ladder).unwrap();
        let target = vec![0.5; grid.bins()];
        let fir = design(&target, &ffts).unwrap();
        let fs = 48_000.0;
        let got = fir
            .response_db(
                fs,
                &[0.0, f64::NAN, 24_000.0, 30_000.0, -5.0],
                &ffts,
                -120.0,
            )
            .unwrap();
        assert!(got.iter().all(|v| v.is_finite()), "{got:?}");
        let half = 20.0 * crate::math::log10(0.5);
        for v in &got {
            assert!((v - half).abs() < 1e-9, "{got:?}");
        }
        let zero = design(&vec![0.0; grid.bins()], &ffts).unwrap();
        let got = zero.response_db(fs, &[1_000.0], &ffts, -120.0).unwrap();
        assert_eq!(got, vec![-120.0]);
    }

    /// 測り（段 7 の P3。`cargo test -p kura-engine --release --lib audio::fir::tests::measure -- --ignored --nocapture`）：
    /// 代表のバンド（48 kHz）ごとに、β と C の組で、段・タップ数・目標との差の最大（20 Hz〜20 kHz の対数の 300 点。目標が
    /// −40 dB より深い所は見ない）。設計の時間は devtools で測る（エンジンの中では時刻を読まない）。
    #[test]
    #[ignore]
    fn measure() {
        let fs = 48_000.0;
        let w = |f: f64| 2.0 * std::f64::consts::PI * f / fs;
        // (名前, 段, 帯域 Hz)
        let cases: Vec<(&str, Vec<Section>, f64)> = vec![
            (
                "ベル 1 kHz Q1 +6",
                vec![biquad::peaking(w(1_000.0), 1.0, 6.0)],
                1_000.0,
            ),
            (
                "ベル 1 kHz Q4 −12",
                vec![biquad::peaking(w(1_000.0), 4.0, -12.0)],
                250.0,
            ),
            (
                "ベル 100 Hz Q10 +6",
                vec![biquad::peaking(w(100.0), 10.0, 6.0)],
                10.0,
            ),
            (
                "ノッチ 100 Hz Q10",
                vec![biquad::notch(w(100.0), 10.0)],
                10.0,
            ),
            (
                "ローシェルフ 100 Hz +6",
                vec![biquad::shelf(w(100.0), 0.707, 6.0, fs, false)],
                141.0,
            ),
            (
                "ハイシェルフ 8 kHz −6",
                vec![biquad::shelf(w(8_000.0), 0.707, -6.0, fs, true)],
                11_300.0,
            ),
            (
                "ローカット 30 Hz 24",
                biquad::cut(w(30.0), 24, false),
                30.0 / (24.0 / 12.0 * 0.7),
            ),
            (
                "ローカット 30 Hz 48",
                biquad::cut(w(30.0), 48, false),
                30.0 / (48.0 / 12.0 * 0.7),
            ),
            (
                "ハイカット 12 kHz 24",
                biquad::cut(w(12_000.0), 24, true),
                12_000.0 / (24.0 / 12.0 * 0.7),
            ),
            (
                "バンドパス 500 Hz Q2",
                vec![biquad::bandpass(w(500.0), 2.0)],
                250.0,
            ),
            (
                "チルト 1 kHz +6",
                vec![biquad::tilt(w(1_000.0), 6.0)],
                2_000.0,
            ),
        ];
        let freqs: Vec<f64> = (0..300)
            .map(|i| 20.0 * math::pow(1_000.0, i as f64 / 299.0))
            .collect();
        for beta in [4.0, 6.0, 8.0] {
            for factor in [4.0, 8.0, 16.0, 32.0] {
                println!("β {beta} C {factor}");
                for (name, sections, bw) in &cases {
                    let (ladder, capped) = Ladder::for_bandwidth_with(*bw, fs, factor);
                    let ffts = DesignFfts::new(ladder).unwrap();
                    let grid = DesignGrid::new(ladder).unwrap();
                    let fir = design_with(&target_of(&grid, sections, 1.0), &ffts, beta).unwrap();
                    let got = fir.response_db(fs, &freqs, &ffts, -300.0).unwrap();
                    let mut worst = 0f64;
                    for (f, g) in freqs.iter().zip(&got) {
                        let want: f64 = sections.iter().map(|s| s.db(w(*f))).sum();
                        if want >= -40.0 {
                            worst = math::max(worst, (g - want).abs());
                        }
                    }
                    println!(
                        "  {name}: k {} L {} {}差 {worst:.3} dB",
                        ladder.order,
                        ladder.taps(),
                        if capped { "上限 " } else { "" }
                    );
                }
            }
        }
    }

    /// 測り（段 7 の P3）：深い所（カットの止める帯・ノッチの中心）にどこまで届くか（β ごと。C は 16）。
    #[test]
    #[ignore]
    fn measure_depth() {
        let fs = 48_000.0;
        let w = |f: f64| 2.0 * std::f64::consts::PI * f / fs;
        let cases: Vec<(&str, Vec<Section>, f64, Vec<f64>)> = vec![
            (
                "ローカット 30 Hz 48",
                biquad::cut(w(30.0), 48, false),
                30.0 / (48.0 / 12.0 * 0.7),
                vec![5.0, 10.0, 15.0, 20.0],
            ),
            (
                "ローカット 80 Hz 24",
                biquad::cut(w(80.0), 24, false),
                80.0 / (24.0 / 12.0 * 0.7),
                vec![10.0, 20.0, 40.0],
            ),
            (
                "ハイカット 5 kHz 48",
                biquad::cut(w(5_000.0), 48, true),
                5_000.0 / (48.0 / 12.0 * 0.7),
                vec![10_000.0, 15_000.0, 20_000.0],
            ),
            (
                "ノッチ 1 kHz Q4",
                vec![biquad::notch(w(1_000.0), 4.0)],
                250.0,
                vec![1_000.0, 1_030.0],
            ),
            (
                "ノッチ 100 Hz Q10",
                vec![biquad::notch(w(100.0), 10.0)],
                10.0,
                vec![100.0, 102.0],
            ),
        ];
        for beta in [4.0, 6.0, 8.0, 10.0] {
            println!("β {beta} C 16");
            for (name, sections, bw, at) in &cases {
                let (ladder, capped) = Ladder::for_bandwidth_with(*bw, fs, 16.0);
                let ffts = DesignFfts::new(ladder).unwrap();
                let grid = DesignGrid::new(ladder).unwrap();
                let fir = design_with(&target_of(&grid, sections, 1.0), &ffts, beta).unwrap();
                let got = fir.response_db(fs, at, &ffts, -300.0).unwrap();
                let pairs: Vec<String> = at
                    .iter()
                    .zip(&got)
                    .map(|(f, g)| {
                        let want: f64 = sections.iter().map(|s| s.db(w(*f))).sum();
                        format!("{f} Hz 目標 {want:.1} → {g:.1}")
                    })
                    .collect();
                println!(
                    "  {name}（L {}{}）：{}",
                    ladder.taps(),
                    if capped { " 上限" } else { "" },
                    pairs.join(" / ")
                );
            }
        }
    }

    /// 実際の応答は目標に近い（凍結した β・C で、上限に届かない代表のバンド。44.1〜192 kHz で ±0.15 dB。目標が −30 dB より深い
    /// 所（カットの止める帯・バンドパスの裾）は見ない）。
    /// H は円周の 0 を中心に置いた h の順変換（虚部はほぼ 0）。
    #[test]
    fn designs_follow_their_targets() {
        for fs in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let w = |f: f64| 2.0 * std::f64::consts::PI * f / fs;
            let cases: Vec<(Vec<Section>, f64)> = vec![
                (vec![biquad::peaking(w(1_000.0), 2.0, 6.0)], 500.0),
                (vec![biquad::peaking(w(3_000.0), 4.0, -12.0)], 750.0),
                (vec![biquad::shelf(w(200.0), 0.707, 6.0, fs, false)], 283.0),
                (
                    vec![biquad::shelf(w(8_000.0), 0.707, -6.0, fs, true)],
                    11_300.0,
                ),
                (vec![biquad::bandpass(w(800.0), 2.0)], 400.0),
                (vec![biquad::tilt(w(1_000.0), 6.0)], 2_000.0),
                (
                    biquad::cut(w(200.0), 24, false),
                    200.0 / (24.0 / 12.0 * 0.7),
                ),
            ];
            let freqs: Vec<f64> = (0..120)
                .map(|i| 20.0 * math::pow(1_000.0, i as f64 / 119.0))
                .filter(|f| *f < 0.45 * fs)
                .collect();
            for (sections, bw) in &cases {
                let (ladder, capped) = Ladder::for_bandwidth(*bw, fs);
                assert!(!capped);
                let ffts = DesignFfts::new(ladder).unwrap();
                let grid = DesignGrid::new(ladder).unwrap();
                let fir = design(&target_of(&grid, sections, 1.0), &ffts).unwrap();
                let got = fir.response_db(fs, &freqs, &ffts, -300.0).unwrap();
                for (f, g) in freqs.iter().zip(&got) {
                    let want: f64 = sections.iter().map(|s| s.db(w(*f))).sum();
                    if want >= -30.0 {
                        assert!(
                            (g - want).abs() < 0.15,
                            "{fs} Hz の {f} Hz：{g} dB と目標 {want} dB（L {}）",
                            ladder.taps()
                        );
                    }
                }
            }
        }
        // H は h の順変換（実部）。虚部はほぼ 0
        let ladder = Ladder::new(MIN_ORDER + 1);
        let ffts = DesignFfts::new(ladder).unwrap();
        let grid = DesignGrid::new(ladder).unwrap();
        let fir = design(
            &target_of(&grid, &[biquad::peaking(0.3, 1.0, 9.0)], 0.8),
            &ffts,
        )
        .unwrap();
        let n = ladder.fft_len();
        let mut x = vec![0.0; n];
        for (m, v) in fir.taps.iter().enumerate() {
            x[m] = *v;
            if m > 0 {
                x[n - m] = *v;
            }
        }
        let (mut re, mut im) = (vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]);
        ffts.block
            .forward(&x, &mut re, &mut im, &mut FftWork::default());
        assert_eq!(re, fir.spectrum);
        assert!(im.iter().all(|v| v.abs() < 1e-12));
    }
}
