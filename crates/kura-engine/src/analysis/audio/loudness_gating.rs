//! `audio.loudness_gating` v1（内部用）：積分ラウドネスの計測と、利得をかけたあとの積分ラウドネスを解くための
//! ブロックのパワー（`analysis::loudness::analyze` の `Measured`）。
//!
//! - 正規化（`audio.normalize` v1）が申告して使う（`Measured::gain_to`）。カタログに出さず、kura-core は DB に置かない
//!   （ブロックの列は曲の長さに比例して大きい）。操作の結果を左右するので、表に出す解析と同じく凍結する
//!   （`goldens-analysis/audio.loudness_gating/`）。値を変えるなら版を上げ、正規化の版も上げて、旧版を両方 legacy に残す。
//! - 正規の形は正規の JSON で、5 つの欄だけ：`integrated`・`samplePeak`・`truePeak`（`Real`。計れないは "nan"、
//!   ゲートを通らない・無音は "-inf"）、`blocks`（`Real` の列）、`measurable`。`Measured` に欄を足しても、この形は変えない
//!   （変えるなら版を上げる）。
//! - 0 フレームの音声も受ける（ブロック 0 個。エラーにしない）。
//! - 見積もりは、計れないレート（3,363 Hz 以下。`loudness::measurable_rate`）ではブロック 0 個として数える（実際の
//!   結果と同じ。数えると、低いレートの長い音声で見積もりだけが上限を超え、正規化の計画を断ってしまう）。

use serde::{Deserialize, Serialize};

use crate::analysis::loudness::{Loudness, Measured, analyze, measurable_rate};
use crate::analysis::output::{json_from_canonical, json_to_canonical};
use crate::analysis::{
    Analysis, AnalysisExample, AnalysisOutput, AnalyzeCx, Encoding, Estimate, NoParams, Real,
};
use crate::error::EngineError;
use crate::op::Numeric;
use crate::shape::{Shape, ValueKind};
use crate::testgen::{InputGen, NonFiniteSample};
use crate::value::Value;

/// 正規の形（JSON）。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Wire {
    integrated: Real,
    sample_peak: Real,
    true_peak: Real,
    blocks: Vec<Real>,
    measurable: bool,
}

impl AnalysisOutput for Measured {
    const ENCODING: Encoding = Encoding::Json;

    fn to_canonical(&self) -> Result<Vec<u8>, EngineError> {
        json_to_canonical(&Wire {
            integrated: Real(self.loudness.integrated),
            sample_peak: Real(self.loudness.sample_peak_db),
            true_peak: Real(self.loudness.true_peak_db),
            blocks: self.blocks().iter().map(|b| Real(*b)).collect(),
            measurable: self.measurable(),
        })
    }

    fn from_canonical(bytes: &[u8]) -> Result<Self, EngineError> {
        let w: Wire = json_from_canonical(bytes)?;
        Ok(Measured::from_parts(
            Loudness {
                integrated: w.integrated.0,
                sample_peak_db: w.sample_peak.0,
                true_peak_db: w.true_peak.0,
            },
            w.blocks.into_iter().map(|b| b.0).collect(),
            w.measurable,
        ))
    }
}

/// ゲーティングの計測（内部用）。
pub struct LoudnessGating;

/// `Real` 1 つの正規の JSON の長さの上限（指数表記を使わないので、最小の非正規化数で 300 字を超える）と区切り。
const REAL_BYTES: u64 = 340;

impl Analysis for LoudnessGating {
    const ID: &'static str = "audio.loudness_gating";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Float;
    const INTERNAL: bool = true;
    type Params = NoParams;
    type Output = Measured;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<Measured, EngineError> {
        cx.check()?;
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が音声ではありません",
                Self::ID
            )));
        };
        analyze(a, cx.mon)
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        vec![
            ex(
                "noise",
                InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 30_000,
                    seed: 61,
                },
            ),
            ex(
                "sine",
                InputGen::Sine {
                    rate: 48_000,
                    channels: 2,
                    frames: 48_000,
                    freq: 1_000,
                    loop_range: None,
                },
            ),
            // 絶対ゲートより静かな区間を含む（利得をかけるとゲートを通る。正規化の例と同じ入力）
            ex(
                "quiet-tail",
                InputGen::SineLevels {
                    rate: 8_000,
                    freq: 1_000,
                    segments: vec![(16_000, -66), (64_000, -73)],
                },
            ),
            // 4 区間に満たない：ブロック 0 個で積分ラウドネスは −∞
            ex(
                "too-short",
                InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 1,
                    frames: 4_000,
                    seed: 62,
                },
            ),
            // 3,363 Hz 以下：積分ラウドネスは計れない（NaN）
            ex(
                "low-rate",
                InputGen::Sine {
                    rate: 3_000,
                    channels: 1,
                    frames: 6_000,
                    freq: 440,
                    loop_range: None,
                },
            ),
            // NaN を含む：計れない（積分・ピークは NaN）。正規化は入力のまま `LoudnessUnmeasurable`
            ex(
                "nan-sample",
                InputGen::NonFinite {
                    base: Box::new(InputGen::AudioNoise {
                        rate: 8_000,
                        channels: 2,
                        frames: 20_050,
                        seed: 63,
                    }),
                    at: vec![(1, 20_049, NonFiniteSample::Nan)],
                },
            ),
            // 全部が無音：ブロックのパワーがすべて 0
            ex(
                "digital-silence",
                InputGen::SineLevels {
                    rate: 8_000,
                    freq: 1_000,
                    segments: vec![(32_000, -1_000)],
                },
            ),
        ]
    }

    fn estimate(_p: &NoParams, input: &Shape) -> Estimate {
        let Shape::Audio {
            rate,
            channels,
            frames,
        } = *input
        else {
            return Estimate::default();
        };
        // 計れないレート（3,363 Hz 以下。`loudness::k_filter` と同じ境）ではブロックは 0 個で、結果は 5 つの欄だけ。
        // 数えると、低いレートの長い音声で見積もりだけが上限を超え、計画で断ってしまう（段 6 では警告つきで描けた）
        let blocks = if measurable_rate(rate) {
            let s = (u64::from(rate) + 5) / 10;
            frames.max().checked_div(s).unwrap_or(0).saturating_sub(3)
        } else {
            0
        };
        Estimate {
            bytes: 128u64.saturating_add(blocks.saturating_add(3).saturating_mul(REAL_BYTES)),
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
    use crate::monitor::NoMonitor;

    /// NaN を含む入力と全部が無音の入力の例は、凍結する前に値を直接確かめる：NaN は計れず（積分・ピークは NaN）、
    /// 無音はブロックのパワーがすべて 0 で積分は −∞、ピークは 0。
    #[test]
    fn nan_and_silence_examples_have_the_described_values() {
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let by_name = |name: &str| {
            let ex = LoudnessGating::examples()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap();
            let v = ex.input.generate().unwrap();
            let m = LoudnessGating::run(&NoParams {}, &v, &cx).unwrap();
            serde_json::from_slice::<serde_json::Value>(&m.to_canonical().unwrap()).unwrap()
        };
        let nan = by_name("nan-sample");
        assert_eq!(nan["measurable"], false);
        assert_eq!(nan["integrated"], "nan");
        assert_eq!(nan["samplePeak"], "nan");
        assert_eq!(nan["truePeak"], "nan");
        let silence = by_name("digital-silence");
        assert_eq!(silence["measurable"], true);
        assert_eq!(silence["integrated"], "-inf");
        let blocks = silence["blocks"].as_array().unwrap();
        assert!(!blocks.is_empty());
        assert!(blocks.iter().all(|b| b == &serde_json::json!(0)));
        assert_eq!(silence["samplePeak"], "-inf");
        assert_eq!(silence["truePeak"], "-inf");
    }

    /// 解析は `loudness::analyze` と同じ値（正規化が使う値）を返し、正規の形は 5 つの欄を持つ。
    #[test]
    fn same_as_the_shared_measurement() {
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        for ex in LoudnessGating::examples() {
            let v = ex.input.generate().unwrap();
            let Value::Audio(a) = &v else { panic!() };
            let got = LoudnessGating::run(&NoParams {}, &v, &cx).unwrap();
            // NaN を含むので値ではなく正規の形で比べる
            let bytes = got.to_canonical().unwrap();
            let want = analyze(a, &NoMonitor).unwrap();
            assert_eq!(bytes, want.to_canonical().unwrap(), "{}", ex.name);
            assert_eq!(got.blocks(), want.blocks());
            let back = Measured::from_canonical(&bytes).unwrap();
            assert_eq!(back.to_canonical().unwrap(), bytes);
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let keys: Vec<&str> = json
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                [
                    "blocks",
                    "integrated",
                    "measurable",
                    "samplePeak",
                    "truePeak"
                ]
            );
        }
        let low = LoudnessGating::examples()[4].input.generate().unwrap();
        let m = LoudnessGating::run(&NoParams {}, &low, &cx).unwrap();
        assert!(!m.measurable() && m.loudness.integrated.is_nan());
        let text = String::from_utf8(m.to_canonical().unwrap()).unwrap();
        assert!(text.contains(r#""integrated":"nan""#), "{text}");
        let short = LoudnessGating::examples()[3].input.generate().unwrap();
        let m = LoudnessGating::run(&NoParams {}, &short, &cx).unwrap();
        assert!(m.measurable() && m.blocks().is_empty());
        assert_eq!(m.loudness.integrated, f64::NEG_INFINITY);
    }

    /// 見積もり：計れないレート（K 特性を作れない。`k_filter` と同じ境）ではブロック 0 個として数え、長さによらない。
    /// 計れるレートではブロックの数に比例する（検証で、8 Hz・1,300 万フレームのモノラルの見積もりが 4.42 GB になり、
    /// 正規化の計画が上限で断られた。実際の結果は 5 つの欄だけ）。
    #[test]
    fn estimate_counts_no_blocks_when_the_rate_cannot_be_measured() {
        use crate::analysis::loudness::{k_filter, measurable_rate};
        use crate::shape::Dim;
        let audio = |rate, frames| Shape::Audio {
            rate,
            channels: 1,
            frames: Dim::Exact(frames),
        };
        // 境は k_filter と同じ（整数のレートでは 3,363 Hz 以下が計れない）
        for rate in [1u32, 8, 414, 3_000, 3_363, 3_364, 8_000, 48_000] {
            assert_eq!(measurable_rate(rate), k_filter(rate).is_ok(), "{rate}");
        }
        assert!(!measurable_rate(3_363) && measurable_rate(3_364));
        let fixed = 128 + 3 * REAL_BYTES;
        for (rate, frames) in [
            (8u32, 13_000_000u64),
            (100, 130_000_000),
            (414, 518_400_000),
            (3_363, u64::MAX),
        ] {
            let e = LoudnessGating::estimate(&NoParams {}, &audio(rate, frames));
            assert_eq!(e.bytes, fixed, "{rate} Hz・{frames} フレーム");
            // 手間は計れなくても標本の数に比例する（トゥルーピーク・サンプルピークは計る）
            assert_eq!(e.cost, frames.saturating_mul(64), "{rate}");
        }
        // 計れるレート：区間 ⌊(fs + 5)/10⌋ = 336 標本、ブロック ⌊n / 336⌋ − 3 個
        let e = LoudnessGating::estimate(&NoParams {}, &audio(3_364, 336 * 1_000));
        assert_eq!(e.bytes, 128 + 1_000 * REAL_BYTES);
        // 計れないレートの見積もりも、実際の正規の形以上（長い音声で）
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let v = InputGen::Sine {
            rate: 8,
            channels: 1,
            frames: 100_000,
            freq: 1,
            loop_range: None,
        }
        .generate()
        .unwrap();
        let bytes = LoudnessGating::run(&NoParams {}, &v, &cx)
            .unwrap()
            .to_canonical()
            .unwrap();
        assert!((bytes.len() as u64) <= fixed, "{}", bytes.len());
    }
}
