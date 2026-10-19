//! `meta.set_pivot` v1：ピボット（基準点）のメタデータを決める（null で消す）。画素は変えない。
//!
//! 座標は画素の角を 0 とする連続の座標で、画像（フレームの列はキャンバス）の範囲 `0..=w × 0..=h` の中であること（右端・下端も可）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::IPoint;
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{bad_param, exact_size};
use crate::render::preview::{Preview, image_bounds, scale_point};
use crate::render::region::{Area, FrameTiles, Reach, splice_value};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPivotParams {
    pub pivot: Option<IPoint>,
}

impl OpParams for SetPivotParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec("pivot", ParamKind::Point, Unit::Px, json!(null))],
        }
    }
}

/// ピボットの設定。
pub struct SetPivot;

fn inside(p: IPoint, w: u64, h: u64) -> bool {
    p.x >= 0 && p.y >= 0 && p.x as u64 <= w && p.y as u64 <= h
}

fn outside() -> Issues {
    bad_param("pivot", "ピボットが画像の外にあります")
}

impl Op for SetPivot {
    const ID: &'static str = "meta.set_pivot";
    const VERSION: u16 = 1;
    type Params = SetPivotParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image, ValueKind::Frames],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Write,
        }
    }

    fn infer(p: &SetPivotParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let (w, h) = match inputs[0] {
            Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => (w, h),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        if let Some(pt) = p.pivot {
            let ok = match exact_size(w, h) {
                Some(s) => inside(pt, u64::from(s.w), u64::from(s.h)),
                None => inside(pt, w.max(), h.max()),
            };
            if !ok {
                return Err(outside());
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
        p: &SetPivotParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let value = match inputs[0] {
            Value::Image(img) => {
                if let Some(pt) = p.pivot
                    && !inside(pt, u64::from(img.px.width()), u64::from(img.px.height()))
                {
                    return Err(EngineError::Invalid(outside()));
                }
                let mut img = (**img).clone();
                img.meta.pivot = p.pivot;
                Value::Image(Arc::new(img))
            }
            Value::Frames(fr) => {
                let c = fr.canvas();
                if let Some(pt) = p.pivot
                    && !inside(pt, u64::from(c.w), u64::from(c.h))
                {
                    return Err(EngineError::Invalid(outside()));
                }
                let mut fr = (**fr).clone();
                fr.meta.pivot = p.pivot;
                Value::Frames(Arc::new(fr))
            }
            Value::Audio(_) => {
                return Err(EngineError::Internal(
                    "meta.set_pivot の入力が音声です".into(),
                ));
            }
        };
        Ok(Applied::value(value))
    }

    fn examples() -> Vec<Example<SetPivotParams>> {
        vec![
            Example {
                name: "bottom-center",
                params: SetPivotParams {
                    pivot: Some(IPoint { x: 8, y: 16 }),
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 16,
                    h: 16,
                    fmt: PixelFormat::Rgba8,
                    count: 2,
                    seed: 92,
                }],
            },
            Example {
                name: "clear",
                params: SetPivotParams { pivot: None },
                inputs: vec![InputGen::Sheet {
                    cols: 2,
                    rows: 1,
                    cell: 8,
                }],
            },
        ]
    }

    // 段 10：画素は入力のまま（範囲は同じ位置。描くタイルは入力のタイルを共有する）
    const REGION: bool = true;

    fn reach(_p: &SetPivotParams, _inputs: &[Shape]) -> Reach {
        Reach::Same
    }

    fn param_change(_old: &SetPivotParams, _new: &SetPivotParams, _inputs: &[Shape]) -> Area {
        // 画素は変わらない（メタデータだけ）
        Area::Empty
    }

    fn effect(_p: &SetPivotParams, _inputs: &[Shape]) -> Area {
        Area::Empty
    }

    fn apply_region(
        p: &SetPivotParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        // 画素を読まない（メタデータ・パレットだけを変える）ので、描くタイルを入力から共有した値に `apply` を当てる
        Some(splice_value(inputs[0], tiles, base).and_then(|v| Self::apply(p, &[&v], cx)))
    }

    // 段 10：プレビューの段。画素は描かず、ピボットを段の座標で書く（段の入力に収める。docs/editing.md §12.7）
    fn preview(_p: &SetPivotParams) -> Preview {
        Preview::Skip
    }

    fn preview_params(
        p: &SetPivotParams,
        level: u8,
        inputs: &[Shape],
        _full: &[Shape],
    ) -> SetPivotParams {
        let pivot = p.pivot.map(|pt| {
            let q = scale_point(pt, level);
            match inputs.first().and_then(image_bounds) {
                Some((w, h)) => IPoint {
                    x: q.x.clamp(0, i64::from(w)),
                    y: q.y.clamp(0, i64::from(h)),
                },
                None => q,
            }
        });
        SetPivotParams { pivot }
    }
}
