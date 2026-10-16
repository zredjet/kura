//! `sprite.pack_grid` v1：フレームの列を格子に並べて、1 枚のスプライトシートにする（画素は動かすだけ）。
//!
//! - セルの大きさはキャンバスの大きさ。フレームは並び順に、行ごとに左から `columns` 個ずつ置く（フレームが少なければ 1 行）。
//!   各フレームはセルの左上からキャンバスの中の位置（offset）に置く（トリムしたフレームも元の位置に戻る）。
//! - 何も置かない部分（間隔・最後の行の空き・トリムしたフレームの外）は `fill` で塗る。`fill` の扱いは `image.canvas` と同じ
//!   （null は透明で、アルファのない形式はアルファ付きになる。Indexed8 はパレットの色を探すか足す）。
//! - メタデータは最初のセルの座標のまま運び、格子（セルの大きさと間隔）を書く。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::IssueCode;
use crate::error::{EngineError, Issues};
use crate::geom::IRect;
use crate::imaging::convert::convert_pixel;
use crate::imaging::pixels::color_bytes;
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::image::canvas::{fill_pixel, out_format};
use crate::ops::util::{bad_param, param_issue};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Grid, Image, PixelFormat, Raster, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackParams {
    /// 1 行に置くフレームの数。
    pub columns: u32,
    pub gap_x: u32,
    pub gap_y: u32,
    /// 何も置かない部分の色（ストレートの RGBA8）。null は透明。
    #[serde(default)]
    pub fill: Option<[u8; 4]>,
}

impl OpParams for PackParams {
    fn schema() -> ParamSchema {
        let int = |max| ParamKind::Int {
            min: 0,
            max,
            step: 1,
        };
        ParamSchema {
            params: vec![
                spec(
                    "columns",
                    ParamKind::Int {
                        min: 1,
                        max: 4_096,
                        step: 1,
                    },
                    Unit::None,
                    json!(8),
                ),
                spec("gapX", int(4_096), Unit::Px, json!(0)),
                spec("gapY", int(4_096), Unit::Px, json!(0)),
                spec(
                    "fill",
                    ParamKind::Color { alpha: true },
                    Unit::None,
                    json!(null),
                ),
            ],
        }
    }
}

/// 格子への結合。
pub struct PackGrid;

/// (列の数, 行の数, 幅, 高さ)。
fn layout(p: &PackParams, n: u64, cw: u64, ch: u64) -> (u64, u64, u64, u64) {
    let cols = u64::from(p.columns.max(1)).min(n.max(1));
    let rows = n.max(1).div_ceil(cols);
    let w = cols
        .saturating_mul(cw)
        .saturating_add((cols - 1).saturating_mul(u64::from(p.gap_x)));
    let h = rows
        .saturating_mul(ch)
        .saturating_add((rows - 1).saturating_mul(u64::from(p.gap_y)));
    (cols, rows, w, h)
}

impl Op for PackGrid {
    const ID: &'static str = "sprite.pack_grid";
    const VERSION: u16 = 1;
    type Params = PackParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "frames",
                accepts: &[ValueKind::Frames],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Write,
        }
    }

    fn infer(p: &PackParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Frames {
            count, w, h, fmt, ..
        } = inputs[0]
        else {
            return Err(Issues::none());
        };
        if p.columns == 0 {
            return Err(bad_param("columns", "列の数は 1 以上にします"));
        }
        let out_fmt = out_format(fmt, p.fill);
        if let (Some(c), false) = (p.fill, out_fmt == PixelFormat::Indexed8) {
            color_bytes(out_fmt, c).map_err(|m| bad_param("fill", m))?;
        }
        // 数・大きさの上限から求めた大きさは、実際の大きさの上限になる（どれも単調）
        let (_, _, ow, oh) = layout(p, count.max(), w.max(), h.max());
        if ow > cx.limits.max_dim || oh > cx.limits.max_dim {
            return Err(param_issue(
                "columns",
                IssueCode::TooLarge,
                "シートが大きすぎます",
            ));
        }
        let exact = count.exact().is_some() && w.exact().is_some() && h.exact().is_some();
        let dim = |v| if exact { Dim::Exact(v) } else { Dim::AtMost(v) };
        Ok(Inferred {
            shape: Shape::Image {
                w: dim(ow),
                h: dim(oh),
                fmt: out_fmt,
            },
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: ow.saturating_mul(oh),
        })
    }

    fn apply(
        p: &PackParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let Value::Frames(fr) = inputs[0] else {
            return Err(EngineError::Internal(
                "sprite.pack_grid の入力がフレームの列ではありません".into(),
            ));
        };
        let canvas = fr.canvas();
        let n = fr.frames().len() as u64;
        let (cols, _, ow, oh) = layout(p, n, u64::from(canvas.w), u64::from(canvas.h));
        let too_large = || EngineError::Limit("シートが大きすぎます".into());
        let (ow, oh) = (
            u32::try_from(ow).map_err(|_| too_large())?,
            u32::try_from(oh).map_err(|_| too_large())?,
        );
        let first = &fr.first().px;
        let in_fmt = first.format();
        let fmt = out_format(in_fmt, p.fill);
        let (fill_px, palette) = fill_pixel(fmt, first.palette().map(|p| p.as_ref()), p.fill)?;
        let palette = palette.or_else(|| first.palette().map(|p| (**p).clone()));
        let in_palette = first.palette().map(|p| p.colors.as_slice());
        let (in_bpp, bpp) = (first.bpp(), fmt.bytes_per_pixel());
        // 各フレームを置く矩形（シートの座標）
        let (px_step, py_step) = (
            i64::from(canvas.w) + i64::from(p.gap_x),
            i64::from(canvas.h) + i64::from(p.gap_y),
        );
        let placed: Vec<(IRect, &Raster)> = fr
            .frames()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (cx_, cy_) = ((i as u64 % cols) as i64, (i as u64 / cols) as i64);
                (
                    IRect::new(
                        cx_ * px_step + f.offset.x,
                        cy_ * py_step + f.offset.y,
                        f.px.width(),
                        f.px.height(),
                    ),
                    &f.px,
                )
            })
            .collect();
        let px = Raster::from_tile_fn(ow, oh, fmt, palette, cx.mon, |t, out| {
            for d in out.chunks_exact_mut(bpp) {
                d.copy_from_slice(&fill_px);
            }
            for (r, src) in &placed {
                let (x0, y0) = (r.x.max(t.x), r.y.max(t.y));
                let (x1, y1) = (r.right().min(t.right()), r.bottom().min(t.bottom()));
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                let cw = (x1 - x0) as u32;
                let mut row = vec![0u8; cw as usize * in_bpp];
                for y in y0..y1 {
                    src.read_rect(IRect::new(x0 - r.x, y - r.y, cw, 1), &mut row)?;
                    let dst = ((y - t.y) as usize * t.w as usize + (x0 - t.x) as usize) * bpp;
                    if in_fmt == fmt {
                        out[dst..dst + row.len()].copy_from_slice(&row);
                    } else {
                        for (i, s) in row.chunks_exact(in_bpp).enumerate() {
                            let o = dst + i * bpp;
                            convert_pixel(
                                in_fmt,
                                in_palette,
                                s,
                                fmt,
                                [0, 0, 0],
                                &mut out[o..o + bpp],
                            )?;
                        }
                    }
                }
            }
            Ok(())
        })?;
        let mut meta = fr.meta.clone();
        meta.grid = Some(Grid {
            cell_w: canvas.w,
            cell_h: canvas.h,
            offset_x: 0,
            offset_y: 0,
            gap_x: p.gap_x,
            gap_y: p.gap_y,
        });
        Ok(Applied::value(Value::Image(Arc::new(Image { px, meta }))))
    }

    fn examples() -> Vec<Example<PackParams>> {
        vec![
            Example {
                name: "three-in-a-row",
                params: PackParams {
                    columns: 8,
                    gap_x: 0,
                    gap_y: 0,
                    fill: None,
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 6,
                    h: 5,
                    fmt: PixelFormat::Rgba8,
                    count: 3,
                    seed: 81,
                }],
            },
            Example {
                name: "wrap-with-gaps-rgb",
                params: PackParams {
                    columns: 2,
                    gap_x: 1,
                    gap_y: 2,
                    fill: Some([0, 255, 0, 255]),
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 4,
                    h: 3,
                    fmt: PixelFormat::Rgb8,
                    count: 5,
                    seed: 82,
                }],
            },
            Example {
                name: "indexed-gains-clear-color",
                params: PackParams {
                    columns: 2,
                    gap_x: 0,
                    gap_y: 0,
                    fill: None,
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 3,
                    h: 3,
                    fmt: PixelFormat::Indexed8,
                    count: 3,
                    seed: 83,
                }],
            },
            Example {
                name: "big-sheet-across-tiles",
                params: PackParams {
                    columns: 3,
                    gap_x: 3,
                    gap_y: 0,
                    fill: None,
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 130,
                    h: 140,
                    fmt: PixelFormat::GrayA8,
                    count: 4,
                    seed: 84,
                }],
            },
        ]
    }
}
