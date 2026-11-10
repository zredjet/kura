//! `meta.anchor_pivot` v1：ピボット（基準点）を 9 点の基準と整数のずれで決める（M14 段 1。相対の指定。画素は変えない）。
//!
//! - ピボットは `(⌊w·ax/2⌋ + offset.x, ⌊h·ay/2⌋ + offset.y)`（w・h は画像、フレームの列はキャンバス。トリムしたフレームの列も
//!   受ける。係数は左・上 0、中央 1、右・下 2）。例：16×16 の `bottom` は (8, 16)、15×9 の `center` は (7, 4)。
//! - 点は `0..=w × 0..=h` の中であること（`meta.set_pivot` と同じ。外なら引数 `offset` の誤りで、黙って詰めない）。大きさの
//!   決まらない入力は実行で確かめる。
//! - 値・メタデータは、計算した点の `meta.set_pivot` と同じ（E51。実装も set_pivot を使う。ほかのメタデータは持ち越す）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::{ANCHOR_NAMES, Anchor, IPoint, anchor_offset};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::meta::set_pivot::{SetPivot, SetPivotParams, inside};
use crate::ops::util::{bad_param, exact_size};
use crate::render::preview::{Preview, scale_point};
use crate::render::region::{Area, FrameTiles, Reach, splice_value};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorPivotParams {
    pub anchor: Anchor,
    /// 基準からのずれ（画素）。
    #[serde(default = "no_offset")]
    pub offset: IPoint,
}

fn no_offset() -> IPoint {
    IPoint { x: 0, y: 0 }
}

impl OpParams for AnchorPivotParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "anchor",
                    ParamKind::Enum {
                        values: ANCHOR_NAMES,
                    },
                    Unit::None,
                    json!("bottom"),
                ),
                spec(
                    "offset",
                    ParamKind::Point,
                    Unit::Px,
                    json!({ "x": 0, "y": 0 }),
                ),
            ],
        }
    }
}

/// 9 点の基準のピボット。
pub struct AnchorPivot;

/// 大きさ `w × h` の画像のピボット（画像の外・あふれるなら None）。
pub(crate) fn pivot_for(p: &AnchorPivotParams, w: u64, h: u64) -> Option<IPoint> {
    let (ax, ay) = p.anchor.factors();
    let base = |len: u64, a: i64| anchor_offset(i64::try_from(len).ok()?, 0, a);
    let pt = IPoint {
        x: base(w, ax)?.checked_add(p.offset.x)?,
        y: base(h, ay)?.checked_add(p.offset.y)?,
    };
    inside(pt, w, h).then_some(pt)
}

fn outside() -> Issues {
    bad_param("offset", "ピボットが画像の外にあります")
}

/// 画像の大きさ（フレームの列はキャンバス）。
fn value_size(v: &Value) -> Result<(u64, u64), EngineError> {
    match v {
        Value::Image(img) => Ok((u64::from(img.px.width()), u64::from(img.px.height()))),
        Value::Frames(fr) => Ok((u64::from(fr.canvas().w), u64::from(fr.canvas().h))),
        Value::Audio(_) => Err(EngineError::Internal(
            "meta.anchor_pivot の入力が音声です".into(),
        )),
    }
}

impl Op for AnchorPivot {
    const ID: &'static str = "meta.anchor_pivot";
    const VERSION: u16 = 1;
    type Params = AnchorPivotParams;

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

    fn infer(
        p: &AnchorPivotParams,
        inputs: &[Shape],
        _cx: &InferCx<'_>,
    ) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let (w, h) = match inputs[0] {
            Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => (w, h),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        // 大きさが決まらなければ実行で確かめる
        if let Some(s) = exact_size(w, h)
            && pivot_for(p, u64::from(s.w), u64::from(s.h)).is_none()
        {
            return Err(outside());
        }
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: 1,
        })
    }

    fn apply(
        p: &AnchorPivotParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let (w, h) = value_size(inputs[0])?;
        let pivot = pivot_for(p, w, h).ok_or_else(|| EngineError::Invalid(outside()))?;
        SetPivot::apply(&SetPivotParams { pivot: Some(pivot) }, inputs, cx)
    }

    fn examples() -> Vec<Example<AnchorPivotParams>> {
        let ex = |name, anchor, (x, y), input| Example {
            name,
            params: AnchorPivotParams {
                anchor,
                offset: IPoint { x, y },
            },
            inputs: vec![input],
        };
        vec![
            // 16×16 の下中央は (8, 16)
            ex(
                "bottom-sprite",
                Anchor::Bottom,
                (0, 0),
                InputGen::NoiseFrames {
                    w: 16,
                    h: 16,
                    fmt: PixelFormat::Rgba8,
                    count: 2,
                    seed: 81,
                },
            ),
            // 15×9 の中央は (7, 4)
            ex(
                "center-odd",
                Anchor::Center,
                (0, 0),
                InputGen::Noise {
                    w: 15,
                    h: 9,
                    fmt: PixelFormat::Rgb8,
                    seed: 82,
                },
            ),
            ex(
                "top-right-with-offset",
                Anchor::TopRight,
                (-2, 3),
                InputGen::Noise {
                    w: 10,
                    h: 6,
                    fmt: PixelFormat::Indexed8,
                    seed: 83,
                },
            ),
            // シートの左の中ほど（格子は残る）
            ex(
                "sheet-left",
                Anchor::Left,
                (0, 0),
                InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 8,
                },
            ),
            ex(
                "trimmed-frames-canvas",
                Anchor::BottomRight,
                (-1, -1),
                InputGen::TrimmedFrames {
                    w: 12,
                    h: 10,
                    fmt: PixelFormat::GrayA8,
                    count: 3,
                    seed: 84,
                },
            ),
        ]
    }

    // 段 10：画素は入力のまま（set_pivot と同じ）
    const REGION: bool = true;

    fn reach(_p: &AnchorPivotParams, _inputs: &[Shape]) -> Reach {
        Reach::Same
    }

    fn param_change(_old: &AnchorPivotParams, _new: &AnchorPivotParams, _inputs: &[Shape]) -> Area {
        // 画素は変わらない（メタデータだけ）
        Area::Empty
    }

    fn effect(_p: &AnchorPivotParams, _inputs: &[Shape]) -> Area {
        Area::Empty
    }

    fn apply_region(
        p: &AnchorPivotParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        // 画素を読まないので、描くタイルを入力から共有した値に `apply` を当てる
        Some(splice_value(inputs[0], tiles, base).and_then(|v| Self::apply(p, &[&v], cx)))
    }

    // 段 10：プレビューの段。画素は描かず、ずれを段に縮めて、段の入力の大きさから点を求め直す（段の入力に収まるように、
    // ずれを詰める。docs/editing.md §12.7）
    fn preview(_p: &AnchorPivotParams) -> Preview {
        Preview::Skip
    }

    fn preview_params(
        p: &AnchorPivotParams,
        level: u8,
        inputs: &[Shape],
        _full: &[Shape],
    ) -> AnchorPivotParams {
        let q = scale_point(p.offset, level);
        let bounds = match inputs.first() {
            Some(Shape::Image { w, h, .. } | Shape::Frames { w, h, .. }) => {
                Some((w.max(), h.max()))
            }
            _ => None,
        };
        let (ax, ay) = p.anchor.factors();
        let fit = |v: i64, len: u64, a: i64| {
            let Some(base) = i64::try_from(len)
                .ok()
                .and_then(|l| anchor_offset(l, 0, a).map(|b| (l, b)))
            else {
                return v;
            };
            v.clamp(-base.1, base.0 - base.1)
        };
        let offset = match bounds {
            Some((w, h)) => IPoint {
                x: fit(q.x, w, ax),
                y: fit(q.y, h, ay),
            },
            None => q,
        };
        AnchorPivotParams {
            anchor: p.anchor,
            offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(anchor: Anchor, x: i64, y: i64) -> AnchorPivotParams {
        AnchorPivotParams {
            anchor,
            offset: IPoint { x, y },
        }
    }

    /// 9 点（16×16・15×9）とずれ、画像の外は None。
    #[test]
    fn pivots_from_anchors() {
        let pts: Vec<(i64, i64)> = Anchor::ALL
            .iter()
            .map(|a| {
                let p = pivot_for(&at(*a, 0, 0), 15, 9).unwrap();
                (p.x, p.y)
            })
            .collect();
        assert_eq!(
            pts,
            [
                (0, 0),
                (7, 0),
                (15, 0),
                (0, 4),
                (7, 4),
                (15, 4),
                (0, 9),
                (7, 9),
                (15, 9)
            ]
        );
        assert_eq!(
            pivot_for(&at(Anchor::Bottom, 0, 0), 16, 16),
            Some(IPoint { x: 8, y: 16 })
        );
        assert_eq!(
            pivot_for(&at(Anchor::TopRight, -2, 3), 10, 6),
            Some(IPoint { x: 8, y: 3 })
        );
        assert_eq!(pivot_for(&at(Anchor::Right, 1, 0), 10, 6), None);
        assert_eq!(pivot_for(&at(Anchor::TopLeft, 0, -1), 10, 6), None);
        assert_eq!(pivot_for(&at(Anchor::Center, i64::MAX, 0), 10, 6), None);
    }

    /// 段の引数は、ずれを縮めて段の入力に収める。
    #[test]
    fn preview_offsets_stay_inside() {
        let stage = [Shape::Image {
            w: crate::shape::Dim::Exact(5),
            h: crate::shape::Dim::Exact(3),
            fmt: PixelFormat::Rgba8,
        }];
        let p = AnchorPivot::preview_params(&at(Anchor::BottomRight, -3, -2), 1, &stage, &stage);
        assert_eq!(p.offset, IPoint { x: -2, y: -1 });
        let p = AnchorPivot::preview_params(&at(Anchor::Center, 40, -40), 2, &stage, &stage);
        assert_eq!(p.offset, IPoint { x: 3, y: -1 });
        assert!(pivot_for(&p, 5, 3).is_some());
    }
}
