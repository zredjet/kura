//! `sprite.split_grid` v1：スプライトシートを格子で切って、フレームの列にする（画素は動かすだけ）。
//!
//! - セルは左上 `offset` から `cell + gap` の間隔で、画像に収まるものだけ（端の欠けたセルは数えない。メタデータの格子と同じ数え方）。
//!   並びは行ごとに左から。各セルがキャンバスいっぱいのフレームになる（表示時間は `durationMs`、繰り返しは無限）。
//! - `skipEmpty` なら全部が透明なセルを除く（数は中身で決まるので `AtMost`）。全部が透明なら最初のセルだけ残す。
//! - メタデータは最初のセルの座標に写す（ピボット・スライスは最初のセルの中のものが残る）。格子はフレームに切ったので消す。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::{IRect, Size};
use crate::imaging::bounds::opaque_bounds;
use crate::mapping::{IntAffine, SpaceMap, TimeMap, map_image_meta_for};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{GridParam, bad_param, image_of};
use crate::shape::{Dim, Fullness, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Frames, Grid, PixelFormat, Raster, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SplitParams {
    pub grid: GridParam,
    /// 各フレームの表示時間（ms）。
    pub duration_ms: u32,
    /// 全部が透明なセルを除く。
    pub skip_empty: bool,
}

impl OpParams for SplitParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "grid",
                    ParamKind::Grid,
                    Unit::Px,
                    json!({ "cellW": 16, "cellH": 16, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 }),
                ),
                spec(
                    "durationMs",
                    ParamKind::Int {
                        min: 1,
                        max: 65_535,
                        step: 1,
                    },
                    Unit::Ms,
                    json!(100),
                ),
                spec("skipEmpty", ParamKind::Bool, Unit::None, json!(false)),
            ],
        }
    }
}

/// 格子での分割。
pub struct SplitGrid;

/// 1 つの軸に収まるセルの数。
pub fn cells_on_axis(len: u64, offset: u32, cell: u32, gap: u32) -> u64 {
    let (off, cell, period) = (
        u64::from(offset),
        u64::from(cell),
        u64::from(cell) + u64::from(gap),
    );
    if cell == 0 || off.saturating_add(cell) > len {
        0
    } else {
        (len - off - cell) / period + 1
    }
}

/// セルの矩形（行ごとに左から）。
pub fn cell_rects(g: &Grid, w: u32, h: u32) -> Vec<IRect> {
    let nx = cells_on_axis(u64::from(w), g.offset_x, g.cell_w, g.gap_x);
    let ny = cells_on_axis(u64::from(h), g.offset_y, g.cell_h, g.gap_y);
    let (px, py) = (
        i64::from(g.cell_w) + i64::from(g.gap_x),
        i64::from(g.cell_h) + i64::from(g.gap_y),
    );
    (0..ny as i64)
        .flat_map(|y| {
            (0..nx as i64).map(move |x| {
                IRect::new(
                    i64::from(g.offset_x) + x * px,
                    i64::from(g.offset_y) + y * py,
                    g.cell_w,
                    g.cell_h,
                )
            })
        })
        .collect()
}

impl Op for SplitGrid {
    const ID: &'static str = "sprite.split_grid";
    const VERSION: u16 = 1;
    type Params = SplitParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Write,
        }
    }

    fn infer(p: &SplitParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Image { w, h, fmt } = inputs[0] else {
            return Err(Issues::none());
        };
        let g = p.grid;
        if g.cell_w == 0 || g.cell_h == 0 {
            return Err(bad_param("grid", "セルの大きさが 0 です"));
        }
        let n = cells_on_axis(w.max(), g.offset_x, g.cell_w, g.gap_x)
            .saturating_mul(cells_on_axis(h.max(), g.offset_y, g.cell_h, g.gap_y));
        if n == 0 {
            return Err(bad_param("grid", "格子のセルが画像に 1 つも入りません"));
        }
        let exact = w.exact().is_some() && h.exact().is_some() && !p.skip_empty;
        Ok(Inferred {
            shape: Shape::Frames {
                count: if exact { Dim::Exact(n) } else { Dim::AtMost(n) },
                w: Dim::Exact(u64::from(g.cell_w)),
                h: Dim::Exact(u64::from(g.cell_h)),
                fmt,
                full: Fullness::Full,
            },
            space: SpaceMap::Affine {
                m: IntAffine::translate(-i64::from(g.offset_x), -i64::from(g.offset_y)),
                out: Size {
                    w: g.cell_w,
                    h: g.cell_h,
                },
            },
            time: TimeMap::Identity,
            cost: w.max().saturating_mul(h.max()),
        })
    }

    fn apply(
        p: &SplitParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let img = image_of(inputs[0], Self::ID)?;
        let rects = cell_rects(&Grid::from(p.grid), img.px.width(), img.px.height());
        if rects.is_empty() {
            return Err(EngineError::Invalid(bad_param(
                "grid",
                "格子のセルが画像に 1 つも入りません",
            )));
        }
        let mut cells: Vec<(Raster, u32)> = Vec::with_capacity(rects.len());
        for r in &rects {
            cx.check()?;
            let px = img.px.derive(r.w, r.h, cx.mon, |t, out| {
                img.px
                    .read_rect(IRect::new(r.x + t.x, r.y + t.y, t.w, t.h), out)
            })?;
            if p.skip_empty && opaque_bounds(&px, 0, cx.mon)?.is_none() {
                continue;
            }
            cells.push((px, p.duration_ms));
        }
        if cells.is_empty() {
            // 全部が透明なら最初のセルだけ
            let r = rects[0];
            let px = img.px.derive(r.w, r.h, cx.mon, |t, out| {
                img.px
                    .read_rect(IRect::new(r.x + t.x, r.y + t.y, t.w, t.h), out)
            })?;
            cells.push((px, p.duration_ms));
        }
        // メタデータは最初のセルの座標に写す（格子は消す）
        let mut meta = img.meta.clone();
        meta.grid = None;
        let (meta, warnings) = map_image_meta_for(
            cx.rules,
            &meta,
            Size {
                w: img.px.width(),
                h: img.px.height(),
            },
            &SpaceMap::Affine {
                m: IntAffine::translate(-i64::from(p.grid.offset_x), -i64::from(p.grid.offset_y)),
                out: Size {
                    w: p.grid.cell_w,
                    h: p.grid.cell_h,
                },
            },
        )?;
        for w in warnings {
            cx.warn(w);
        }
        Ok(Applied::value(Value::Frames(Arc::new(Frames::full(
            cells, meta, 0,
        )?))))
    }

    fn examples() -> Vec<Example<SplitParams>> {
        let grid = |cell_w, cell_h, offset_x, offset_y, gap_x, gap_y| GridParam {
            cell_w,
            cell_h,
            offset_x,
            offset_y,
            gap_x,
            gap_y,
        };
        vec![
            Example {
                name: "sheet-4x2",
                params: SplitParams {
                    grid: grid(16, 16, 0, 0, 0, 0),
                    duration_ms: 80,
                    skip_empty: false,
                },
                inputs: vec![InputGen::Sheet {
                    cols: 4,
                    rows: 2,
                    cell: 16,
                }],
            },
            Example {
                name: "offset-gap-indexed",
                params: SplitParams {
                    grid: grid(5, 4, 1, 2, 2, 1),
                    duration_ms: 100,
                    skip_empty: false,
                },
                inputs: vec![InputGen::Noise {
                    w: 20,
                    h: 13,
                    fmt: PixelFormat::Indexed8,
                    seed: 71,
                }],
            },
            Example {
                name: "skip-empty-cells",
                params: SplitParams {
                    grid: grid(8, 8, 0, 0, 0, 0),
                    duration_ms: 120,
                    skip_empty: true,
                },
                inputs: vec![InputGen::Sprite {
                    w: 32,
                    h: 16,
                    rect: IRect::new(9, 2, 10, 4),
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_single;

    #[test]
    fn counts_cells_like_the_grid_metadata() {
        assert_eq!(cells_on_axis(70, 2, 16, 1), 4);
        assert_eq!(cells_on_axis(68, 2, 16, 1), 3);
        assert_eq!(cells_on_axis(10, 11, 1, 0), 0);
        // 大きな値でもあふれない
        assert!(cells_on_axis(u64::MAX, u32::MAX, u32::MAX, u32::MAX) > 0);
        assert_eq!(
            cells_on_axis(u64::from(u32::MAX), u32::MAX, u32::MAX, u32::MAX),
            0
        );
    }

    #[test]
    fn skips_empty_cells_and_keeps_one_when_all_are_empty() {
        let run = |rect| {
            render_single(
                SplitGrid::ID,
                1,
                serde_json::to_value(SplitParams {
                    grid: GridParam {
                        cell_w: 8,
                        cell_h: 8,
                        offset_x: 0,
                        offset_y: 0,
                        gap_x: 0,
                        gap_y: 0,
                    },
                    duration_ms: 50,
                    skip_empty: true,
                })
                .unwrap(),
                &[InputGen::Sprite { w: 32, h: 16, rect }],
            )
            .unwrap()
        };
        // 9..19 × 2..6 は 1 行目の 2・3 番目のセルにまたがる
        let Value::Frames(fr) = run(IRect::new(9, 2, 10, 4)) else {
            panic!()
        };
        assert_eq!(fr.frames().len(), 2);
        let Value::Frames(none) = run(IRect::new(40, 40, 1, 1)) else {
            panic!()
        };
        assert_eq!(none.frames().len(), 1);
    }
}
