//! `image.crop` v1：矩形で切り抜く（画素は動かすだけなので、ビット単位で正確）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::{IRect, Size};
use crate::mapping::{IntAffine, SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Image, PixelFormat, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropParams {
    /// 切り抜く矩形（入力の中に収まること）。
    pub rect: IRect,
}

impl OpParams for CropParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "rect",
                ParamKind::Rect,
                Unit::Px,
                json!({ "x": 0, "y": 0, "w": 1, "h": 1 }),
            )],
        }
    }
}

/// 切り抜き。
pub struct Crop;

fn out_of_range() -> Issues {
    Issues::one(
        Issue::error(IssueCode::OutOfRange, "切り抜く範囲が画像の外に出ています").param("rect"),
    )
}

impl Op for Crop {
    const ID: &'static str = "image.crop";
    const VERSION: u16 = 1;
    type Params = CropParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::PerFrame,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &CropParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Image { w, h, fmt } = inputs[0] else {
            return Err(out_of_range());
        };
        let r = p.rect;
        if r.w == 0 || r.h == 0 || r.x < 0 || r.y < 0 {
            return Err(out_of_range());
        }
        // 大きさが決まっていれば、ここで範囲を確かめる（決まっていなければ実行のときに確かめる）
        if let (Some(w), Some(h)) = (w.exact(), h.exact())
            && (r.right() as u64 > w || r.bottom() as u64 > h)
        {
            return Err(out_of_range());
        }
        if r.right() as u64 > w.max() || r.bottom() as u64 > h.max() {
            return Err(out_of_range());
        }
        Ok(Inferred {
            shape: Shape::Image {
                w: Dim::Exact(u64::from(r.w)),
                h: Dim::Exact(u64::from(r.h)),
                fmt,
            },
            space: SpaceMap::Affine {
                m: IntAffine::translate(-r.x, -r.y),
                out: Size { w: r.w, h: r.h },
            },
            time: TimeMap::Identity,
            cost: u64::from(r.w) * u64::from(r.h),
        })
    }

    fn apply(
        p: &CropParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        let Value::Image(img) = inputs[0] else {
            return Err(EngineError::Internal(
                "image.crop の入力が画像ではありません".into(),
            ));
        };
        let r = p.rect;
        if !r.inside(img.px.width(), img.px.height()) {
            return Err(EngineError::Invalid(out_of_range()));
        }
        let px = img.px.derive(r.w, r.h, cx.mon, |t, out| {
            img.px
                .read_rect(IRect::new(r.x + t.x, r.y + t.y, t.w, t.h), out)
        })?;
        Ok(Applied::value(Value::Image(Arc::new(Image {
            px,
            meta: img.meta.clone(),
        }))))
    }

    fn examples() -> Vec<Example<CropParams>> {
        vec![
            Example {
                name: "rgba-across-tiles",
                params: CropParams {
                    rect: IRect::new(10, 20, 300, 260),
                },
                inputs: vec![InputGen::Noise {
                    w: 600,
                    h: 400,
                    fmt: PixelFormat::Rgba8,
                    seed: 1,
                }],
            },
            Example {
                name: "indexed-keeps-palette",
                params: CropParams {
                    rect: IRect::new(5, 3, 10, 7),
                },
                inputs: vec![InputGen::Noise {
                    w: 40,
                    h: 30,
                    fmt: PixelFormat::Indexed8,
                    seed: 2,
                }],
            },
            Example {
                name: "sheet-moves-grid-and-pivot",
                params: CropParams {
                    rect: IRect::new(8, 0, 48, 32),
                },
                inputs: vec![InputGen::Sheet {
                    cols: 4,
                    rows: 2,
                    cell: 16,
                }],
            },
            Example {
                name: "frames-per-frame",
                params: CropParams {
                    rect: IRect::new(1, 1, 6, 5),
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 9,
                    h: 8,
                    fmt: PixelFormat::GrayA16,
                    count: 3,
                    seed: 4,
                }],
            },
            Example {
                name: "whole-image",
                params: CropParams {
                    rect: IRect::new(0, 0, 33, 17),
                },
                inputs: vec![InputGen::Checker {
                    w: 33,
                    h: 17,
                    fmt: PixelFormat::Rgb8,
                    cell: 4,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_single;
    use proptest::prelude::*;

    #[test]
    fn copies_exactly_the_pixels_of_the_rect() {
        let input = InputGen::Noise {
            w: 600,
            h: 300,
            fmt: PixelFormat::Rgb16,
            seed: 3,
        }
        .generate()
        .unwrap();
        let rect = IRect::new(250, 7, 300, 280);
        let out = crate::testing::render_value(
            Crop::ID,
            1,
            serde_json::to_value(CropParams { rect }).unwrap(),
            input.clone(),
        )
        .unwrap();
        let (Value::Image(src), Value::Image(dst)) = (&input, &out) else {
            panic!()
        };
        assert_eq!((dst.px.width(), dst.px.height()), (300, 280));
        for y in 0..280 {
            for x in 0..300 {
                assert_eq!(
                    dst.px.pixel(x, y),
                    src.px.pixel(x + 250, y + 7),
                    "({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn rejects_rects_outside() {
        let input = InputGen::Noise {
            w: 10,
            h: 10,
            fmt: PixelFormat::Gray8,
            seed: 0,
        };
        for rect in [
            IRect::new(5, 5, 6, 1),
            IRect::new(-1, 0, 2, 2),
            IRect::new(0, 0, 0, 1),
        ] {
            assert!(
                render_single(
                    Crop::ID,
                    1,
                    serde_json::to_value(CropParams { rect }).unwrap(),
                    std::slice::from_ref(&input)
                )
                .is_err()
            );
        }
    }

    proptest! {
        /// 切り抜きの合成：r1 で切ってから r2 で切るのは、合成した 1 回の切り抜きと同じ。
        #[test]
        fn crop_composes(x1 in 0i64..20, y1 in 0i64..20, w1 in 1u32..30, h1 in 1u32..30, x2 in 0i64..10, y2 in 0i64..10, w2 in 1u32..10, h2 in 1u32..10) {
            prop_assume!(x2 + i64::from(w2) <= i64::from(w1) && y2 + i64::from(h2) <= i64::from(h1));
            let input = InputGen::Noise { w: 50, h: 50, fmt: PixelFormat::Rgba8, seed: 9 };
            let r1 = IRect::new(x1, y1, w1, h1);
            let r2 = IRect::new(x2, y2, w2, h2);
            let once = render_single(Crop::ID, 1, serde_json::to_value(CropParams { rect: IRect::new(x1 + x2, y1 + y2, w2, h2) }).unwrap(), std::slice::from_ref(&input)).unwrap();
            let first = crate::testing::render_value(Crop::ID, 1, serde_json::to_value(CropParams { rect: r1 }).unwrap(), input.generate().unwrap()).unwrap();
            let twice = crate::testing::render_value(Crop::ID, 1, serde_json::to_value(CropParams { rect: r2 }).unwrap(), first).unwrap();
            prop_assert_eq!(once, twice);
        }
    }
}
