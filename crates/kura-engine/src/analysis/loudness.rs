//! ラウドネスの計測（ITU-R BS.1770-4/-5）とトゥルーピーク（同 Annex 2）。正規化（`audio.normalize`）と、段 8 の解析
//! （`audio.loudness`）が同じ関数を使う（E26）。式はここで凍結する（変えるなら使う操作の版を上げる）。
//!
//! - K 特性：1 段目（頭部のシェルフ）→ 2 段目（RLB の高域通過）の 2 次の直接形 I を順に、状態は f64、0 から始める。
//!   係数は libebur128 の式（アナログの原型を `tan` で事前に歪めた双一次変換）。2 段目の分子は、どのレートでも 48 kHz の
//!   表（BS.1770 Table 2）と同じ周波数特性になるように `d(48 kHz) / d(fs)` を掛ける（libebur128 は掛けないので、
//!   192 kHz で 0.03 dB ほど違う）。fs が 2·f0（3,363.9 Hz。整数のレートでは 3,363 Hz）以下では積分ラウドネスを計れない（NaN）。
//! - ゲーティング：S = ⌊(fs + 5) / 10⌋ 標本（約 100 ms）ずつの二乗和を f64 で順に取り、ブロック j は区間 j〜j+3 の 4 つを
//!   `((q0 + q1) + q2) + q3` の順に足して 4S で割る（400 ms、75% の重なり）。端数の区間は使わない。
//!   ブロックのパワーはチャンネルの順に重みを掛けて足す。絶対ゲート −70 LKFS・相対ゲート −10 LU は、エネルギーで厳密な
//!   「>」で比べる（BS.1770 の式 (7)）。平均はブロックの番号の順に足す。通るブロックがなければ −∞。
//! - 重み：チャンネルの数から並びを決める（1：C、2：L・R、3：L・R・C、4：L・R・Ls・Rs、5：L・R・C・Ls・Rs、
//!   6：L・R・C・LFE・Ls・Rs、8：L・R・C・LFE・後方 2・側方 2（Annex 3 で側方を 1.41）、ほか：先頭 3 つが 1、4 つ目が
//!   LFE、5・6 つ目が 1.41、残りは計らない）。L・R・C は 1.0、Ls・Rs は 1.41、LFE は計らない。モノラルは重み 1。
//! - トゥルーピーク：Annex 2 の 48 タップ（12 タップ × 4 相、係数は k/8192）で、どのレートでも 4 倍にして、
//!   y[4m+q] = Σ_{r=0..11} T[r][q]·x[m−r]（f64、r の順）。入力の前後は 0 で、終わりも流し切る。各チャンネルの |y| の最大と
//!   サンプルピークの大きいほう（表の補間点は標本の位置を通らないため）。
//! - 標本に NaN・∞ があれば、どの値も NaN（計れない。IIR の状態が NaN のままになり、その後ろを黙って計らないのを防ぐ）。
//! - 利得をかけたあとの積分ラウドネス（`Measured::integrated_at`）は、同じブロックのパワーに利得の 2 乗を掛けて、ゲートを
//!   取り直して求める（K 特性は線形なので流し直さない）。絶対ゲートをまたぐブロックが入れ替わるので、利得の分だけ
//!   ずらした値にはならない。積分ラウドネスがちょうど目標になる利得（`Measured::gain_to`）は、ゲートを通る組ごとの
//!   直線の区間を調べて厳密に解く（正規化が使う）。
//! - 段 8 で足したもの（表示の `audio.loudness` だけが使う。正規化の値は変えない）：
//!   - 短期ラウドネス：区間 j〜j+29（3 秒）を先頭から順に足して 30S で割り、重みを掛けてチャンネルの順に足す
//!     （400 ms のブロックと同じ組み立て）。区間 1 つずつずらす（10 Hz。EBU Tech 3342 v3 以降）。流し切りの無音は付けない。
//!   - LRA（`loudness_range`）：EBU Tech 3342 v4。絶対ゲート −70 LUFS と相対ゲート −20 LU はどちらも「≥」（積分ラウドネスの
//!     「>」と違う）、パーセンタイルは補間なしの最近順位を整数で（`range_rank`）、LRA = 10·log10(高) − 10·log10(低)。
//!     libebur128（ebur128 クレート）は短期を 1 秒ごとに取るので、音楽らしい信号では 0.1〜0.4 LU ほど違う
//!     （EBU の試験 1〜4 はどちらもちょうどの値）。
//!   - 最大のモーメンタリ（400 ms のブロック）・最大の短期（3 秒）：ゲートなしのパワーの最大。
//! - 速さ（段 8。値は段 6 とビット単位で同じ）：K 特性（チャンネルごとに直列）と、トゥルーピーク・サンプルピーク・有限の
//!   確かめ（`SUM_BLOCK` ずつのブロック）を 1 つの `par_blocks` で計る。トゥルーピークは作業列に写してから足し、内側で
//!   範囲を確かめない。段 6 の実装をテストの参照に残し、ビット一致を確かめる。
//! - 窓の核（M13 段 1。値は同じ）：走査は窓を順に受ける `LoudnessScan`（K 特性の状態・区間のパワー・直前の 11 標本・ピークを
//!   運ぶ）。`analyze`・`analyze_detailed` は値をチャンクの窓で流し、範囲の描画の流し解き（`audio.loudness_gating` の流れ）は
//!   計画のノードの出力を窓ごとに描いて流す。どの切り方でも同じ（`scan_by_windows_equals_the_reference`）。

use crate::determinism::{SUM_BLOCK, par_blocks, sum_ordered};
use crate::error::EngineError;
use crate::math;
use crate::monitor::Monitor;
use crate::value::Audio;

/// 計測の結果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// 積分ラウドネス（LUFS）。ゲートを通るブロックがなければ −∞。計れない（レートが低い・NaN や ∞ の標本）なら NaN。
    pub integrated: f64,
    /// サンプルピーク（dBFS）。無音なら −∞。NaN や ∞ の標本があれば NaN。
    pub sample_peak_db: f64,
    /// トゥルーピーク（dBTP）。無音なら −∞。NaN や ∞ の標本があれば NaN。
    pub true_peak_db: f64,
}

/// 規格の定数 −0.691（997 Hz での K 特性の利得を打ち消す）。
const OFFSET: f64 = -0.691;

/// 1 段目（頭部のシェルフ）の原型の定数（libebur128）。
const SHELF_F0: f64 = 1681.974450955533;
const SHELF_G: f64 = 3.999843853973347;
const SHELF_Q: f64 = 0.7071752369554196;
const SHELF_VB_EXP: f64 = 0.4996667741545416;
/// 2 段目（高域通過）の原型の定数（libebur128）。
const HP_F0: f64 = 38.13547087602444;
const HP_Q: f64 = 0.5003270373238773;

/// 2 次のフィルタの係数（a0 で割ったもの）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b: [f64; 3],
    pub a: [f64; 2],
}

/// サンプルレート `fs` で積分ラウドネスを計れるか（K 特性を作れるか）。fs が 2·f0（3,363.9 Hz。整数のレートでは
/// 3,363 Hz）以下では計れない。`k_filter` と、`audio.loudness_gating` の見積もり（計れなければブロック 0 個）が
/// 同じ境を使う。
pub fn measurable_rate(fs: u32) -> bool {
    f64::from(fs) > 2.0 * SHELF_F0
}

/// サンプルレート `fs` の K 特性の 2 段の係数。計れないレート（`measurable_rate` が偽）は `Unsupported`。
pub fn k_filter(fs: u32) -> Result<[Biquad; 2], EngineError> {
    if !measurable_rate(fs) {
        return Err(EngineError::Unsupported(format!(
            "サンプルレート {fs} Hz ではラウドネスを計れません"
        )));
    }
    let fs = f64::from(fs);
    let k = math::tan(std::f64::consts::PI * SHELF_F0 / fs);
    let vh = math::pow(10.0, SHELF_G / 20.0);
    let vb = math::pow(vh, SHELF_VB_EXP);
    let a0 = 1.0 + k / SHELF_Q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / SHELF_Q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / SHELF_Q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / SHELF_Q + k * k) / a0],
    };
    let d_of = |fs: f64| {
        let k = math::tan(std::f64::consts::PI * HP_F0 / fs);
        (k, 1.0 + k / HP_Q + k * k)
    };
    let (k, d) = d_of(fs);
    let (_, d48) = d_of(48_000.0);
    let g = d48 / d;
    let hp = Biquad {
        b: [g, -2.0 * g, g],
        a: [2.0 * (k * k - 1.0) / d, (1.0 - k / HP_Q + k * k) / d],
    };
    Ok([shelf, hp])
}

/// チャンネルの重み（0 は計らない）。並びはチャンネルの数で決める（モジュールの説明）。
pub fn weights(channels: usize) -> Vec<f64> {
    const SURROUND: f64 = 1.41;
    match channels {
        1 => vec![1.0],
        2 => vec![1.0, 1.0],
        3 => vec![1.0, 1.0, 1.0],
        4 => vec![1.0, 1.0, SURROUND, SURROUND],
        5 => vec![1.0, 1.0, 1.0, SURROUND, SURROUND],
        8 => vec![1.0, 1.0, 1.0, 0.0, 1.0, 1.0, SURROUND, SURROUND],
        n => (0..n)
            .map(|i| match i {
                0..=2 => 1.0,
                4 | 5 => SURROUND,
                _ => 0.0,
            })
            .collect(),
    }
}

/// トゥルーピークの補間の表（BS.1770 Annex 2。行 r = 0..11、列は相 q = 0..3。値 × 8192）。
const TRUE_PEAK: [[i32; 4]; 12] = [
    [14, -239, -155, -68],
    [90, 240, 271, 122],
    [-161, -424, -477, -218],
    [272, 730, 832, 390],
    [-487, -1364, -1641, -838],
    [1125, 3810, 6388, 7964],
    [7964, 6388, 3810, 1125],
    [-838, -1641, -1364, -487],
    [390, 832, 730, 272],
    [-218, -477, -424, -161],
    [122, 271, 240, 90],
    [-68, -155, -239, 14],
];

/// 中止を確かめる間隔（標本）。
const CHECK_EVERY: usize = 1 << 16;

/// トゥルーピークの係数（相 q・行 r）。値は `TRUE_PEAK[r][q] / 8192`（f64）。
fn true_peak_coefficients() -> [[f64; 12]; 4] {
    let mut c = [[0f64; 12]; 4];
    for (q, cq) in c.iter_mut().enumerate() {
        for (slot, row) in cq.iter_mut().zip(TRUE_PEAK.iter()) {
            *slot = f64::from(row[q]) / 8192.0;
        }
    }
    c
}

fn to_db(linear: f64) -> f64 {
    if linear > 0.0 {
        20.0 * math::log10(linear)
    } else {
        f64::NEG_INFINITY
    }
}

/// 計測の結果と、利得をかけたあとの積分ラウドネスを求めるためのブロックのパワー。
#[derive(Debug, Clone, PartialEq)]
pub struct Measured {
    pub loudness: Loudness,
    /// 重みを掛けて足したブロックのパワー（番号の順。計れなければ空）。
    blocks: Vec<f64>,
    /// 積分ラウドネスを計れるか（レートが計れる範囲で、標本がすべて有限）。
    measurable: bool,
}

impl Measured {
    /// 利得 `gain_db`（dB）をかけた音声の積分ラウドネス（LUFS）。ブロックのパワーを 10^(gain/10) 倍してゲートを
    /// 取り直す。計れなければ NaN、ゲートを通るブロックがなければ −∞。
    pub fn integrated_at(&self, gain_db: f64) -> f64 {
        if !self.measurable {
            return f64::NAN;
        }
        let k = if gain_db == 0.0 {
            1.0
        } else {
            math::pow(10.0, gain_db / 10.0)
        };
        integrated(&self.blocks, k)
    }

    /// 利得をかけたあとの積分ラウドネスがちょうど `target`（LUFS）になる利得（dB）。ブロックがなければ（計れない・
    /// 無音・短い）None。
    ///
    /// 利得 g でブロック j が絶対ゲートを通るのは g > t_j = 10·log10(A / z_j) のとき（A は −70 LKFS のパワー）。
    /// パワーの大きい順に並べた上から m 個が通る区間 (t_m, t_{m+1}] では、相対ゲートは利得によらない（両辺が同じ倍率）
    /// ので、積分ラウドネスは g + h_m（h_m は上から m 個のうち相対ゲートを通るものの平均のラウドネス）の直線になる。
    /// 区間ごとに g = target − h_m が区間の中にあるかを調べ、あるもののうち |g| が最小（同じなら小さい g）を選ぶ。
    /// 回数の打ち切りがなく O(n log n)。解がなければ（target が −70 以下など）、区間の端に寄せた候補のうち
    /// `integrated_at` が target にいちばん近い g（この場合だけ候補の数 × O(n)）。値は、候補を 1 つずつ比べて選んでいた
    /// M13 の前の求め方とビット一致（`gain_to_matches_the_quadratic_reference`）。
    pub fn gain_to(&self, target: f64) -> Option<f64> {
        if !self.measurable {
            return None;
        }
        let mut z: Vec<f64> = self.blocks.iter().copied().filter(|p| *p > 0.0).collect();
        if z.is_empty() {
            return None;
        }
        // 大きい順（同じ値は並びによらない）
        z.sort_by(|a, b| b.total_cmp(a));
        let abs_gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
        let thr = |p: f64| 10.0 * math::log10(abs_gate / p);
        let mut prefix = Vec::with_capacity(z.len() + 1);
        prefix.push(0.0f64);
        for p in &z {
            prefix.push(prefix[prefix.len() - 1] + p);
        }
        // 区間ごとの候補 (g, 解か) を区間の順に集める（ここまで O(n log n)）
        let mut cands: Vec<(f64, bool)> = Vec::with_capacity(z.len());
        let mut c = 0usize;
        let mut m = 0usize;
        while m < z.len() {
            // 同じパワーのブロックはいっしょに通る
            let mut end = m + 1;
            while end < z.len() && z[end] == z[m] {
                end += 1;
            }
            m = end;
            let lo = thr(z[m - 1]);
            let hi = z.get(m).map_or(f64::INFINITY, |p| thr(*p));
            // 相対ゲート（平均の 0.1 倍より大きい）を通るのは上から c 個（平均は下がる一方なので c は増える一方）
            let rel = prefix[m] / m as f64 * 0.1;
            while c < m && z[c] > rel {
                c += 1;
            }
            let h = OFFSET + 10.0 * math::log10(prefix[c] / c as f64);
            let g = target - h;
            if g > lo && g <= hi {
                cands.push((g, true));
            } else if hi.is_finite() {
                cands.push((hi, false));
            } else {
                cands.push((math::max(lo, g), false));
            }
        }
        // 解があれば、解のうち |g| が最小（同じなら小さい g、それも同じなら先のもの）。解は近似にいつも勝ち、解どうしの
        // 比べは g だけで決まるので、近似の候補を評価しなくても同じ答えになる（`integrated_at` は O(n) なので呼ばない）
        let mut exact: Option<f64> = None;
        for &(g, is_exact) in &cands {
            if is_exact
                && exact.is_none_or(|bg| g.abs() < bg.abs() || (g.abs() == bg.abs() && g < bg))
            {
                exact = Some(g);
            }
        }
        if exact.is_some() {
            return exact;
        }
        // 解がなければ、近似の候補を区間の順に評価して target にいちばん近いもの（同じなら先のもの）
        let mut best: Option<(f64, f64)> = None; // (g, 誤差)
        for &(g, _) in &cands {
            let i = self.integrated_at(g);
            let err = if i.is_finite() {
                (i - target).abs()
            } else {
                f64::INFINITY
            };
            if best.is_none_or(|(_, berr)| err < berr) {
                best = Some((g, err));
            }
        }
        best.map(|(g, _)| g)
    }
}

/// 解析 `audio.loudness_gating`（正規化が申告して使う内部用の解析）の正規の形を作るための口。
impl Measured {
    /// 重みを掛けて足したブロックのパワー（番号の順。計れなければ空）。
    pub fn blocks(&self) -> &[f64] {
        &self.blocks
    }

    /// 積分ラウドネスを計れるか（レートが計れる範囲で、標本がすべて有限）。
    pub fn measurable(&self) -> bool {
        self.measurable
    }

    /// 部分から組み立てる（正規の形から読むとき）。
    pub(crate) fn from_parts(loudness: Loudness, blocks: Vec<f64>, measurable: bool) -> Self {
        Self {
            loudness,
            blocks,
            measurable,
        }
    }
}

/// ブロックのパワー（`k` 倍したもの）から積分ラウドネスを求める。
fn integrated(z: &[f64], k: f64) -> f64 {
    // しきい値はエネルギーで比べる（LUFS に直してから比べると、log10 の丸めで上下が食い違いうる）
    let abs_gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
    let gated = |pass: &dyn Fn(f64) -> bool| -> (f64, usize) {
        let kept: Vec<f64> = z.iter().map(|p| p * k).filter(|p| pass(*p)).collect();
        (sum_ordered(&kept), kept.len())
    };
    let (sum_abs, n_abs) = gated(&|p| p > abs_gate);
    if n_abs == 0 {
        return f64::NEG_INFINITY;
    }
    // 相対ゲート：絶対ゲートを通ったブロックのパワーの平均 − 10 LU（10^(−10/10) は 0.1 のリテラル）
    let rel = sum_abs / n_abs as f64 * 0.1;
    let (sum, n) = gated(&|p| p > abs_gate && p > rel);
    if n == 0 {
        f64::NEG_INFINITY
    } else {
        OFFSET + 10.0 * math::log10(sum / n as f64)
    }
}

/// 積分ラウドネス・サンプルピーク・トゥルーピークを計る。
pub fn measure(a: &Audio, mon: &dyn Monitor) -> Result<Loudness, EngineError> {
    Ok(analyze(a, mon)?.loudness)
}

/// 1 回の走査の結果。
struct Pass {
    /// チャンネルごとの K 特性の区間のパワー（重みが 0 のチャンネル・計れないレートは空）。
    segments: Vec<Vec<f64>>,
    /// サンプルピーク（線形）。
    sample_peak: f64,
    /// トゥルーピーク（線形。サンプルピークを含む）。
    true_peak: f64,
    /// 標本がすべて有限か。
    finite: bool,
    /// K 特性の係数（3,363 Hz 以下は None）。
    filter: Option<[Biquad; 2]>,
}

/// K 特性の状態（チャンネル 1 つ）：段ごとの x1・x2・y1・y2、区間の二乗和の途中、流した標本の数。
#[derive(Debug, Clone, Copy, Default)]
struct KState {
    st: [[f64; 4]; 2],
    acc: f64,
    i: usize,
}

/// 1 つの仕事の結果（K 特性の区間のパワーか、トゥルーピークのブロック）。
enum Part {
    Segments(KState, Vec<f64>),
    Skip,
    Peaks {
        true_peak: f64,
        sample_peak: f64,
        finite: bool,
    },
}

/// K 特性を `x` に通して状態を進め、閉じた区間の二乗和を返す（端数の区間（`used` から先）は入れない）。標本を 1 本の列として
/// 順に流したものと同じ演算を同じ順に行う（窓の切れ目で状態を運ぶ）。
fn k_filter_run(
    filt: &[Biquad; 2],
    mut k: KState,
    x: &[f32],
    s: usize,
    used: usize,
    mon: &dyn Monitor,
) -> Result<(KState, Vec<f64>), EngineError> {
    let take = used.saturating_sub(k.i).min(x.len());
    let mut q = Vec::new();
    q.try_reserve_exact(take / s.max(1) + 1)
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    for v in &x[..take] {
        if k.i.is_multiple_of(CHECK_EVERY) && mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let mut y = f64::from(*v);
        for (f, st) in filt.iter().zip(k.st.iter_mut()) {
            let out =
                f.b[0] * y + f.b[1] * st[0] + f.b[2] * st[1] - f.a[0] * st[2] - f.a[1] * st[3];
            st[1] = st[0];
            st[0] = y;
            st[3] = st[2];
            st[2] = out;
            y = out;
        }
        k.acc += y * y;
        if (k.i + 1).is_multiple_of(s) {
            q.push(k.acc);
            k.acc = 0.0;
        }
        k.i += 1;
    }
    Ok((k, q))
}

/// トゥルーピークの作業列 `w`（f64。w[k] = x[m0 + k − 11]）の、位置 m0 から w.len() − 11 個の出力の位置の
/// トゥルーピーク（線形。サンプルピークを含む）とサンプルピーク（線形）。
///
/// 式は段 6 のまま：y[4m+q] = Σ_{r=0..11} c[q][r]·x[m−r]（r の順に足す。範囲の外の x は 0）。作業列に写してから足すので、内側で
/// 範囲を確かめない（範囲の外の項は ±0 を足すだけで、y は −0 にならない（+0 から始めて ±0 を足しても +0、打ち消し合っても
/// +0）ので y のビットも同じ）。最大はどの順に取っても同じ（NaN のないとき。NaN があれば呼び手が値を捨てる）。
fn true_peak_window(w: &[f64], coef: &[[f64; 12]; 4]) -> (f64, f64) {
    let (mut tp, mut sp) = (0f64, 0f64);
    for win in w.windows(12) {
        let Some(win) = win.first_chunk::<12>() else {
            continue;
        };
        for cq in coef {
            let mut y = 0f64;
            // r = 0..11 の順：x[m − r] = win[11 − r]
            for (c, v) in cq.iter().zip(win.iter().rev()) {
                y += c * v;
            }
            tp = math::max(tp, y.abs());
        }
        // 標本 x[m]（m ≥ len は詰めた 0 で、最大を変えない）
        sp = math::max(sp, win[11].abs());
    }
    (math::max(tp, sp), sp)
}

/// ラウドネスの窓の核（M13 段 1）：音声を頭から順に窓（任意の長さ。実行器はチャンクに揃える）で受けて、全体を 1 度に
/// 走査したもの（段 8 の `scan`）とビット単位で同じ結果を出す。持つ状態はチャンネルごとの K 特性の状態と区間のパワー・
/// 直前の 11 標本（トゥルーピーク用）・ピークの最大・有限か。`analyze`・`analyze_detailed` と流し解き（`audio.loudness_gating`
/// の流れ）が同じ核を通る。
pub struct LoudnessScan {
    rate: u32,
    channels: usize,
    frames: usize,
    filter: Option<[Biquad; 2]>,
    /// 区間の標本の数 S。
    s: usize,
    /// 区間に入れる標本の数（端数の区間を除く）。
    used: usize,
    /// チャンネルごとに K 特性を計るか（重みが 0 でなく、レートが計れる）。
    measured: Vec<bool>,
    coef: [[f64; 12]; 4],
    k: Vec<KState>,
    segments: Vec<Vec<f64>>,
    /// チャンネルごとの直前の 11 標本（x[pos − 11..pos]。頭より前は 0）。
    tail: Vec<[f32; 11]>,
    pos: usize,
    sample_peak: f64,
    true_peak: f64,
    finite: bool,
}

impl LoudnessScan {
    /// レート `rate`・`channels` チャンネル・長さ `frames` の音声を受け始める（全体の長さを知っているので、端数の区間を
    /// 外せる）。チャンネルが 0 なら `Internal`。
    pub fn new(rate: u32, channels: usize, frames: u64) -> Result<Self, EngineError> {
        if channels == 0 {
            return Err(EngineError::Internal(
                "ラウドネスの計測にチャンネルがありません".into(),
            ));
        }
        let frames =
            usize::try_from(frames).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        let filter = k_filter(rate).ok();
        let s = ((u64::from(rate) + 5) / 10) as usize;
        let w = weights(channels);
        let segments_total = frames / s.max(1);
        let measured: Vec<bool> = (0..channels)
            .map(|c| filter.is_some() && w.get(c).copied().unwrap_or(0.0) != 0.0)
            .collect();
        let mut segments = Vec::with_capacity(channels);
        for m in &measured {
            let mut q = Vec::new();
            if *m {
                q.try_reserve_exact(segments_total)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            }
            segments.push(q);
        }
        Ok(Self {
            rate,
            channels,
            frames,
            filter,
            s: s.max(1),
            used: segments_total * s.max(1),
            measured,
            coef: true_peak_coefficients(),
            k: vec![KState::default(); channels],
            segments,
            tail: vec![[0f32; 11]; channels],
            pos: 0,
            sample_peak: 0.0,
            true_peak: 0.0,
            finite: true,
        })
    }

    /// フレーム `start` からの全チャンネルの標本を受ける（頭から順・重ならず・全体の長さの中）。K 特性（チャンネルごとに
    /// 直列）と、トゥルーピーク・サンプルピーク・有限の確かめ（`SUM_BLOCK` ずつのブロック）を 1 つの `par_blocks` で計る。
    pub fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let n = planes.first().map_or(0, |p| p.len());
        if start != self.pos as u64
            || planes.len() != self.channels
            || planes.iter().any(|p| p.len() != n)
            || self.pos.checked_add(n).is_none_or(|e| e > self.frames)
        {
            return Err(EngineError::Internal(format!(
                "ラウドネスの窓が順でないか形が違います（{start} から {n} フレーム。次は {}・長さ {}）",
                self.pos, self.frames
            )));
        }
        if n == 0 {
            return Ok(());
        }
        let blocks = n.div_ceil(SUM_BLOCK);
        let stride = blocks + 1;
        let tasks = self
            .channels
            .checked_mul(stride)
            .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))?;
        let this = &*self;
        let parts = par_blocks(tasks, mon, |t| {
            let (c, r) = (t / stride, t % stride);
            let x = planes.get(c).ok_or_else(|| {
                EngineError::Internal("ラウドネスの仕事の番号が範囲の外です".into())
            })?;
            if r == 0 {
                return match &this.filter {
                    Some(f) if this.measured[c] => {
                        let (k, q) = k_filter_run(f, this.k[c], x, this.s, this.used, mon)?;
                        Ok(Part::Segments(k, q))
                    }
                    _ => Ok(Part::Skip),
                };
            }
            // 出力の位置 m ∈ [b0, b1)（窓の中の位置）。作業列は x[m0 − 11..m1)（窓より前は直前の 11 標本）
            let b0 = (r - 1) * SUM_BLOCK;
            let b1 = (b0 + SUM_BLOCK).min(n);
            let mut w: Vec<f64> = Vec::new();
            w.try_reserve_exact(b1 - b0 + 11)
                .map_err(|_| EngineError::Limit("ラウドネスの作業領域を確保できません".into()))?;
            let mut finite = true;
            for j in 0..b1 - b0 + 11 {
                // 窓の中の位置 b0 + j − 11
                let v = match (b0 + j).checked_sub(11) {
                    Some(i) => x[i],
                    None => this.tail[c][b0 + j],
                };
                finite &= v.is_finite();
                w.push(f64::from(v));
            }
            let (true_peak, sample_peak) = true_peak_window(&w, &this.coef);
            Ok(Part::Peaks {
                true_peak,
                sample_peak,
                finite,
            })
        })?;
        for (t, p) in parts.into_iter().enumerate() {
            let c = t / stride;
            match p {
                Part::Segments(k, q) => {
                    self.k[c] = k;
                    self.segments[c].extend_from_slice(&q);
                }
                Part::Skip => {}
                Part::Peaks {
                    true_peak,
                    sample_peak,
                    finite,
                } => {
                    self.true_peak = math::max(self.true_peak, true_peak);
                    self.sample_peak = math::max(self.sample_peak, sample_peak);
                    self.finite &= finite;
                }
            }
        }
        // 直前の 11 標本を新しくする
        for (tail, x) in self.tail.iter_mut().zip(planes) {
            let mut joined = [0f32; 22];
            joined[..11].copy_from_slice(tail);
            let k = x.len().min(11);
            joined[11..11 + k].copy_from_slice(&x[x.len() - k..]);
            tail.copy_from_slice(&joined[k..k + 11]);
        }
        self.pos += n;
        Ok(())
    }

    /// 全体を受けたあと：出力の位置 m ∈ [len, len + 11) の流し切りを、直前の 11 標本と 0 で計る。受けた長さが全体と
    /// 違えば `Internal`。
    fn finish_pass(mut self) -> Result<(Pass, u32, usize, usize), EngineError> {
        if self.pos != self.frames {
            return Err(EngineError::Internal(format!(
                "ラウドネスの窓が全体を覆っていません（{} / {} フレーム）",
                self.pos, self.frames
            )));
        }
        for tail in &self.tail {
            let mut w = [0f64; 22];
            for (d, v) in w.iter_mut().zip(tail.iter()) {
                self.finite &= v.is_finite();
                *d = f64::from(*v);
            }
            let (tp, sp) = true_peak_window(&w, &self.coef);
            self.true_peak = math::max(self.true_peak, tp);
            self.sample_peak = math::max(self.sample_peak, sp);
        }
        let pass = Pass {
            segments: self.segments,
            sample_peak: self.sample_peak,
            true_peak: self.true_peak,
            finite: self.finite,
            filter: self.filter,
        };
        Ok((pass, self.rate, self.channels, self.frames))
    }

    /// 全体を受けたあと、計測（`analyze` と同じ値）。
    pub fn finish(self) -> Result<Measured, EngineError> {
        let (pass, rate, channels, frames) = self.finish_pass()?;
        Ok(finish(rate, channels, frames, pass, None)?.measured)
    }

    /// 全体を受けたあと、計測に短期ラウドネスのブロックのパワー（10 Hz）を添えたもの（`analyze_detailed` と同じ値）。
    pub fn finish_detailed(self) -> Result<Detailed, EngineError> {
        let (pass, rate, channels, frames) = self.finish_pass()?;
        finish(rate, channels, frames, pass, Some(SHORT_TERM_HOP))
    }
}

/// 値をチャンクの窓で流して `LoudnessScan` に渡す（`analyze`・`analyze_detailed` の走査）。
fn scan_value(a: &Audio, mon: &dyn Monitor) -> Result<LoudnessScan, EngineError> {
    let mut scan = LoudnessScan::new(a.rate(), a.channels(), a.frames() as u64)?;
    a.for_each_chunk(mon, |k, data| {
        let views: Vec<&[f32]> = data.iter().map(|d| &**d).collect();
        scan.feed(a.chunk_range(k).start as u64, &views, mon)
    })?;
    Ok(scan)
}

/// 区間のパワーから、`len_segments` 個の区間をつないだブロックのパワーを `hop` 区間ずつずらして求める
/// （チャンネル c の区間を先頭から順に足し、ブロックの標本の数で割り、重み w_c を掛けてチャンネルの順に足す）。
/// 400 ms のブロック（4 区間）は `((q0 + q1) + q2) + q3` の順になる（段 6 の式）。端数の区間は使わない。
fn joined_blocks(
    segments: &[Vec<f64>],
    w: &[f64],
    s: usize,
    len_segments: usize,
    hop: usize,
    total_segments: usize,
) -> Result<Vec<f64>, EngineError> {
    let count = if hop == 0 || len_segments == 0 || total_segments < len_segments {
        0
    } else {
        (total_segments - len_segments) / hop + 1
    };
    let mut z = Vec::new();
    z.try_reserve_exact(count)
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let len = len_segments as f64 * s as f64;
    for k in 0..count {
        let j = k * hop;
        let mut p = 0f64;
        for (ch, q) in segments.iter().enumerate() {
            let wc = w.get(ch).copied().unwrap_or(0.0);
            if wc == 0.0 {
                continue;
            }
            let Some(run) = q.get(j..j + len_segments) else {
                return Err(EngineError::Internal("ラウドネスの区間が足りません".into()));
            };
            // 先頭から順に足す（0 + q0 = q0 なので ((q0 + q1) + q2) + … と同じ）
            let block = run.iter().fold(0.0, |acc, v| acc + v);
            p += wc * (block / len);
        }
        z.push(p);
    }
    Ok(z)
}

/// 短期ラウドネスのブロックの間隔（区間の数）。1 区間（約 100 ms。10 Hz）ずつずらす
/// （EBU Tech 3342 v3 以降の「少なくとも 10 Hz」。libebur128 は 1 秒ごと）。
pub const SHORT_TERM_HOP: usize = 1;
/// 短期ラウドネスのブロックの長さ（区間の数。3 秒）。
pub const SHORT_TERM_SEGMENTS: usize = 30;

/// 計測（`analyze` と同じ値）に、表示（`audio.loudness`）に使う短期ラウドネスのブロックのパワーと、計れない理由を
/// 添えたもの。
#[derive(Debug, Clone, PartialEq)]
pub struct Detailed {
    /// `analyze` と同じ値（正規化が使う値。E26）。
    pub measured: Measured,
    /// 短期ラウドネス（3 秒）のブロックのパワー（区間 1 つずつずらす。重みを掛けてチャンネルの順に足したもの。
    /// ゲートなし）。計れない・3 秒に満たないときは空。
    pub short_term: Vec<f64>,
    /// 標本に NaN・∞ がある（どの値も計れない）。
    pub non_finite: bool,
}

fn finish(
    rate: u32,
    channels: usize,
    frames: usize,
    pass: Pass,
    short_term_hop: Option<usize>,
) -> Result<Detailed, EngineError> {
    if !pass.finite {
        return Ok(Detailed {
            measured: Measured {
                loudness: Loudness {
                    integrated: f64::NAN,
                    sample_peak_db: f64::NAN,
                    true_peak_db: f64::NAN,
                },
                blocks: Vec::new(),
                measurable: false,
            },
            short_term: Vec::new(),
            non_finite: true,
        });
    }
    let peaks = |integrated| Loudness {
        integrated,
        sample_peak_db: to_db(pass.sample_peak),
        true_peak_db: to_db(pass.true_peak),
    };
    if pass.filter.is_none() {
        return Ok(Detailed {
            measured: Measured {
                loudness: peaks(f64::NAN),
                blocks: Vec::new(),
                measurable: false,
            },
            short_term: Vec::new(),
            non_finite: false,
        });
    }
    let s = ((u64::from(rate) + 5) / 10) as usize;
    let w = weights(channels);
    let segments = frames / s;
    let z = joined_blocks(&pass.segments, &w, s, 4, 1, segments)?;
    let short_term = match short_term_hop {
        Some(hop) => joined_blocks(&pass.segments, &w, s, SHORT_TERM_SEGMENTS, hop, segments)?,
        None => Vec::new(),
    };
    Ok(Detailed {
        measured: Measured {
            loudness: peaks(integrated(&z, 1.0)),
            blocks: z,
            measurable: true,
        },
        short_term,
        non_finite: false,
    })
}

/// 計る（`measure` と同じ値に、利得をかけたあとの積分ラウドネスを求めるためのブロックのパワーを添える）。
pub fn analyze(a: &Audio, mon: &dyn Monitor) -> Result<Measured, EngineError> {
    scan_value(a, mon)?.finish()
}

/// 計る（`analyze` と同じ値に、短期ラウドネスのブロックのパワー（10 Hz）を添える。`audio.loudness` が使う）。
pub fn analyze_detailed(a: &Audio, mon: &dyn Monitor) -> Result<Detailed, EngineError> {
    scan_value(a, mon)?.finish_detailed()
}

/// ブロックのパワー（重みを掛けて足したもの）をラウドネス（LUFS）にする。0 以下は −∞。
pub fn power_to_lufs(p: f64) -> f64 {
    if p > 0.0 {
        OFFSET + 10.0 * math::log10(p)
    } else {
        f64::NEG_INFINITY
    }
}

/// ブロックのパワーの最大（ゲートなし。ブロックがなければ None）。最大のモーメンタリ（400 ms）・最大の短期（3 秒）に使う。
pub fn max_power(blocks: &[f64]) -> Option<f64> {
    (!blocks.is_empty()).then(|| blocks.iter().copied().fold(0.0, math::max))
}

/// LRA の順位（0 始まり）：n 個を昇順に並べたときの p パーセンタイル。補間なしの最近順位 ⌊((n−1)·p + 50) / 100⌋
/// （EBU Tech 3342 の MATLAB の `round((n−1)·p/100 + 1)`（1 始まり）と同じ。整数で求め、浮動小数の丸めに頼らない）。
pub fn range_rank(n: usize, percentile: u32) -> usize {
    let n = n as u128;
    if n == 0 {
        return 0;
    }
    let r = ((n - 1) * u128::from(percentile) + 50) / 100;
    usize::try_from(r).unwrap_or(usize::MAX)
}

/// ラウドネスレンジ（LRA、LU。EBU Tech 3342 v4）を短期ラウドネスのブロックのパワーから求める。
///
/// 1. 絶対ゲート：パワー ≥ 10^((−70 + 0.691)/10)（−70 LUFS。エネルギーで比べる）。
/// 2. 相対ゲート：通ったものの平均（番号の順に足す）の 0.01 倍（−20 LU）以上。
/// 3. 残った n 個を昇順に並べ、低 = 順位 `range_rank(n, 10)`、高 = `range_rank(n, 95)`。
/// 4. LRA = 10·log10(高) − 10·log10(低)（−0.691 は打ち消し合う）。
///
/// どちらのゲートも「≥」（Tech 3342。積分ラウドネスのゲートは BS.1770 の「>」で、比べ方が違う）。
/// 絶対ゲートを通るブロックがなければ None（相対ゲートは最大のブロックを必ず通すので、ここでは 0 個にならない）。
pub fn loudness_range(short_term: &[f64]) -> Result<Option<f64>, EngineError> {
    let abs_gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
    let mut kept = Vec::new();
    kept.try_reserve_exact(short_term.len())
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    kept.extend(short_term.iter().copied().filter(|p| *p >= abs_gate));
    if kept.is_empty() {
        return Ok(None);
    }
    // 相対ゲート：−20 LU（10^(−20/10) は 0.01 のリテラル）
    let rel = sum_ordered(&kept) / kept.len() as f64 * 0.01;
    kept.retain(|p| *p >= rel);
    kept.sort_by(|a, b| a.total_cmp(b));
    let n = kept.len();
    let (Some(lo), Some(hi)) = (kept.get(range_rank(n, 10)), kept.get(range_rank(n, 95))) else {
        return Ok(None);
    };
    Ok(Some(10.0 * math::log10(*hi) - 10.0 * math::log10(*lo)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks_of(levels: &[(f64, usize)]) -> Measured {
        let mut blocks = Vec::new();
        for &(lufs, n) in levels {
            let z = math::pow(10.0, (lufs - OFFSET) / 10.0);
            blocks.extend(std::iter::repeat_n(z, n));
        }
        Measured {
            loudness: Loudness {
                integrated: f64::NAN,
                sample_peak_db: f64::NAN,
                true_peak_db: f64::NAN,
            },
            blocks,
            measurable: true,
        }
    }

    /// 利得を厳密に解く：絶対ゲートをまたぐ区切りが多い形（検証で、反復では 32 回で −23.38 に止まった）・−70 より
    /// 静かな入力・ブロック 1 つでも、目標ちょうどに届く。
    #[test]
    fn gain_to_reaches_the_target_exactly() {
        let mut levels = vec![(-23.17, 100)];
        for i in 0..40 {
            levels.push((-49.20 - 0.05 * f64::from(i), 10 + 10 * i as usize));
        }
        for i in 0..40 {
            levels.push((-70.4 - 0.4 * f64::from(i), 50 + (i as usize % 29)));
        }
        let m = blocks_of(&levels);
        for target in [-23.0, -16.0, -40.0, -60.0, -68.9] {
            let g = m.gain_to(target).unwrap();
            let i = m.integrated_at(g);
            assert!((i - target).abs() < 1e-9, "{target}: {g} → {i}");
        }
        let quiet = blocks_of(&[(-75.0, 30), (-90.0, 30)]);
        assert_eq!(quiet.integrated_at(0.0), f64::NEG_INFINITY);
        let g = quiet.gain_to(-23.0).unwrap();
        assert!((quiet.integrated_at(g) + 23.0).abs() < 1e-9);
        let one = blocks_of(&[(-40.0, 1)]);
        let g = one.gain_to(-69.0).unwrap();
        assert!((one.integrated_at(g) + 69.0).abs() < 1e-9);
        // ブロックがない（無音・短い）
        assert_eq!(blocks_of(&[]).gain_to(-23.0), None);
        let mut silent = blocks_of(&[(-40.0, 3)]);
        silent.blocks.iter_mut().for_each(|b| *b = 0.0);
        assert_eq!(silent.gain_to(-23.0), None);
    }

    /// M13 の前の `gain_to`（候補ごとに近似なら `integrated_at` を評価して 1 つずつ比べる。O(n²)）。速い道の参照。
    fn gain_to_quadratic_reference(m: &Measured, target: f64) -> Option<f64> {
        if !m.measurable {
            return None;
        }
        let mut z: Vec<f64> = m.blocks.iter().copied().filter(|p| *p > 0.0).collect();
        if z.is_empty() {
            return None;
        }
        z.sort_by(|a, b| b.total_cmp(a));
        let abs_gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
        let thr = |p: f64| 10.0 * math::log10(abs_gate / p);
        let mut prefix = Vec::with_capacity(z.len() + 1);
        prefix.push(0.0f64);
        for p in &z {
            prefix.push(prefix[prefix.len() - 1] + p);
        }
        let mut best: Option<(f64, bool, f64)> = None;
        let consider = |g: f64, exact: bool, best: &mut Option<(f64, bool, f64)>| {
            let err = if exact {
                0.0
            } else {
                let i = m.integrated_at(g);
                if i.is_finite() {
                    (i - target).abs()
                } else {
                    f64::INFINITY
                }
            };
            let better = match *best {
                None => true,
                Some((bg, bexact, berr)) => match (exact, bexact) {
                    (true, false) => true,
                    (false, true) => false,
                    (true, true) => g.abs() < bg.abs() || (g.abs() == bg.abs() && g < bg),
                    (false, false) => err < berr,
                },
            };
            if better {
                *best = Some((g, exact, err));
            }
        };
        let mut c = 0usize;
        let mut k = 0usize;
        while k < z.len() {
            let mut end = k + 1;
            while end < z.len() && z[end] == z[k] {
                end += 1;
            }
            k = end;
            let lo = thr(z[k - 1]);
            let hi = z.get(k).map_or(f64::INFINITY, |p| thr(*p));
            let rel = prefix[k] / k as f64 * 0.1;
            while c < k && z[c] > rel {
                c += 1;
            }
            let h = OFFSET + 10.0 * math::log10(prefix[c] / c as f64);
            let g = target - h;
            if g > lo && g <= hi {
                consider(g, true, &mut best);
            } else if hi.is_finite() {
                consider(hi, false, &mut best);
            } else {
                consider(math::max(lo, g), false, &mut best);
            }
        }
        best.map(|(g, _, _)| g)
    }

    /// E19（M13 段 1）：`gain_to` の 2 段の求め方（解を先に、なければ近似を評価）は、旧い O(n²) の求め方とビット一致。
    /// ChaCha8 のランダムなブロックの列（同じ値の並び・0・絶対ゲートより小さい値・解のない並び）と、目標 −69〜0 と
    /// 解のない目標（−70 以下・NaN）で照らす。
    #[test]
    fn gain_to_matches_the_quadratic_reference() {
        use rand_chacha::rand_core::Rng;
        let mut r = crate::determinism::rng(0x6a17);
        let mut unit = move || f64::from(r.next_u32()) / 4_294_967_296.0;
        let mut seen_exact = 0usize;
        let mut seen_approx = 0usize;
        for case in 0..160usize {
            let n = 1 + (unit() * 160.0) as usize;
            let mut levels: Vec<(f64, usize)> = Vec::new();
            let repeat_level = -30.0 - unit() * 20.0;
            for _ in 0..n {
                let kind = unit();
                let lufs = if kind < 0.15 {
                    // 同じ値の並び
                    repeat_level
                } else if kind < 0.3 {
                    // 絶対ゲートより小さい値
                    -70.5 - unit() * 30.0
                } else if case % 7 == 0 {
                    // ゲートの跳びが大きい並び（解のない目標が出やすい）
                    if unit() < 0.5 { -10.0 } else { -69.0 }
                } else {
                    -75.0 + unit() * 75.0
                };
                levels.push((lufs, 1 + (unit() * 3.0) as usize));
            }
            let mut m = blocks_of(&levels);
            // 0 のブロック（無音）を混ぜる
            for b in m.blocks.iter_mut() {
                if unit() < 0.05 {
                    *b = 0.0;
                }
            }
            let mut targets: Vec<f64> = (0..6).map(|_| -69.0 + unit() * 69.0).collect();
            targets.extend([-23.0, 0.0, -69.0, -70.0, -85.0, f64::NAN]);
            for t in targets {
                let want = gain_to_quadratic_reference(&m, t);
                let got = m.gain_to(t);
                assert_eq!(
                    got.map(f64::to_bits),
                    want.map(f64::to_bits),
                    "case {case} target {t}: {got:?} ≠ {want:?}"
                );
                if let Some(g) = got {
                    if (m.integrated_at(g) - t).abs() < 1e-9 {
                        seen_exact += 1;
                    } else {
                        seen_approx += 1;
                    }
                }
            }
        }
        // 解のある場合も、ない場合（近似の評価）も網に入っている
        assert!(
            seen_exact > 40 && seen_approx > 40,
            "{seen_exact} {seen_approx}"
        );
    }

    /// 48 kHz の係数は BS.1770 の Table 1・2 と一致する。
    #[test]
    fn k_filter_matches_the_48k_table() {
        let [shelf, hp] = k_filter(48_000).unwrap();
        let want_shelf = [
            1.53512485958697,
            -2.69169618940638,
            1.19839281085285,
            -1.69065929318241,
            0.73248077421585,
        ];
        let got = [shelf.b[0], shelf.b[1], shelf.b[2], shelf.a[0], shelf.a[1]];
        for (g, w) in got.iter().zip(want_shelf) {
            assert!((g - w).abs() < 1e-12, "{g} {w}");
        }
        assert_eq!(hp.b, [1.0, -2.0, 1.0]);
        assert!((hp.a[0] - -1.99004745483398).abs() < 1e-12);
        assert!((hp.a[1] - 0.99007225036621).abs() < 1e-12);
        assert!(k_filter(3_000).is_err());
    }

    /// トゥルーピークの表：q3 は q0 の逆順、q2 は q1 の逆順。
    #[test]
    fn true_peak_table_is_symmetric() {
        for r in 0..12 {
            assert_eq!(TRUE_PEAK[r][3], TRUE_PEAK[11 - r][0]);
            assert_eq!(TRUE_PEAK[r][2], TRUE_PEAK[11 - r][1]);
        }
    }

    // ---- 段 6 の実装（参照。段 8 で速くした `analyze` とのビット一致を確かめる。変えない） ----

    fn true_peak_reference(x: &[f32]) -> f64 {
        let total = x.len() + 11;
        let blocks = par_blocks(total.div_ceil(SUM_BLOCK), &NoMonitor, |b| {
            let start = b * SUM_BLOCK;
            let end = start.saturating_add(SUM_BLOCK).min(total);
            let mut peak = 0f64;
            for m in start..end {
                for q in 0..4 {
                    let mut y = 0f64;
                    for (r, row) in TRUE_PEAK.iter().enumerate() {
                        if let Some(v) = m.checked_sub(r).and_then(|i| x.get(i)) {
                            y += f64::from(row[q]) / 8192.0 * f64::from(*v);
                        }
                    }
                    peak = math::max(peak, y.abs());
                }
                if let Some(v) = x.get(m) {
                    peak = math::max(peak, f64::from(v.abs()));
                }
            }
            Ok(peak)
        })
        .unwrap();
        blocks.into_iter().fold(0.0, math::max)
    }

    /// 段 6 の K 特性の区間のパワー（1 チャンネルの連続した列を順に流す）。
    fn segment_powers_reference(x: &[f32], filt: &[Biquad; 2], s: usize) -> Vec<f64> {
        let segments = x.len() / s;
        let mut q = Vec::new();
        let mut st = [[0f64; 4]; 2];
        let mut acc = 0f64;
        for (i, v) in x.iter().take(segments * s).enumerate() {
            let mut y = f64::from(*v);
            for (f, st) in filt.iter().zip(st.iter_mut()) {
                let out =
                    f.b[0] * y + f.b[1] * st[0] + f.b[2] * st[1] - f.a[0] * st[2] - f.a[1] * st[3];
                st[1] = st[0];
                st[0] = y;
                st[3] = st[2];
                st[2] = out;
                y = out;
            }
            acc += y * y;
            if (i + 1).is_multiple_of(s) {
                q.push(acc);
                acc = 0.0;
            }
        }
        q
    }

    fn analyze_reference(a: &Audio) -> Measured {
        let planes = a.to_planes().unwrap();
        if planes.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Measured {
                loudness: Loudness {
                    integrated: f64::NAN,
                    sample_peak_db: f64::NAN,
                    true_peak_db: f64::NAN,
                },
                blocks: Vec::new(),
                measurable: false,
            };
        }
        let mut sample_peak = 0f64;
        let mut true_peak_lin = 0f64;
        for plane in &planes {
            sample_peak = plane
                .iter()
                .fold(sample_peak, |m, v| math::max(m, f64::from(v.abs())));
            true_peak_lin = math::max(true_peak_lin, true_peak_reference(plane));
        }
        let peaks = |integrated| Loudness {
            integrated,
            sample_peak_db: to_db(sample_peak),
            true_peak_db: to_db(true_peak_lin),
        };
        let Ok(filt) = k_filter(a.rate()) else {
            return Measured {
                loudness: peaks(f64::NAN),
                blocks: Vec::new(),
                measurable: false,
            };
        };
        let s = ((u64::from(a.rate()) + 5) / 10) as usize;
        let w = weights(a.channels());
        let powers = par_blocks(planes.len(), &NoMonitor, |ch| {
            if w.get(ch).copied().unwrap_or(0.0) == 0.0 {
                return Ok(Vec::new());
            }
            Ok(segment_powers_reference(&planes[ch], &filt, s))
        })
        .unwrap();
        let segments = a.frames() / s;
        let blocks = segments.saturating_sub(3);
        let mut z = Vec::new();
        let len = 4.0 * s as f64;
        for j in 0..blocks {
            let mut p = 0f64;
            for (ch, q) in powers.iter().enumerate() {
                let wc = w.get(ch).copied().unwrap_or(0.0);
                if wc == 0.0 {
                    continue;
                }
                let block = ((q[j] + q[j + 1]) + q[j + 2]) + q[j + 3];
                p += wc * (block / len);
            }
            z.push(p);
        }
        Measured {
            loudness: peaks(integrated(&z, 1.0)),
            blocks: z,
            measurable: true,
        }
    }

    use crate::monitor::NoMonitor;
    use rand_chacha::rand_core::Rng;
    use std::sync::Arc;

    fn uniform(r: &mut rand_chacha::ChaCha8Rng) -> f64 {
        f64::from(r.next_u32()) / f64::from(u32::MAX) * 2.0 - 1.0
    }

    /// 音楽らしい信号：低域に寄せた雑音と正弦を混ぜ、`step` フレームごとに大きさ（`lo_db`〜`hi_db` の乱数）を変える。
    fn program(
        rate: u32,
        channels: usize,
        frames: usize,
        step: usize,
        (lo_db, hi_db): (f64, f64),
        seed: u64,
    ) -> Audio {
        let mut env = crate::determinism::rng(seed);
        let levels: Vec<f64> = (0..frames.div_ceil(step.max(1)))
            .map(|_| math::db_to_gain(lo_db + (hi_db - lo_db) * (uniform(&mut env) * 0.5 + 0.5)))
            .collect();
        let planes = (0..channels)
            .map(|c| {
                let mut r = crate::determinism::rng_stream(seed, c as u64 + 1);
                let mut lp = 0f64;
                let step_rad =
                    2.0 * std::f64::consts::PI * (220.0 + 110.0 * c as f64) / f64::from(rate);
                Arc::new(
                    (0..frames)
                        .map(|i| {
                            lp = 0.7 * lp + 0.3 * uniform(&mut r);
                            let s = math::sin(step_rad * i as f64);
                            (levels[i / step.max(1)] * (0.6 * lp + 0.4 * s)) as f32
                        })
                        .collect::<Vec<f32>>(),
                )
            })
            .collect();
        Audio::new(rate, planes, crate::value::AudioMeta::default()).unwrap()
    }

    /// 重いテストを回す小さなスレッドの池（2 スレッド）。
    fn small_pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
    }

    fn same_measured(a: &Measured, b: &Measured) -> bool {
        a.loudness.integrated.to_bits() == b.loudness.integrated.to_bits()
            && a.loudness.sample_peak_db.to_bits() == b.loudness.sample_peak_db.to_bits()
            && a.loudness.true_peak_db.to_bits() == b.loudness.true_peak_db.to_bits()
            && a.measurable == b.measurable
            && a.blocks.len() == b.blocks.len()
            && a.blocks
                .iter()
                .zip(&b.blocks)
                .all(|(x, y)| x.to_bits() == y.to_bits())
    }

    /// 速くした `analyze` は段 6 の実装とビット単位で同じ（ChaCha8 の多チャンネル・多レート・端の長さ・NaN・∞・無音・
    /// 3,363 Hz 以下）。スレッド数によらない。
    #[test]
    fn analyze_matches_the_stage6_reference_bit_for_bit() {
        let mut cases: Vec<Audio> = Vec::new();
        let rates = [3_000u32, 8_000, 11_025, 22_050, 44_100, 48_000, 96_000];
        let lengths = [0usize, 1, 5, 11, 12, 13, 4_095, 4_096, 4_097, 4_107, 9_000];
        let mut seed = 0u64;
        for (k, rate) in rates.iter().enumerate() {
            for (i, frames) in lengths.iter().enumerate() {
                let channels = [1usize, 2, 3, 5, 6, 8, 9][(k + i) % 7];
                seed += 1;
                cases.push(program(*rate, channels, *frames, 1_000, (-60.0, 0.0), seed));
            }
        }
        // 長め（ブロックが多い・ゲートをまたぐ）
        cases.push(program(48_000, 2, 48_000, 4_800, (-90.0, -10.0), 101));
        cases.push(program(44_100, 6, 30_000, 2_000, (-75.0, -20.0), 102));
        // 大きな値（1 を超える）・無音・NaN・∞（ブロックの頭・境・終わり）
        cases.push(program(48_000, 2, 20_000, 3_000, (0.0, 30.0), 103));
        cases.push(
            Audio::new(
                48_000,
                vec![Arc::new(vec![0.0; 20_000]), Arc::new(vec![0.0; 20_000])],
                Default::default(),
            )
            .unwrap(),
        );
        for (bad, at) in [
            (f32::NAN, 0usize),
            (f32::INFINITY, 4_100),
            (f32::NEG_INFINITY, 9_999),
        ] {
            let a = program(44_100, 2, 10_000, 3_000, (-30.0, -10.0), 104);
            let mut p = (*a.to_planes().unwrap()[1]).clone();
            p[at] = bad;
            cases.push(
                Audio::new(
                    44_100,
                    vec![a.to_planes().unwrap()[0].clone(), Arc::new(p)],
                    Default::default(),
                )
                .unwrap(),
            );
        }
        // 重いので小さなスレッドの池で回す（同時に走るほかのテストの時間を圧迫しない）
        small_pool().install(|| {
            for (i, a) in cases.iter().enumerate() {
                let want = analyze_reference(a);
                let got = analyze(a, &NoMonitor).unwrap();
                assert!(
                    same_measured(&got, &want),
                    "{i}（{} Hz・{} ch・{} フレーム）：{got:?} と {want:?}",
                    a.rate(),
                    a.channels(),
                    a.frames()
                );
                if i % 4 == 0 {
                    let detailed = analyze_detailed(a, &NoMonitor).unwrap();
                    assert!(same_measured(&detailed.measured, &want), "{i}");
                }
            }
        });
        // スレッド数によらない
        let a = &cases[cases.len() - 5];
        let want = analyze_reference(a);
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let got = pool.install(|| analyze(a, &NoMonitor).unwrap());
            assert!(same_measured(&got, &want), "{threads} スレッド");
        }
    }

    /// E39（M13 段 1）：`LoudnessScan` に窓（1・7・4,096・C・ランダムな切れ目）で渡した結果は、段 6 の実装（参照）と
    /// ビット単位で同じ（短期のパワーも窓によらない）。順でない窓・長さの外・足りない窓は断る。
    #[test]
    fn scan_by_windows_equals_the_reference() {
        use rand_chacha::rand_core::Rng;
        let c = crate::value::AUDIO_CHUNK;
        let mut cases = vec![
            program(48_000, 2, 20_000, 3_000, (-40.0, -5.0), 201),
            program(48_000, 2, 1_900, 300, (-40.0, -5.0), 209),
            program(8_000, 1, 2 * c + 37, 2_000, (-80.0, -10.0), 202),
            program(44_100, 6, 9_000, 1_000, (-30.0, 0.0), 203),
            program(3_000, 2, 5_000, 500, (-20.0, 0.0), 204),
            program(48_000, 1, 7, 1, (-10.0, 0.0), 205),
            program(22_050, 2, 0, 1, (-10.0, 0.0), 206),
        ];
        let a = program(44_100, 2, 10_000, 3_000, (-30.0, -10.0), 207);
        let mut p = (*a.to_planes().unwrap()[0]).clone();
        p[6_000] = f32::NAN;
        cases.push(
            Audio::new(
                44_100,
                vec![Arc::new(p), a.to_planes().unwrap()[1].clone()],
                Default::default(),
            )
            .unwrap(),
        );
        let mut r = crate::determinism::rng(208);
        small_pool().install(|| {
            for (i, a) in cases.iter().enumerate() {
                let want = analyze_reference(a);
                let want_detailed = analyze_detailed(a, &NoMonitor).unwrap();
                let planes = a.to_planes().unwrap();
                let n = a.frames();
                for step in [1usize, 7, 4_096, c, 0] {
                    // 細かい窓は短い音声だけ（窓ごとに並列の仕事を立てるので、デバッグのビルドで重い）
                    if (step == 1 && n > 2_000) || (step == 7 && n > 20_000) {
                        continue;
                    }
                    let mut scan = LoudnessScan::new(a.rate(), a.channels(), n as u64).unwrap();
                    let mut at = 0;
                    while at < n {
                        // 0 はランダムな切れ目
                        let len = if step == 0 {
                            1 + (r.next_u64() % 9_000) as usize
                        } else {
                            step
                        };
                        let end = (at + len).min(n);
                        let views: Vec<&[f32]> = planes.iter().map(|p| &p[at..end]).collect();
                        scan.feed(at as u64, &views, &NoMonitor).unwrap();
                        at = end;
                    }
                    let got = scan.finish_detailed().unwrap();
                    assert!(
                        same_measured(&got.measured, &want),
                        "{i}・窓 {step}：{:?} と {want:?}",
                        got.measured
                    );
                    assert_eq!(got.short_term, want_detailed.short_term, "{i}・窓 {step}");
                }
            }
        });
        let a = &cases[0];
        let planes = a.to_planes().unwrap();
        let views =
            |s: usize, e: usize| -> Vec<&[f32]> { planes.iter().map(|p| &p[s..e]).collect() };
        let mut scan = LoudnessScan::new(a.rate(), 2, a.frames() as u64).unwrap();
        assert!(scan.feed(5, &views(5, 10), &NoMonitor).is_err());
        assert!(scan.feed(0, &views(0, 10)[..1], &NoMonitor).is_err());
        scan.feed(0, &views(0, 10), &NoMonitor).unwrap();
        assert!(scan.finish().is_err(), "足りない窓");
        assert!(LoudnessScan::new(48_000, 0, 1).is_err());
    }

    /// 理由のある値：NaN・∞ は `non_finite`、3,363 Hz 以下は計れない（ピークだけ）、3 秒に満たなければ短期は空。
    #[test]
    fn detailed_reports_short_term_blocks_and_reasons() {
        // 8 kHz（S = 800）：30 区間ちょうどで短期のブロック 1 つ、1 区間足すと 2 つ
        for (frames, want) in [
            (23_999usize, 0usize),
            (24_000, 1),
            (24_799, 1),
            (24_800, 2),
            (48_000, 31),
        ] {
            let a = program(8_000, 2, frames, 800, (-30.0, -10.0), 7);
            let d = analyze_detailed(&a, &NoMonitor).unwrap();
            assert_eq!(d.short_term.len(), want, "{frames}");
            assert!(!d.non_finite);
            // 400 ms のブロックは `analyze` と同じ
            assert_eq!(d.measured.blocks.len(), (frames / 800).saturating_sub(3));
        }
        let low = program(3_000, 1, 30_000, 300, (-20.0, -10.0), 8);
        let d = analyze_detailed(&low, &NoMonitor).unwrap();
        assert!(!d.non_finite && !d.measured.measurable && d.short_term.is_empty());
        assert!(d.measured.loudness.sample_peak_db.is_finite());
        let mut p = (*program(8_000, 1, 30_000, 800, (-20.0, -10.0), 9)
            .to_planes()
            .unwrap()[0])
            .clone();
        p[10] = f32::NAN;
        let nan = Audio::new(8_000, vec![Arc::new(p)], Default::default()).unwrap();
        let d = analyze_detailed(&nan, &NoMonitor).unwrap();
        assert!(d.non_finite && d.short_term.is_empty() && d.measured.blocks.is_empty());
        // 一定の大きさの正弦：短期のブロックはどれも 400 ms のブロックとほぼ同じ値
        let sine = Audio::new(
            48_000,
            vec![Arc::new(
                (0..48_000 * 4)
                    .map(|i| {
                        (0.25
                            * math::sin(
                                2.0 * std::f64::consts::PI * 1_000.0 * f64::from(i) / 48_000.0,
                            )) as f32
                    })
                    .collect(),
            )],
            Default::default(),
        )
        .unwrap();
        let d = analyze_detailed(&sine, &NoMonitor).unwrap();
        let m = max_power(&d.measured.blocks).unwrap();
        let st = max_power(&d.short_term).unwrap();
        assert!((power_to_lufs(m) - power_to_lufs(st)).abs() < 0.01);
        assert!((loudness_range(&d.short_term).unwrap().unwrap()).abs() < 0.01);
        assert_eq!(max_power(&[]), None);
        assert_eq!(power_to_lufs(0.0), f64::NEG_INFINITY);
    }

    /// 順位の整数の式は、MATLAB の `round((n−1)·p/100 + 1)`（f64）と、libebur128 の `(size_t)((n−1)·0.95 + 0.5)` と同じ。
    #[test]
    fn range_rank_matches_matlab_and_libebur128() {
        for n in 1usize..=20_000 {
            for p in [10u32, 95] {
                let matlab = ((n - 1) as f64 * f64::from(p) / 100.0 + 1.0).round() as usize - 1;
                let libebur128 = ((n - 1) as f64 * (f64::from(p) / 100.0) + 0.5) as usize;
                assert_eq!(range_rank(n, p), matlab, "{n} {p}");
                assert_eq!(range_rank(n, p), libebur128, "{n} {p}");
            }
        }
        assert_eq!(range_rank(0, 95), 0);
        assert_eq!(range_rank(1, 10), 0);
        assert_eq!(range_rank(1, 95), 0);
    }

    /// LRA の定義：ゲートはどちらも「≥」、補間なしの順位、10·log10(高) − 10·log10(低)。
    #[test]
    fn loudness_range_follows_tech_3342() {
        let z = |lufs: f64| math::pow(10.0, (lufs - OFFSET) / 10.0);
        // 2 つの大きさ（同じ数）：低 = −30、高 = −20 で 10 LU
        let mut st: Vec<f64> = std::iter::repeat_n(z(-20.0), 100).collect();
        st.extend(std::iter::repeat_n(z(-30.0), 100));
        let lra = loudness_range(&st).unwrap().unwrap();
        assert!((lra - 10.0).abs() < 1e-9, "{lra}");
        // 一定なら 0
        assert_eq!(loudness_range(&[z(-23.0); 10]).unwrap(), Some(0.0));
        // −70 LUFS ちょうどは絶対ゲートを通る（≥）。より静かなものだけなら None
        let gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
        assert_eq!(loudness_range(&[gate, gate]).unwrap(), Some(0.0));
        assert_eq!(loudness_range(&[gate * 0.999_999, 0.0]).unwrap(), None);
        assert_eq!(loudness_range(&[]).unwrap(), None);
        // 相対ゲート：平均（−20 LUFS が 99 個と −50 LUFS が 1 個で約 −20.04）の −20 LU より静かな値は落ちる
        let mut st: Vec<f64> = std::iter::repeat_n(z(-20.0), 99).collect();
        st.push(z(-50.0));
        let lra = loudness_range(&st).unwrap().unwrap();
        assert_eq!(lra, 0.0, "相対ゲートより静かな値は落ちる");
        // −39 LUFS は相対ゲート（約 −40.04）を通るので、低い順位に入る（n = 100 で順位 10 は −39 の 10 個のうち）
        let mut st: Vec<f64> = std::iter::repeat_n(z(-20.0), 89).collect();
        st.extend(std::iter::repeat_n(z(-39.0), 11));
        let lra = loudness_range(&st).unwrap().unwrap();
        assert!((lra - 19.0).abs() < 1e-9, "{lra}");
        // 低い順位・高い順位：n = 21 個の −40〜−20（1 LU ずつ）。低 = 順位 2（−38）、高 = 順位 19（−21）
        let st: Vec<f64> = (0..21).map(|i| z(-40.0 + f64::from(i))).collect();
        let lra = loudness_range(&st).unwrap().unwrap();
        assert!((lra - 17.0).abs() < 1e-9, "{lra}");
    }

    /// LRA の相対ゲートの平均は、絶対ゲートを通ったブロックだけで取る（Tech 3342。検証で、全ブロックの平均に変えても
    /// どのテストも落ちなかった）。−70 LUFS より静かな区間（無音・静かな導入）が多いと、全ブロックの平均では相対
    /// ゲートが下がり、落ちるはずのブロックが残る。
    #[test]
    fn loudness_range_relative_gate_averages_only_the_gated_blocks() {
        let z = |lufs: f64| math::pow(10.0, (lufs - OFFSET) / 10.0);
        // −80 × 400（絶対ゲートで落ちる）、−20 × 50、−45 × 50。通ったものの平均は約 −23 LUFS で、相対ゲートは約 −43：
        // −45 は落ちて LRA 0。全ブロックの平均（約 −30）なら相対ゲートは約 −50 で −45 が残り、LRA 25 LU になる
        let mut st: Vec<f64> = std::iter::repeat_n(z(-80.0), 400).collect();
        st.extend(std::iter::repeat_n(z(-20.0), 50));
        st.extend(std::iter::repeat_n(z(-45.0), 50));
        assert_eq!(loudness_range(&st).unwrap(), Some(0.0));
        // 静かなブロックの位置（前・間・後ろ）によらない
        let mut mixed: Vec<f64> = Vec::new();
        for i in 0..500 {
            mixed.push(match i % 10 {
                0 => z(-20.0),
                5 => z(-45.0),
                _ => z(-80.0),
            });
        }
        assert_eq!(loudness_range(&mixed).unwrap(), Some(0.0));
        // 比べる：静かなブロックがなければ同じ値（−20 × 50・−45 × 50 だけでも −45 は落ちる）
        let mut loud = std::iter::repeat_n(z(-20.0), 50).collect::<Vec<_>>();
        loud.extend(std::iter::repeat_n(z(-45.0), 50));
        assert_eq!(loudness_range(&loud).unwrap(), Some(0.0));
        // 相対ゲートを通る大きさ（−35）なら、静かなブロックがあっても低い順位に入る（LRA 15）
        let mut st: Vec<f64> = std::iter::repeat_n(z(-80.0), 400).collect();
        st.extend(std::iter::repeat_n(z(-20.0), 50));
        st.extend(std::iter::repeat_n(z(-35.0), 50));
        let lra = loudness_range(&st).unwrap().unwrap();
        assert!((lra - 15.0).abs() < 1e-9, "{lra}");
    }

    /// ゲートの「ちょうど」の境：LRA の相対ゲートは「≥」（Tech 3342）、積分ラウドネスの絶対ゲート（BS.1770 の式 (7)）と
    /// 相対ゲートは「>」（BS.1770）。積分のほうは、`gain_to` が解く区間（下の端を含まず、上の端を含む (t_m, t_{m+1}]）と、
    /// `gain_to` の中の相対ゲートとも一致する。
    #[test]
    fn gates_at_the_exact_boundary() {
        // (a) LRA の相対ゲート：1.0 × 20 と 124.75 × 80 は、和 10000・平均 100 で、相対ゲートがちょうど 1.0 になる
        let mut st: Vec<f64> = std::iter::repeat_n(1.0, 20).collect();
        st.extend(std::iter::repeat_n(124.75, 80));
        assert_eq!(sum_ordered(&st), 10_000.0);
        assert_eq!(sum_ordered(&st) / st.len() as f64 * 0.01, 1.0);
        // ≥ なので 1.0 は残り、低 = 1.0（順位 10）、高 = 124.75（順位 94）。> なら 1.0 が落ちて 0
        let lra = loudness_range(&st).unwrap().unwrap();
        assert_eq!(lra, 10.0 * math::log10(124.75) - 10.0 * math::log10(1.0));
        assert!((lra - 20.960_405_54).abs() < 1e-6, "{lra}");

        // (b) 積分ラウドネスの絶対ゲート：ブロックのパワーがちょうど −70 LUFS（A）なら通らない（−∞）、1 ULP 上なら通る
        let gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
        let up = f64::from_bits(gate.to_bits() + 1);
        let exact = Measured::from_parts(blocks_of(&[]).loudness, vec![gate; 4], true);
        assert_eq!(exact.integrated_at(0.0), f64::NEG_INFINITY);
        let above = Measured::from_parts(blocks_of(&[]).loudness, vec![up; 4], true);
        let i = above.integrated_at(0.0);
        assert!((i + 70.0).abs() < 1e-9, "{i}");

        // gain_to の区間との一致：ブロック A がゲートを通る利得の区間の端 t = 10·log10(A / A) = 0 は、通らない側
        // （区間は g > t）。上から 1 個（2A）だけが通る区間は (10·log10(1/2), 0] で、その上の端 g = 0 でも A は通らない
        let two = Measured::from_parts(blocks_of(&[]).loudness, vec![2.0 * gate, gate], true);
        let only_top = OFFSET + 10.0 * math::log10(2.0 * gate);
        assert_eq!(two.integrated_at(0.0), only_top);
        // 目標がちょうど上の端の利得 0 になるとき、gain_to は 0 を解とし、そこでの積分ラウドネスは目標そのもの
        // （絶対ゲートを「≥」にすると、利得 0 で A も通って平均 1.5A になり、目標からずれる）
        assert_eq!(two.gain_to(only_top), Some(0.0));
        assert_eq!(two.integrated_at(0.0), only_top);
        // 区間 (0, ∞)：A も通る。わずかに上げると 2 つの平均（相対ゲートは 0.15A で両方通る）
        let g = 1e-6;
        let both = OFFSET + 10.0 * math::log10(1.5 * gate) + g;
        assert!((two.integrated_at(g) - both).abs() < 1e-9);
        let solved = two.gain_to(both).unwrap();
        assert!(solved > 0.0 && (solved - g).abs() < 1e-9, "{solved}");

        // (c) 積分ラウドネスの相対ゲート：ブロックのパワー 1.0 と 11.0 × 9 は、和 100・平均 10 で、相対ゲートがちょうど
        // 1.0 になる。「>」なので 1.0 は落ち、11.0 の 9 個の平均で −0.691 + 10·log10 11（「≥」なら 1.0 も残って平均 10）
        let mut z = vec![1.0];
        z.extend(std::iter::repeat_n(11.0, 9));
        assert_eq!(sum_ordered(&z), 100.0);
        assert_eq!(sum_ordered(&z) / z.len() as f64 * 0.1, 1.0);
        let edge = Measured::from_parts(blocks_of(&[]).loudness, z, true);
        let want = OFFSET + 10.0 * math::log10(11.0);
        assert!((want - 9.722_926_851_582).abs() < 1e-9, "{want}");
        assert_eq!(edge.integrated_at(0.0), want);
        // gain_to の中の相対ゲートも「>」：目標がちょうどその値なら利得 0（「≥」なら 1.0 も数えて、10·log10 1.1 ≈ 0.41 dB
        // 余計に上げる）
        assert_eq!(edge.gain_to(want), Some(0.0));
        assert_eq!(edge.integrated_at(edge.gain_to(want).unwrap()), want);
    }

    /// 照合（dev 依存の ebur128 0.1.10）：短期を 1 秒ごとにした内部の関数の LRA が ebur128（リストのモード）と 1e-6 LU、
    /// 48 kHz の積分ラウドネスが 1e-6 LU、44.1 kHz で 0.01 LU（kura は 2 段目の分子を 48 kHz の特性にそろえる）、
    /// サンプルピークが同じ。
    #[test]
    fn matches_ebur128_with_one_second_short_term() {
        use ebur128::{EbuR128, Mode};
        // （レート, チャンネル数, 秒, 大きさを変える間隔, 大きさの範囲, 種）。6 チャンネルの並びは ebur128 の既定と同じ
        let cases = [
            (
                48_000u32,
                2usize,
                12usize,
                48_000usize,
                (-40.0, -6.0),
                11u64,
            ),
            (48_000, 1, 10, 4_800, (-35.0, -10.0), 12),
            (48_000, 6, 5, 24_000, (-80.0, -15.0), 13),
            (44_100, 2, 5, 44_100, (-40.0, -6.0), 15),
        ];
        let mut inputs: Vec<(Audio, String)> = cases
            .iter()
            .map(|&(rate, channels, secs, step, range, seed)| {
                (
                    program(rate, channels, rate as usize * secs, step, range, seed),
                    format!("{rate} Hz・{channels} ch・種 {seed}"),
                )
            })
            .collect();
        // 無音（−70 LUFS より静か）で始まる形：20 秒の無音・10 秒の −20 dBFS・10 秒の −45 dBFS の 997 Hz の正弦。
        // 相対ゲートの平均を絶対ゲートを通ったブロックで取れば −45 の短期（約 −48 LUFS）は落ちて LRA は約 4.7 LU、
        // 全ブロックの平均で取ると残って約 25 LU になる（ebur128 も通ったブロックの平均で取る）
        let quiet_intro = {
            let rate = 48_000usize;
            let tone = |db: f64, from: usize| {
                let amp = math::db_to_gain(db);
                (from..from + rate * 10).map(move |i| {
                    (amp * math::sin(2.0 * std::f64::consts::PI * 997.0 * i as f64 / rate as f64))
                        as f32
                })
            };
            let mut p = vec![0f32; rate * 20];
            p.extend(tone(-20.0, rate * 20));
            p.extend(tone(-45.0, rate * 30));
            Audio::new(48_000, vec![Arc::new(p)], Default::default()).unwrap()
        };
        inputs.push((quiet_intro, "無音で始まる 48 kHz・1 ch".into()));
        let abs_gate = math::pow(10.0, (-70.0 - OFFSET) / 10.0);
        small_pool().install(|| {
            for (a, ctx) in &inputs {
                let (rate, channels) = (a.rate(), a.channels());
                let (ours, ten_hz) = analyze_with_one_second_hop(a);
                // トゥルーピークは照らさない（ebur128 は別の補間で、debug では遅い。Tech 3341 の試験は E19 で確かめる）
                let mode = Mode::I | Mode::LRA | Mode::SAMPLE_PEAK;
                let mut e = EbuR128::new(channels as u32, rate, mode).unwrap();
                let owned = a.to_planes().unwrap();
                let planes: Vec<&[f32]> = owned.iter().map(|p| p.as_slice()).collect();
                e.add_frames_planar_f32(&planes).unwrap();
                let lra = loudness_range(&ours.short_term).unwrap().unwrap();
                let want_lra = e.loudness_range().unwrap();
                let want_i = e.loudness_global().unwrap();
                if ctx.starts_with("無音") {
                    // 絶対ゲートで落ちる短期のブロックがあり、−45 の短期は相対ゲートで落ちる
                    assert!(ours.short_term.iter().any(|p| *p < abs_gate), "{ctx}");
                    assert!((lra - 4.7).abs() < 0.2, "{ctx}：LRA {lra}");
                }
                assert!(
                    (lra - want_lra).abs() < 1e-6,
                    "{ctx}：LRA {lra} と {want_lra}"
                );
                let i = ours.measured.loudness.integrated;
                let tol = if rate == 48_000 { 1e-6 } else { 0.01 };
                assert!((i - want_i).abs() < tol, "{ctx}：積分 {i} と {want_i}");
                let sp = (0..channels as u32)
                    .map(|c| e.sample_peak(c).unwrap())
                    .fold(0.0, math::max);
                assert!(
                    (ours.measured.loudness.sample_peak_db - 20.0 * math::log10(sp)).abs() < 1e-9,
                    "{ctx}：サンプルピーク"
                );
                // 本番の 10 Hz の窓は、1 秒ごとの窓を 10 個おきに含む（同じ窓はビット単位で同じ）
                let segments = a.frames() / ((rate as usize + 5) / 10);
                assert_eq!(ten_hz.len(), segments - 29, "{ctx}");
                assert_eq!(ours.short_term.len(), (segments - 30) / 10 + 1, "{ctx}");
                for (k, p) in ours.short_term.iter().enumerate() {
                    assert_eq!(ten_hz[k * 10].to_bits(), p.to_bits(), "{ctx}");
                }
            }
        });
        // 本番の口（10 Hz）も同じ走査
        let a = program(48_000, 1, 48_000 * 5, 4_800, (-30.0, -10.0), 16);
        let (ours, ten_hz) = analyze_with_one_second_hop(&a);
        let detailed = analyze_detailed(&a, &NoMonitor).unwrap();
        assert!(same_measured(&detailed.measured, &ours.measured));
        assert_eq!(detailed.short_term, ten_hz);
    }

    /// 短期ラウドネスを 1 秒ごと（`hop` = 10 区間。libebur128 と同じ）にした計測と、同じ走査の 10 Hz の短期のパワー
    /// （照合だけ）。
    fn analyze_with_one_second_hop(a: &Audio) -> (Detailed, Vec<f64>) {
        let (pass, rate, channels, frames) =
            scan_value(a, &NoMonitor).unwrap().finish_pass().unwrap();
        let s = ((u64::from(a.rate()) + 5) / 10) as usize;
        let ten_hz = joined_blocks(
            &pass.segments,
            &weights(a.channels()),
            s,
            SHORT_TERM_SEGMENTS,
            SHORT_TERM_HOP,
            a.frames() / s,
        )
        .unwrap();
        (
            finish(rate, channels, frames, pass, Some(10)).unwrap(),
            ten_hz,
        )
    }

    /// ベンチ（ふだんは回さない）：`cargo test --release -p kura-engine --lib analysis::loudness::tests::bench -- --ignored --nocapture`。
    /// 段 6 の実装（参照）と速くした `analyze`、短期と LRA を足した `analyze_detailed` の時間（3 回の最小）。
    #[test]
    #[ignore]
    #[allow(clippy::disallowed_methods)]
    fn bench_analyze_against_reference() {
        let time = |f: &dyn Fn() -> Measured| {
            let mut best = f64::INFINITY;
            let mut out = None;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                out = Some(f());
                best = best.min(t.elapsed().as_secs_f64());
            }
            (best, out.unwrap())
        };
        for (rate, secs) in [(44_100u32, 300usize), (48_000, 300), (48_000, 3_600)] {
            let a = program(
                rate,
                2,
                rate as usize * secs,
                rate as usize,
                (-40.0, -6.0),
                1,
            );
            for threads in [1usize, 16] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                pool.install(|| {
                    let (t0, m0) = time(&|| analyze_reference(&a));
                    let (t1, m1) = time(&|| analyze(&a, &NoMonitor).unwrap());
                    let (t2, m2) = time(&|| analyze_detailed(&a, &NoMonitor).unwrap().measured);
                    assert!(same_measured(&m0, &m1) && same_measured(&m0, &m2));
                    eprintln!(
                        "{rate} Hz・{secs} 秒・ステレオ・{threads} スレッド：段 6 {t0:.3} 秒 → {t1:.3} 秒（短期・LRA 込み {t2:.3} 秒）"
                    );
                });
            }
        }
    }
}
