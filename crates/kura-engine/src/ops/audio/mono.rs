//! `audio.mono` v1：チャンネルの平均で 1 チャンネルにする。
//!
//! 標本は、チャンネルの順に f64 で足してチャンネルの数で割り、f32 に丸める（`(Σ x_c) / C as f32`）。
//! 1 チャンネルの入力はそのまま返す。ループ点・マーカー・タグはそのまま運ぶ。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::audio::{AudioKernel, copy_from, render_all};
use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamSchema, PortSpec, Signature, expect_inputs,
};
use crate::ops::util::{audio_of, audio_shape};
use crate::render::region::Reach;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Audio, Value};

/// 引数（なし）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonoParams {}

impl OpParams for MonoParams {
    fn schema() -> ParamSchema {
        ParamSchema { params: vec![] }
    }
}

/// モノラル化。
pub struct Mono;

/// 平均の核。
struct MonoKernel {
    audio: Arc<Audio>,
}

impl AudioKernel for MonoKernel {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        1
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn render(&self, _ch: usize, start: u64, out: &mut [f32]) {
        let planes = self.audio.planes();
        if planes.len() == 1 {
            copy_from(&planes[0], start, out);
            return;
        }
        let n = planes.len() as f64;
        let mut buf = vec![0f32; out.len()];
        let mut sum = vec![0f64; out.len()];
        for plane in planes {
            copy_from(plane, start, &mut buf);
            for (acc, v) in sum.iter_mut().zip(&buf) {
                *acc += f64::from(*v);
            }
        }
        for (o, s) in out.iter_mut().zip(sum) {
            *o = (s / n) as f32;
        }
    }
}

impl Op for Mono {
    const ID: &'static str = "audio.mono";
    const VERSION: u16 = 1;
    type Params = MonoParams;

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

    fn infer(_p: &MonoParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((rate, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        Ok(Inferred {
            shape: Shape::Audio {
                rate,
                channels: 1,
                frames,
            },
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: frames.max().saturating_mul(u64::from(channels)),
        })
    }

    fn apply(
        _p: &MonoParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let audio = audio_of(inputs[0], Self::ID)?;
        if audio.channels() == 1 {
            return Ok(Applied::value(Value::Audio(audio.clone())));
        }
        let k = MonoKernel {
            audio: audio.clone(),
        };
        Ok(Applied::value(render_all(&k, audio.meta.clone(), cx.mon)?))
    }

    fn work_bytes(_p: &Self::Params, _inputs: &[Shape], out: &Shape) -> crate::op::Work {
        crate::audio::render_all_work(out)
    }

    fn audio_kernel(
        _p: &MonoParams,
        inputs: &[&Value],
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(
            audio_of(inputs[0], Self::ID)
                .map(|a| Box::new(MonoKernel { audio: a.clone() }) as Box<dyn AudioKernel>),
        )
    }

    fn examples() -> Vec<Example<MonoParams>> {
        vec![
            Example {
                name: "stereo-average",
                params: MonoParams {},
                inputs: vec![InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 9_000,
                    seed: 51,
                }],
            },
            Example {
                name: "six-channels",
                params: MonoParams {},
                inputs: vec![InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 6,
                    frames: 5_000,
                    seed: 52,
                }],
            },
        ]
    }

    // 段 10：範囲（同じ位置。全チャンネル）
    fn reach(_p: &MonoParams, _inputs: &[Shape]) -> Reach {
        Reach::SameTime
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;
    use serde_json::json;

    #[test]
    fn averages_channels() {
        let input = Mono::examples()[0].inputs[0].generate().unwrap();
        let out = render_value(Mono::ID, 1, json!({}), input.clone()).unwrap();
        let (Value::Audio(a), Value::Audio(src)) = (&out, &input) else {
            panic!("音声ではありません")
        };
        assert_eq!(a.channels(), 1);
        let want = ((f64::from(src.planes()[0][77]) + f64::from(src.planes()[1][77])) / 2.0) as f32;
        assert_eq!(a.planes()[0][77], want);
        // 1 チャンネルならそのまま
        assert_eq!(
            render_value(Mono::ID, 1, json!({}), out.clone()).unwrap(),
            out
        );
    }
}
