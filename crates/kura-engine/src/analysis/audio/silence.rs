//! `audio.silence` v1：前後の無音とクリップ。
//!
//! - 引数 `thresholdDb`（−120〜0 dBFS、既定 −60。ffmpeg の silencedetect の既定 0.001 と同じ）。閾値 t =
//!   `math::db_to_gain(thresholdDb)`。フレーム i が「音あり」⇔ どれかのチャンネルで |x| > t（f64 で比べる。閾値ちょうどは
//!   無音）。NaN は無音（書き出しで 0 になるのと同じ）、±∞ は音あり。チャンネルを混ぜない（逆相で打ち消さない）。
//! - `content`：最初と最後の「音あり」のフレームの [start, end)（end は最後の音ありのフレーム + 1）。全部が無音
//!   （長さ 0 を含む）なら null。`lead` = start、`trail` = frames − end（null なら どちらも frames）。将来の「無音の
//!   トリム」は同じ解析を同じ引数で申告して `content` で切る（E26。段 8 では「`content` で `audio.trim` すると、もう一度
//!   解析した余白が 0」を確かめる）。
//! - クリップ：`clipRuns` は 16 ビットの格子（`codec::pcm::quantize(x, 16)`。波形の段と同じ）の同じ端（32,767 か
//!   −32,768）に 3 標本以上続く列の数（チャンネルごとに数えて足す。Audacity の既定と同じ 3。1・2 標本だけ端に触れるのは
//!   ピークをそろえた音声でふつうに起きるので数えない。正の端と負の端が交互なら続きにしない。NaN は 0 で列を切る）。
//!   `overs` は |x| > 1 の有限の標本の数（整数で書き出すと割れる）、`nonFinite` は NaN・±∞ の標本の数。
//! - 並列はチャンネルごと（チャンネルの中は直列。65,536 標本ごとに中止を確かめる）。
//! - 閾値の dB → 倍率が浮動小数（crate::math）なので `Numeric::Float`。比べるのは f64 の比較だけ。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::audio::FrameSpan;
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate};
use crate::error::EngineError;
use crate::math;
use crate::op::{Numeric, OpParams, ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Shape, ValueKind};
use crate::testgen::{InputGen, MetaGen, NonFiniteSample};
use crate::value::{Audio, Value};

/// 引数。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SilenceParams {
    /// 無音とみなす大きさ（dBFS。どれかのチャンネルの |x| がこの倍率を超えるフレームが「音あり」）。
    pub threshold_db: f64,
}

/// 閾値の既定（dBFS）。
pub const DEFAULT_THRESHOLD_DB: f64 = -60.0;
/// クリップの列とみなす長さ（標本）。
pub const CLIP_RUN: u64 = 3;

impl Default for SilenceParams {
    fn default() -> Self {
        Self {
            threshold_db: DEFAULT_THRESHOLD_DB,
        }
    }
}

impl OpParams for SilenceParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "thresholdDb",
                ParamKind::Float {
                    min: -120.0,
                    max: 0.0,
                    step: 0.1,
                },
                Unit::Db,
                json!(DEFAULT_THRESHOLD_DB),
            )],
        }
    }
}

/// 結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SilenceReport {
    /// 音声の長さ（フレーム）。
    pub frames: u64,
    /// 最初と最後の「音あり」のフレームの [start, end)。全部が無音なら null。
    pub content: Option<FrameSpan>,
    /// 前の無音（フレーム。content の start。null なら frames）。
    pub lead: u64,
    /// 後ろの無音（フレーム。frames − content の end。null なら frames）。
    pub trail: u64,
    /// 16 ビットの格子の同じ端に 3 標本以上続く列の数（チャンネルの合計）。
    pub clip_runs: u64,
    /// |x| > 1 の有限の標本の数（チャンネルの合計）。
    pub overs: u64,
    /// NaN・±∞ の標本の数（チャンネルの合計）。
    pub non_finite: u64,
}
crate::json_output!(SilenceReport);

/// 1 チャンネルの走査の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ChannelScan {
    first: Option<usize>,
    last: Option<usize>,
    clip_runs: u64,
    overs: u64,
    non_finite: u64,
}

/// 中止を確かめる間隔（標本）。
const CHECK_EVERY: usize = 1 << 16;

/// 16 ビットの格子の正の端に写る最小より小さい値：`pcm::quantize(x, 16) == 32_767` ⇔ x > 32,766.5 / 32,768
/// （x·32,768 は f64 で正確で、32,766.5 ちょうどは偶数の 32,766 に丸まる）。65,533 / 65,536 は f32 で正確に表せる。
const TOP: f32 = 65_533.0 / 65_536.0;
/// 負の端に写る最大の値：`pcm::quantize(x, 16) == −32_768` ⇔ x ≤ −32,767.5 / 32,768（−32,767.5 は偶数の −32,768 へ）。
const BOTTOM: f32 = -65_535.0 / 65_536.0;

/// 16 ビットの格子の端：正の端 1、負の端 −1、ほか 0（`pcm::quantize(x, 16)` が 32,767・−32,768 か。NaN は 0）。
/// 標本ごとに丸めずに、同じ境で比べる（テストで `quantize` と照らす）。
fn rail(x: f32) -> i8 {
    if x > TOP {
        1
    } else if x <= BOTTOM {
        -1
    } else {
        0
    }
}

fn scan_channel(x: &[f32], t: f64, cx: &AnalyzeCx<'_>) -> Result<ChannelScan, EngineError> {
    let mut s = ChannelScan::default();
    let (mut run, mut side) = (0u64, 0i8);
    for (i, v) in x.iter().enumerate() {
        if i % CHECK_EVERY == 0 {
            cx.check()?;
        }
        let v = *v;
        if f64::from(v).abs() > t {
            s.first.get_or_insert(i);
            s.last = Some(i);
        }
        if !v.is_finite() {
            s.non_finite += 1;
        } else if v.abs() > 1.0 {
            s.overs += 1;
        }
        let r = rail(v);
        if r != 0 && r == side {
            run += 1;
        } else {
            if run >= CLIP_RUN {
                s.clip_runs += 1;
            }
            run = u64::from(r != 0);
            side = r;
        }
    }
    if run >= CLIP_RUN {
        s.clip_runs += 1;
    }
    Ok(s)
}

/// 前後の無音とクリップを数える。
pub fn scan(
    a: &Audio,
    p: &SilenceParams,
    cx: &AnalyzeCx<'_>,
) -> Result<SilenceReport, EngineError> {
    let t = math::db_to_gain(p.threshold_db);
    let planes = a.planes();
    let scans = cx.par_blocks(planes.len(), |ch| match planes.get(ch) {
        Some(x) => scan_channel(x, t, cx),
        None => Err(EngineError::Internal(
            "無音の仕事の番号が範囲の外です".into(),
        )),
    })?;
    let frames = a.frames() as u64;
    let first = scans.iter().filter_map(|s| s.first).min();
    let last = scans.iter().filter_map(|s| s.last).max();
    let content = match (first, last) {
        (Some(f), Some(l)) => Some(FrameSpan {
            start: f as u64,
            end: l as u64 + 1,
        }),
        _ => None,
    };
    Ok(SilenceReport {
        frames,
        content,
        lead: content.map_or(frames, |c| c.start),
        trail: content.map_or(frames, |c| frames - c.end),
        clip_runs: scans.iter().map(|s| s.clip_runs).sum(),
        overs: scans.iter().map(|s| s.overs).sum(),
        non_finite: scans.iter().map(|s| s.non_finite).sum(),
    })
}

/// 前後の無音とクリップ。
pub struct SilenceAnalysis;

impl Analysis for SilenceAnalysis {
    const ID: &'static str = "audio.silence";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Float;
    type Params = SilenceParams;
    type Output = SilenceReport;

    fn run(p: &SilenceParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<SilenceReport, EngineError> {
        cx.check()?;
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が音声ではありません",
                Self::ID
            )));
        };
        scan(a, p, cx)
    }

    fn examples() -> Vec<AnalysisExample<SilenceParams>> {
        let at = |threshold_db| SilenceParams { threshold_db };
        let levels = InputGen::SineLevels {
            rate: 8_000,
            freq: 1_000,
            segments: vec![
                (4_000, -1_000),
                (2_000, -70),
                (8_000, -20),
                (2_000, -70),
                (4_000, -1_000),
            ],
        };
        vec![
            // 衝撃の繰り返し：最後の衝撃のあとが無音
            AnalysisExample {
                name: "bursts",
                params: SilenceParams::default(),
                input: InputGen::Bursts {
                    rate: 44_100,
                    channels: 2,
                    frames: 30_000,
                    period: 7_000,
                    seed: 71,
                },
            },
            // −70 dBFS の区間は既定の閾値では無音、−80 dBFS の閾値では音あり
            AnalysisExample {
                name: "quiet-edges-default",
                params: SilenceParams::default(),
                input: levels.clone(),
            },
            AnalysisExample {
                name: "quiet-edges-80",
                params: at(-80.0),
                input: levels,
            },
            // 1 を超える大きな音：端に張り付く列と overs
            AnalysisExample {
                name: "swell-clipping",
                params: at(-30.0),
                input: InputGen::Swell {
                    rate: 22_050,
                    channels: 2,
                    frames: 30_000,
                    freqs: vec![200, 700],
                    from_db: -40,
                    to_db: 9,
                },
            },
            // 全部が無音
            AnalysisExample {
                name: "silent",
                params: SilenceParams::default(),
                input: InputGen::SineLevels {
                    rate: 8_000,
                    freq: 440,
                    segments: vec![(6_000, -1_000)],
                },
            },
            // 長さ 0
            AnalysisExample {
                name: "empty",
                params: at(-120.0),
                input: InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 1,
                    frames: 0,
                    seed: 72,
                },
            },
            // 閾値 0 dBFS：|x| > 1 の標本だけが音あり（乱数の音声は [−1, 1) なので全部が無音）
            AnalysisExample {
                name: "zero-db-threshold",
                params: at(0.0),
                input: InputGen::WithMeta {
                    base: Box::new(InputGen::AudioNoise {
                        rate: 16_000,
                        channels: 3,
                        frames: 10_000,
                        seed: 73,
                    }),
                    meta: MetaGen::default(),
                },
            },
            // NaN は無音（前の無音の中の NaN は余白を縮めない）、±∞ は音あり（後ろの無音の中の +∞ の 3 標本が content を
            // 延ばし、16 ビットの格子の端の列になる）。nonFinite に数え、overs には数えない
            AnalysisExample {
                name: "non-finite-edges",
                params: SilenceParams::default(),
                input: InputGen::NonFinite {
                    base: Box::new(InputGen::SineLevels {
                        rate: 8_000,
                        freq: 1_000,
                        segments: vec![(2_000, -1_000), (4_000, -20), (2_000, -1_000)],
                    }),
                    at: vec![
                        (0, 100, NonFiniteSample::Nan),
                        (0, 3_000, NonFiniteSample::NegInf),
                        (0, 7_000, NonFiniteSample::Inf),
                        (0, 7_001, NonFiniteSample::Inf),
                        (0, 7_002, NonFiniteSample::Inf),
                        (0, 7_500, NonFiniteSample::Nan),
                    ],
                },
            },
            // 無音（0）に NaN を置いたもの：最も低い閾値でも全部が無音で content は null。nonFinite は NaN の数
            AnalysisExample {
                name: "nan-is-silent",
                params: at(-120.0),
                input: InputGen::NonFinite {
                    base: Box::new(InputGen::SineLevels {
                        rate: 8_000,
                        freq: 440,
                        segments: vec![(6_000, -1_000)],
                    }),
                    at: vec![
                        (0, 0, NonFiniteSample::Nan),
                        (0, 2_999, NonFiniteSample::Nan),
                        (0, 5_999, NonFiniteSample::Nan),
                    ],
                },
            },
        ]
    }

    fn estimate(_p: &SilenceParams, input: &Shape) -> Estimate {
        let Shape::Audio {
            channels, frames, ..
        } = *input
        else {
            return Estimate::default();
        };
        Estimate {
            // 7 つの u64 の欄（20 桁まで）とキー
            bytes: 512,
            cost: frames.max().saturating_mul(u64::from(channels)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::pcm;
    use crate::monitor::NoMonitor;
    use crate::value::AudioMeta;
    use rand_chacha::rand_core::Rng;
    use std::sync::Arc;

    /// 端の比べ方は `pcm::quantize(x, 16)` と同じ（境の前後の値・±1・±∞・NaN・乱数）。
    #[test]
    fn rail_is_the_16_bit_quantizer() {
        let q = |x: f32| match pcm::quantize(x, 16) {
            32_767 => 1i8,
            -32_768 => -1,
            _ => 0,
        };
        let mut xs = vec![
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            2.0,
            -2.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::MAX,
            f32::MIN,
            TOP,
            BOTTOM,
        ];
        for v in [TOP, BOTTOM, 32_767.0 / 32_768.0, -32_767.0 / 32_768.0, 0.5] {
            let b = v.to_bits();
            for d in 1..40u32 {
                xs.push(f32::from_bits(b + d));
                xs.push(f32::from_bits(b - d));
            }
        }
        let mut r = crate::determinism::rng(3);
        for _ in 0..100_000 {
            xs.push(f32::from_bits(r.next_u32()));
            xs.push((r.next_u32() >> 8) as f32 / 8_388_608.0 * 2.0 - 1.0);
        }
        for x in xs {
            assert_eq!(rail(x), q(x), "{x:e}");
        }
        assert_eq!((rail(TOP), rail(BOTTOM)), (0, -1));
    }

    fn run(planes: Vec<Vec<f32>>, threshold_db: f64) -> SilenceReport {
        let a = Audio::new(
            8_000,
            planes.into_iter().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap();
        let limits = crate::Limits::default();
        scan(
            &a,
            &SilenceParams { threshold_db },
            &AnalyzeCx {
                mon: &NoMonitor,
                limits: &limits,
            },
        )
        .unwrap()
    }

    /// 総当たりの参照（定義をそのまま）。
    fn reference(planes: &[Vec<f32>], threshold_db: f64) -> SilenceReport {
        let t = math::db_to_gain(threshold_db);
        let n = planes[0].len();
        let loud = |i: usize| planes.iter().any(|p| f64::from(p[i]).abs() > t);
        let first = (0..n).find(|i| loud(*i));
        let last = (0..n).rev().find(|i| loud(*i));
        let content = first.zip(last).map(|(f, l)| FrameSpan {
            start: f as u64,
            end: l as u64 + 1,
        });
        let mut runs = 0u64;
        for p in planes {
            let mut i = 0;
            while i < n {
                let r = rail(p[i]);
                let mut j = i;
                while j < n && r != 0 && rail(p[j]) == r {
                    j += 1;
                }
                if r != 0 && j - i >= 3 {
                    runs += 1;
                }
                i = j.max(i + 1);
            }
        }
        let all = planes.iter().flatten();
        SilenceReport {
            frames: n as u64,
            content,
            lead: content.map_or(n as u64, |c| c.start),
            trail: content.map_or(n as u64, |c| n as u64 - c.end),
            clip_runs: runs,
            overs: all
                .clone()
                .filter(|v| v.is_finite() && v.abs() > 1.0)
                .count() as u64,
            non_finite: all.filter(|v| !v.is_finite()).count() as u64,
        }
    }

    /// 閾値ちょうどは無音、1 ULP 上は音あり。NaN は無音、∞ は音あり。どれかのチャンネルで決まる。
    #[test]
    fn content_bounds_follow_the_definition() {
        let t = math::db_to_gain(-60.0);
        let at = t as f32; // f32 に丸めた閾値（f64 で比べるので、丸めの上下で判定が分かれる）
        let above = if f64::from(at) > t {
            at
        } else {
            f32::from_bits(at.to_bits() + 1)
        };
        let below = if f64::from(at) > t {
            f32::from_bits(at.to_bits() - 1)
        } else {
            at
        };
        assert!(f64::from(above) > t && f64::from(below) <= t);
        let r = run(
            vec![vec![0.0, below, above, 0.0, -above, below, 0.0]],
            -60.0,
        );
        assert_eq!(r.content, Some(FrameSpan { start: 2, end: 5 }));
        assert_eq!((r.lead, r.trail), (2, 2));
        let r = run(vec![vec![f32::NAN, 0.0, f32::INFINITY, f32::NAN]], -60.0);
        assert_eq!(r.content, Some(FrameSpan { start: 2, end: 3 }));
        assert_eq!(r.non_finite, 3);
        // どれかのチャンネル：左の始まりと右の終わり
        let r = run(
            vec![vec![0.0, 0.5, 0.0, 0.0], vec![0.0, 0.0, 0.0, -0.5]],
            -60.0,
        );
        assert_eq!(r.content, Some(FrameSpan { start: 1, end: 4 }));
        // 全部が無音・長さ 0
        let r = run(vec![vec![0.0; 10], vec![below; 10]], -60.0);
        assert_eq!((r.content, r.lead, r.trail), (None, 10, 10));
        let r = run(vec![vec![]], -60.0);
        assert_eq!((r.content, r.lead, r.trail, r.frames), (None, 0, 0, 0));
    }

    /// クリップ：3 標本の列は 1 つ、2 標本は数えない、4 標本も 1 つ、正と負の交互は数えない、NaN は列を切る。
    /// 端は 16 ビットの格子（16・24 ビットの元の最大値・−1.0・1 を超える値・+∞）。
    #[test]
    fn clip_runs_and_overs() {
        let top16 = 32_767.0 / 32_768.0;
        let top24 = 8_388_607.0 / 8_388_608.0;
        let cases: Vec<(Vec<f32>, u64, u64)> = vec![
            (vec![1.0, 1.0, 1.0], 1, 0),
            (vec![1.0, 1.0, 0.0, 1.0, 1.0], 0, 0),
            (vec![-1.0, -1.0, -1.0, -1.0], 1, 0),
            (vec![1.0, -1.0, 1.0, -1.0, 1.0, -1.0], 0, 0),
            (vec![top16, top24, 1.5, 0.0, f32::INFINITY, 7.0, 1.0], 2, 2),
            (vec![1.0, 1.0, f32::NAN, 1.0, 1.0], 0, 0),
            (vec![0.999_95, 0.999_95, 0.999_95], 0, 0),
            (vec![-1.0, -1.0, -1.0, 0.5, 1.0, 1.0, 1.0], 2, 0),
        ];
        for (x, runs, overs) in cases {
            let r = run(vec![x.clone()], -60.0);
            assert_eq!((r.clip_runs, r.overs), (runs, overs), "{x:?}");
        }
        // チャンネルごとに数えて足す（列はチャンネルをまたがない）
        let r = run(vec![vec![1.0, 1.0, 1.0], vec![-2.0, -2.0, -2.0]], -60.0);
        assert_eq!((r.clip_runs, r.overs), (2, 3));
    }

    /// 例の値を直接確かめる（ゴールデンに凍結する前に）：NaN は無音、±∞ は音あり・端の列、nonFinite に数えて overs に
    /// 数えない。総当たりの参照とも同じ。
    #[test]
    fn non_finite_examples() {
        for (name, want) in [
            (
                "non-finite-edges",
                SilenceReport {
                    frames: 8_000,
                    content: Some(FrameSpan {
                        start: 2_001,
                        end: 7_003,
                    }),
                    lead: 2_001,
                    trail: 997,
                    clip_runs: 1,
                    overs: 0,
                    non_finite: 6,
                },
            ),
            (
                "nan-is-silent",
                SilenceReport {
                    frames: 6_000,
                    content: None,
                    lead: 6_000,
                    trail: 6_000,
                    clip_runs: 0,
                    overs: 0,
                    non_finite: 3,
                },
            ),
        ] {
            let ex = SilenceAnalysis::examples()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap();
            let Value::Audio(a) = ex.input.generate().unwrap() else {
                panic!()
            };
            let planes: Vec<Vec<f32>> = a.planes().iter().map(|p| p.to_vec()).collect();
            let got = run(planes.clone(), ex.params.threshold_db);
            assert_eq!(got, want, "{name}");
            assert_eq!(got, reference(&planes, ex.params.threshold_db), "{name}");
        }
    }

    /// 総当たりの参照と同じ（ChaCha8 の音声。無音・端・NaN・∞ を混ぜる。閾値を変えて）。
    #[test]
    fn matches_the_brute_force_reference() {
        let mut r = crate::determinism::rng(5);
        for case in 0..60 {
            let channels = 1 + case % 3;
            let n = (r.next_u32() % 300) as usize;
            let planes: Vec<Vec<f32>> = (0..channels)
                .map(|_| {
                    (0..n)
                        .map(|i| {
                            let u = r.next_u32();
                            // 前後は小さく、ときどき端・NaN・∞
                            let edge = i < n / 4 || i > 3 * n / 4;
                            match u % 23 {
                                0 => 1.0,
                                1 => -1.0,
                                2 => f32::NAN,
                                3 => f32::INFINITY,
                                4 => 1.5,
                                _ if edge => ((u >> 8) as f32 / 8_388_608.0 - 1.0) * 0.000_5,
                                _ => (u >> 8) as f32 / 8_388_608.0 - 1.0,
                            }
                        })
                        .collect()
                })
                .collect();
            if n == 0 {
                continue;
            }
            for db in [-120.0, -66.0, -60.0, -20.0, 0.0] {
                assert_eq!(
                    run(planes.clone(), db),
                    reference(&planes, db),
                    "{case} {db}"
                );
            }
        }
    }
}
