//! `audio.set_loop` v1：ループ区間（フレーム。end は含まない）を決める。null で消す。標本は変えない。
//!
//! 区間は 0 ≤ start < end ≤ 長さ であること。メタデータを書く操作（`MetaAccess::Write`）なので、ループ点のほかの
//! メタデータ（マーカー・タグ・元の形式）は入力のまま持ち越す。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::{AudioKernel, Identity};
use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{audio_of, audio_shape, bad_param};
use crate::render::region::{Area, Reach};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Audio, LoopRange, Value};

/// ループの区間（引数の型。メタデータの型は流用しない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameRange {
    pub start: u64,
    pub end: u64,
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetLoopParams {
    #[serde(rename = "loop")]
    pub range: Option<FrameRange>,
}

impl OpParams for SetLoopParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "loop",
                ParamKind::FrameRange,
                Unit::Samples,
                json!(null),
            )],
        }
    }
}

/// ループ区間の設定。
pub struct SetLoop;

fn bad_range() -> Issues {
    bad_param("loop", "ループの区間が空か、音声の長さを超えています")
}

fn fits(r: FrameRange, n: u64) -> bool {
    r.start < r.end && r.end <= n
}

impl Op for SetLoop {
    const ID: &'static str = "audio.set_loop";
    const VERSION: u16 = 1;
    type Params = SetLoopParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "audio",
                accepts: &[ValueKind::Audio],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Write,
        }
    }

    fn infer(p: &SetLoopParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((_, _, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        if let Some(r) = p.range {
            let n = match frames {
                Dim::Exact(n) | Dim::AtMost(n) => n,
            };
            if !fits(r, n) {
                return Err(bad_range());
            }
        }
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: 1,
        })
    }

    fn apply(
        p: &SetLoopParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let audio = audio_of(inputs[0], Self::ID)?;
        if let Some(r) = p.range
            && !fits(r, audio.frames() as u64)
        {
            return Err(EngineError::Invalid(bad_range()));
        }
        let mut meta = audio.meta.clone();
        meta.loop_range = p.range.map(|r| LoopRange {
            start: r.start,
            end: r.end,
        });
        // 標本の実体は共有する
        let out = Audio::new(audio.rate(), audio.planes().to_vec(), meta)?;
        Ok(Applied::value(Value::Audio(std::sync::Arc::new(out))))
    }

    fn audio_kernel(
        _p: &SetLoopParams,
        inputs: &[&Value],
        _cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(
            audio_of(inputs[0], Self::ID)
                .map(|a| Box::new(Identity(a.clone())) as Box<dyn AudioKernel>),
        )
    }

    fn examples() -> Vec<Example<SetLoopParams>> {
        let sine = InputGen::Sine {
            rate: 22_050,
            channels: 2,
            frames: 8_000,
            freq: 220,
            loop_range: Some(LoopRange { start: 10, end: 20 }),
        };
        vec![
            Example {
                name: "set",
                params: SetLoopParams {
                    range: Some(FrameRange {
                        start: 1_000,
                        end: 7_999,
                    }),
                },
                inputs: vec![sine.clone()],
            },
            Example {
                name: "clear",
                params: SetLoopParams { range: None },
                inputs: vec![sine],
            },
        ]
    }

    // 段 10：範囲（同じ位置）。標本は変わらない（ループ点のメタデータだけ）
    fn reach(_p: &SetLoopParams, _inputs: &[Shape]) -> Reach {
        Reach::SameTime
    }

    fn param_change(_old: &SetLoopParams, _new: &SetLoopParams, _inputs: &[Shape]) -> Area {
        Area::Empty
    }

    fn effect(_p: &SetLoopParams, _inputs: &[Shape]) -> Area {
        Area::Empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    #[test]
    fn sets_and_clears() {
        let input = SetLoop::examples()[0].inputs[0].generate().unwrap();
        let set = render_value(
            SetLoop::ID,
            1,
            json!({ "loop": { "start": 5, "end": 8_000 } }),
            input.clone(),
        )
        .unwrap();
        let Value::Audio(a) = &set else {
            panic!("音声ではありません")
        };
        assert_eq!(
            a.meta.loop_range,
            Some(LoopRange {
                start: 5,
                end: 8_000
            })
        );
        let cleared = render_value(SetLoop::ID, 1, json!({ "loop": null }), set).unwrap();
        let Value::Audio(c) = &cleared else {
            panic!("音声ではありません")
        };
        assert_eq!(c.meta.loop_range, None);
        for bad in [
            json!({ "loop": { "start": 5, "end": 5 } }),
            json!({ "loop": { "start": 0, "end": 8_001 } }),
            json!({ "loop": { "start": 0, "end": 10, "kind": 1 } }),
        ] {
            assert!(
                render_value(SetLoop::ID, 1, bad.clone(), input.clone()).is_err(),
                "{bad}"
            );
        }
    }
}
