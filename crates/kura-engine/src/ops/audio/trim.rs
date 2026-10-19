//! `audio.trim` v1：区間 [start, end) を切り出す（フレーム。end が null なら終わりまで）。
//!
//! 標本はそのまま写す（整数だけ）。ループ点・マーカーは区間で写す（`TimeMap::Segments`。はみ出たものは詰めるか落として
//! 警告。§6）。トリムを 2 回するのは、合成した 1 回のトリムと同じ（E16）。区間が空・長さを超えるものは断る。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::{AudioKernel, copy_from, render_all};
use crate::error::{EngineError, Issues};
use crate::mapping::{Seg, SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{MAX_FRAME_PARAM, audio_of, audio_shape, bad_param};
use crate::render::region::Reach;
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::{InputGen, MetaGen};
use crate::value::{Audio, LoopRange, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrimParams {
    /// 始まり（フレーム。含む）。
    pub start: u64,
    /// 終わり（フレーム。含まない）。null なら終わりまで。
    pub end: Option<u64>,
}

impl OpParams for TrimParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "start",
                    ParamKind::Int {
                        min: 0,
                        max: MAX_FRAME_PARAM,
                        step: 1,
                    },
                    Unit::Samples,
                    json!(0),
                ),
                spec(
                    "end",
                    ParamKind::Int {
                        min: 1,
                        max: MAX_FRAME_PARAM,
                        step: 1,
                    },
                    Unit::Samples,
                    json!(null),
                ),
            ],
        }
    }
}

/// 区間の切り出し。
pub struct Trim;

/// 長さ `n` の音声での区間 [start, end)。空・長さを超えるなら None。
fn range(p: &TrimParams, n: u64) -> Option<(u64, u64)> {
    let end = p.end.unwrap_or(n);
    (p.start < end && end <= n).then_some((p.start, end))
}

fn bad_range() -> Issues {
    bad_param("end", "区間が空か、音声の長さを超えています")
}

/// 切り出しの核。
struct TrimKernel {
    audio: Arc<Audio>,
    start: u64,
    frames: u64,
}

impl AudioKernel for TrimKernel {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn render(&self, ch: usize, start: u64, out: &mut [f32]) {
        copy_from(
            &self.audio.planes()[ch],
            self.start.saturating_add(start),
            out,
        );
    }
}

fn kernel(p: &TrimParams, inputs: &[&Value]) -> Result<TrimKernel, EngineError> {
    let audio = audio_of(inputs[0], Trim::ID)?;
    let (start, end) =
        range(p, audio.frames() as u64).ok_or_else(|| EngineError::Invalid(bad_range()))?;
    Ok(TrimKernel {
        audio: audio.clone(),
        start,
        frames: end - start,
    })
}

impl Op for Trim {
    const ID: &'static str = "audio.trim";
    const VERSION: u16 = 1;
    type Params = TrimParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "audio",
                accepts: &[ValueKind::Audio],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &TrimParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((rate, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        let (frames, src_end) = match frames {
            Dim::Exact(n) => {
                let (start, end) = range(p, n).ok_or_else(bad_range)?;
                (Dim::Exact(end - start), end)
            }
            // 長さが分からないときは、上限の長さで区間が成り立つかだけを確かめる（実際は実行で確かめる）
            Dim::AtMost(m) => {
                let (start, end) = range(p, m).ok_or_else(bad_range)?;
                (Dim::AtMost(end - start), end)
            }
        };
        Ok(Inferred {
            shape: Shape::Audio {
                rate,
                channels,
                frames,
            },
            space: SpaceMap::Identity,
            time: TimeMap::Segments {
                segs: vec![Seg {
                    src_start: p.start,
                    src_end,
                    dst_start: 0,
                }],
            },
            cost: frames.max().saturating_mul(u64::from(channels)),
        })
    }

    fn apply(
        p: &TrimParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let k = kernel(p, inputs)?;
        if k.start == 0 && k.frames == k.audio.frames() as u64 {
            return Ok(Applied::value(Value::Audio(k.audio)));
        }
        let meta = k.audio.meta.clone();
        Ok(Applied::value(render_all(&k, meta, cx.mon)?))
    }

    fn work_bytes(_p: &Self::Params, _inputs: &[Shape], out: &Shape) -> crate::op::Work {
        crate::audio::render_all_work(out)
    }

    fn audio_kernel(
        p: &TrimParams,
        inputs: &[&Value],
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(kernel(p, inputs).map(|k| Box::new(k) as Box<dyn AudioKernel>))
    }

    fn examples() -> Vec<Example<TrimParams>> {
        let with_meta = InputGen::WithMeta {
            base: Box::new(InputGen::AudioNoise {
                rate: 44_100,
                channels: 2,
                frames: 12_000,
                seed: 31,
            }),
            meta: MetaGen {
                loop_range: Some(LoopRange {
                    start: 2_000,
                    end: 9_000,
                }),
                markers: vec![(500, "intro".into()), (6_000, "mid".into())],
                ..MetaGen::default()
            },
        };
        vec![
            Example {
                name: "middle-with-loop-and-markers",
                params: TrimParams {
                    start: 1_000,
                    end: Some(8_000),
                },
                inputs: vec![with_meta.clone()],
            },
            Example {
                name: "to-the-end",
                params: TrimParams {
                    start: 4_096,
                    end: None,
                },
                inputs: vec![with_meta],
            },
            Example {
                name: "whole-is-identity",
                params: TrimParams {
                    start: 0,
                    end: None,
                },
                inputs: vec![InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 1,
                    frames: 5_000,
                    seed: 32,
                }],
            },
        ]
    }

    // 段 10：範囲（区間をずらす）
    fn reach(p: &TrimParams, inputs: &[Shape]) -> Reach {
        // 長さが分からなければ上限の長さで区間を求める（その先には標本がないので保守的）
        let Some((_, _, frames)) = inputs.first().and_then(audio_shape) else {
            return Reach::Global;
        };
        match range(p, frames.max()) {
            Some((start, end)) => Reach::Segments(vec![Seg {
                src_start: start,
                src_end: end,
                dst_start: 0,
            }]),
            None => Reach::Global,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    #[test]
    fn copies_samples_and_maps_metadata() {
        let input = Trim::examples()[0].inputs[0].generate().unwrap();
        let out = render_value(
            Trim::ID,
            1,
            json!({ "start": 1_000, "end": 8_000 }),
            input.clone(),
        )
        .unwrap();
        let (Value::Audio(a), Value::Audio(src)) = (&out, &input) else {
            panic!("音声ではありません")
        };
        assert_eq!(a.frames(), 7_000);
        assert_eq!(a.planes()[1][..], src.planes()[1][1_000..8_000]);
        assert_eq!(
            a.meta.loop_range,
            Some(LoopRange {
                start: 1_000,
                end: 7_000
            })
        );
        assert_eq!(
            a.meta.markers.iter().map(|m| m.pos).collect::<Vec<_>>(),
            vec![5_000]
        );
    }

    #[test]
    fn refuses_empty_or_long_ranges() {
        let input = Trim::examples()[2].inputs[0].generate().unwrap();
        for params in [
            json!({ "start": 10, "end": 10 }),
            json!({ "start": 0, "end": 5_001 }),
            json!({ "start": 5_000, "end": null }),
        ] {
            assert!(
                render_value(Trim::ID, 1, params.clone(), input.clone()).is_err(),
                "{params}"
            );
        }
    }
}
