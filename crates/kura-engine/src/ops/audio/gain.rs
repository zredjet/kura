//! `audio.gain` v1：音量をデシベルで変える。0dB は何も変えない（E17）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::determinism::{AUDIO_BLOCK, block_count, par_blocks};
use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::math;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Audio, LoopRange, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GainParams {
    /// 増やす量（dB）。負なら小さくする。
    pub db: f64,
}

impl OpParams for GainParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "db",
                ParamKind::Float {
                    min: -96.0,
                    max: 48.0,
                    step: 0.1,
                },
                Unit::Db,
                json!(0.0),
            )],
        }
    }
}

/// 音量。
pub struct Gain;

impl Op for Gain {
    const ID: &'static str = "audio.gain";
    const VERSION: u16 = 1;
    type Params = GainParams;

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

    fn infer(_p: &GainParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let shape = inputs[0];
        let cost = match shape {
            Shape::Audio {
                channels, frames, ..
            } => frames.max() * u64::from(channels),
            _ => 0,
        };
        Ok(Inferred {
            shape,
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost,
        })
    }

    fn apply(
        p: &GainParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        let Value::Audio(audio) = inputs[0] else {
            return Err(EngineError::Internal(
                "audio.gain の入力が音声ではありません".into(),
            ));
        };
        cx.check()?;
        if p.db == 0.0 {
            // 何も変えない（同じ実体を返す）
            return Ok(Applied::value(Value::Audio(audio.clone())));
        }
        let factor = math::db_to_gain(p.db);
        let mut planes = Vec::with_capacity(audio.channels());
        for plane in audio.planes() {
            let blocks = par_blocks(block_count(plane.len(), AUDIO_BLOCK), cx.mon, |b| {
                let start = b * AUDIO_BLOCK;
                let end = (start + AUDIO_BLOCK).min(plane.len());
                // 1 サンプルごとに独立した計算なので、分け方によらず同じ結果になる
                Ok(plane[start..end]
                    .iter()
                    .map(|s| (f64::from(*s) * factor) as f32)
                    .collect::<Vec<f32>>())
            })?;
            planes.push(Arc::new(blocks.concat()));
        }
        Ok(Applied::value(Value::Audio(Arc::new(Audio::new(
            audio.rate(),
            planes,
            audio.meta.clone(),
        )?))))
    }

    fn examples() -> Vec<Example<GainParams>> {
        vec![
            Example {
                name: "minus-6db-stereo",
                params: GainParams { db: -6.0 },
                inputs: vec![InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 2,
                    frames: 20_000,
                    seed: 11,
                }],
            },
            Example {
                name: "zero-is-identity",
                params: GainParams { db: 0.0 },
                inputs: vec![InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 1,
                    frames: 5_000,
                    seed: 12,
                }],
            },
            Example {
                name: "keeps-loop",
                params: GainParams { db: 3.5 },
                inputs: vec![InputGen::Sine {
                    rate: 22_050,
                    channels: 2,
                    frames: 9_000,
                    freq: 440,
                    loop_range: Some(LoopRange {
                        start: 100,
                        end: 8_000,
                    }),
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    #[test]
    fn zero_db_changes_nothing() {
        let input = InputGen::AudioNoise {
            rate: 48_000,
            channels: 2,
            frames: 10_000,
            seed: 1,
        }
        .generate()
        .unwrap();
        let out = render_value(Gain::ID, 1, json!({ "db": 0.0 }), input.clone()).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn minus_6db_roughly_halves() {
        let input = InputGen::AudioNoise {
            rate: 48_000,
            channels: 1,
            frames: 1_000,
            seed: 2,
        }
        .generate()
        .unwrap();
        let Value::Audio(out) =
            render_value(Gain::ID, 1, json!({ "db": -6.0 }), input.clone()).unwrap()
        else {
            panic!()
        };
        let Value::Audio(src) = input else { panic!() };
        for (a, b) in src.planes()[0].iter().zip(out.planes()[0].iter()) {
            assert!((b - a * 0.501_187).abs() < 1e-6);
        }
    }

    #[test]
    fn out_of_range_is_rejected() {
        let input = InputGen::AudioNoise {
            rate: 48_000,
            channels: 1,
            frames: 10,
            seed: 2,
        }
        .generate()
        .unwrap();
        assert!(render_value(Gain::ID, 1, json!({ "db": 100.0 }), input.clone()).is_err());
        assert!(render_value(Gain::ID, 1, json!({ "db": "loud" }), input).is_err());
    }
}
