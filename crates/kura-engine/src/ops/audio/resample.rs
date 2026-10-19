//! `audio.resample` v1：サンプルレートを変える（`audio::resample` の多相 FIR。式はそちらで凍結）。
//!
//! 同じレートなら何もしない。出力の長さは round_half_even(frames · to / from)、ループ点・マーカーは `TimeMap::Rate`
//! で写す。端は 0 とみなす（両端の約 h 標本はその影響を受ける）。係数の表（L · K 個の f64）が
//! `min(max_value_bytes, 256 MiB)` を超える組み合わせ（例：767999 → 768000）と、出力が 0 フレームになる入力は断る。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::resample::{Filter, design, out_frames, ratio, table_len, taps};
use crate::audio::{AudioKernel, render_all};
use crate::error::{EngineError, IssueCode, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{audio_of, audio_shape, bad_param, param_issue};
use crate::render::region::Reach;
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Audio, LoopRange, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResampleParams {
    /// 変換したあとのサンプルレート（Hz）。
    pub rate: u32,
}

impl OpParams for ResampleParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "rate",
                ParamKind::Int {
                    min: 1_000,
                    max: 768_000,
                    step: 1,
                },
                Unit::Hz,
                json!(48_000),
            )],
        }
    }
}

/// サンプルレートの変換。
pub struct Resample;

/// 係数の表の上限（バイト）。
fn table_budget(limits: &crate::limits::Limits) -> u64 {
    limits.max_value_bytes.min(256 << 20)
}

fn check_table(from: u32, to: u32, limits: &crate::limits::Limits) -> Result<(), Issues> {
    if table_len(from, to).saturating_mul(8) > table_budget(limits) {
        return Err(param_issue(
            "rate",
            IssueCode::TooLarge,
            format!("{from} Hz から {to} Hz への変換は、フィルタが大きすぎて扱えません"),
        ));
    }
    Ok(())
}

fn empty() -> Issues {
    bad_param("rate", "短すぎて、変換すると 0 フレームになります")
}

/// 変換の核。
struct ResampleKernel {
    audio: Arc<Audio>,
    filter: Arc<Filter>,
    rate: u32,
    frames: u64,
}

impl AudioKernel for ResampleKernel {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) {
        let plane = &self.audio.planes()[ch];
        for (k, o) in out.iter_mut().enumerate() {
            *o = self.filter.output(plane, start + k as u64);
        }
    }
}

fn kernel(
    p: &ResampleParams,
    inputs: &[&Value],
    cx: &ApplyCx<'_>,
) -> Result<Box<dyn AudioKernel>, EngineError> {
    let audio = audio_of(inputs[0], Resample::ID)?;
    let from = audio.rate();
    if from == p.rate {
        return Ok(Box::new(crate::audio::Identity(audio.clone())));
    }
    check_table(from, p.rate, cx.limits).map_err(EngineError::Invalid)?;
    let frames = out_frames(audio.frames() as u64, from, p.rate)
        .filter(|n| *n > 0)
        .ok_or_else(|| EngineError::Invalid(empty()))?;
    let filter = design(from, p.rate, cx.mon)?;
    Ok(Box::new(ResampleKernel {
        audio: audio.clone(),
        filter: Arc::new(filter),
        rate: p.rate,
        frames,
    }))
}

impl Op for Resample {
    const ID: &'static str = "audio.resample";
    const VERSION: u16 = 1;
    type Params = ResampleParams;

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

    fn infer(p: &ResampleParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((from, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        if from == p.rate {
            return Ok(Inferred {
                shape: inputs[0],
                space: SpaceMap::Identity,
                time: TimeMap::Identity,
                cost: 1,
            });
        }
        check_table(from, p.rate, cx.limits)?;
        let conv = |n: u64| out_frames(n, from, p.rate).ok_or_else(empty);
        let frames = match frames {
            Dim::Exact(n) => {
                let out = conv(n)?;
                if out == 0 {
                    return Err(empty());
                }
                Dim::Exact(out)
            }
            Dim::AtMost(m) => Dim::AtMost(conv(m)?),
        };
        Ok(Inferred {
            shape: Shape::Audio {
                rate: p.rate,
                channels,
                frames,
            },
            space: SpaceMap::Identity,
            time: TimeMap::Rate { from, to: p.rate },
            cost: frames
                .max()
                .saturating_mul(u64::from(channels))
                .saturating_mul(taps(from, p.rate)),
        })
    }

    fn apply(
        p: &ResampleParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let audio = audio_of(inputs[0], Self::ID)?;
        if audio.rate() == p.rate {
            return Ok(Applied::value(Value::Audio(audio.clone())));
        }
        let k = kernel(p, inputs, cx)?;
        Ok(Applied::value(render_all(
            k.as_ref(),
            audio.meta.clone(),
            cx.mon,
        )?))
    }

    fn work_bytes(_p: &Self::Params, _inputs: &[Shape], out: &Shape) -> crate::op::Work {
        crate::audio::render_all_work(out)
    }

    fn audio_kernel(
        p: &ResampleParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(kernel(p, inputs, cx))
    }

    fn examples() -> Vec<Example<ResampleParams>> {
        vec![
            Example {
                name: "up-44100-to-48000-with-loop",
                params: ResampleParams { rate: 48_000 },
                inputs: vec![InputGen::Sine {
                    rate: 44_100,
                    channels: 2,
                    frames: 8_820,
                    freq: 1_000,
                    loop_range: Some(LoopRange {
                        start: 4_410,
                        end: 8_820,
                    }),
                }],
            },
            Example {
                name: "down-48000-to-44100",
                params: ResampleParams { rate: 44_100 },
                inputs: vec![InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 1,
                    frames: 6_000,
                    seed: 71,
                }],
            },
            Example {
                name: "same-rate-is-identity",
                params: ResampleParams { rate: 22_050 },
                inputs: vec![InputGen::AudioNoise {
                    rate: 22_050,
                    channels: 2,
                    frames: 3_000,
                    seed: 72,
                }],
            },
        ]
    }

    // 段 10：範囲（多相 FIR の窓）
    fn reach(p: &ResampleParams, inputs: &[Shape]) -> Reach {
        let Some((from, _, _)) = inputs.first().and_then(audio_shape) else {
            return Reach::Global;
        };
        if from == p.rate {
            return Reach::SameTime;
        }
        let (l, m) = ratio(from, p.rate);
        Reach::Rate {
            l,
            m,
            h: taps(from, p.rate) / 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    #[test]
    fn length_and_loop_follow_the_rate() {
        let input = Resample::examples()[0].inputs[0].generate().unwrap();
        let out = render_value(Resample::ID, 1, json!({ "rate": 48_000 }), input).unwrap();
        let Value::Audio(a) = &out else {
            panic!("音声ではありません")
        };
        assert_eq!((a.rate(), a.frames()), (48_000, 9_600));
        assert_eq!(
            a.meta.loop_range,
            Some(LoopRange {
                start: 4_800,
                end: 9_600
            })
        );
    }

    /// 1 kHz の正弦は変換しても 1 kHz の正弦（端を除いて、理想の値との差が十分小さい）。
    #[test]
    fn sine_stays_a_sine() {
        let rate_in = 44_100u32;
        let n = 8_820usize;
        let plane: Vec<f32> = (0..n)
            .map(|i| {
                (0.5 * crate::math::sin(
                    2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / f64::from(rate_in),
                )) as f32
            })
            .collect();
        let a = Audio::new(rate_in, vec![Arc::new(plane)], Default::default()).unwrap();
        let out = render_value(
            Resample::ID,
            1,
            json!({ "rate": 48_000 }),
            Value::Audio(Arc::new(a)),
        )
        .unwrap();
        let Value::Audio(b) = &out else {
            panic!("音声ではありません")
        };
        let mut worst = 0f64;
        for i in 400..(b.frames() - 400) {
            let want =
                0.5 * crate::math::sin(2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 48_000.0);
            worst = crate::math::max(worst, (f64::from(b.planes()[0][i]) - want).abs());
        }
        assert!(worst < 1e-6, "{worst}");
    }

    #[test]
    fn refuses_huge_tables_and_empty_outputs() {
        let tiny = Value::Audio(Arc::new(
            Audio::new(96_000, vec![Arc::new(vec![0.1f32])], Default::default()).unwrap(),
        ));
        assert!(render_value(Resample::ID, 1, json!({ "rate": 44_100 }), tiny).is_err());
        let a = Value::Audio(Arc::new(
            Audio::new(
                767_999,
                vec![Arc::new(vec![0.1f32; 100])],
                Default::default(),
            )
            .unwrap(),
        ));
        assert!(render_value(Resample::ID, 1, json!({ "rate": 768_000 }), a).is_err());
    }
}
