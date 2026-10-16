//! `paint.actions` v1：ペン・消しゴム・塗りつぶしの動作を順に当てる（docs/editing.md §10。規則は `crate::paint`）。
//!
//! 引数は追記だけで増える。前の引数の出力に追記した動作だけを当てる差分の適用（`apply_appended`）は、
//! 全体を当て直したものとビット単位で同じ（E21）。手間（`PaintAction::work`：ストロークの道の点の数 × 筆の画素の数と
//! 画素の集合の大きさ、塗りつぶしは画像の画素の数）の合計は、ノード 1 つあたり `MAX_PAINT_WORK` まで（遠く離れた点・大きな筆・
//! 塗りつぶしの繰り返しで、描くのに時間のかかりすぎる文書を推論の段階で断る。超えた時点で数えるのをやめる）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::IPoint;
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::bad_param;
use crate::paint::{
    Blend, Brush, BrushShape, Connectivity, PaintAction, PaintColor, PointList, Tool,
    apply_actions, apply_actions_from,
};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Value};

/// `paint.actions` のノード 1 つあたりの描画の手間の上限。
pub const MAX_PAINT_WORK: u64 = 1 << 30;

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaintParams {
    pub actions: Vec<PaintAction>,
}

impl OpParams for PaintParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec("actions", ParamKind::Strokes, Unit::None, json!([]))],
        }
    }
}

/// 描画。
pub struct Actions;

impl Op for Actions {
    const ID: &'static str = "paint.actions";
    const VERSION: u16 = 1;
    type Params = PaintParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image, ValueKind::Frames],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &PaintParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let (fmt, frames, w, h) = match inputs[0] {
            Shape::Image { fmt, w, h } => (fmt, Some(1), w.max(), h.max()),
            Shape::Frames {
                fmt, count, w, h, ..
            } => (fmt, count.exact(), w.max(), h.max()),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        let mut work = 0u64;
        for (i, a) in p.actions.iter().enumerate() {
            a.check(fmt)
                .map_err(|m| bad_param("actions", format!("動作 {i}：{m}")))?;
            if let Some(n) = frames
                && u64::from(a.frame()) >= n
            {
                return Err(bad_param(
                    "actions",
                    format!("動作 {i}：フレーム {} はありません（{n} 枚）", a.frame()),
                ));
            }
            work = work.saturating_add(a.work(w, h));
            if work > MAX_PAINT_WORK {
                return Err(Issues::one(
                    Issue::error(
                        IssueCode::TooLarge,
                        format!("描画の手間が大きすぎます（動作 {i} まで。点が遠すぎるか、筆・塗りつぶしが多すぎます）"),
                    )
                    .param("actions"),
                ));
            }
        }
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: work,
        })
    }

    fn apply(
        p: &PaintParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        Ok(Applied::value(apply_actions(
            inputs[0], &p.actions, cx.mon,
        )?))
    }

    fn apply_appended(
        prev: &PaintParams,
        prev_out: &Value,
        p: &PaintParams,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        let rest = p.actions.strip_prefix(prev.actions.as_slice())?;
        Some(
            cx.check()
                .and_then(|()| apply_actions_from(prev_out, rest, prev.actions.len(), cx.mon))
                .map(Applied::value),
        )
    }

    fn examples() -> Vec<Example<PaintParams>> {
        let pts =
            |v: &[(i64, i64)]| PointList(v.iter().map(|(x, y)| IPoint { x: *x, y: *y }).collect());
        let pen = |color: [u8; 4], blend, shape, size, pp, points| PaintAction::Stroke {
            tool: Tool::Pen,
            color: Some(PaintColor::Rgba(color)),
            blend,
            brush: Brush { shape, size },
            pixel_perfect: pp,
            frame: 0,
            points,
        };
        vec![
            Example {
                name: "pen-and-eraser-across-tiles",
                params: PaintParams {
                    actions: vec![
                        pen(
                            [255, 0, 0, 255],
                            Blend::Replace,
                            BrushShape::Circle,
                            5,
                            false,
                            pts(&[(-3, 10), (250, 40), (262, 300), (520, 299)]),
                        ),
                        pen(
                            [0, 0, 255, 128],
                            Blend::Over,
                            BrushShape::Square,
                            9,
                            false,
                            pts(&[(100, 100), (400, 120)]),
                        ),
                        PaintAction::Stroke {
                            tool: Tool::Eraser,
                            color: None,
                            blend: Blend::Replace,
                            brush: Brush {
                                shape: BrushShape::Square,
                                size: 3,
                            },
                            pixel_perfect: false,
                            frame: 0,
                            points: pts(&[(255, 0), (255, 299)]),
                        },
                    ],
                },
                inputs: vec![InputGen::Noise {
                    w: 520,
                    h: 300,
                    fmt: PixelFormat::Rgba8,
                    seed: 101,
                }],
            },
            Example {
                name: "pixel-perfect-1px",
                params: PaintParams {
                    actions: vec![pen(
                        [10, 20, 30, 255],
                        Blend::Replace,
                        BrushShape::Square,
                        1,
                        true,
                        pts(&[(0, 0), (1, 0), (1, 1), (2, 1), (2, 2), (9, 3), (3, 9)]),
                    )],
                },
                inputs: vec![InputGen::Checker {
                    w: 12,
                    h: 12,
                    fmt: PixelFormat::Rgb8,
                    cell: 3,
                }],
            },
            Example {
                name: "gray16-over",
                params: PaintParams {
                    actions: vec![pen(
                        [200, 200, 200, 100],
                        Blend::Over,
                        BrushShape::Circle,
                        4,
                        false,
                        pts(&[(1, 1), (8, 5)]),
                    )],
                },
                inputs: vec![InputGen::Noise {
                    w: 10,
                    h: 8,
                    fmt: PixelFormat::Gray16,
                    seed: 102,
                }],
            },
            Example {
                name: "indexed-fill-and-pen",
                params: PaintParams {
                    actions: vec![
                        PaintAction::Fill {
                            at: IPoint { x: 0, y: 0 },
                            color: PaintColor::Index(5),
                            tolerance: 0,
                            connectivity: Connectivity::Eight,
                            contiguous: true,
                            frame: 0,
                        },
                        PaintAction::Stroke {
                            tool: Tool::Pen,
                            color: Some(PaintColor::Index(2)),
                            blend: Blend::Replace,
                            brush: Brush {
                                shape: BrushShape::Square,
                                size: 2,
                            },
                            pixel_perfect: false,
                            frame: 0,
                            points: pts(&[(3, 3), (10, 3)]),
                        },
                        PaintAction::Stroke {
                            tool: Tool::Eraser,
                            color: None,
                            blend: Blend::Replace,
                            brush: Brush {
                                shape: BrushShape::Circle,
                                size: 3,
                            },
                            pixel_perfect: false,
                            frame: 0,
                            points: pts(&[(6, 6)]),
                        },
                    ],
                },
                inputs: vec![InputGen::Checker {
                    w: 14,
                    h: 10,
                    fmt: PixelFormat::Indexed8,
                    cell: 2,
                }],
            },
            Example {
                name: "fill-global-tolerance",
                params: PaintParams {
                    actions: vec![PaintAction::Fill {
                        at: IPoint { x: 4, y: 4 },
                        color: PaintColor::Rgba([0, 255, 0, 255]),
                        tolerance: 40,
                        connectivity: Connectivity::Four,
                        contiguous: false,
                        frame: 0,
                    }],
                },
                inputs: vec![InputGen::Noise {
                    w: 30,
                    h: 20,
                    fmt: PixelFormat::Rgba8,
                    seed: 103,
                }],
            },
            // 9 タイル以上に触れる描画（タイルごとに並列に書く経路）
            Example {
                name: "indexed-global-fill-and-eraser-many-tiles",
                params: PaintParams {
                    actions: vec![
                        PaintAction::Fill {
                            at: IPoint { x: 3, y: 3 },
                            color: PaintColor::Index(9),
                            tolerance: 0,
                            connectivity: Connectivity::Four,
                            contiguous: false,
                            frame: 0,
                        },
                        PaintAction::Stroke {
                            tool: Tool::Eraser,
                            color: None,
                            blend: Blend::Replace,
                            brush: Brush {
                                shape: BrushShape::Circle,
                                size: 7,
                            },
                            pixel_perfect: false,
                            frame: 0,
                            points: pts(&[(0, 0), (1099, 899), (0, 899)]),
                        },
                        PaintAction::Stroke {
                            tool: Tool::Pen,
                            color: Some(PaintColor::Index(3)),
                            blend: Blend::Replace,
                            brush: Brush {
                                shape: BrushShape::Square,
                                size: 3,
                            },
                            pixel_perfect: false,
                            frame: 0,
                            points: pts(&[(-20, 450), (1120, 452)]),
                        },
                    ],
                },
                inputs: vec![InputGen::Noise {
                    w: 1100,
                    h: 900,
                    fmt: PixelFormat::Indexed8,
                    seed: 105,
                }],
            },
            // トリムしたフレームの列（位置が 0 でない）：座標はキャンバスの座標で、フレームの外は描かない
            Example {
                name: "trimmed-frames-canvas-coordinates",
                params: PaintParams {
                    actions: vec![
                        PaintAction::Stroke {
                            tool: Tool::Pen,
                            color: Some(PaintColor::Rgba([9, 9, 9, 255])),
                            blend: Blend::Over,
                            brush: Brush {
                                shape: BrushShape::Square,
                                size: 2,
                            },
                            pixel_perfect: false,
                            frame: 2,
                            points: pts(&[(0, 0), (11, 9)]),
                        },
                        PaintAction::Fill {
                            at: IPoint { x: 0, y: 0 },
                            color: PaintColor::Rgba([1, 2, 3, 4]),
                            tolerance: 255,
                            connectivity: Connectivity::Four,
                            contiguous: true,
                            frame: 2,
                        },
                        PaintAction::Fill {
                            at: IPoint { x: 6, y: 5 },
                            color: PaintColor::Rgba([5, 6, 7, 255]),
                            tolerance: 60,
                            connectivity: Connectivity::Eight,
                            contiguous: true,
                            frame: 3,
                        },
                    ],
                },
                inputs: vec![InputGen::TrimmedFrames {
                    w: 12,
                    h: 10,
                    fmt: PixelFormat::Rgba8,
                    count: 4,
                    seed: 106,
                }],
            },
            Example {
                name: "frames-second-frame",
                params: PaintParams {
                    actions: vec![PaintAction::Stroke {
                        tool: Tool::Pen,
                        color: Some(PaintColor::Rgba([1, 2, 3, 255])),
                        blend: Blend::Replace,
                        brush: Brush {
                            shape: BrushShape::Square,
                            size: 1,
                        },
                        pixel_perfect: false,
                        frame: 1,
                        points: pts(&[(0, 0), (5, 3)]),
                    }],
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 6,
                    h: 4,
                    fmt: PixelFormat::Rgba8,
                    count: 2,
                    seed: 104,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::testing::render_single;

    fn run(input: InputGen, actions: Vec<PaintAction>) -> Result<Value, EngineError> {
        render_single(
            Actions::ID,
            1,
            serde_json::to_value(PaintParams { actions }).unwrap(),
            &[input],
        )
    }

    fn stroke(tool: Tool, color: Option<PaintColor>, blend: Blend, frame: u32) -> PaintAction {
        PaintAction::Stroke {
            tool,
            color,
            blend,
            brush: Brush {
                shape: BrushShape::Square,
                size: 1,
            },
            pixel_perfect: false,
            frame,
            points: PointList(vec![IPoint { x: 0, y: 0 }]),
        }
    }

    #[test]
    fn checks_actions_against_the_format() {
        let rgb = InputGen::Noise {
            w: 2,
            h: 2,
            fmt: PixelFormat::Rgb8,
            seed: 0,
        };
        let idx = InputGen::Noise {
            w: 2,
            h: 2,
            fmt: PixelFormat::Indexed8,
            seed: 0,
        };
        let rgba = |c| Some(PaintColor::Rgba(c));
        // アルファのない画像：半透明の置き換え・消しゴムは断る。重ねるのは良い
        assert!(
            run(
                rgb.clone(),
                vec![stroke(Tool::Pen, rgba([1, 2, 3, 9]), Blend::Replace, 0)]
            )
            .is_err()
        );
        assert!(
            run(
                rgb.clone(),
                vec![stroke(Tool::Eraser, None, Blend::Replace, 0)]
            )
            .is_err()
        );
        assert!(
            run(
                rgb.clone(),
                vec![stroke(Tool::Pen, rgba([1, 2, 3, 9]), Blend::Over, 0)]
            )
            .is_ok()
        );
        // パレットの画像は番号だけ・置き換えだけ
        assert!(
            run(
                idx.clone(),
                vec![stroke(Tool::Pen, rgba([1, 2, 3, 255]), Blend::Replace, 0)]
            )
            .is_err()
        );
        assert!(
            run(
                idx.clone(),
                vec![stroke(
                    Tool::Pen,
                    Some(PaintColor::Index(1)),
                    Blend::Over,
                    0
                )]
            )
            .is_err()
        );
        assert!(
            run(
                idx.clone(),
                vec![stroke(
                    Tool::Pen,
                    Some(PaintColor::Index(16)),
                    Blend::Replace,
                    0
                )]
            )
            .is_err()
        );
        assert!(
            run(
                idx,
                vec![stroke(
                    Tool::Pen,
                    Some(PaintColor::Index(15)),
                    Blend::Replace,
                    0
                )]
            )
            .is_ok()
        );
        // 画像のフレームは 0 だけ
        assert!(
            run(
                rgb,
                vec![stroke(Tool::Pen, rgba([1, 2, 3, 255]), Blend::Replace, 1)]
            )
            .is_err()
        );
    }

    #[test]
    fn far_points_are_refused_before_drawing() {
        let far = PaintAction::Stroke {
            tool: Tool::Pen,
            color: Some(PaintColor::Rgba([0, 0, 0, 255])),
            blend: Blend::Replace,
            brush: Brush {
                shape: BrushShape::Square,
                size: 255,
            },
            pixel_perfect: false,
            frame: 0,
            points: PointList(vec![
                IPoint {
                    x: -(1 << 31),
                    y: 0,
                },
                IPoint { x: 1 << 31, y: 0 },
            ]),
        };
        let r = run(
            InputGen::Noise {
                w: 2,
                h: 2,
                fmt: PixelFormat::Rgba8,
                seed: 0,
            },
            vec![far],
        );
        assert!(
            matches!(r, Err(EngineError::Invalid(i)) if i.0.iter().any(|x| x.code == IssueCode::TooLarge))
        );
    }

    /// 差分の適用は全体を当て直したものと同じ。先頭が違えば None。
    #[test]
    fn appended_actions_match_a_full_replay() {
        for ex in Actions::examples() {
            let input = ex.inputs[0].generate().unwrap();
            let all = &ex.params.actions;
            for k in 0..=all.len() {
                let prev = PaintParams {
                    actions: all[..k].to_vec(),
                };
                let mut cx = ApplyCx {
                    mon: &NoMonitor,
                    limits: &crate::Limits::default(),
                    seed: 0,
                    rules: crate::doc::RULES,
                    warnings: Vec::new(),
                };
                let prev_out = Actions::apply(&prev, &[&input], &mut cx).unwrap().value;
                let inc = Actions::apply_appended(&prev, &prev_out, &ex.params, &mut cx)
                    .unwrap()
                    .unwrap()
                    .value;
                let full = Actions::apply(&ex.params, &[&input], &mut cx)
                    .unwrap()
                    .value;
                assert_eq!(inc, full, "{} {k}", ex.name);
            }
            if all.len() >= 2 {
                let other = PaintParams {
                    actions: vec![all[1].clone()],
                };
                let mut cx = ApplyCx {
                    mon: &NoMonitor,
                    limits: &crate::Limits::default(),
                    seed: 0,
                    rules: crate::doc::RULES,
                    warnings: Vec::new(),
                };
                assert!(Actions::apply_appended(&other, &input, &ex.params, &mut cx).is_none());
            }
        }
    }
}
