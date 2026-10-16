//! `audio.normalize` v1：積分ラウドネス（BS.1770。`analysis::loudness::measure`）を目標に合わせる。
//!
//! 利得（dB）は、利得をかけたあとの積分ラウドネスがちょうど目標になる値（`Measured::gain_to`）。利得をかけると
//! 絶対ゲート（−70 LKFS）をまたぐブロックが入れ替わるので、1 回の計測の差（目標 − 積分ラウドネス）では届かないことが
//! ある。計測したブロックのパワーを大きい順に並べ、ゲートを通る組ごとに積分ラウドネスが利得の直線になることを使って、
//! 組の範囲に入る解を厳密に求める（|利得| が最小のもの）。−70 LKFS より静かな入力も届く。
//! 標本は `audio.gain` と同じ式（`GainKernel`）なので、同じ dB を指定したゲインとビット単位で同じになる。
//! `peakLimit`（dBTP）があり、利得をかけたトゥルーピークが上限を超えるなら、上限にちょうど届くまで利得を下げて
//! `GainLimited` の警告を出す（目標より小さく仕上がる）。計れない（ブロックのパワーがすべて 0（無音）・4 区間
//! （⌊(fs + 5)/10⌋ 標本の 4 倍。約 400 ms）に満たない・3,363 Hz 以下・NaN や ∞ の標本）ときは何も変えずに
//! `LoudnessUnmeasurable` の警告を出す。目標は −69〜0 LUFS（ゲートは厳密な「>」なので −70 ちょうどには届かない）。
//! 計測は、段 8 の解析（`audio.loudness`）と同じ関数なので、表示するラウドネスと正規化で使う値が一致する（E26）。
//! 2 パスの計測は、段 6 では apply の中で行う（申告の口は段 8）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::loudness;
use crate::audio::{AudioKernel, Identity, render_all};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::audio::gain::GainKernel;
use crate::ops::util::{audio_of, audio_shape};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::Value;

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeParams {
    /// 目標の積分ラウドネス（LUFS）。
    pub target: f64,
    /// トゥルーピークの上限（dBTP）。null なら抑えない。
    pub peak_limit: Option<f64>,
}

impl OpParams for NormalizeParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "target",
                    ParamKind::Float {
                        min: -69.0,
                        max: 0.0,
                        step: 0.1,
                    },
                    Unit::Lufs,
                    json!(-23.0),
                ),
                spec(
                    "peakLimit",
                    ParamKind::Float {
                        min: -30.0,
                        max: 0.0,
                        step: 0.1,
                    },
                    Unit::Db,
                    json!(null),
                ),
            ],
        }
    }
}

/// 正規化。
pub struct Normalize;

/// 計測して利得（dB）を決める。計れなければ None（警告を足す）。
fn gain_db(
    p: &NormalizeParams,
    a: &crate::value::Audio,
    cx: &mut ApplyCx<'_>,
) -> Result<Option<f64>, EngineError> {
    let m = loudness::analyze(a, cx.mon)?;
    let Some(mut db) = m.gain_to(p.target) else {
        cx.warn(Issue::warning(
            IssueCode::LoudnessUnmeasurable,
            "無音か、短い（4 区間 ≒ 400 ms に満たない）か、レートが低い（3,363 Hz 以下）か、NaN や ∞ を含むので、ラウドネスを計れず正規化しませんでした",
        ));
        return Ok(None);
    };
    let peak = m.loudness.true_peak_db;
    if let Some(limit) = p.peak_limit
        && peak + db > limit
    {
        db = limit - peak;
        cx.warn(Issue::warning(
            IssueCode::GainLimited,
            format!(
                "トゥルーピークの上限 {limit} dBTP に当たるので、利得を {db:.2} dB に抑えました"
            ),
        ));
    }
    Ok(Some(db))
}

impl Op for Normalize {
    const ID: &'static str = "audio.normalize";
    const VERSION: u16 = 1;
    type Params = NormalizeParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "audio",
                accepts: &[ValueKind::Audio],
            }],
            lift: Lift::None,
            numeric: Numeric::Float,
            meta: MetaAccess::Map,
        }
    }

    fn infer(
        _p: &NormalizeParams,
        inputs: &[Shape],
        _cx: &InferCx<'_>,
    ) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((_, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            // 計測（K 特性とトゥルーピークの 48 回の積和）と書き込み
            cost: frames
                .max()
                .saturating_mul(u64::from(channels))
                .saturating_mul(64),
        })
    }

    fn apply(
        p: &NormalizeParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let audio = audio_of(inputs[0], Self::ID)?;
        let Some(db) = gain_db(p, audio, cx)? else {
            return Ok(Applied::value(Value::Audio(audio.clone())));
        };
        let k = GainKernel::new(audio.clone(), db);
        Ok(Applied::value(render_all(&k, audio.meta.clone(), cx.mon)?))
    }

    fn audio_kernel(
        p: &NormalizeParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        let run = |cx: &mut ApplyCx<'_>| -> Result<Box<dyn AudioKernel>, EngineError> {
            let audio = audio_of(inputs[0], Self::ID)?;
            // 計れなければ apply と同じく入力のまま
            Ok(match gain_db(p, audio, cx)? {
                Some(db) => Box::new(GainKernel::new(audio.clone(), db)),
                None => Box::new(Identity(audio.clone())),
            })
        };
        Some(run(cx))
    }

    fn examples() -> Vec<Example<NormalizeParams>> {
        vec![
            Example {
                name: "sine-to-minus-16",
                params: NormalizeParams {
                    target: -16.0,
                    peak_limit: None,
                },
                inputs: vec![InputGen::Sine {
                    rate: 48_000,
                    channels: 2,
                    frames: 48_000,
                    freq: 1_000,
                    loop_range: None,
                }],
            },
            Example {
                name: "noise-limited-by-peak",
                params: NormalizeParams {
                    target: -2.0,
                    peak_limit: Some(-3.0),
                },
                inputs: vec![InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 30_000,
                    seed: 61,
                }],
            },
            // 絶対ゲートをまたぐ：静かな区間が利得をかけるとゲートを通り、計算に加わる
            Example {
                name: "quiet-tail-crosses-the-gate",
                params: NormalizeParams {
                    target: -23.0,
                    peak_limit: None,
                },
                inputs: vec![InputGen::SineLevels {
                    rate: 8_000,
                    freq: 1_000,
                    segments: vec![(16_000, -66), (64_000, -73)],
                }],
            },
            Example {
                name: "too-short-is-unchanged",
                params: NormalizeParams {
                    target: -23.0,
                    peak_limit: None,
                },
                inputs: vec![InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 1,
                    frames: 4_000,
                    seed: 62,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::testing::render_value;

    fn measure(v: &Value) -> loudness::Loudness {
        let Value::Audio(a) = v else {
            panic!("音声ではありません")
        };
        loudness::measure(a, &NoMonitor).unwrap()
    }

    #[test]
    fn reaches_the_target_and_respects_the_peak_limit() {
        let ex = Normalize::examples();
        let input = ex[0].inputs[0].generate().unwrap();
        let out = render_value(
            Normalize::ID,
            1,
            json!({ "target": -16.0, "peakLimit": null }),
            input,
        )
        .unwrap();
        assert!((measure(&out).integrated + 16.0).abs() < 0.05);
        // 上限に当たる：トゥルーピークがちょうど上限になり、GainLimited の警告が出る
        let noise = ex[1].inputs[0].generate().unwrap();
        let before = measure(&noise);
        let engine = crate::Engine::new().unwrap();
        let graph = crate::testing::single_node_graph(
            Normalize::ID,
            1,
            json!({ "target": -2.0, "peakLimit": -3.0 }),
        )
        .unwrap();
        let r = crate::testing::render_full(
            &engine,
            &graph,
            noise,
            crate::digest::Digest::default(),
            &NoMonitor,
        )
        .unwrap();
        assert!(
            r.warnings.iter().any(|w| w.code == IssueCode::GainLimited),
            "{:?}",
            r.warnings
        );
        let m = measure(&r.outputs[0].value);
        assert!((m.true_peak_db + 3.0).abs() < 0.01, "{}", m.true_peak_db);
        assert!(m.integrated < -2.0 - 0.5 && before.true_peak_db - 3.0 > -2.0 - before.integrated);
        // 静かな区間が利得で絶対ゲートを越えても、目標に届く（1 回の差では −24.6 LUFS ほどになる形）
        let quiet = ex[2].inputs[0].generate().unwrap();
        let first = measure(&quiet).integrated;
        let out = render_value(
            Normalize::ID,
            1,
            json!({ "target": -23.0, "peakLimit": null }),
            quiet.clone(),
        )
        .unwrap();
        assert!((measure(&out).integrated + 23.0).abs() < 0.01);
        let once = render_value("audio.gain", 1, json!({ "db": -23.0 - first }), quiet).unwrap();
        assert!(
            measure(&once).integrated < -23.5,
            "{}",
            measure(&once).integrated
        );
        // 短いものは変えない
        let short = ex[3].inputs[0].generate().unwrap();
        let out = render_value(
            Normalize::ID,
            1,
            json!({ "target": -23.0, "peakLimit": null }),
            short.clone(),
        )
        .unwrap();
        assert_eq!(out, short);
    }

    /// 計れないもの（低いレート・NaN を含む）は、描くときのエラーにせず、警告を出して入力のまま返す。
    #[test]
    fn unmeasurable_inputs_are_left_alone() {
        use crate::value::{Audio, AudioMeta};
        use std::sync::Arc;
        let low = Value::Audio(Arc::new(
            Audio::new(
                3_000,
                vec![Arc::new(vec![0.25f32; 6_000])],
                AudioMeta::default(),
            )
            .unwrap(),
        ));
        let mut nan = vec![0.25f32; 20_000];
        nan[5] = f32::NAN;
        let nan = Value::Audio(Arc::new(
            Audio::new(8_000, vec![Arc::new(nan)], AudioMeta::default()).unwrap(),
        ));
        let mut inf = vec![0.25f32; 20_000];
        inf[7] = f32::INFINITY;
        let inf = Value::Audio(Arc::new(
            Audio::new(8_000, vec![Arc::new(inf)], AudioMeta::default()).unwrap(),
        ));
        let engine = crate::Engine::new().unwrap();
        let graph = crate::testing::single_node_graph(
            Normalize::ID,
            1,
            json!({ "target": -23.0, "peakLimit": -1.0 }),
        )
        .unwrap();
        for v in [low, nan, inf] {
            let r = crate::testing::render_full(
                &engine,
                &graph,
                v.clone(),
                crate::digest::Digest::default(),
                &NoMonitor,
            )
            .unwrap();
            assert!(
                r.warnings
                    .iter()
                    .any(|w| w.code == IssueCode::LoudnessUnmeasurable)
            );
            assert_eq!(r.outputs[0].value.digest().unwrap(), v.digest().unwrap());
        }
    }

    #[test]
    fn matches_gain_bit_for_bit() {
        let input = Normalize::examples()[0].inputs[0].generate().unwrap();
        let Value::Audio(a) = &input else {
            panic!("音声ではありません")
        };
        let m = loudness::measure(a, &NoMonitor).unwrap();
        let out = render_value(
            Normalize::ID,
            1,
            json!({ "target": -20.0, "peakLimit": null }),
            input.clone(),
        )
        .unwrap();
        let gained = render_value(
            "audio.gain",
            1,
            json!({ "db": -20.0 - m.integrated }),
            input,
        )
        .unwrap();
        assert_eq!(out, gained);
    }
}
