//! `audio.loudness` v1：積分ラウドネス・LRA・サンプルピーク・トゥルーピーク・最大のモーメンタリ・最大の短期。
//!
//! - 計算は `analysis::loudness::analyze_detailed`（正規化が申告する `audio.loudness_gating` と同じ走査・同じ式）。
//!   表示する積分ラウドネス・トゥルーピークは、正規化が使う値とビット単位で同じ（E26）。
//! - 値ごとに理由を持つ（`Level` = `Reasoned<f64, Unmeasured>`。正規の JSON は `{"kind":"value","v":…}` か
//!   `{"kind":"<理由>"}`）。値はどれも有限。理由の優先は nonFinite → lowRate → tooShort → silent → belowGate：
//!   - `nonFinite`：標本に NaN・∞ がある（6 つの欄すべて）。
//!   - `lowRate`：3,363 Hz 以下（K 特性を作れない）。積分・LRA・最大 2 つ（ピークは値）。
//!   - `tooShort`：積分・最大のモーメンタリは 400 ms（4 区間）、LRA・最大の短期は 3 秒（30 区間）に満たない
//!     （区間は ⌊(fs + 5)/10⌋ 標本）。400 ms より短い音声はピーク（サンプル・トゥルー）だけを出す（ユーザーの決定）。
//!   - `silent`：計るブロックのパワーがすべて 0（ピークは標本がすべて 0）。重みが 0 のチャンネル（LFE）は計らない。
//!   - `belowGate`：ブロックはあるが、絶対ゲート（−70 LUFS）を通るものがない（積分・LRA）。
//! - 積分（LUFS）は BS.1770 のゲート（「>」）、LRA（LU）は EBU Tech 3342 のゲート（「≥」。短期は 10 Hz）、最大の
//!   モーメンタリ（400 ms）・最大の短期（3 秒）はゲートなしの最大（LUFS。−70 より静かでも値）。サンプルピーク（dBFS）・
//!   トゥルーピーク（dBTP）は段 6 の式。
//! - 正規化 v1 の「計れない」（何もせず警告）は、積分が `tooShort`・`silent`・`lowRate`・`nonFinite` のときと一致する
//!   （`belowGate` は利得をかければ届くので正規化する）。
//! - 浮動小数（`Numeric::Float`。crate::math と IEEE の四則だけ）。

use serde::{Deserialize, Serialize};

use crate::analysis::loudness::{
    Detailed, analyze_detailed, loudness_range, max_power, power_to_lufs,
};
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate, NoParams, Reasoned};
use crate::error::EngineError;
use crate::op::Numeric;
use crate::shape::{Shape, ValueKind};
use crate::testgen::{InputGen, NonFiniteSample};
use crate::value::Value;

/// 値がない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unmeasured {
    /// 長さが足りない（積分・最大のモーメンタリは 400 ms、LRA・最大の短期は 3 秒）。
    TooShort,
    /// 計る音がない（ブロックのパワー・標本がすべて 0）。
    Silent,
    /// 絶対ゲート（−70 LUFS）を通るブロックがない。
    BelowGate,
    /// サンプルレートが 3,363 Hz 以下（K 特性を作れない）。
    LowRate,
    /// 標本に NaN・∞ がある。
    NonFinite,
}

/// 値（有限の LUFS・LU・dBFS・dBTP）か、値がない理由。
pub type Level = Reasoned<f64, Unmeasured>;

/// 結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoudnessReport {
    /// 積分ラウドネス（LUFS。BS.1770。正規化が使う値と同じ）。
    pub integrated: Level,
    /// ラウドネスレンジ（LU。EBU Tech 3342。短期は 10 Hz）。
    pub range: Level,
    /// サンプルピーク（dBFS）。
    pub sample_peak: Level,
    /// トゥルーピーク（dBTP。4 倍の補間。正規化の上限が使う値と同じ）。
    pub true_peak: Level,
    /// 最大のモーメンタリ（400 ms のブロックの最大。LUFS。ゲートなし）。
    pub momentary_max: Level,
    /// 最大の短期（3 秒のブロックの最大。LUFS。ゲートなし）。
    pub short_term_max: Level,
}
crate::json_output!(LoudnessReport);

fn value_or(v: f64, why: Unmeasured) -> Level {
    if v.is_finite() {
        Reasoned::Value(v)
    } else {
        Reasoned::Reason(why)
    }
}

/// ブロックのパワーの最大を LUFS にした値（ブロックがなければ `tooShort`、すべて 0 なら `silent`）。
fn max_level(blocks: &[f64]) -> Level {
    match max_power(blocks) {
        None => Reasoned::Reason(Unmeasured::TooShort),
        Some(p) if p > 0.0 => value_or(power_to_lufs(p), Unmeasured::Silent),
        Some(_) => Reasoned::Reason(Unmeasured::Silent),
    }
}

impl LoudnessReport {
    /// 計測から結果を作る（理由の割り当てはモジュールの説明のとおり）。
    pub fn from_detailed(d: &Detailed) -> Result<Self, EngineError> {
        use Unmeasured::*;
        if d.non_finite {
            let r = Reasoned::Reason(NonFinite);
            return Ok(Self {
                integrated: r,
                range: r,
                sample_peak: r,
                true_peak: r,
                momentary_max: r,
                short_term_max: r,
            });
        }
        let m = &d.measured;
        let sample_peak = value_or(m.loudness.sample_peak_db, Silent);
        let true_peak = value_or(m.loudness.true_peak_db, Silent);
        if !m.measurable() {
            let r = Reasoned::Reason(LowRate);
            return Ok(Self {
                integrated: r,
                range: r,
                sample_peak,
                true_peak,
                momentary_max: r,
                short_term_max: r,
            });
        }
        let blocks = m.blocks();
        let integrated = if blocks.is_empty() {
            Reasoned::Reason(TooShort)
        } else if blocks.iter().all(|p| *p <= 0.0) {
            Reasoned::Reason(Silent)
        } else {
            value_or(m.loudness.integrated, BelowGate)
        };
        let range = if d.short_term.is_empty() {
            Reasoned::Reason(TooShort)
        } else if d.short_term.iter().all(|p| *p <= 0.0) {
            Reasoned::Reason(Silent)
        } else {
            match loudness_range(&d.short_term)? {
                Some(lra) => value_or(lra, BelowGate),
                None => Reasoned::Reason(BelowGate),
            }
        };
        Ok(Self {
            integrated,
            range,
            sample_peak,
            true_peak,
            momentary_max: max_level(blocks),
            short_term_max: max_level(&d.short_term),
        })
    }
}

/// ラウドネス。
pub struct LoudnessAnalysis;

impl Analysis for LoudnessAnalysis {
    const ID: &'static str = "audio.loudness";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Float;
    type Params = NoParams;
    type Output = LoudnessReport;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<LoudnessReport, EngineError> {
        cx.check()?;
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が音声ではありません",
                Self::ID
            )));
        };
        LoudnessReport::from_detailed(&analyze_detailed(a, cx.mon)?)
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        vec![
            // EBU Tech 3342 の試験 1 の形（−20 dBFS と −30 dBFS を 6 秒ずつ。8 kHz のモノラル）：LRA は 10 LU
            ex(
                "ebu-3342-1-8k",
                InputGen::SineLevels {
                    rate: 8_000,
                    freq: 1_000,
                    segments: vec![(48_000, -20), (48_000, -30)],
                },
            ),
            ex(
                "noise-stereo",
                InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 150_000,
                    seed: 81,
                },
            ),
            // 衝撃の繰り返し（大きさが大きく変わる。ゲートで落ちるブロックがある）
            ex(
                "bursts",
                InputGen::Bursts {
                    rate: 22_050,
                    channels: 2,
                    frames: 90_000,
                    period: 11_025,
                    seed: 82,
                },
            ),
            // 5.1（LFE は計らない）
            ex(
                "five-one",
                InputGen::AudioNoise {
                    rate: 16_000,
                    channels: 6,
                    frames: 56_000,
                    seed: 83,
                },
            ),
            // 1 秒：積分・最大のモーメンタリは値、LRA・最大の短期は 3 秒に満たない
            ex(
                "one-second",
                InputGen::Sine {
                    rate: 48_000,
                    channels: 2,
                    frames: 48_000,
                    freq: 1_000,
                    loop_range: None,
                },
            ),
            // 400 ms に満たない：ピークだけ
            ex(
                "too-short",
                InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 1,
                    frames: 4_000,
                    seed: 84,
                },
            ),
            // −70 LUFS より静か：積分・LRA はゲートを通らない、最大のモーメンタリ・短期は値
            ex(
                "below-gate",
                InputGen::SineLevels {
                    rate: 8_000,
                    freq: 1_000,
                    segments: vec![(32_000, -80)],
                },
            ),
            // 無音（振幅は f32 で 0 になる）
            ex(
                "silent",
                InputGen::SineLevels {
                    rate: 8_000,
                    freq: 1_000,
                    segments: vec![(32_000, -1_000)],
                },
            ),
            // 3,363 Hz 以下：ピークだけ
            ex(
                "low-rate",
                InputGen::Sine {
                    rate: 3_000,
                    channels: 1,
                    frames: 12_000,
                    freq: 440,
                    loop_range: None,
                },
            ),
            // 長さ 0
            ex(
                "empty",
                InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 2,
                    frames: 0,
                    seed: 85,
                },
            ),
            // NaN が最後の端数の区間（400 ms のブロックに入らない標本）にある：どの欄も nonFinite
            ex(
                "nan-in-the-partial-segment",
                InputGen::NonFinite {
                    base: Box::new(InputGen::AudioNoise {
                        rate: 8_000,
                        channels: 2,
                        frames: 20_050,
                        seed: 86,
                    }),
                    at: vec![(1, 20_049, NonFiniteSample::Nan)],
                },
            ),
            // +∞ と 3,363 Hz 以下：nonFinite が lowRate より先
            ex(
                "inf-at-a-low-rate",
                InputGen::NonFinite {
                    base: Box::new(InputGen::Sine {
                        rate: 3_000,
                        channels: 1,
                        frames: 12_000,
                        freq: 440,
                        loop_range: None,
                    }),
                    at: vec![(0, 5, NonFiniteSample::Inf)],
                },
            ),
            // −∞ と 400 ms に満たない長さ：nonFinite が tooShort より先（ピークも nonFinite）
            ex(
                "neg-inf-too-short",
                InputGen::NonFinite {
                    base: Box::new(InputGen::AudioNoise {
                        rate: 44_100,
                        channels: 1,
                        frames: 4_000,
                        seed: 87,
                    }),
                    at: vec![(0, 0, NonFiniteSample::NegInf)],
                },
            ),
        ]
    }

    fn estimate(_p: &NoParams, input: &Shape) -> Estimate {
        let Shape::Audio {
            channels, frames, ..
        } = *input
        else {
            return Estimate::default();
        };
        Estimate {
            // 6 つの欄（`{"kind":"value","v":<最短の小数>}` は 1 つ 50 バイト未満）とキー
            bytes: 1_024,
            // K 特性とトゥルーピーク（48 回の積和）
            cost: frames
                .max()
                .saturating_mul(u64::from(channels))
                .saturating_mul(64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::AnalysisOutput;
    use crate::analysis::loudness::analyze;
    use crate::monitor::NoMonitor;
    use crate::value::{Audio, AudioMeta};
    use std::sync::Arc;

    fn run(v: &Value) -> LoudnessReport {
        let limits = crate::Limits::default();
        LoudnessAnalysis::run(
            &NoParams {},
            v,
            &AnalyzeCx {
                mon: &NoMonitor,
                limits: &limits,
            },
        )
        .unwrap()
    }

    fn reason(l: &Level) -> Option<Unmeasured> {
        l.reason().copied()
    }

    /// 例ごとの理由：値の欄・理由の欄が説明どおり。値はどれも有限で、正規の形が往復する。
    #[test]
    fn examples_have_the_documented_reasons() {
        use Unmeasured::*;
        let want: [(&str, [Option<Unmeasured>; 6]); 13] = [
            ("ebu-3342-1-8k", [None; 6]),
            ("noise-stereo", [None; 6]),
            ("bursts", [None; 6]),
            ("five-one", [None; 6]),
            (
                "one-second",
                [None, Some(TooShort), None, None, None, Some(TooShort)],
            ),
            (
                "too-short",
                [
                    Some(TooShort),
                    Some(TooShort),
                    None,
                    None,
                    Some(TooShort),
                    Some(TooShort),
                ],
            ),
            (
                "below-gate",
                [Some(BelowGate), Some(BelowGate), None, None, None, None],
            ),
            ("silent", [Some(Silent); 6]),
            (
                "low-rate",
                [
                    Some(LowRate),
                    Some(LowRate),
                    None,
                    None,
                    Some(LowRate),
                    Some(LowRate),
                ],
            ),
            (
                "empty",
                [
                    Some(TooShort),
                    Some(TooShort),
                    Some(Silent),
                    Some(Silent),
                    Some(TooShort),
                    Some(TooShort),
                ],
            ),
            ("nan-in-the-partial-segment", [Some(NonFinite); 6]),
            ("inf-at-a-low-rate", [Some(NonFinite); 6]),
            ("neg-inf-too-short", [Some(NonFinite); 6]),
        ];
        let examples = LoudnessAnalysis::examples();
        assert_eq!(examples.len(), want.len());
        for (ex, (name, reasons)) in examples.iter().zip(want) {
            assert_eq!(ex.name, name);
            let v = ex.input.generate().unwrap();
            let r = run(&v);
            let got = [
                reason(&r.integrated),
                reason(&r.range),
                reason(&r.sample_peak),
                reason(&r.true_peak),
                reason(&r.momentary_max),
                reason(&r.short_term_max),
            ];
            assert_eq!(got, reasons, "{name}: {r:?}");
            for l in [
                &r.integrated,
                &r.range,
                &r.sample_peak,
                &r.true_peak,
                &r.momentary_max,
                &r.short_term_max,
            ] {
                if let Some(v) = l.value() {
                    assert!(v.is_finite(), "{name}");
                }
            }
            let bytes = r.to_canonical().unwrap();
            assert_eq!(LoudnessReport::from_canonical(&bytes).unwrap(), r);
        }
        // 試験 1 の形：LRA は 10 LU、最大の短期は −20 dBFS の正弦（モノラル）のラウドネスに近い
        let r = run(&examples[0].input.generate().unwrap());
        let lra = *r.range.value().unwrap();
        assert!((lra - 10.0).abs() < 0.01, "{lra}");
        let below = run(&examples[6].input.generate().unwrap());
        assert!(*below.momentary_max.value().unwrap() < -70.0);
    }

    /// 積分ラウドネス・ピークは `loudness::analyze`（正規化が使う値）とビット単位で同じ。最大のモーメンタリは
    /// ブロックの最大。
    #[test]
    fn integrated_and_peaks_are_the_shared_measurement() {
        for ex in LoudnessAnalysis::examples() {
            let v = ex.input.generate().unwrap();
            let Value::Audio(a) = &v else { panic!() };
            let m = analyze(a, &NoMonitor).unwrap();
            let r = run(&v);
            for (got, want) in [
                (&r.integrated, m.loudness.integrated),
                (&r.sample_peak, m.loudness.sample_peak_db),
                (&r.true_peak, m.loudness.true_peak_db),
            ] {
                match got.value() {
                    Some(g) => assert_eq!(g.to_bits(), want.to_bits(), "{}", ex.name),
                    None => assert!(!want.is_finite(), "{}", ex.name),
                }
            }
            if let Some(mm) = r.momentary_max.value() {
                let max = m.blocks().iter().copied().fold(0.0, crate::math::max);
                assert_eq!(mm.to_bits(), power_to_lufs(max).to_bits());
            }
        }
    }

    /// NaN・∞ はどの欄も nonFinite。正規の JSON の形（キーの並び・理由の名前）。
    #[test]
    fn non_finite_samples_and_the_canonical_shape() {
        let mut p = vec![0.25f32; 20_000];
        p[7] = f32::INFINITY;
        let v = Value::Audio(Arc::new(
            Audio::new(8_000, vec![Arc::new(p)], AudioMeta::default()).unwrap(),
        ));
        let r = run(&v);
        let text = String::from_utf8(r.to_canonical().unwrap()).unwrap();
        assert_eq!(
            text,
            r#"{"integrated":{"kind":"nonFinite"},"momentaryMax":{"kind":"nonFinite"},"range":{"kind":"nonFinite"},"samplePeak":{"kind":"nonFinite"},"shortTermMax":{"kind":"nonFinite"},"truePeak":{"kind":"nonFinite"}}"#
        );
        for bad in [
            r#"{"integrated":{"kind":"value","v":1}}"#,
            r#"{"integrated":{"kind":"loud"},"momentaryMax":{"kind":"nonFinite"},"range":{"kind":"nonFinite"},"samplePeak":{"kind":"nonFinite"},"shortTermMax":{"kind":"nonFinite"},"truePeak":{"kind":"nonFinite"}}"#,
        ] {
            assert!(LoudnessReport::from_canonical(bad.as_bytes()).is_err());
        }
    }
}
