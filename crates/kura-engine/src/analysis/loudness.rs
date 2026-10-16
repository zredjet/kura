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

/// サンプルレート `fs` の K 特性の 2 段の係数。
pub fn k_filter(fs: u32) -> Result<[Biquad; 2], EngineError> {
    let fs = f64::from(fs);
    if fs <= 2.0 * SHELF_F0 {
        return Err(EngineError::Unsupported(format!(
            "サンプルレート {fs} Hz ではラウドネスを計れません"
        )));
    }
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

/// 1 チャンネルを K 特性に通し、S 標本ずつの二乗和を返す（端数の区間は入れない）。
fn segment_powers(
    x: &[f32],
    filt: &[Biquad; 2],
    s: usize,
    mon: &dyn Monitor,
) -> Result<Vec<f64>, EngineError> {
    let segments = x.len() / s;
    let mut q = Vec::new();
    q.try_reserve_exact(segments)
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    // 段ごとの状態：x1・x2・y1・y2
    let mut st = [[0f64; 4]; 2];
    let mut acc = 0f64;
    for (i, v) in x.iter().take(segments * s).enumerate() {
        if i % CHECK_EVERY == 0 && mon.canceled() {
            return Err(EngineError::Canceled);
        }
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
        if (i + 1) % s == 0 {
            q.push(acc);
            acc = 0.0;
        }
    }
    Ok(q)
}

/// 1 チャンネルのトゥルーピーク（線形。サンプルピークとの大きいほう）。
fn true_peak(x: &[f32], mon: &dyn Monitor) -> Result<f64, EngineError> {
    // 出力の位置 m（0..len+11）を SUM_BLOCK ずつに分けて並列に。最大は分け方によらない
    let total = x.len() + 11;
    let blocks = par_blocks(total.div_ceil(SUM_BLOCK), mon, |b| {
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
    })?;
    Ok(blocks.into_iter().fold(0.0, math::max))
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
    /// `integrated_at` が target にいちばん近い g。
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
        let mut best: Option<(f64, bool, f64)> = None; // (g, 解か, 誤差)
        let consider = |g: f64, exact: bool, best: &mut Option<(f64, bool, f64)>| {
            let err = if exact {
                0.0
            } else {
                let i = self.integrated_at(g);
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
                consider(g, true, &mut best);
            } else if hi.is_finite() {
                consider(hi, false, &mut best);
            } else {
                consider(math::max(lo, g), false, &mut best);
            }
        }
        best.map(|(g, _, _)| g)
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

/// 計る（`measure` と同じ値に、利得をかけたあとの積分ラウドネスを求めるためのブロックのパワーを添える）。
pub fn analyze(a: &Audio, mon: &dyn Monitor) -> Result<Measured, EngineError> {
    let planes = a.planes();
    if planes.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
        return Ok(Measured {
            loudness: Loudness {
                integrated: f64::NAN,
                sample_peak_db: f64::NAN,
                true_peak_db: f64::NAN,
            },
            blocks: Vec::new(),
            measurable: false,
        });
    }
    let mut sample_peak = 0f64;
    let mut true_peak_lin = 0f64;
    for plane in planes {
        sample_peak = plane
            .iter()
            .fold(sample_peak, |m, v| math::max(m, f64::from(v.abs())));
        true_peak_lin = math::max(true_peak_lin, true_peak(plane, mon)?);
    }
    let peaks = |integrated| Loudness {
        integrated,
        sample_peak_db: to_db(sample_peak),
        true_peak_db: to_db(true_peak_lin),
    };
    let Ok(filt) = k_filter(a.rate()) else {
        return Ok(Measured {
            loudness: peaks(f64::NAN),
            blocks: Vec::new(),
            measurable: false,
        });
    };
    let s = ((u64::from(a.rate()) + 5) / 10) as usize;
    let w = weights(a.channels());
    // チャンネルごとに K 特性（IIR なので時間方向には分けない）
    let powers = par_blocks(planes.len(), mon, |ch| {
        if w.get(ch).copied().unwrap_or(0.0) == 0.0 {
            return Ok(Vec::new());
        }
        segment_powers(&planes[ch], &filt, s, mon)
    })?;
    let segments = a.frames() / s;
    let blocks = segments.saturating_sub(3);
    let mut z = Vec::new();
    z.try_reserve_exact(blocks)
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
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
    Ok(Measured {
        loudness: peaks(integrated(&z, 1.0)),
        blocks: z,
        measurable: true,
    })
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
}
