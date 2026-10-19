//! `audio.gain` v1：音量をデシベルで変える。0dB は何も変えない（E17）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::{AudioKernel, copy_from, render_all};
use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::math;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::audio_of;
use crate::render::region::Reach;
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

/// 音量の核：標本ごとに `(f64 × 倍率) as f32`（1 標本ごとに独立した計算）。`audio.normalize` も使う。
pub struct GainKernel {
    audio: Arc<Audio>,
    factor: f64,
}

impl GainKernel {
    /// `db` だけ音量を変える核（倍率は `math::db_to_gain`）。
    pub fn new(audio: Arc<Audio>, db: f64) -> Self {
        Self {
            audio,
            factor: math::db_to_gain(db),
        }
    }
}

impl AudioKernel for GainKernel {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) {
        copy_from(&self.audio.planes()[ch], start, out);
        for s in out.iter_mut() {
            *s = (f64::from(*s) * self.factor) as f32;
        }
    }
}

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
        let audio = audio_of(inputs[0], Self::ID)?;
        cx.check()?;
        if p.db == 0.0 {
            // 何も変えない（同じ実体を返す）
            return Ok(Applied::value(Value::Audio(audio.clone())));
        }
        let k = GainKernel::new(audio.clone(), p.db);
        Ok(Applied::value(render_all(&k, audio.meta.clone(), cx.mon)?))
    }

    fn work_bytes(_p: &Self::Params, _inputs: &[Shape], out: &Shape) -> crate::op::Work {
        crate::audio::render_all_work(out)
    }

    fn audio_kernel(
        p: &GainParams,
        inputs: &[&Value],
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(
            audio_of(inputs[0], Self::ID)
                .map(|a| Box::new(GainKernel::new(a.clone(), p.db)) as Box<dyn AudioKernel>),
        )
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

    // 段 10：範囲（同じ位置。全チャンネル）
    fn reach(_p: &GainParams, _inputs: &[Shape]) -> Reach {
        Reach::SameTime
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
