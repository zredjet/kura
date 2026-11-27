//! `audio.eq` v1 の直線位相（M17 段 7。docs/editing.md §18.6、ADR-239）。式は v1 の直線位相として凍結する。
//!
//! - **設計**（[`LinearEq`]。核を作るときに、覚えになければ 1 度だけ）：推論を通したバンドの列（`eq::resolve`）の最も狭い帯域
//!   から長さの梯子の段を決め（`audio::fir::Ladder`）、設計の格子のビンごとに目標の振幅を求めて FIR を作る
//!   （`audio::fir::design`）。目標は各バンドの段の振幅（零位相）を列の順に掛けたもの。M/S のバンドがあるステレオは、領域を
//!   替える T = [[½,½],[½,−½]] と T⁻¹ = [[1,1],[1,−1]] も含めてビンごとの 2×2 の実の行列の積にし、4 本の FIR（出力 c =
//!   Σ_i h_ci ∗ 入力 i）を作る。ほかはチャンネルごとに、当てるバンドの並びが同じなら同じ FIR を分け合い、どのバンドも当たらない
//!   チャンネルは FIR を作らず出力の利得だけを掛ける（最小位相と同じ値）。出力の利得は目標に掛ける。
//! - **設計の覚え**（[`designed`]）：設計は引数・レート・チャンネルだけで決まる純粋な関数なので、新しいものから 4 つ・合わせて
//!   16 MiB までをプロセスで覚える（値に効かない。予算の外）。範囲の描画の値はキャッシュに残らないので、表示・先読み・
//!   スペクトル・試聴・握る計画の頼みのたびに核を作り直す（k = 16 の M/S の設計は 34 ms。release）。
//! - **核**（[`LinearKernel`]）：重なり保存。出力のブロック j（[jB, (j+1)B)。絶対位置 0 に結ぶ）は、入力の窓
//!   [jB − D, (j+1)B + D)（長さ N。0 より前と長さの外は 0）を順変換して H を掛け、逆変換した [D, D + B) を取る。ブロックの値は
//!   j だけで決まるので、区間の切り方によらず同じ値。組ごとに最後に描いたブロックを覚え（値に効かない。錠は計算の間は
//!   持たない）、組を一度に描く口はブロックごとに組の全チャンネルへ写す（M/S の組で FFT を 2 回回さない）。
//! - 有限でない標本は窓の中で 0 として通し、その位置の出力は元の値（最小位相と同じ見え方）。状態の表がなく範囲の描画で入力の
//!   全体を知れないので、`NonFiniteSamples` の警告は出さない（核と `apply` の警告をそろえる）。

use std::sync::{Arc, Mutex, PoisonError};

use crate::audio::AudioKernel;
use crate::audio::fft::{FftWork, RealFft};
use crate::audio::fir::{DesignFfts, DesignGrid, Fir, Ladder, OVERSAMPLE, design};
use crate::codec::try_vec;
use crate::error::EngineError;
use crate::op::Work;
use crate::ops::audio::eq::{Resolved, Target};
use crate::value::Audio;

/// 推論を通したバンドの列の長さの梯子の段（最も狭い帯域から）と、上限に届いたか。
pub fn ladder_of(r: &Resolved, rate: u32) -> (Ladder, bool) {
    let fs = f64::from(rate.max(1));
    let bw = r
        .bands
        .iter()
        .map(|b| b.bandwidth)
        .fold(f64::INFINITY, crate::math::min);
    Ladder::for_bandwidth(bw, fs)
}

/// 組の幅（M/S のバンドがあるステレオは 2）。
pub fn width_of(r: &Resolved, channels: usize) -> usize {
    if r.has_ms() && channels == 2 { 2 } else { 1 }
}

/// 組の幅 1 のとき、チャンネル `c` に当てるバンドの番号の並び（列の順）。
fn channel_key(r: &Resolved, c: usize) -> Vec<usize> {
    r.bands
        .iter()
        .enumerate()
        .filter(|(_, b)| match b.target {
            Target::All => true,
            Target::Channel(x) => x == c,
            Target::Mid | Target::Side => false,
        })
        .map(|(i, _)| i)
        .collect()
}

/// 作る FIR の本数（組の幅 2 は 4 本、幅 1 は当てるバンドの並びの違うチャンネルの数。どのバンドも当たらないチャンネルは数えない）。
fn fir_count(r: &Resolved, channels: usize) -> usize {
    if width_of(r, channels) == 2 {
        return 4;
    }
    let mut keys: Vec<Vec<usize>> = Vec::new();
    for c in 0..channels {
        let key = channel_key(r, c);
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys.len()
}

/// 組ごとの列の束（H か FIR。幅 1 は 1 本、幅 2 は 4 本）。
type Lanes = Arc<Vec<Vec<f64>>>;

/// 直線位相の設計（モジュールの説明）。
pub struct LinearEq {
    pub ladder: Ladder,
    /// 長さの上限に届いた（低い帯の形が目標から少しずれる）。
    pub capped: bool,
    channels: usize,
    width: usize,
    /// 出力の利得の倍率（どのバンドも当たらないチャンネルに掛ける）。
    gain: f64,
    /// 組ごとの H（幅 1 は [H]、幅 2 は [H00, H01, H10, H11]）。None はどのバンドも当たらない組（利得だけ）。
    lanes: Vec<Option<Lanes>>,
    /// 組ごとの FIR（h[0]〜h[D]。H と同じ並び。テストと曲線）。
    taps: Vec<Option<Lanes>>,
    fft: RealFft,
    /// 持っている量（H・FIR・FFT の表。分け合う列は 1 回だけ数える。設計の覚えの上限に使う）。
    bytes: u64,
}

/// バンド 1 つの、設計の格子のビン `k` の振幅（段の振幅の積）。
fn magnitude(grid: &DesignGrid, sections: &[crate::audio::biquad::Section], k: usize) -> f64 {
    let (c1, s1, c2, s2) = grid.cos_sin(k);
    sections
        .iter()
        .map(|s| s.power_cs(c1, s1, c2, s2).sqrt())
        .product()
}

/// FIR 1 本の持つ量（H と h）。
fn fir_bytes(fir: &Fir) -> u64 {
    (fir.spectrum.len() as u64 + fir.taps.len() as u64).saturating_mul(8)
}

impl LinearEq {
    /// 推論を通したバンドの列 `r`（段のあるもの）・チャンネル数・出力の利得の倍率 `gain`・レートから設計する（覚えを通すなら
    /// [`designed`]）。
    pub fn new(r: &Resolved, channels: usize, gain: f64, rate: u32) -> Result<Self, EngineError> {
        let (ladder, capped) = ladder_of(r, rate);
        let width = width_of(r, channels);
        let ffts = DesignFfts::new(ladder)?;
        let grid = DesignGrid::new(ladder)?;
        let bins = grid.bins();
        let mut bytes = 0u64;
        let mut design_one = |target: &[f64]| -> Result<Fir, EngineError> {
            let fir = if target.iter().all(|v| *v == 0.0) {
                // 0 の成分（行列の対角の外が 0 のとき）：設計しない
                Fir {
                    ladder,
                    taps: try_vec(ladder.delay() + 1, 0f64)?,
                    spectrum: try_vec(ladder.fft_len() / 2 + 1, 0f64)?,
                }
            } else {
                design(target, &ffts)?
            };
            bytes = bytes.saturating_add(fir_bytes(&fir));
            Ok(fir)
        };
        let mut lanes = Vec::new();
        let mut taps = Vec::new();
        if width == 2 {
            // ビンごとの 2×2 の行列（列の順に左から掛ける）
            let mut t = [
                try_vec(bins, 0f64)?,
                try_vec(bins, 0f64)?,
                try_vec(bins, 0f64)?,
                try_vec(bins, 0f64)?,
            ];
            for k in 0..bins {
                let mut m = [1.0f64, 0.0, 0.0, 1.0];
                for b in &r.bands {
                    let a = magnitude(&grid, &b.sections, k);
                    let f = match b.target {
                        Target::All => [a, 0.0, 0.0, a],
                        Target::Channel(0) => [a, 0.0, 0.0, 1.0],
                        Target::Channel(_) => [1.0, 0.0, 0.0, a],
                        Target::Mid => {
                            let (p, q) = (0.5 * (a + 1.0), 0.5 * (a - 1.0));
                            [p, q, q, p]
                        }
                        Target::Side => {
                            let (p, q) = (0.5 * (1.0 + a), 0.5 * (1.0 - a));
                            [p, q, q, p]
                        }
                    };
                    m = [
                        f[0] * m[0] + f[1] * m[2],
                        f[0] * m[1] + f[1] * m[3],
                        f[2] * m[0] + f[3] * m[2],
                        f[2] * m[1] + f[3] * m[3],
                    ];
                }
                for (e, v) in t.iter_mut().zip(m) {
                    e[k] = v * gain;
                }
            }
            let mut spectra = Vec::with_capacity(4);
            let mut hs = Vec::with_capacity(4);
            for e in &t {
                let fir = design_one(e)?;
                spectra.push(fir.spectrum);
                hs.push(fir.taps);
            }
            lanes.push(Some(Arc::new(spectra)));
            taps.push(Some(Arc::new(hs)));
        } else {
            // チャンネルごと：当てるバンドの並びが同じなら分け合う。どのバンドも当たらなければ利得だけ
            let mut keys: Vec<Vec<usize>> = Vec::new();
            let mut made: Vec<(Lanes, Lanes)> = Vec::new();
            for c in 0..channels {
                let key = channel_key(r, c);
                if key.is_empty() {
                    lanes.push(None);
                    taps.push(None);
                    continue;
                }
                let at = keys.iter().position(|k| *k == key);
                let pair = match at {
                    Some(i) => made[i].clone(),
                    None => {
                        let mut target = try_vec(bins, 0f64)?;
                        for (k, v) in target.iter_mut().enumerate() {
                            *v = key
                                .iter()
                                .map(|&i| magnitude(&grid, &r.bands[i].sections, k))
                                .product::<f64>()
                                * gain;
                        }
                        let fir = design_one(&target)?;
                        let pair = (Arc::new(vec![fir.spectrum]), Arc::new(vec![fir.taps]));
                        keys.push(key);
                        made.push(pair.clone());
                        pair
                    }
                };
                lanes.push(Some(pair.0));
                taps.push(Some(pair.1));
            }
        }
        let fft = ffts.block;
        let bytes = bytes.saturating_add(fft.resident_bytes());
        Ok(Self {
            ladder,
            capped,
            channels,
            width,
            gain,
            lanes,
            taps,
            fft,
            bytes,
        })
    }

    /// 組の幅。
    pub fn width(&self) -> usize {
        self.width
    }

    /// 組 `lane` の FIR（h[0]〜h[D]。幅 2 は [h00, h01, h10, h11]）。どのバンドも当たらない組（利得だけ）・ない組は None。
    pub fn taps(&self, lane: usize) -> Option<&[Vec<f64>]> {
        self.taps
            .get(lane)
            .and_then(|t| t.as_ref())
            .map(|t| t.as_slice())
    }

    /// 持っている量（バイト）。
    pub fn resident_bytes(&self) -> u64 {
        self.bytes
    }
}

/// 設計の覚えの数の上限。
const MEMO_COUNT: usize = 4;
/// 設計の覚えの量の上限（新しい 1 つはこれを超えても持つ）。
const MEMO_BYTES: u64 = 16 << 20;

/// 設計の覚え（モジュールの説明。新しいものほど後ろ。数・量の上限を持つ）。
struct DesignMemo {
    entries: Vec<(Vec<u64>, Arc<LinearEq>)>,
    count: usize,
    bytes: u64,
}

impl DesignMemo {
    const fn new(count: usize, bytes: u64) -> Self {
        Self {
            entries: Vec::new(),
            count,
            bytes,
        }
    }

    /// 鍵 `key` の設計（当たれば新しい側へ移す）。
    fn get(&mut self, key: &[u64]) -> Option<Arc<LinearEq>> {
        let at = self.entries.iter().position(|(k, _)| k.as_slice() == key)?;
        let hit = self.entries.remove(at);
        let eq = hit.1.clone();
        self.entries.push(hit);
        Some(eq)
    }

    /// 鍵 `key` を持っているか（使った順は変えない）。
    fn contains(&self, key: &[u64]) -> bool {
        self.entries.iter().any(|(k, _)| k.as_slice() == key)
    }

    /// 設計を足し、古いものから数・量の上限まで落とす（足したものは残す）。
    fn put(&mut self, key: Vec<u64>, eq: Arc<LinearEq>) {
        if self.contains(&key) {
            return;
        }
        self.entries.push((key, eq));
        while self.entries.len() > 1
            && (self.entries.len() > self.count
                || self
                    .entries
                    .iter()
                    .map(|(_, e)| e.resident_bytes())
                    .fold(0u64, u64::saturating_add)
                    > self.bytes)
        {
            self.entries.remove(0);
        }
    }
}

/// プロセスの設計の覚え（値に効かない）。
static DESIGNS: Mutex<DesignMemo> = Mutex::new(DesignMemo::new(MEMO_COUNT, MEMO_BYTES));

/// 設計の鍵：設計を決めるものすべて（レート・チャンネル数・利得・バンドごとの当てる信号・帯域・段の係数のビット）。
fn design_key(r: &Resolved, channels: usize, gain: f64, rate: u32) -> Vec<u64> {
    let mut k = Vec::with_capacity(4 + r.bands.len() * 8);
    k.extend([
        u64::from(rate),
        channels as u64,
        gain.to_bits(),
        r.bands.len() as u64,
    ]);
    for b in &r.bands {
        let (tag, at) = match b.target {
            Target::All => (0, 0),
            Target::Channel(c) => (1, c as u64),
            Target::Mid => (2, 0),
            Target::Side => (3, 0),
        };
        k.extend([tag, at, b.bandwidth.to_bits(), b.sections.len() as u64]);
        for s in &b.sections {
            k.extend([
                s.b0.to_bits(),
                s.b1.to_bits(),
                s.b2.to_bits(),
                s.a1.to_bits(),
                s.a2.to_bits(),
            ]);
        }
    }
    k
}

/// 覚えを通した設計（モジュールの説明。値は [`LinearEq::new`] と同じ）。
pub fn designed(
    r: &Resolved,
    channels: usize,
    gain: f64,
    rate: u32,
) -> Result<Arc<LinearEq>, EngineError> {
    let key = design_key(r, channels, gain, rate);
    if let Some(eq) = DESIGNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return Ok(eq);
    }
    // 設計の間は錠を持たない（同じ鍵を 2 つの仕事が同時に設計しても、同じ値で先のものが残る）
    let eq = Arc::new(LinearEq::new(r, channels, gain, rate)?);
    DESIGNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .put(key, eq.clone());
    Ok(eq)
}

/// 核を作る固定の時間の見込み（ns。`Op::kernel_setup_ns`）：覚えにあれば 0、なければ 100 ns × 2^k × (1 + FIR の本数)
/// （`tests/stateful.rs` の `eq_linear_design_timings`：48 kHz、release で k = 12・14・16 の FIR 1 本が 0.69・2.87・13.1 ms、
/// M/S の 4 本が 1.56・7.29・33.7 ms）。
pub fn setup_ns(r: &Resolved, channels: usize, gain: f64, rate: u32) -> f64 {
    let key = design_key(r, channels, gain, rate);
    if DESIGNS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(&key)
    {
        return 0.0;
    }
    let (ladder, _) = ladder_of(r, rate);
    100.0 * ladder.block() as f64 * (1 + fir_count(r, channels)) as f64
}

/// 曲線の合成（`eq::curve`）：全バンドの目標（チャンネルは分けない）に出力の利得 `gain` を掛けた FIR の、周波数 `freqs`（Hz）
/// ごとの実際の応答（dB。`biquad::MIN_DB` で止める）と、長さの梯子の段・上限に届いたか。
pub fn total_response(
    r: &Resolved,
    rate: u32,
    gain: f64,
    freqs: &[f64],
) -> Result<(Vec<f64>, Ladder, bool), EngineError> {
    let (ladder, capped) = ladder_of(r, rate);
    let ffts = DesignFfts::new(ladder)?;
    let grid = DesignGrid::new(ladder)?;
    let mut target = try_vec(grid.bins(), 0f64)?;
    for (k, v) in target.iter_mut().enumerate() {
        *v = r
            .bands
            .iter()
            .map(|b| magnitude(&grid, &b.sections, k))
            .product::<f64>()
            * gain;
    }
    let fir = design(&target, &ffts)?;
    let db = fir.response_db(
        f64::from(rate.max(1)),
        freqs,
        &ffts,
        crate::audio::biquad::MIN_DB,
    )?;
    Ok((db, ladder, capped))
}

/// 直線位相の作業領域の見積もり（`Op::work_bytes`）：仕事ごとに入力の窓（組のチャンネルごとの f32 と、f64 1 本）・スペクトル・
/// 逆変換・FFT の作業列・ブロックの出力、共有に H・FFT の表・覚え・設計の一時の領域。
pub fn linear_work(ladder: Ladder, channels: usize, width: usize) -> Work {
    let n = ladder.fft_len() as u64;
    let half = n / 2 + 1;
    let b = ladder.block() as u64;
    let w = width.max(1) as u64;
    let lanes = (channels as u64 / w).max(1);
    let per_task = w
        .saturating_mul(n * 4 + n * 8 + half * 16)
        .saturating_add(half * 16 + n * 8 + (n / 2) * 16)
        .saturating_add(w * b * 4);
    let spectra = lanes.saturating_mul(w * w).saturating_mul(half * 8 * 2);
    let fft = n * 4 + n * 16;
    let memo = lanes.saturating_mul(w * b * 4);
    let design = (n * OVERSAMPLE as u64).saturating_mul(8 * 6);
    Work {
        shared: spectra
            .saturating_add(fft)
            .saturating_add(memo)
            .saturating_add(design),
        per_task,
    }
}

/// 組のブロック（ブロックの番号と、組のチャンネルごとの B 個の出力）。
type Block = (u64, Arc<Vec<Vec<f32>>>);

/// 直線位相の核（モジュールの説明）。
pub struct LinearKernel {
    audio: Arc<Audio>,
    eq: Arc<LinearEq>,
    memo: Vec<Mutex<Option<Block>>>,
}

impl LinearKernel {
    pub fn new(audio: Arc<Audio>, eq: Arc<LinearEq>) -> Self {
        let lanes = eq.channels / eq.width.max(1);
        Self {
            audio,
            eq,
            memo: (0..lanes).map(|_| Mutex::new(None)).collect(),
        }
    }

    /// 組 `lane` のブロック `j`（覚えていればそれ、なければ描いて覚える）。
    fn block(
        &self,
        lane: usize,
        h: &[Vec<f64>],
        j: u64,
    ) -> Result<Arc<Vec<Vec<f32>>>, EngineError> {
        let slot = self
            .memo
            .get(lane)
            .ok_or_else(|| EngineError::Internal(format!("直線位相の組 {lane} はありません")))?;
        {
            let memo = slot.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, out)) = memo.as_ref()
                && *at == j
            {
                return Ok(out.clone());
            }
        }
        let out = Arc::new(self.compute(lane, h, j)?);
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some((j, out.clone()));
        Ok(out)
    }

    /// 組 `lane`（H は `h`）のブロック `j` を描く（重なり保存）。
    fn compute(&self, lane: usize, h: &[Vec<f64>], j: u64) -> Result<Vec<Vec<f32>>, EngineError> {
        let ladder = self.eq.ladder;
        let (n, b, d) = (ladder.fft_len(), ladder.block(), ladder.delay());
        let w = self.eq.width;
        let half = n / 2 + 1;
        // 窓の始まり（jB − D。0 より前は 0 で埋める）
        let start = j
            .checked_mul(b as u64)
            .ok_or_else(|| EngineError::Internal("直線位相のブロックの位置があふれます".into()))?;
        let (skip, from) = if start >= d as u64 {
            (0usize, start - d as u64)
        } else {
            ((d as u64 - start) as usize, 0u64)
        };
        let mut work = FftWork::default();
        let mut spectra: Vec<(Vec<f64>, Vec<f64>)> = Vec::with_capacity(w);
        // 組のチャンネルごとの読んだ窓（有限でない標本の位置に元の値を戻すのに使う）
        let mut raws: Vec<Vec<f32>> = Vec::with_capacity(w);
        let mut x = try_vec(n, 0f64)?;
        for i in 0..w {
            let ch = lane * w + i;
            let mut raw = try_vec(n, 0f32)?;
            if skip < n {
                self.audio.read(ch, from, &mut raw[skip..])?;
            }
            for (xv, rv) in x.iter_mut().zip(&raw) {
                *xv = if rv.is_finite() { f64::from(*rv) } else { 0.0 };
            }
            let mut re = try_vec(half, 0f64)?;
            let mut im = try_vec(half, 0f64)?;
            self.eq.fft.forward(&x, &mut re, &mut im, &mut work);
            spectra.push((re, im));
            raws.push(raw);
        }
        let mut out = Vec::with_capacity(w);
        let mut yr = try_vec(half, 0f64)?;
        let mut yi = try_vec(half, 0f64)?;
        let mut y = try_vec(n, 0f64)?;
        for (c, raw) in raws.iter().enumerate() {
            yr.fill(0.0);
            yi.fill(0.0);
            for (i, (re, im)) in spectra.iter().enumerate() {
                let hk = &h[c * w + i];
                for k in 0..half {
                    yr[k] += hk[k] * re[k];
                    yi[k] += hk[k] * im[k];
                }
            }
            self.eq.fft.inverse(&yr, &yi, &mut y, &mut work);
            let mut o = try_vec(b, 0f32)?;
            for (t, (v, r)) in o.iter_mut().zip(&raw[d..d + b]).enumerate() {
                *v = if r.is_finite() { y[d + t] as f32 } else { *r };
            }
            out.push(o);
        }
        Ok(out)
    }

    /// 組 `lane` の [start, start + len) を、組のチャンネルごとの `outs`（`only` なら組のそのチャンネルだけ）に書く。ブロックは
    /// 1 回ずつ描いて全チャンネルへ写す。どのバンドも当たらない組は入力に利得だけを掛ける（有限でない標本は元の値）。
    fn fill(
        &self,
        lane: usize,
        only: Option<usize>,
        start: u64,
        outs: &mut [&mut [f32]],
    ) -> Result<(), EngineError> {
        let len = outs.first().map_or(0, |o| o.len());
        if outs.iter().any(|o| o.len() != len) {
            return Err(EngineError::Internal(
                "直線位相の組の出力の長さがそろっていません".into(),
            ));
        }
        let lanes =
            self.eq.lanes.get(lane).ok_or_else(|| {
                EngineError::Internal(format!("直線位相の組 {lane} はありません"))
            })?;
        let Some(h) = lanes else {
            // 利得だけ（組の幅 1）
            for out in outs.iter_mut() {
                self.audio.read(lane, start, out)?;
                for v in out.iter_mut() {
                    if v.is_finite() {
                        *v = (f64::from(*v) * self.eq.gain) as f32;
                    }
                }
            }
            return Ok(());
        };
        let b = self.eq.ladder.block() as u64;
        let mut done = 0usize;
        while done < len {
            let pos = start + done as u64;
            let j = pos / b;
            let block = self.block(lane, h, j)?;
            let at = (pos - j * b) as usize;
            let take = (len - done).min(b as usize - at);
            for (i, out) in outs.iter_mut().enumerate() {
                let c = only.unwrap_or(i);
                let src = block.get(c).ok_or_else(|| {
                    EngineError::Internal("直線位相のチャンネルが足りません".into())
                })?;
                out[done..done + take].copy_from_slice(&src[at..at + take]);
            }
            done += take;
        }
        Ok(())
    }
}

impl AudioKernel for LinearKernel {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn lane_width(&self) -> usize {
        self.eq.width
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        let w = self.eq.width.max(1);
        self.fill(ch / w, Some(ch % w), start, &mut [out])
    }

    fn render_lane(
        &self,
        lane: usize,
        start: u64,
        outs: &mut [&mut [f32]],
    ) -> Result<(), EngineError> {
        self.fill(lane, None, start, outs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::op::Op;
    use crate::ops::audio::eq::{Band, BandChannel, BandType, Eq, EqParams, Phase, band, resolve};
    use crate::render::region::Reach;
    use crate::value::{AUDIO_CHUNK, AudioMeta, Value};

    /// 1 ノードの文書で描いた出力の平面。
    fn run(engine: &crate::Engine, p: &EqParams, input: &Value) -> Vec<Vec<f32>> {
        let graph =
            crate::testing::single_node_graph(Eq::ID, 1, serde_json::to_value(p).unwrap()).unwrap();
        let r = crate::testing::render_full(
            engine,
            &graph,
            input.clone(),
            crate::digest::Digest::default(),
            &NoMonitor,
        )
        .unwrap();
        let Value::Audio(got) = &r.outputs[0].value else {
            panic!()
        };
        planes(got)
    }

    fn planes(a: &Audio) -> Vec<Vec<f32>> {
        (0..a.channels())
            .map(|c| {
                let mut p = vec![0f32; a.frames()];
                a.read(c, 0, &mut p).unwrap();
                p
            })
            .collect()
    }

    fn audio(rate: u32, planes: Vec<Vec<f32>>) -> Value {
        Value::Audio(Arc::new(
            Audio::new(
                rate,
                planes.into_iter().map(Arc::new).collect(),
                AudioMeta::default(),
            )
            .unwrap(),
        ))
    }

    fn linear(bands: Vec<Band>, gain_db: f64) -> EqParams {
        EqParams {
            bands,
            phase: Phase::Linear,
            gain_db,
        }
    }

    /// 直線位相のすべての例で、出力が設計した FIR の時間領域の素直な畳み込み（出力 c = Σ_i h_ci ∗ 入力 i。有限でない
    /// 標本は 0、その位置は元の値）と合う。位置は 0・D・B±1・C±1・終わりと、全体に散らした点。段がなければ入力のまま。
    #[test]
    fn linear_matches_direct_convolution() {
        let engine = crate::Engine::new().unwrap();
        let mut checked = 0;
        for ex in Eq::examples()
            .into_iter()
            .filter(|ex| ex.params.phase == Phase::Linear)
        {
            let input = ex.inputs[0].generate().unwrap();
            let Value::Audio(a) = &input else { panic!() };
            let x = planes(a);
            let got = run(&engine, &ex.params, &input);
            let ch = a.channels();
            let r = resolve(&ex.params, a.rate(), ch).unwrap();
            if r.bands.is_empty() {
                for (g, w) in got.iter().zip(&x) {
                    assert!(g.iter().zip(w).all(|(p, q)| p.to_bits() == q.to_bits()));
                }
                continue;
            }
            let eq = LinearEq::new(&r, ch, crate::math::db_to_gain(ex.params.gain_db), a.rate())
                .unwrap();
            let (d, b) = (eq.ladder.delay(), eq.ladder.block());
            let w = eq.width();
            let n = a.frames();
            let mut ts = vec![
                0,
                1,
                d.saturating_sub(1),
                d,
                d + 1,
                b - 1,
                b,
                b + 1,
                AUDIO_CHUNK - 1,
                AUDIO_CHUNK,
                AUDIO_CHUNK + 1,
                n.saturating_sub(2),
                n - 1,
            ];
            ts.extend((0..n).step_by(n / 41 + 1));
            ts.retain(|t| *t < n);
            let at = |i: usize, t: i64| -> f64 {
                if t < 0 || t as usize >= n {
                    return 0.0;
                }
                let v = x[i][t as usize];
                if v.is_finite() { f64::from(v) } else { 0.0 }
            };
            for &t in &ts {
                for c in 0..ch {
                    if !x[c][t].is_finite() {
                        assert_eq!(got[c][t].to_bits(), x[c][t].to_bits(), "{}", ex.name);
                        continue;
                    }
                    let (lane, ci) = (c / w, c % w);
                    let Some(taps) = eq.taps(lane) else {
                        // どのバンドも当たらない組：利得だけ（最小位相と同じ値）
                        let gain = crate::math::db_to_gain(ex.params.gain_db);
                        assert_eq!(
                            got[c][t].to_bits(),
                            ((f64::from(x[c][t]) * gain) as f32).to_bits(),
                            "{}：位置 {t}・チャンネル {c}",
                            ex.name
                        );
                        continue;
                    };
                    let (mut want, mut bound) = (0.0f64, 0.0f64);
                    for i in 0..w {
                        let h = &taps[ci * w + i];
                        let src = lane * w + i;
                        for m in -(d as i64)..=(d as i64) {
                            let v = h[m.unsigned_abs() as usize] * at(src, t as i64 - m);
                            want += v;
                            bound += v.abs();
                        }
                    }
                    let g = f64::from(got[c][t]);
                    assert!(
                        (g - want).abs() <= 2e-7 + 1e-6 * bound,
                        "{}：位置 {t}・チャンネル {c} で {g} と {want}",
                        ex.name
                    );
                    assert!(g.is_finite(), "{}：位置 {t}", ex.name);
                }
            }
            checked += 1;
        }
        assert!(checked >= 8, "{checked}");
    }

    /// 設計の覚え：当たれば新しい側へ移し、数・量の上限を超えたら古いものから落とす（足したものは量の上限を超えても残す）。
    /// 覚えを通した設計は、通さない設計とビットまで同じ。
    #[test]
    fn the_design_memo_keeps_the_newest() {
        let p = |freq: f64| linear(vec![band(BandType::Bell, freq, 3.0, 1.0)], 0.0);
        let make = |freq: f64| {
            let r = resolve(&p(freq), 8_000, 1).unwrap();
            (
                design_key(&r, 1, 1.0, 8_000),
                Arc::new(LinearEq::new(&r, 1, 1.0, 8_000).unwrap()),
            )
        };
        let (k1, e1) = make(1_000.0);
        let (k2, e2) = make(1_200.0);
        let (k3, e3) = make(1_400.0);
        assert_ne!(k1, k2);
        let mut m = DesignMemo::new(2, u64::MAX);
        m.put(k1.clone(), e1.clone());
        m.put(k2.clone(), e2);
        assert!(Arc::ptr_eq(&m.get(&k1).unwrap(), &e1));
        // k1 を使ったので、k3 を足すと k2 が落ちる
        m.put(k3.clone(), e3);
        assert!(m.contains(&k1) && !m.contains(&k2) && m.contains(&k3));
        // 量の上限：新しいものだけが残る
        let mut small = DesignMemo::new(4, e1.resident_bytes());
        small.put(k1.clone(), e1.clone());
        let (k4, e4) = make(1_600.0);
        small.put(k4.clone(), e4);
        assert!(!small.contains(&k1) && small.contains(&k4));
        // 覚えを通した設計は同じ値
        let r = resolve(&p(1_000.0), 8_000, 1).unwrap();
        let via = designed(&r, 1, 1.0, 8_000).unwrap();
        assert_eq!(via.taps(0), e1.taps(0));
        // 段の係数・利得が違えば鍵も違う
        assert_ne!(design_key(&r, 1, 0.5, 8_000), k1);
        assert_ne!(design_key(&r, 2, 1.0, 8_000), k1);
    }

    /// M/S のバンド：中央（L = R）だけ・横（L = −R）だけの入力で、M のバンドは中央にだけ、S のバンドは横にだけ当たる
    /// （行列の合成が M/S の領域の行き来と合う）。
    #[test]
    fn mid_and_side_bands_act_on_their_signal() {
        let engine = crate::Engine::new().unwrap();
        let rate = 48_000;
        let Value::Audio(nz) = crate::testgen::InputGen::AudioNoise {
            rate,
            channels: 1,
            frames: 12_000,
            seed: 7,
        }
        .generate()
        .unwrap() else {
            panic!()
        };
        let x = planes(&nz).remove(0);
        let neg: Vec<f32> = x.iter().map(|v| -v).collect();
        let bell = band(BandType::Bell, 1_200.0, 8.0, 1.5);
        let mono = run(
            &engine,
            &linear(vec![bell.clone()], 0.0),
            &audio(rate, vec![x.clone()]),
        );
        let close = |a: &[f32], b: &[f32], what: &str| {
            for (t, (p, q)) in a.iter().zip(b).enumerate() {
                assert!((p - q).abs() <= 2e-6, "{what}：位置 {t} で {p} と {q}");
            }
        };
        for (channel, on_mid) in [(BandChannel::Mid, true), (BandChannel::Side, false)] {
            let p = linear(
                vec![Band {
                    channel,
                    ..bell.clone()
                }],
                0.0,
            );
            let mid = run(&engine, &p, &audio(rate, vec![x.clone(), x.clone()]));
            let side = run(&engine, &p, &audio(rate, vec![x.clone(), neg.clone()]));
            let negm: Vec<f32> = mono[0].iter().map(|v| -v).collect();
            if on_mid {
                close(&mid[0], &mono[0], "M・中央の L");
                close(&mid[1], &mono[0], "M・中央の R");
                close(&side[0], &x, "M・横の L");
                close(&side[1], &neg, "M・横の R");
            } else {
                close(&mid[0], &x, "S・中央の L");
                close(&mid[1], &x, "S・中央の R");
                close(&side[0], &mono[0], "S・横の L");
                close(&side[1], &negm, "S・横の R");
            }
        }
    }

    /// 正弦の定常の振幅が目標（段の振幅の和）と ±0.1 dB（上限に届かない各型。遅れを補うので位相もそろう）。
    #[test]
    fn sine_follows_the_target() {
        use BandType::*;
        let engine = crate::Engine::new().unwrap();
        let rate = 48_000u32;
        let cases = vec![
            band(Bell, 1_000.0, 9.0, 1.5),
            band(LowShelf, 300.0, 6.0, 2.0),
            band(HighShelf, 8_000.0, -9.0, 0.5),
            Band {
                slope: 36,
                ..band(LowCut, 200.0, 0.0, 0.7)
            },
            Band {
                slope: 18,
                ..band(HighCut, 5_000.0, 0.0, 0.7)
            },
            band(Notch, 2_000.0, 0.0, 2.0),
            band(Tilt, 1_000.0, 6.0, 0.7),
        ];
        let n = 48_000usize;
        for b in cases {
            let p = linear(vec![b.clone()], 0.0);
            let r = resolve(&p, rate, 1).unwrap();
            assert!(!ladder_of(&r, rate).1, "{b:?}");
            for f in [60.0, 440.0, 1_000.0, 3_000.0, 9_000.0, 15_000.0] {
                let x: Vec<f32> = (0..n)
                    .map(|i| {
                        (0.25
                            * crate::math::sin(
                                2.0 * std::f64::consts::PI * f * i as f64 / f64::from(rate),
                            )) as f32
                    })
                    .collect();
                let y = run(&engine, &p, &audio(rate, vec![x.clone()])).remove(0);
                let rms = |v: &[f32]| {
                    (v[n / 4..3 * n / 4]
                        .iter()
                        .map(|s| f64::from(*s) * f64::from(*s))
                        .sum::<f64>()
                        / (n / 2) as f64)
                        .sqrt()
                };
                let got = 20.0 * crate::math::log10(rms(&y) / rms(&x));
                let w = 2.0 * std::f64::consts::PI * f / f64::from(rate);
                let want: f64 = r.bands[0].sections.iter().map(|s| s.db(w)).sum();
                if want < -30.0 {
                    continue;
                }
                assert!((got - want).abs() < 0.1, "{b:?} {f} Hz：{got} と {want}");
            }
        }
    }

    /// 曲線の合成（直線位相）は、インパルスの応答（描いた出力）の DTFT と ±0.02 dB（−30 dB より上。上限に届くものも）。
    /// `fir` は長さ・遅れ・上限を添え、最小位相・段がないときはない。
    #[test]
    fn curve_is_the_actual_response() {
        use crate::ops::audio::eq::curve;
        use BandType::*;
        let engine = crate::Engine::new().unwrap();
        let rate = 48_000u32;
        let freqs: Vec<f64> = (0..160)
            .map(|k| 20.0 * crate::math::pow(1_200.0, f64::from(k) / 159.0))
            .collect();
        for (bands, capped) in [
            (
                vec![
                    band(Bell, 1_000.0, 6.0, 1.0),
                    Band {
                        slope: 24,
                        ..band(LowCut, 200.0, 0.0, 0.707)
                    },
                    band(HighShelf, 8_000.0, -4.0, 0.707),
                ],
                false,
            ),
            // 100 Hz・Q 10 のノッチ（帯域 10 Hz）は上限に届く
            (vec![band(Notch, 100.0, 0.0, 10.0)], true),
        ] {
            let p = linear(bands.clone(), -1.0);
            let c = curve(&p, rate, 1, &freqs).unwrap();
            let fir = c.fir.unwrap();
            assert_eq!(fir.capped, capped);
            assert_eq!(fir.taps, 2 * fir.delay_frames + 1);
            let d = fir.delay_frames;
            let (n, t0) = (4 * d + 10, 2 * d);
            let mut x = vec![0f32; n];
            x[t0] = 1.0;
            let y = run(&engine, &p, &audio(rate, vec![x])).remove(0);
            for (k, f) in freqs.iter().enumerate() {
                if c.total[k] < -30.0 {
                    continue;
                }
                let w = 2.0 * std::f64::consts::PI * f / f64::from(rate);
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for m in -(d as i64)..=(d as i64) {
                    let v = f64::from(y[(t0 as i64 + m) as usize]);
                    re += v * crate::math::cos(w * m as f64);
                    im -= v * crate::math::sin(w * m as f64);
                }
                let db = 10.0 * crate::math::log10(re * re + im * im);
                assert!(
                    (db - c.total[k]).abs() < 0.02,
                    "{bands:?} {f} Hz：{db} と {}",
                    c.total[k]
                );
            }
            // 最小位相は段の和で、fir はない
            let m = curve(
                &EqParams {
                    phase: Phase::Minimum,
                    ..p.clone()
                },
                rate,
                1,
                &freqs,
            )
            .unwrap();
            assert!(m.fir.is_none());
        }
        let empty = curve(&linear(Vec::new(), -3.0), rate, 2, &freqs).unwrap();
        assert!(empty.fir.is_none() && empty.total.iter().all(|t| *t == -3.0));
    }

    /// 申告：状態の表を申告せず、届く範囲は Window（D・D・B）、作業領域は FFT の長さに比例。設計は 2 回でビット一致。
    /// 段がない引数（出力の利得だけを含む）は位相によらず同じ値。
    #[test]
    fn declarations_and_determinism() {
        let engine = crate::Engine::new().unwrap();
        let p = linear(
            vec![Band {
                channel: BandChannel::Mid,
                ..band(BandType::Bell, 2_000.0, 4.0, 2.0)
            }],
            -2.0,
        );
        let input = crate::testgen::InputGen::AudioNoise {
            rate: 48_000,
            channels: 2,
            frames: 30_000,
            seed: 3,
        }
        .generate()
        .unwrap();
        let shape = [input.shape()];
        assert!(Eq::needs(&p, &shape).unwrap().is_empty());
        let r = resolve(&p, 48_000, 2).unwrap();
        let (ladder, capped) = ladder_of(&r, 48_000);
        assert!(!capped);
        // 帯域 1 kHz：L ≥ 16 × 48,000 / 1,000 = 768 → 1,025
        assert_eq!(ladder.taps(), 1_025);
        let want = Reach::Window {
            before: 512,
            after: 512,
            align: 1_024,
            frames: 30_000,
        };
        assert_eq!(Eq::reach(&p, &shape), want);
        assert_eq!(Eq::audio_reach(&p, &shape), want);
        let work = Eq::work_bytes(&p, &shape, &input.shape());
        assert_eq!(work, linear_work(ladder, 2, 2));
        assert!(work.per_task >= 2 * 2_048 * 12, "{work:?}");
        let a = LinearEq::new(&r, 2, 0.5, 48_000).unwrap();
        let b = LinearEq::new(&r, 2, 0.5, 48_000).unwrap();
        assert_eq!(a.width(), 2);
        let bits = |e: &LinearEq| -> Vec<u64> {
            e.taps(0)
                .unwrap()
                .iter()
                .flatten()
                .map(|v| v.to_bits())
                .collect()
        };
        assert_eq!(bits(&a), bits(&b));
        // h01 と h10 は同じ（M のバンドの行列は対称）
        let t = a.taps(0).unwrap();
        assert_eq!(t[1], t[2]);
        // 段がない：位相によらず同じ値
        for bands in [Vec::new(), vec![band(BandType::Bell, 500.0, 0.0, 1.0)]] {
            let lin = run(&engine, &linear(bands.clone(), -6.0), &input);
            let min = run(
                &engine,
                &EqParams {
                    phase: Phase::Minimum,
                    ..linear(bands, -6.0)
                },
                &input,
            );
            assert_eq!(lin, min);
        }
    }
}
