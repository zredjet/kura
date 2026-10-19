//! `paint.actions` v1：ペン・消しゴム・塗りつぶしの動作を順に当てる（docs/editing.md §10。規則は `crate::paint`）。
//!
//! 引数は追記だけで増える。前の引数の出力に追記した動作だけを当てる差分の適用（`apply_appended`）は、
//! 全体を当て直したものとビット単位で同じ（E21）。手間（`PaintAction::work`：ストロークの道の点の数 × 筆の画素の数と
//! 画素の集合の大きさ、塗りつぶしは画像の画素の数）の合計は、ノード 1 つあたり `MAX_PAINT_WORK` まで（遠く離れた点・大きな筆・
//! 塗りつぶしの繰り返しで、描くのに時間のかかりすぎる文書を推論の段階で断る。超えた時点で数えるのをやめる）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::{IPoint, IRect};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, Work, expect_inputs, spec,
    unreadable_params,
};
use crate::ops::util::bad_param;
use crate::paint::{
    ActionList, Blend, Brush, BrushShape, Connectivity, PaintAction, PaintColor, PointList, Tool,
    apply_actions, apply_actions_from,
};
use crate::render::region::{Area, Reach, Rects};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Value};

/// `paint.actions` のノード 1 つあたりの描画の手間の上限。
pub const MAX_PAINT_WORK: u64 = 1 << 30;

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaintParams {
    /// 動作の列（64 個ずつの切れ目を共有する。JSON では配列）。
    pub actions: ActionList,
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

/// 推論で動作を照らす相手（入力の形式・フレームの数・大きさの上限）。
struct Target {
    fmt: PixelFormat,
    frames: Option<u64>,
    w: u64,
    h: u64,
}

impl Target {
    fn of(inputs: &[Shape]) -> Result<Self, Issues> {
        expect_inputs(inputs, &Actions::signature())?;
        match inputs[0] {
            Shape::Image { fmt, w, h } => Ok(Self {
                fmt,
                frames: Some(1),
                w: w.max(),
                h: h.max(),
            }),
            Shape::Frames {
                fmt, count, w, h, ..
            } => Ok(Self {
                fmt,
                frames: count.exact(),
                w: w.max(),
                h: h.max(),
            }),
            Shape::Audio { .. } => Err(Issues::none()),
        }
    }

    /// 動作（引数の中の番号と組）を順に確かめ、手間を `work` に足していく。上限を越えた時点で断る。
    fn check<'a>(
        &self,
        actions: impl Iterator<Item = (usize, &'a PaintAction)>,
        mut work: u64,
    ) -> Result<u64, Issues> {
        for (i, a) in actions {
            a.check(self.fmt)
                .map_err(|m| bad_param("actions", format!("動作 {i}：{m}")))?;
            if let Some(n) = self.frames
                && u64::from(a.frame()) >= n
            {
                return Err(bad_param(
                    "actions",
                    format!("動作 {i}：フレーム {} はありません（{n} 枚）", a.frame()),
                ));
            }
            work = work.saturating_add(a.work(self.w, self.h));
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
        Ok(work)
    }

    fn inferred(&self, shape: Shape, work: u64) -> Inferred {
        Inferred {
            shape,
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: work,
        }
    }
}

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
        let target = Target::of(inputs)?;
        let work = target.check(p.actions.iter().enumerate(), 0)?;
        Ok(target.inferred(inputs[0], work))
    }

    /// 動作を順に当てるので、動作ごとの作業領域（[`PaintAction::work_bytes`]。連結の塗りつぶしは集合 2 枚と区間の待ち行列の
    /// 上限）の最大。フレームの列はキャンバスの大きさで求める。差分の適用（足した動作だけ）も、全体の申告に収まる。
    fn work_bytes(p: &PaintParams, _inputs: &[Shape], out: &Shape) -> Work {
        let (w, h) = match *out {
            Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => (w.max(), h.max()),
            Shape::Audio { .. } => return Work::ZERO,
        };
        Work::shared(
            p.actions
                .iter()
                .map(|a| a.work_bytes(w, h))
                .max()
                .unwrap_or(0),
        )
    }

    fn apply(
        p: &PaintParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        Ok(Applied::value(apply_actions(
            inputs[0],
            p.actions.iter(),
            cx.mon,
        )?))
    }

    fn apply_appended(
        prev: &PaintParams,
        prev_out: &Value,
        p: &PaintParams,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        let from = p.actions.appended_since(&prev.actions)?;
        Some(
            cx.check()
                .and_then(|()| {
                    apply_actions_from(prev_out, p.actions.iter_from(from), from, cx.mon)
                })
                .map(Applied::value),
        )
    }

    /// 足した動作だけを読み、前の列の切れ目を共有したまま足す（O(足した分 ＋ 切れ目の数)）。読めない動作の指摘は、全体を
    /// 読んだときと同じ（要素は 1 つずつ同じ型で読み、最初に読めなかったもので止まる）。
    fn parse_appended(
        prev: &PaintParams,
        field: &str,
        items: &[&serde_json::Value],
    ) -> Option<Result<PaintParams, Issues>> {
        if field != "actions" {
            return None;
        }
        let mut actions = prev.actions.clone();
        for item in items {
            match serde_json::from_value::<PaintAction>((*item).clone()) {
                Ok(a) => actions.push(a),
                Err(e) => return Some(Err(unreadable_params(Self::ID, &e))),
            }
        }
        Some(Ok(PaintParams { actions }))
    }

    /// 前の推論の手間に、足した動作の確かめと手間だけを足す（推論は動作ごとに順に足すので、全体を推論したものと同じ）。
    fn infer_appended(
        prev: &PaintParams,
        prev_inferred: &Inferred,
        p: &PaintParams,
        inputs: &[Shape],
        _cx: &InferCx<'_>,
    ) -> Option<Result<Inferred, Issues>> {
        let from = p.actions.appended_since(&prev.actions)?;
        // 前の推論は同じ入力の形のもの（出力の形は入力と同じ）。違えば全体を推論する
        if inputs.first() != Some(&prev_inferred.shape) || prev_inferred.cost > MAX_PAINT_WORK {
            return None;
        }
        Some(Target::of(inputs).and_then(|target| {
            let added = p
                .actions
                .iter_from(from)
                .enumerate()
                .map(|(k, a)| (from + k, a));
            let work = target.check(added, prev_inferred.cost)?;
            Ok(target.inferred(inputs[0], work))
        }))
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
                    actions: ActionList::from(vec![
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
                    ]),
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
                    actions: ActionList::from(vec![pen(
                        [10, 20, 30, 255],
                        Blend::Replace,
                        BrushShape::Square,
                        1,
                        true,
                        pts(&[(0, 0), (1, 0), (1, 1), (2, 1), (2, 2), (9, 3), (3, 9)]),
                    )]),
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
                    actions: ActionList::from(vec![pen(
                        [200, 200, 200, 100],
                        Blend::Over,
                        BrushShape::Circle,
                        4,
                        false,
                        pts(&[(1, 1), (8, 5)]),
                    )]),
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
                    actions: ActionList::from(vec![
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
                    ]),
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
                    actions: ActionList::from(vec![PaintAction::Fill {
                        at: IPoint { x: 4, y: 4 },
                        color: PaintColor::Rgba([0, 255, 0, 255]),
                        tolerance: 40,
                        connectivity: Connectivity::Four,
                        contiguous: false,
                        frame: 0,
                    }]),
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
                    actions: ActionList::from(vec![
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
                    ]),
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
                    actions: ActionList::from(vec![
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
                    ]),
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
                    actions: ActionList::from(vec![PaintAction::Stroke {
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
                    }]),
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

    // 段 10：範囲（塗りつぶしがなければ同じ位置）と、変わった範囲（筆の外接矩形。塗りつぶしはそのフレーム全体）
    fn reach(p: &PaintParams, _inputs: &[Shape]) -> Reach {
        if p.actions
            .iter()
            .any(|a| matches!(a, PaintAction::Fill { .. }))
        {
            Reach::Global
        } else {
            Reach::Same
        }
    }

    fn param_change(old: &PaintParams, new: &PaintParams, inputs: &[Shape]) -> Area {
        let clip = canvas_clip(inputs);
        let (a, b) = (&old.actions, &new.actions);
        // 共通の先頭と共通の末尾を除いた、違う動作の新旧の範囲の和。追記（前の列を共有する）なら先頭の比べを飛ばす
        // （`appended_since` は共有した切れ目を比べないので、5 万本の文書でも追記ごとに O(足した分 ＋ 切れ目の数)）
        let head = b.appended_since(a).unwrap_or_else(|| {
            (0..a.len().min(b.len()))
                .take_while(|&i| a.get(i) == b.get(i))
                .count()
        });
        let (a_left, b_left) = (a.len() - head, b.len() - head);
        let tail = (1..=a_left.min(b_left))
            .take_while(|&k| a.get(a.len() - k) == b.get(b.len() - k))
            .count();
        let mut area = Area::Empty;
        let mut frames = std::collections::BTreeSet::new();
        for act in (head..a.len() - tail)
            .filter_map(|i| a.get(i))
            .chain((head..b.len() - tail).filter_map(|i| b.get(i)))
        {
            area.union_with(&action_area(act, clip));
            frames.insert(act.frame());
        }
        // 後ろにある同じ動作のうち、ストロークは画素ごとの関数なので広げない。塗りつぶしは中身で範囲が決まるので、
        // 変わったフレームのものはそのフレーム全体
        for act in b.iter_from(b.len() - tail) {
            if let PaintAction::Fill { frame, .. } = act
                && frames.contains(frame)
            {
                area.union_with(&Area::whole_frame(*frame));
            }
        }
        area
    }

    fn effect(p: &PaintParams, inputs: &[Shape]) -> Area {
        let clip = canvas_clip(inputs);
        p.actions.iter().fold(Area::Empty, |mut area, act| {
            area.union_with(&action_area(act, clip));
            area
        })
    }
}

/// キャンバスの矩形（筆の範囲を切る。大きさの上限で、保守的）。
fn canvas_clip(inputs: &[Shape]) -> IRect {
    let (w, h) = match inputs.first() {
        Some(Shape::Image { w, h, .. } | Shape::Frames { w, h, .. }) => (w.max(), h.max()),
        _ => (u64::from(u32::MAX), u64::from(u32::MAX)),
    };
    IRect::new(
        0,
        0,
        u32::try_from(w).unwrap_or(u32::MAX),
        u32::try_from(h).unwrap_or(u32::MAX),
    )
}

/// ストロークの範囲を区切る数の上限（`Rects` の上限 64 の内）と、1 区切りの点の数の下限。
const STROKE_PIECES: usize = 32;
const STROKE_PIECE_MIN: usize = 8;

/// 動作 1 つが変えうる範囲（キャンバスの座標）。ストロークは点の列を区切り（境の点は両方に入れる）、区切りごとの
/// `stroke_bounds` の和：道は隣り合う 2 点の外接矩形の中を通り（ピクセルパーフェクトは点を除くだけ）、押す画素は道の点に
/// 筆の広がりを足した中にあるので、和は押した画素をすべて覆う（1 つの外接矩形より締まり、後ろの範囲の描画のタイルが
/// 減る）。塗りつぶしはそのフレーム全体。
fn action_area(act: &PaintAction, clip: IRect) -> Area {
    match act {
        PaintAction::Stroke {
            brush,
            points,
            frame,
            ..
        } => {
            let pts = &points.0;
            let chunk = pts.len().div_ceil(STROKE_PIECES).max(STROKE_PIECE_MIN);
            let mut rects = Rects::new();
            let mut start = 0;
            while start < pts.len() {
                let end = start.saturating_add(chunk + 1).min(pts.len());
                if let Some(r) = crate::paint::stroke::stroke_bounds(&pts[start..end], *brush, clip)
                {
                    rects.push(r);
                }
                if end == pts.len() {
                    break;
                }
                start = end - 1;
            }
            Area::frame(*frame, rects)
        }
        PaintAction::Fill { frame, .. } => Area::whole_frame(*frame),
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
            serde_json::to_value(PaintParams {
                actions: actions.into(),
            })
            .unwrap(),
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

    /// 作業領域（`work_bytes`）：塗りつぶしは同じ色の画素の集合とタイルごとの語（集める間は両方）、連結なら塗った画素の
    /// 集合と区間の待ち行列（上限まで先に確保する。1px の市松を 8 連結で塗ると、区間が 1 画素ずつになって画素の数に比例して
    /// 伸びる）。ストロークは画素の集合・筆の形・点の写し。動作は順に当てるので、合計ではなく最大。フレームの列はキャンバスの
    /// 大きさで求める。下の式は fill.rs・stroke.rs の確保の形（凍結）。
    #[test]
    fn work_covers_the_fill_and_stroke_buffers() {
        use crate::shape::{Dim, Fullness};
        let (w, h) = (4096u64, 4096u64);
        let img = Shape::Image {
            w: Dim::Exact(w),
            h: Dim::AtMost(h),
            fmt: PixelFormat::Gray8,
        };
        let mask = w.div_ceil(64) * 8 * h;
        let queue = (w * h / 2 + 2 * h + 65_536) * 12;
        let fill = |contiguous| PaintAction::Fill {
            at: IPoint { x: 0, y: 0 },
            color: PaintColor::Rgba([1, 2, 3, 255]),
            tolerance: 0,
            connectivity: Connectivity::Eight,
            contiguous,
            frame: 0,
        };
        let big_stroke = PaintAction::Stroke {
            tool: Tool::Pen,
            color: Some(PaintColor::Rgba([1, 2, 3, 255])),
            blend: Blend::Replace,
            brush: Brush {
                shape: BrushShape::Square,
                size: 255,
            },
            pixel_perfect: false,
            frame: 0,
            points: PointList(vec![IPoint { x: 0, y: 0 }; 1000]),
        };
        let work = |actions: Vec<PaintAction>, shape: &Shape| {
            let p = PaintParams {
                actions: actions.into(),
            };
            Actions::work_bytes(&p, &[*shape], shape)
        };
        let contiguous = work(vec![fill(true)], &img);
        assert_eq!(
            contiguous.per_task, 0,
            "動作は順に当てる（並列の仕事ごとの分はない）"
        );
        assert!(contiguous.total(3) >= 2 * mask + queue, "{contiguous:?}");
        assert!(work(vec![fill(false)], &img).total(3) >= 2 * mask);
        assert!(work(vec![big_stroke.clone()], &img).total(3) >= mask + (255 * 255 + 1000) * 16);
        // 合計ではなく最大
        assert_eq!(
            work(vec![fill(true), big_stroke.clone(), fill(true)], &img),
            contiguous
        );
        assert_eq!(work(vec![], &img), Work::ZERO);
        // フレームの列はキャンバスの大きさ（トリムしたフレームもキャンバスに収まる）
        let frames = Shape::Frames {
            count: Dim::Exact(3),
            w: Dim::Exact(w),
            h: Dim::Exact(h),
            fmt: PixelFormat::Rgba8,
            full: Fullness::Trimmed,
        };
        assert_eq!(work(vec![fill(true)], &frames), contiguous);
    }

    /// ストロークの範囲（区切りの外接矩形の和）は、長い点の列・ピクセルパーフェクト・大きな筆・画像の外にはみ出す道でも、
    /// 押した画素をすべて覆う（区切りの境の点を両方に入れるため）。長い道では 1 つの外接矩形より締まる。
    #[test]
    fn stroke_area_covers_long_strokes() {
        let input = InputGen::Noise {
            w: 300,
            h: 200,
            fmt: PixelFormat::Rgba8,
            seed: 7,
        };
        let base = input.generate().unwrap();
        let clip = IRect::new(0, 0, 300, 200);
        let mut split = 0;
        for case in 0..42u64 {
            let n = [1usize, 2, 9, 17, 64, 257, 600][case as usize % 7];
            // 画像の外（-20〜）にもはみ出す道
            let points: Vec<IPoint> =
                crate::testing::bench::walk_stroke(case, n, 340, 240, 4 + case as u32 % 20)
                    .into_iter()
                    .map(|p| IPoint {
                        x: p.x - 20,
                        y: p.y - 20,
                    })
                    .collect();
            let size = [1u8, 3, 8, 32][case as usize % 4];
            let act = PaintAction::Stroke {
                tool: Tool::Pen,
                color: Some(PaintColor::Rgba([250, 10, 20, 255])),
                blend: Blend::Replace,
                brush: Brush {
                    shape: if case % 3 == 0 {
                        BrushShape::Square
                    } else {
                        BrushShape::Circle
                    },
                    size,
                },
                pixel_perfect: size == 1 && case % 2 == 0,
                frame: 0,
                points: PointList(points.clone()),
            };
            let out = run(input.clone(), vec![act.clone()]).unwrap();
            let area = action_area(&act, clip);
            crate::testing::region::check_contains(&base, &out, &area)
                .unwrap_or_else(|m| panic!("{case}（{n} 点・{size}px）: {m}"));
            // 区切りの矩形は、1 つの外接矩形の中にある
            let whole = crate::paint::stroke::stroke_bounds(&points, act_brush(&act), clip);
            if let Area::Pixels(map) = &area {
                let rects = map[&0].rects();
                let inside = |r: &IRect, w: &IRect| {
                    r.x >= w.x && r.y >= w.y && r.right() <= w.right() && r.bottom() <= w.bottom()
                };
                assert!(
                    rects.iter().all(|r| whole.is_some_and(|w| inside(r, &w))),
                    "{case}"
                );
                split += usize::from(rects.len() > 1);
            }
        }
        assert!(split >= 10, "{split}");
    }

    fn act_brush(act: &PaintAction) -> Brush {
        match act {
            PaintAction::Stroke { brush, .. } => *brush,
            PaintAction::Fill { .. } => unreachable!(),
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

    fn dyn_op() -> std::sync::Arc<dyn crate::op::DynOp> {
        crate::ops::registry()
            .unwrap()
            .current()
            .into_iter()
            .find(|o| o.id() == Actions::ID)
            .unwrap()
    }

    fn typed(p: &crate::op::AnyParams) -> &PaintParams {
        p.downcast_ref::<PaintParams>().unwrap()
    }

    /// 読めない動作の JSON（種類を知らない・点の列の書き方が違う・項目が足りない・知らない項目）。
    fn unreadable_items() -> Vec<serde_json::Value> {
        vec![
            json!({ "type": "smudge", "frame": 0 }),
            json!({ "type": "stroke", "tool": "pen", "color": { "rgba": [1, 2, 3, 255] }, "blend": "replace",
                    "brush": { "shape": "square", "size": 1 }, "pixelPerfect": false, "frame": 0, "points": "!!" }),
            json!({ "type": "fill", "at": { "x": 0, "y": 0 } }),
            json!({ "type": "fill", "at": { "x": 0, "y": 0 }, "color": { "index": 1 }, "tolerance": 0,
                    "connectivity": "four", "contiguous": true, "frame": 0, "extra": 1 }),
            json!(7),
        ]
    }

    /// 追記の読み込み：例の動作を切れ目ごとに前半と後半に分け、`parse_appended(parse(前半), 後半)` が `parse(全体)` と同じ
    /// （型としても JSON としても）。後半に読めない要素があれば、全体を読んだときと同じ指摘。ほかの引数の名前は None。
    #[test]
    fn parse_appended_equals_parse() {
        let op = dyn_op();
        let mut checked = 0;
        for ex in op.examples().unwrap() {
            let all = ex.params["actions"].as_array().unwrap().clone();
            for k in 0..=all.len() {
                let prev = op.parse(&json!({ "actions": all[..k] })).unwrap();
                let items: Vec<&serde_json::Value> = all[k..].iter().collect();
                let got = op
                    .parse_appended(prev.as_ref(), "actions", &items)
                    .expect("差分で読める")
                    .unwrap();
                let want = op.parse(&ex.params).unwrap();
                assert_eq!(typed(got.as_ref()), typed(want.as_ref()), "{} {k}", ex.name);
                assert_eq!(
                    op.params_json(got.as_ref()).unwrap(),
                    op.params_json(want.as_ref()).unwrap()
                );
                // 読めない要素：全体を読んだときと同じ指摘
                for bad in unreadable_items() {
                    let mut with_bad: Vec<serde_json::Value> = all[k..].to_vec();
                    with_bad.insert(with_bad.len() / 2, bad.clone());
                    let mut whole = all[..k].to_vec();
                    whole.extend(with_bad.iter().cloned());
                    let want = op.parse(&json!({ "actions": whole })).unwrap_err();
                    let refs: Vec<&serde_json::Value> = with_bad.iter().collect();
                    let got = op
                        .parse_appended(prev.as_ref(), "actions", &refs)
                        .expect("差分で読む")
                        .unwrap_err();
                    assert_eq!(got, want, "{} {k} {bad}", ex.name);
                    checked += 1;
                }
                assert!(op.parse_appended(prev.as_ref(), "points", &items).is_none());
            }
        }
        assert!(checked > 20);
    }

    /// 追記の推論：`infer_appended(前半, infer(前半), 全体)` が `infer(全体)` と同じ（形・手間・指摘）。断る動作（筆の大きさ 0・
    /// ないフレーム・手間の上限を越える筆）を後半に入れても同じ指摘。前半への追記でなければ None。
    #[test]
    fn infer_appended_equals_infer() {
        let op = dyn_op();
        let limits = crate::Limits::default();
        let cx = InferCx { limits: &limits };
        let far = |x: i64| PaintAction::Stroke {
            tool: Tool::Pen,
            color: Some(PaintColor::Rgba([0, 0, 0, 255])),
            blend: Blend::Replace,
            brush: Brush {
                shape: BrushShape::Square,
                size: 255,
            },
            pixel_perfect: false,
            frame: 0,
            points: PointList(vec![IPoint { x: 0, y: 0 }, IPoint { x, y: 0 }]),
        };
        let mut zero = stroke(
            Tool::Pen,
            Some(PaintColor::Rgba([1, 2, 3, 255])),
            Blend::Replace,
            0,
        );
        if let PaintAction::Stroke { brush, .. } = &mut zero {
            brush.size = 0;
        }
        let extra = [
            None,
            Some(zero),
            Some(stroke(
                Tool::Pen,
                Some(PaintColor::Rgba([1, 2, 3, 255])),
                Blend::Replace,
                9,
            )),
            // 1 本では上限の内、2 本で越える
            Some(far(9_900)),
        ];
        let mut compared = 0;
        for ex in Actions::examples() {
            let shape = ex.inputs[0].generate().unwrap().shape();
            let all: Vec<PaintAction> = ex.params.actions.iter().cloned().collect();
            for k in 0..=all.len() {
                let prev = PaintParams {
                    actions: all[..k].to_vec().into(),
                };
                let Ok(prev_inferred) = Actions::infer(&prev, &[shape], &cx) else {
                    continue;
                };
                for add in &extra {
                    let mut actions = all.clone();
                    if let Some(a) = add {
                        actions.insert(k + (all.len() - k) / 2, a.clone());
                        if matches!(a, PaintAction::Stroke { brush, .. } if brush.size == 255) {
                            actions.push(a.clone());
                        }
                    }
                    let p = PaintParams {
                        actions: actions.into(),
                    };
                    let want = Actions::infer(&p, &[shape], &cx);
                    let got = op
                        .infer_appended(&prev, &prev_inferred, &p, &[shape], &cx)
                        .expect("差分で推論できる");
                    assert_eq!(got, want, "{} {k}", ex.name);
                    compared += 1;
                }
                // 前半への追記でなければ None
                if k >= 1 {
                    let mut other = all[..k].to_vec();
                    other[0] = far(3);
                    let p = PaintParams {
                        actions: other.into(),
                    };
                    assert!(
                        op.infer_appended(&prev, &prev_inferred, &p, &[shape], &cx)
                            .is_none()
                    );
                }
            }
        }
        assert!(compared > 40);
    }

    /// 差分の適用は全体を当て直したものと同じ。先頭が違えば None。
    #[test]
    fn appended_actions_match_a_full_replay() {
        for ex in Actions::examples() {
            let input = ex.inputs[0].generate().unwrap();
            let all = ex.params.actions.to_vec();
            for k in 0..=all.len() {
                let prev = PaintParams {
                    actions: all[..k].to_vec().into(),
                };
                let limits = crate::Limits::default();
                let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, crate::doc::RULES);
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
                    actions: vec![all[1].clone()].into(),
                };
                let limits = crate::Limits::default();
                let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, crate::doc::RULES);
                assert!(Actions::apply_appended(&other, &input, &ex.params, &mut cx).is_none());
            }
        }
    }
}
