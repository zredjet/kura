//! スペクトルの式 `kura-spec/1`（M17 段 4。docs/editing.md §18.4）。
//!
//! 表示だけの式（版をまたいだ値の約束はしない。式を変えたら番号を上げる）。`Analysis` には登録せず、タイルはセッション
//! （kura-core）の部品が作る。エンジンの規則（`math`・`par_blocks`）には従う。
//!
//! - 窓：N = 2^round(log2(レート × 4,096 / 48,000))（256〜16,384）、周期 Hann、hop = N/4。フレーム j の中心は j·hop
//!   （頭・尾の外の標本は 0）。窓ごとに、窓の中の有効な（長さの内の）標本の平均（直流）を引いてから窓をかける。有限でない
//!   標本は 0 として食べる。フレームの数は ⌈長さ / hop⌉（中心が長さの内のもの）。
//! - 低域：上端が 200 Hz 以下の帯は 4N 点の窓（長い窓）。長い窓は 4 フレームに 1 回（中心 4k·hop）とり、間のフレームは
//!   パワーを直線で補う（長い窓の hop は 4N/4 でふつうの重なり）。
//! - 値はパワー密度で、窓の長さによらない：P_k = |X_k|² × 2/Σw² × 3/N（N は短い窓。振幅 1 の正弦が短い窓のビンの中心で
//!   0 dB、長い窓では約 +6 dB。白色雑音は 200 Hz の境で段にならない）。
//! - 帯：20 Hz × 2^(k/48)（1/48 オクターブ）からナイキストまで（最後の帯はナイキストで切る。48 kHz で 491 帯）。帯の値は、中心の周波数が帯に入るビンの P の平均。ビンの
//!   ない帯は、帯の中心（幾何平均）を挟む 2 つのビンの P を周波数で直線に補う。
//! - チャンネル：番号・M = (L+R)/2・S = (L−R)/2（ステレオだけ。L・R の複素スペクトルの和と差から）・和（各チャンネルの
//!   パワーの和）。
//! - タイルの量子化：u8 で dB = q/2 − 120（偶数への丸め、範囲の外は端で止める。P ≤ 0 は 0）。

use std::sync::Arc;

use crate::audio::fft::{FftWork, RealFft};
use crate::error::EngineError;
use crate::math;
use crate::monitor::Monitor;
use crate::value::AUDIO_CHUNK;

/// 式の識別子。
pub const SPEC_ID: &str = "kura-spec/1";
/// 式の番号（タイルの形式に入れる）。
pub const SPEC_VERSION: u16 = 1;
/// 帯の下端（Hz）。
pub const LOW_HZ: f64 = 20.0;
/// 1 オクターブの帯の数。
pub const BANDS_PER_OCTAVE: u32 = 48;
/// 上端がこれ以下の帯は長い窓（Hz）。
pub const LONG_BELOW_HZ: f64 = 200.0;
/// 長い窓の倍率（長い窓は LONG × N 点、LONG フレームに 1 回）。
pub const LONG: usize = 4;
/// 量子化の下端（dB。q = 0）と刻み（dB）。
pub const DB_FLOOR: f64 = -120.0;
pub const DB_STEP: f64 = 0.5;
/// 窓の大きさの範囲。
pub const MIN_N: usize = 256;
pub const MAX_N: usize = 16_384;

/// チャンネルの選び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpecChannel {
    /// チャンネルの番号（0 から）。
    Index(u16),
    /// (L+R)/2（ステレオだけ）。
    Mid,
    /// (L−R)/2（ステレオだけ）。
    Side,
    /// 各チャンネルのパワーの和。
    Sum,
}

impl SpecChannel {
    /// `channels` チャンネルの音声で選べるか。
    pub fn valid_for(self, channels: usize) -> bool {
        match self {
            Self::Index(c) => usize::from(c) < channels,
            Self::Mid | Self::Side => channels == 2,
            Self::Sum => channels >= 1,
        }
    }
}

/// 帯の値の取り方。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pick {
    /// ビン [k0, k1) の P の平均。
    Bins(usize, usize),
    /// P[k](1 − t) + P[k + 1]t。
    Lerp(usize, f64),
}

/// 帯 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct SpecBand {
    /// 下端・上端（Hz）。
    pub lo: f64,
    pub hi: f64,
    /// 長い窓の帯か。
    pub long: bool,
    pick: Pick,
}

/// レート 1 つの式（窓・帯の表・FFT）。作るのは軽くないので、セッションが使い回す。
#[derive(Debug, Clone)]
pub struct SpecParams {
    rate: u32,
    n: usize,
    hop: usize,
    bands: Vec<SpecBand>,
    short: Arc<RealFft>,
    long: Arc<RealFft>,
    window: Vec<f64>,
    long_window: Vec<f64>,
    norm_short: f64,
    norm_long: f64,
}

/// 窓の大きさ：2^round(log2(rate × 4,096 / 48,000)) を [MIN_N, MAX_N] に。round は整数で決める（(32·rate)² ≥ 2^(2p−1)·375²
/// の最大の p。rate × 4,096 / 48,000 = rate × 32 / 375）。
pub fn window_for_rate(rate: u32) -> usize {
    let lhs = u128::from(rate) * 32;
    let lhs = lhs * lhs;
    let mut p = 0u32;
    while p < 40 {
        let next = p + 1;
        // 2^(2·next − 1) · 375² ≤ lhs なら next も満たす
        let rhs = (1u128 << (2 * next - 1)) * 375 * 375;
        if rhs > lhs {
            break;
        }
        p = next;
    }
    (1usize << p.min(24)).clamp(MIN_N, MAX_N)
}

/// 周期 Hann（長さ n）。
fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * math::cos(2.0 * std::f64::consts::PI * i as f64 / n as f64))
        .collect()
}

impl SpecParams {
    /// レート `rate` の式。
    pub fn for_rate(rate: u32) -> Result<Self, EngineError> {
        if rate == 0 {
            return Err(EngineError::Invalid(crate::error::Issues::one(
                crate::error::Issue::error(crate::error::IssueCode::BadInput, "レートが 0 です"),
            )));
        }
        let n = window_for_rate(rate);
        let long_n = n * LONG;
        let fs = f64::from(rate);
        let nyq = 0.5 * fs;
        // 帯の端：20·2^(k/48) ≤ ナイキスト
        let mut edges = vec![LOW_HZ];
        loop {
            let k = edges.len() as f64;
            let e = LOW_HZ * math::pow(2.0, k / f64::from(BANDS_PER_OCTAVE));
            if e > nyq {
                break;
            }
            edges.push(e);
        }
        // ナイキストまでの端の帯（1/48 オクターブより狭い）
        if edges.last().is_some_and(|e| *e < nyq) {
            edges.push(nyq);
        }
        let mut bands = Vec::with_capacity(edges.len().saturating_sub(1));
        for w in edges.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            let long = hi <= LONG_BELOW_HZ;
            let nw = if long { long_n } else { n } as f64;
            let last = (if long { long_n } else { n }) / 2;
            let k0 = ((lo * nw / fs).ceil() as usize).min(last);
            let k1 = ((hi * nw / fs).ceil() as usize).min(last + 1);
            let pick = if k1 > k0 {
                Pick::Bins(k0, k1)
            } else {
                let x = (lo * hi).sqrt() * nw / fs;
                let k = (x.floor() as usize).min(last.saturating_sub(1));
                Pick::Lerp(k, x - k as f64)
            };
            bands.push(SpecBand { lo, hi, long, pick });
        }
        let window = hann(n);
        let long_window = hann(long_n);
        let sum_sq = |w: &[f64]| w.iter().map(|v| v * v).fold(0.0, |a, b| a + b);
        let scale = 3.0 / n as f64;
        Ok(Self {
            rate,
            n,
            hop: n / 4,
            norm_short: 2.0 / sum_sq(&window) * scale,
            norm_long: 2.0 / sum_sq(&long_window) * scale,
            bands,
            short: Arc::new(RealFft::new(n)?),
            long: Arc::new(RealFft::new(long_n)?),
            window,
            long_window,
        })
    }

    /// レート。
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// 短い窓の大きさ N。
    pub fn n(&self) -> usize {
        self.n
    }

    /// hop（N/4）。
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// 帯。
    pub fn bands(&self) -> &[SpecBand] {
        &self.bands
    }

    /// 持っている表（帯・窓・FFT の表）のバイト数の見積もり（呼び手の取り置きに数える）。
    pub fn resident_bytes(&self) -> u64 {
        let f64s = self.window.len() + self.long_window.len();
        let fft = self
            .short
            .resident_bytes()
            .saturating_add(self.long.resident_bytes());
        ((self.bands.len() * std::mem::size_of::<SpecBand>() + f64s * 8) as u64).saturating_add(fft)
    }

    /// フレーム [j0, j1) の帯の値（[`Self::frame_bands`]）を求める間の作業領域の見積もり（行と長い窓の行。f64）。
    pub fn work_bytes(&self, j0: u64, j1: u64) -> u64 {
        let frames = j1.saturating_sub(j0);
        let longs = frames / LONG as u64 + 2;
        frames
            .saturating_add(longs)
            .saturating_mul(self.bands.len() as u64)
            .saturating_mul(8)
    }

    /// 長さ `len` のフレームの数（中心が長さの内のもの）。
    pub fn frames(&self, len: u64) -> u64 {
        len.div_ceil(self.hop as u64)
    }

    /// 1 チャンク（`AUDIO_CHUNK`）のフレームの数（チャンク ÷ hop。hop は 2 の冪で 4,096 以下なので割り切れる）。
    pub fn frames_per_chunk(&self) -> u64 {
        (AUDIO_CHUNK / self.hop) as u64
    }

    /// チャンク [c0, c1) のタイルを作るのに要る標本の区間（長さ `len` の内。前後に長い窓の半分 2N）。
    pub fn needed_span(&self, c0: u64, c1: u64, len: u64) -> (u64, u64) {
        let per = self.frames_per_chunk();
        self.frames_span(c0.saturating_mul(per), c1.saturating_mul(per), len)
    }

    /// フレーム [j0, j1) の帯の値（[`Self::frame_bands`]）に要る標本の区間（長さ `len` の内）：長い窓をとる 4 の倍数の
    /// フレーム ⌊j0/4⌋·4 〜 ⌈j1/4⌉·4 の中心の前後 2N。
    pub fn frames_span(&self, j0: u64, j1: u64, len: u64) -> (u64, u64) {
        let (hop, l) = (self.hop as u64, LONG as u64);
        let half = (self.n * LONG / 2) as u64;
        let a = (j0 / l * l).saturating_mul(hop).saturating_sub(half);
        let b = j1
            .div_ceil(l)
            .saturating_mul(l)
            .saturating_mul(hop)
            .saturating_add(half)
            .min(len);
        (a.min(len), b)
    }

    /// 窓の大きさ `nw`・窓 `w` で、中心 `center` のフレームの各チャンネルの複素スペクトル（直流を引いてから窓）。
    fn spectra(
        &self,
        input: &SpecInput<'_>,
        center: i64,
        long: bool,
        chans: &[usize],
        work: &mut FftWork,
    ) -> Vec<(Vec<f64>, Vec<f64>)> {
        let (fft, w) = if long {
            (&self.long, &self.long_window)
        } else {
            (&self.short, &self.window)
        };
        let nw = w.len();
        let start = center - (nw / 2) as i64;
        let mut x = vec![0.0f64; nw];
        chans
            .iter()
            .map(|&c| {
                let (mut sum, mut count) = (0.0f64, 0u64);
                for (i, v) in x.iter_mut().enumerate() {
                    *v = input.sample(c, start + i as i64).unwrap_or(f64::NAN);
                    if !v.is_nan() {
                        sum += *v;
                        count += 1;
                    }
                }
                let mean = if count > 0 { sum / count as f64 } else { 0.0 };
                for (v, wv) in x.iter_mut().zip(w) {
                    *v = if v.is_nan() { 0.0 } else { (*v - mean) * wv };
                }
                let (mut re, mut im) = (vec![0.0; nw / 2 + 1], vec![0.0; nw / 2 + 1]);
                fft.forward(&x, &mut re, &mut im, work);
                (re, im)
            })
            .collect()
    }

    /// ビンごとの P（チャンネルの選び方で合わせる）。
    fn bin_power(
        &self,
        spectra: &[(Vec<f64>, Vec<f64>)],
        chan: SpecChannel,
        long: bool,
    ) -> Vec<f64> {
        let norm = if long {
            self.norm_long
        } else {
            self.norm_short
        };
        let bins = spectra.first().map_or(0, |s| s.0.len());
        (0..bins)
            .map(|k| {
                let p = match chan {
                    SpecChannel::Index(_) => {
                        let (r, i) = (spectra[0].0[k], spectra[0].1[k]);
                        r * r + i * i
                    }
                    SpecChannel::Mid | SpecChannel::Side => {
                        let sign = if chan == SpecChannel::Mid { 1.0 } else { -1.0 };
                        let r = 0.5 * (spectra[0].0[k] + sign * spectra[1].0[k]);
                        let i = 0.5 * (spectra[0].1[k] + sign * spectra[1].1[k]);
                        r * r + i * i
                    }
                    SpecChannel::Sum => spectra
                        .iter()
                        .fold(0.0, |a, (re, im)| a + re[k] * re[k] + im[k] * im[k]),
                };
                p * norm
            })
            .collect()
    }

    /// 帯の値（`long` の帯だけ、または短い帯だけ）を `out` に書く。
    fn bands_of(&self, p: &[f64], long: bool, out: &mut [f64]) {
        for (b, o) in self.bands.iter().zip(out.iter_mut()) {
            if b.long != long {
                continue;
            }
            *o = match b.pick {
                Pick::Bins(k0, k1) => {
                    let s = p[k0..k1].iter().fold(0.0, |a, v| a + v);
                    s / (k1 - k0) as f64
                }
                Pick::Lerp(k, t) => {
                    let a = p.get(k).copied().unwrap_or(0.0);
                    let b = p.get(k + 1).copied().unwrap_or(a);
                    a * (1.0 - t) + b * t
                }
            };
        }
    }

    /// フレーム [j0, j1) の帯の値（P。フレーム × 帯）を求める。`input` は [`Self::frames_span`] の区間を持つこと。
    /// 長い窓は 4 の倍数のフレーム（j1 の次の 4 の倍数まで）でとり、間を直線で補う。フレームは `par_blocks` で並べる。
    pub fn frame_bands(
        &self,
        input: &SpecInput<'_>,
        chan: SpecChannel,
        j0: u64,
        j1: u64,
        mon: &dyn Monitor,
    ) -> Result<Vec<Vec<f64>>, EngineError> {
        if !chan.valid_for(input.planes.len()) {
            return Err(EngineError::Invalid(crate::error::Issues::one(
                crate::error::Issue::error(
                    crate::error::IssueCode::BadParams,
                    "このチャンネルの数では選べないチャンネルです",
                ),
            )));
        }
        let chans: Vec<usize> = match chan {
            SpecChannel::Index(c) => vec![usize::from(c)],
            SpecChannel::Mid | SpecChannel::Side => vec![0, 1],
            SpecChannel::Sum => (0..input.planes.len()).collect(),
        };
        if j1 <= j0 {
            return Ok(Vec::new());
        }
        let hop = self.hop as i64;
        let l = LONG as u64;
        let (l0, l1) = (j0 / l * l, j1.div_ceil(l).saturating_mul(l));
        // 長い窓（4 の倍数のフレーム l0, l0+4, …, l1）
        let long_count = ((l1 - l0) / l + 1) as usize;
        let longs = crate::determinism::par_blocks(long_count, mon, |i| {
            let j = l0 + i as u64 * l;
            let mut work = FftWork::default();
            let s = self.spectra(input, j as i64 * hop, true, &chans, &mut work);
            let p = self.bin_power(&s, chan, true);
            let mut out = vec![0.0; self.bands.len()];
            self.bands_of(&p, true, &mut out);
            Ok(out)
        })?;
        let count = (j1 - j0) as usize;
        crate::determinism::par_blocks(count, mon, |i| {
            let j = j0 + i as u64;
            let mut work = FftWork::default();
            let s = self.spectra(input, j as i64 * hop, false, &chans, &mut work);
            let p = self.bin_power(&s, chan, false);
            let mut out = vec![0.0; self.bands.len()];
            self.bands_of(&p, false, &mut out);
            // 長い帯：前後の長い窓を直線で
            let k = ((j - l0) / l) as usize;
            let r = (j % l) as f64 / l as f64;
            let (a, b) = (&longs[k], longs.get(k + 1).unwrap_or(&longs[k]));
            for (idx, band) in self.bands.iter().enumerate() {
                if band.long {
                    out[idx] = if r == 0.0 {
                        a[idx]
                    } else {
                        a[idx] * (1.0 - r) + b[idx] * r
                    };
                }
            }
            Ok(out)
        })
    }
}

/// 帯の値（P）を u8 に量子化する（dB = q/2 − 120。偶数への丸め、範囲の外は端で止める。P ≤ 0・NaN は 0）。
pub fn quantize(p: f64) -> u8 {
    if p.is_nan() || p <= 0.0 {
        return 0;
    }
    let db = 10.0 * math::log10(p);
    let q = ((db - DB_FLOOR) / DB_STEP).round_ties_even();
    if q.is_nan() || q <= 0.0 {
        0
    } else if q >= 255.0 {
        255
    } else {
        q as u8
    }
}

/// u8 の値の dB。
pub fn dequantize(q: u8) -> f64 {
    f64::from(q) * DB_STEP + DB_FLOOR
}

/// 流した標本（チャンネル × 区間）。`start` は planes の頭の絶対位置、`len` はポートの長さ。区間の外は 0（呼び手が
/// 長さの外を読ませない）。
#[derive(Debug, Clone)]
pub struct SpecInput<'a> {
    pub start: u64,
    pub len: u64,
    pub planes: &'a [Vec<f32>],
}

impl SpecInput<'_> {
    /// 位置 `pos` の標本（長さの外・区間の外は None：窓で 0 にし、直流の平均に数えない）。有限でない標本は 0。
    fn sample(&self, ch: usize, pos: i64) -> Option<f64> {
        if pos < 0 || pos as u64 >= self.len {
            return None;
        }
        let i = (pos as u64).checked_sub(self.start)? as usize;
        let v = *self.planes.get(ch)?.get(i)?;
        Some(if v.is_finite() { f64::from(v) } else { 0.0 })
    }
}

/// チャンク 1 つのタイルの値と要約。
#[derive(Debug, Clone, PartialEq)]
pub struct SpecChunk {
    /// チャンクの番号。
    pub chunk: u64,
    /// 最初のフレームの番号。
    pub first_frame: u64,
    /// フレームの数（最後のチャンクは少ない）。
    pub frames: u32,
    /// u8（フレーム × 帯。フレームが外）。
    pub data: Vec<u8>,
    /// 帯ごとの P の和（f64。フレームの順に足す）と最大。区間の平均に使う。
    pub sum: Vec<f64>,
    pub max: Vec<f64>,
}

impl SpecParams {
    /// チャンク [c0, c1) のタイル（ポートの長さ `len`。`input` は `needed_span(c0, c1, len)` を持つこと）。
    pub fn chunks(
        &self,
        input: &SpecInput<'_>,
        chan: SpecChannel,
        c0: u64,
        c1: u64,
        mon: &dyn Monitor,
    ) -> Result<Vec<SpecChunk>, EngineError> {
        let per = self.frames_per_chunk();
        let total = self.frames(input.len);
        if c1 <= c0 {
            return Ok(Vec::new());
        }
        let (j0, j1) = (
            c0.saturating_mul(per).min(total),
            c1.saturating_mul(per).min(total),
        );
        let rows = self.frame_bands(input, chan, j0, j1, mon)?;
        let bands = self.bands.len();
        let mut out = Vec::new();
        for c in c0..c1 {
            let (a, b) = (
                c.saturating_mul(per).min(total),
                c.saturating_add(1).saturating_mul(per).min(total),
            );
            if a >= b {
                break;
            }
            let mut data = Vec::with_capacity((b - a) as usize * bands);
            let (mut sum, mut max) = (vec![0.0f64; bands], vec![0.0f64; bands]);
            for j in a..b {
                let row = &rows[(j - j0) as usize];
                for (k, &p) in row.iter().enumerate() {
                    data.push(quantize(p));
                    sum[k] += p;
                    max[k] = math::max(max[k], p);
                }
            }
            out.push(SpecChunk {
                chunk: c,
                first_frame: a,
                frames: (b - a) as u32,
                data,
                sum,
                max,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    fn sine(rate: u32, freq: f64, amp: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|t| {
                (amp * math::sin(2.0 * std::f64::consts::PI * freq * t as f64 / f64::from(rate)))
                    as f32
            })
            .collect()
    }

    fn noise(frames: usize, seed: u64, amp: f64) -> Vec<f32> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..frames)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((((s >> 11) as f64 / (1u64 << 53) as f64) - 0.5) * 2.0 * amp) as f32
            })
            .collect()
    }

    fn db(p: f64) -> f64 {
        10.0 * math::log10(p)
    }

    /// 窓の大きさはレートから丸める。帯は 1/48 オクターブで、48 kHz で 491 帯。長い窓の帯は上端 200 Hz 以下。
    #[test]
    fn windows_and_bands() {
        for (rate, n) in [
            (8_000, 512),
            (22_050, 2_048),
            (32_000, 2_048),
            (44_100, 4_096),
            (48_000, 4_096),
            (88_200, 8_192),
            (96_000, 8_192),
            (192_000, 16_384),
            (384_000, 16_384),
        ] {
            assert_eq!(window_for_rate(rate), n, "{rate}");
        }
        let p = SpecParams::for_rate(48_000).unwrap();
        assert_eq!(p.bands().len(), 491);
        assert_eq!(p.hop(), 1_024);
        assert_eq!(p.frames_per_chunk(), 64);
        assert!(p.bands().iter().all(|b| b.long == (b.hi <= LONG_BELOW_HZ)));
        assert!(p.bands().iter().any(|b| b.long));
        assert!((p.bands()[0].lo - 20.0).abs() < 1e-12);
        assert_eq!(p.bands().last().unwrap().hi, 24_000.0);
        assert_eq!(p.frames(0), 0);
        assert_eq!(p.frames(1), 1);
        assert_eq!(p.frames(2_048), 2);
        assert_eq!(p.frames(2_049), 3);
    }

    /// 振幅 1 の正弦は、その周波数の帯で短い窓なら約 0 dB（ビンの中心で 0 dB。帯の平均で少し下がる）、ほかの帯ははるかに
    /// 低い。直流は引くので、直流だけの入力はどの帯も 0（P = 0）。
    #[test]
    fn sines_and_dc() {
        let rate = 48_000;
        let p = SpecParams::for_rate(rate).unwrap();
        let len = 4 * AUDIO_CHUNK;
        // ビンの中心の周波数（短い窓のビン 100 = 1,171.875 Hz）
        let f = 100.0 * f64::from(rate) / p.n() as f64;
        let planes = vec![sine(rate, f, 1.0, len)];
        let input = SpecInput {
            start: 0,
            len: len as u64,
            planes: &planes,
        };
        let rows = p
            .frame_bands(&input, SpecChannel::Index(0), 100, 104, &NoMonitor)
            .unwrap();
        let band = p
            .bands()
            .iter()
            .position(|b| b.lo <= f && f < b.hi)
            .unwrap();
        for row in &rows {
            assert!(
                db(row[band]) > -3.0 && db(row[band]) < 0.5,
                "{}",
                db(row[band])
            );
            let far = p.bands().iter().position(|b| b.lo > 4.0 * f).unwrap();
            assert!(db(row[far]) < db(row[band]) - 60.0);
        }
        let dc = vec![vec![0.5f32; len]];
        let input = SpecInput {
            start: 0,
            len: len as u64,
            planes: &dc,
        };
        let rows = p
            .frame_bands(&input, SpecChannel::Index(0), 10, 14, &NoMonitor)
            .unwrap();
        assert!(rows.iter().flatten().all(|v| *v < 1e-20), "直流は引く");
    }

    /// 白色雑音は 200 Hz の境（長い窓と短い窓）で段にならない（窓の長さによらない密度）。
    #[test]
    fn white_noise_has_no_step_at_the_crossover() {
        let rate = 48_000;
        let p = SpecParams::for_rate(rate).unwrap();
        let len = 16 * AUDIO_CHUNK;
        let planes = vec![noise(len, 9, 0.5)];
        let input = SpecInput {
            start: 0,
            len: len as u64,
            planes: &planes,
        };
        let rows = p
            .frame_bands(&input, SpecChannel::Index(0), 64, 64 + 256, &NoMonitor)
            .unwrap();
        let mean = |pred: &dyn Fn(&SpecBand) -> bool| {
            let idx: Vec<usize> = (0..p.bands().len())
                .filter(|&i| pred(&p.bands()[i]))
                .collect();
            let mut s = 0.0;
            for row in &rows {
                for &i in &idx {
                    s += row[i];
                }
            }
            db(s / (rows.len() * idx.len()) as f64)
        };
        let below = mean(&|b: &SpecBand| b.long && b.lo > 120.0);
        let above = mean(&|b: &SpecBand| !b.long && b.hi < 400.0);
        assert!(
            (below - above).abs() < 0.5,
            "200 Hz の境：{below:.2} と {above:.2}"
        );
    }

    /// M・S・和：L = R なら S は 0 で M は L と同じ、和は L の 2 倍。モノラルは番号 0 と和だけ。
    #[test]
    fn channels() {
        let rate = 44_100;
        let p = SpecParams::for_rate(rate).unwrap();
        let len = 2 * AUDIO_CHUNK;
        let l = noise(len, 3, 0.3);
        let planes = vec![l.clone(), l];
        let input = SpecInput {
            start: 0,
            len: len as u64,
            planes: &planes,
        };
        let get = |c| p.frame_bands(&input, c, 20, 22, &NoMonitor).unwrap();
        let (left, mid, side, sum) = (
            get(SpecChannel::Index(0)),
            get(SpecChannel::Mid),
            get(SpecChannel::Side),
            get(SpecChannel::Sum),
        );
        for f in 0..2 {
            for k in 0..p.bands().len() {
                assert!((mid[f][k] - left[f][k]).abs() <= 1e-9 * left[f][k] + 1e-30);
                assert!(side[f][k] < 1e-25);
                assert!((sum[f][k] - 2.0 * left[f][k]).abs() <= 1e-9 * left[f][k] + 1e-30);
            }
        }
        let mono = vec![noise(len, 4, 0.3)];
        let input = SpecInput {
            start: 0,
            len: len as u64,
            planes: &mono,
        };
        assert!(
            p.frame_bands(&input, SpecChannel::Mid, 0, 1, &NoMonitor)
                .is_err()
        );
        assert!(
            p.frame_bands(&input, SpecChannel::Index(1), 0, 1, &NoMonitor)
                .is_err()
        );
        assert!(
            p.frame_bands(&input, SpecChannel::Sum, 0, 1, &NoMonitor)
                .is_ok()
        );
    }

    /// タイル：持っている区間（needed_span より広くても）・フレームの区切りによらず同じ値。区間の外（頭・尾）は 0。
    /// 最後のチャンクはフレームが少ない。要約の和と最大は量子化の前の P。
    #[test]
    fn chunks_do_not_depend_on_the_slicing() {
        let rate = 48_000;
        let p = SpecParams::for_rate(rate).unwrap();
        let len = 3 * AUDIO_CHUNK + 5_000;
        let full = vec![noise(len, 11, 0.4), sine(rate, 440.0, 0.25, len)];
        let whole = SpecInput {
            start: 0,
            len: len as u64,
            planes: &full,
        };
        let all = p
            .chunks(&whole, SpecChannel::Sum, 0, 4, &NoMonitor)
            .unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[3].frames as u64, p.frames(len as u64) - 3 * 64);
        for c in 0..4u64 {
            let (a, b) = p.needed_span(c, c + 1, len as u64);
            let part: Vec<Vec<f32>> = full
                .iter()
                .map(|ch| ch[a as usize..b as usize].to_vec())
                .collect();
            let input = SpecInput {
                start: a,
                len: len as u64,
                planes: &part,
            };
            let one = p
                .chunks(&input, SpecChannel::Sum, c, c + 1, &NoMonitor)
                .unwrap();
            assert_eq!(one[0], all[c as usize], "チャンク {c}");
        }
        // 要約は量子化の前の P の、フレームの順の和と最大
        let rows = p
            .frame_bands(&whole, SpecChannel::Sum, 64, 128, &NoMonitor)
            .unwrap();
        for k in 0..p.bands().len() {
            let s = rows.iter().fold(0.0, |a, r| a + r[k]);
            let m = rows.iter().fold(0.0f64, |a, r| math::max(a, r[k]));
            assert_eq!(all[1].sum[k].to_bits(), s.to_bits(), "帯 {k}");
            assert_eq!(all[1].max[k].to_bits(), m.to_bits(), "帯 {k}");
            assert_eq!(all[1].data[k], quantize(rows[0][k]));
        }
        // 4 の倍数でないフレームの区間も、frames_span の区間だけで同じ値（長い窓の補いが区切りによらない）
        for (j0, j1) in [
            (3u64, 9u64),
            (65, 66),
            (130, 199),
            (190, p.frames(len as u64)),
        ] {
            let (a, b) = p.frames_span(j0, j1, len as u64);
            let part: Vec<Vec<f32>> = full
                .iter()
                .map(|ch| ch[a as usize..b as usize].to_vec())
                .collect();
            let input = SpecInput {
                start: a,
                len: len as u64,
                planes: &part,
            };
            let some = p
                .frame_bands(&input, SpecChannel::Sum, j0, j1, &NoMonitor)
                .unwrap();
            let want = p
                .frame_bands(&whole, SpecChannel::Sum, j0, j1, &NoMonitor)
                .unwrap();
            assert_eq!(some, want, "{j0}..{j1}");
        }
    }

    /// 逆さ・空の区間は空（あふれない）。表と作業領域の見積もりは 0 でない。
    #[test]
    fn empty_ranges_and_sizes() {
        let p = SpecParams::for_rate(48_000).unwrap();
        let planes = vec![vec![0.0f32; 10]];
        let input = SpecInput {
            start: 0,
            len: 10,
            planes: &planes,
        };
        let ch = SpecChannel::Index(0);
        assert!(
            p.frame_bands(&input, ch, 5, 3, &NoMonitor)
                .unwrap()
                .is_empty()
        );
        assert!(
            p.frame_bands(&input, ch, 4, 4, &NoMonitor)
                .unwrap()
                .is_empty()
        );
        assert!(p.chunks(&input, ch, 3, 1, &NoMonitor).unwrap().is_empty());
        assert!(
            p.chunks(&input, ch, u64::MAX - 1, u64::MAX, &NoMonitor)
                .unwrap()
                .is_empty()
        );
        assert!(p.resident_bytes() > (4 * 4_096 * 8) as u64);
        assert_eq!(p.work_bytes(0, 64), (64 + 18) * 491 * 8);
    }

    /// 測り（`--release -- --ignored --nocapture`。`RAYON_NUM_THREADS=3`）：48 kHz ステレオの和で、1 チャンク・8 チャンクの
    /// タイルの時間（中央値 5 回）。
    #[test]
    #[ignore = "測り"]
    fn chunk_timings() {
        #![allow(clippy::disallowed_methods)]
        let rate = 48_000;
        let p = SpecParams::for_rate(rate).unwrap();
        let len = 12 * AUDIO_CHUNK;
        let planes = vec![noise(len, 1, 0.5), noise(len, 2, 0.5)];
        let input = SpecInput {
            start: 0,
            len: len as u64,
            planes: &planes,
        };
        for (chan, name) in [(SpecChannel::Index(0), "L"), (SpecChannel::Sum, "和")] {
            for n in [1u64, 8] {
                let mut t: Vec<f64> = (0..5)
                    .map(|_| {
                        let s = std::time::Instant::now();
                        p.chunks(&input, chan, 2, 2 + n, &NoMonitor).unwrap();
                        s.elapsed().as_secs_f64() * 1e3
                    })
                    .collect();
                t.sort_by(f64::total_cmp);
                eprintln!(
                    "{name}・{n} チャンク：{:.1} ms（中央値。最小 {:.1}・最大 {:.1}）",
                    t[2], t[0], t[4]
                );
            }
        }
    }

    /// 量子化：dB = q/2 − 120、偶数への丸め、範囲の外は端、0 以下・NaN は 0。
    #[test]
    fn quantization() {
        assert_eq!(quantize(0.0), 0);
        assert_eq!(quantize(-1.0), 0);
        assert_eq!(quantize(f64::NAN), 0);
        assert_eq!(quantize(1e-20), 0);
        assert_eq!(quantize(1.0), 240);
        assert_eq!(quantize(1e3), 255);
        assert_eq!(dequantize(240), 0.0);
        assert_eq!(dequantize(0), -120.0);
        for q in 0..=255u8 {
            let p = math::pow(10.0, dequantize(q) / 10.0);
            assert_eq!(quantize(p), q, "{q}");
        }
    }
}
