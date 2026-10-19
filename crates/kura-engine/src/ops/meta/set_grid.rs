//! `meta.set_grid` v1：スプライトの格子のメタデータを決める（null で消す）。画素は変えない。
//!
//! セルの大きさは 1 以上、最初のセルが画像（フレームの列はキャンバス）に収まること。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{GridParam, bad_param, exact_size};
use crate::render::preview::{Preview, image_bounds, scale_exact};
use crate::render::region::{Area, FrameTiles, Reach, splice_value};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Grid, PixelFormat, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGridParams {
    pub grid: Option<GridParam>,
}

impl OpParams for SetGridParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec("grid", ParamKind::Grid, Unit::Px, json!(null))],
        }
    }
}

/// 格子の設定。
pub struct SetGrid;

fn fits(g: &GridParam, w: u64, h: u64) -> bool {
    g.cell_w > 0
        && g.cell_h > 0
        && u64::from(g.offset_x) + u64::from(g.cell_w) <= w
        && u64::from(g.offset_y) + u64::from(g.cell_h) <= h
}

fn not_fit() -> Issues {
    bad_param("grid", "格子の最初のセルが画像に収まりません")
}

impl Op for SetGrid {
    const ID: &'static str = "meta.set_grid";
    const VERSION: u16 = 1;
    type Params = SetGridParams;

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

    fn infer(p: &SetGridParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let (w, h) = match inputs[0] {
            Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => (w, h),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        if let Some(g) = &p.grid {
            // 大きさが決まっていなければ上限で確かめ、実行で確かめ直す
            let ok = match exact_size(w, h) {
                Some(s) => fits(g, u64::from(s.w), u64::from(s.h)),
                None => fits(g, w.max(), h.max()),
            };
            if !ok {
                return Err(not_fit());
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
        p: &SetGridParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let value = match inputs[0] {
            Value::Image(img) => {
                if let Some(g) = &p.grid
                    && !fits(g, u64::from(img.px.width()), u64::from(img.px.height()))
                {
                    return Err(EngineError::Invalid(not_fit()));
                }
                let mut img = (**img).clone();
                img.meta.grid = p.grid.map(Grid::from);
                Value::Image(Arc::new(img))
            }
            Value::Frames(fr) => {
                let c = fr.canvas();
                if let Some(g) = &p.grid
                    && !fits(g, u64::from(c.w), u64::from(c.h))
                {
                    return Err(EngineError::Invalid(not_fit()));
                }
                let mut fr = (**fr).clone();
                fr.meta.grid = p.grid.map(Grid::from);
                Value::Frames(Arc::new(fr))
            }
            Value::Audio(_) => {
                return Err(EngineError::Internal(
                    "meta.set_grid の入力が音声です".into(),
                ));
            }
        };
        Ok(Applied::value(value))
    }

    fn examples() -> Vec<Example<SetGridParams>> {
        vec![
            Example {
                name: "set",
                params: SetGridParams {
                    grid: Some(GridParam {
                        cell_w: 8,
                        cell_h: 6,
                        offset_x: 1,
                        offset_y: 0,
                        gap_x: 1,
                        gap_y: 2,
                    }),
                },
                inputs: vec![InputGen::Noise {
                    w: 30,
                    h: 20,
                    fmt: PixelFormat::Rgba8,
                    seed: 91,
                }],
            },
            Example {
                name: "clear",
                params: SetGridParams { grid: None },
                inputs: vec![InputGen::Sheet {
                    cols: 2,
                    rows: 2,
                    cell: 8,
                }],
            },
        ]
    }

    // 段 10：画素は入力のまま（範囲は同じ位置。描くタイルは入力のタイルを共有する）
    const REGION: bool = true;

    fn reach(_p: &SetGridParams, _inputs: &[Shape]) -> Reach {
        Reach::Same
    }

    fn param_change(_old: &SetGridParams, _new: &SetGridParams, _inputs: &[Shape]) -> Area {
        // 画素は変わらない（メタデータだけ）
        Area::Empty
    }

    fn effect(_p: &SetGridParams, _inputs: &[Shape]) -> Area {
        Area::Empty
    }

    fn apply_region(
        p: &SetGridParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        // 画素を読まない（メタデータ・パレットだけを変える）ので、描くタイルを入力から共有した値に `apply` を当てる
        Some(splice_value(inputs[0], tiles, base).and_then(|v| Self::apply(p, &[&v], cx)))
    }

    // 段 10：プレビューの段（docs/editing.md §12.7）。画素は描かず、格子を段の座標で書く（2^k で割り切れない・段の入力に
    // 収まらない格子は消す）
    fn preview(_p: &SetGridParams) -> Preview {
        Preview::Skip
    }

    fn preview_params(
        p: &SetGridParams,
        level: u8,
        inputs: &[Shape],
        _full: &[Shape],
    ) -> SetGridParams {
        let scaled = p.grid.and_then(|g| {
            let g = GridParam {
                cell_w: scale_exact(g.cell_w, level)?,
                cell_h: scale_exact(g.cell_h, level)?,
                offset_x: scale_exact(g.offset_x, level)?,
                offset_y: scale_exact(g.offset_y, level)?,
                gap_x: scale_exact(g.gap_x, level)?,
                gap_y: scale_exact(g.gap_y, level)?,
            };
            let (w, h) = inputs.first().and_then(image_bounds)?;
            fits(&g, u64::from(w), u64::from(h)).then_some(g)
        });
        SetGridParams { grid: scaled }
    }
}
