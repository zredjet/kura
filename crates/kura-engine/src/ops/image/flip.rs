//! `image.flip` v1：左右・上下の反転（画素は動かすだけなので、ビット単位で正確）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::{IRect, Size};
use crate::mapping::{IntAffine, SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::render::preview::Preview;
use crate::render::region::{FrameTiles, Reach, Tiles, base_image, exact_image_size, pick_of};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Image, PixelFormat, Raster, Value};

/// 反転の向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlipAxis {
    /// 左右。
    Horizontal,
    /// 上下。
    Vertical,
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlipParams {
    pub axis: FlipAxis,
}

impl OpParams for FlipParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "axis",
                ParamKind::Enum {
                    values: &["horizontal", "vertical"],
                },
                Unit::None,
                json!("horizontal"),
            )],
        }
    }
}

/// 反転。
pub struct Flip;

fn space(axis: FlipAxis, w: u32, h: u32) -> SpaceMap {
    let m = match axis {
        FlipAxis::Horizontal => IntAffine::flip_h(w),
        FlipAxis::Vertical => IntAffine::flip_v(h),
    };
    SpaceMap::Affine {
        m,
        out: Size { w, h },
    }
}

/// 反転した画素を作る（フレームの列からも使う）。
pub fn flip_raster(px: &Raster, axis: FlipAxis, cx: &ApplyCx<'_>) -> Result<Raster, EngineError> {
    px.derive(px.width(), px.height(), cx.mon, flip_tile(px, axis))
}

/// 反転した画素の、`pick` のタイルだけを描いたもの（段 10。範囲の描画。ほかは `base` のタイルか無効なタイル）。
pub fn flip_raster_picked(
    px: &Raster,
    axis: FlipAxis,
    cx: &ApplyCx<'_>,
    pick: &Tiles,
    base: Option<&Raster>,
) -> Result<Raster, EngineError> {
    px.derive_picked(
        px.width(),
        px.height(),
        cx.mon,
        pick,
        base,
        flip_tile(px, axis),
    )
}

/// 出力のタイル 1 枚を描く閉包（全体の描画と範囲の描画が同じものを使う）。
fn flip_tile(
    px: &Raster,
    axis: FlipAxis,
) -> impl Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send + '_ {
    let (w, h, bpp) = (px.width(), px.height(), px.bpp());
    move |t, out| {
        let src = match axis {
            FlipAxis::Horizontal => IRect::new(i64::from(w) - t.right(), t.y, t.w, t.h),
            FlipAxis::Vertical => IRect::new(t.x, i64::from(h) - t.bottom(), t.w, t.h),
        };
        px.read_rect(src, out)?;
        let row_len = t.w as usize * bpp;
        match axis {
            FlipAxis::Horizontal => {
                for row in out.chunks_exact_mut(row_len) {
                    // 画素の順を逆にする（画素の中のバイトの順は保つ）
                    let n = t.w as usize;
                    for i in 0..n / 2 {
                        let (a, b) = (i * bpp, (n - 1 - i) * bpp);
                        for k in 0..bpp {
                            row.swap(a + k, b + k);
                        }
                    }
                }
            }
            FlipAxis::Vertical => {
                let rows = t.h as usize;
                for i in 0..rows / 2 {
                    let (top, bottom) = out.split_at_mut((rows - 1 - i) * row_len);
                    top[i * row_len..(i + 1) * row_len].swap_with_slice(&mut bottom[..row_len]);
                }
            }
        }
        Ok(())
    }
}

impl Op for Flip {
    const ID: &'static str = "image.flip";
    const VERSION: u16 = 1;
    type Params = FlipParams;

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

    fn infer(p: &FlipParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let shape = inputs[0];
        let Shape::Image { w, h, .. } = shape else {
            return Err(Issues::none());
        };
        // 大きさが決まっていなければ、写し方は実行で決まる
        let space = match (w.exact(), h.exact()) {
            (Some(w), Some(h)) => space(p.axis, w as u32, h as u32),
            _ => SpaceMap::Unknown,
        };
        Ok(Inferred {
            shape,
            space,
            time: TimeMap::Identity,
            cost: w.max() * h.max(),
        })
    }

    fn apply(
        p: &FlipParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        let Value::Image(img) = inputs[0] else {
            return Err(EngineError::Internal(
                "image.flip の入力が画像ではありません".into(),
            ));
        };
        let px = flip_raster(&img.px, p.axis, cx)?;
        let actual = space(p.axis, img.px.width(), img.px.height());
        Ok(Applied {
            value: Value::Image(Arc::new(Image {
                px,
                meta: img.meta.clone(),
            })),
            space: Some(actual),
            time: None,
        })
    }

    fn examples() -> Vec<Example<FlipParams>> {
        vec![
            Example {
                name: "horizontal-rgba-across-tiles",
                params: FlipParams {
                    axis: FlipAxis::Horizontal,
                },
                inputs: vec![InputGen::Noise {
                    w: 517,
                    h: 300,
                    fmt: PixelFormat::Rgba8,
                    seed: 5,
                }],
            },
            Example {
                name: "vertical-rgb16-odd",
                params: FlipParams {
                    axis: FlipAxis::Vertical,
                },
                inputs: vec![InputGen::Noise {
                    w: 31,
                    h: 263,
                    fmt: PixelFormat::Rgb16,
                    seed: 6,
                }],
            },
            Example {
                name: "sheet-keeps-grid",
                params: FlipParams {
                    axis: FlipAxis::Horizontal,
                },
                inputs: vec![InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 16,
                }],
            },
            Example {
                name: "indexed-frames",
                params: FlipParams {
                    axis: FlipAxis::Vertical,
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 7,
                    h: 5,
                    fmt: PixelFormat::Indexed8,
                    count: 2,
                    seed: 7,
                }],
            },
        ]
    }

    // 段 10：範囲（鏡映の逆写し）と範囲の描画
    const REGION: bool = true;

    fn reach(p: &FlipParams, inputs: &[Shape]) -> Reach {
        let Some(size) = inputs.first().and_then(exact_image_size) else {
            return Reach::Global;
        };
        match space(p.axis, size.w, size.h) {
            SpaceMap::Affine { m, out } => Reach::Affine {
                m,
                in_size: size,
                out_size: out,
            },
            _ => Reach::Global,
        }
    }

    fn apply_region(
        p: &FlipParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        Some((|| {
            let Value::Image(img) = inputs[0] else {
                return Err(EngineError::Internal(
                    "image.flip の入力が画像ではありません".into(),
                ));
            };
            let (w, h) = (img.px.width(), img.px.height());
            let pick = pick_of(tiles, 0, w, h);
            let px = flip_raster_picked(&img.px, p.axis, cx, &pick, base_image(base)?)?;
            Ok(Applied {
                value: Value::Image(Arc::new(Image {
                    px,
                    meta: img.meta.clone(),
                })),
                space: Some(space(p.axis, w, h)),
                time: None,
            })
        })())
    }

    // 段 10：プレビューの段（引数はそのまま）
    fn preview(_p: &FlipParams) -> Preview {
        Preview::Scaled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;
    use proptest::prelude::*;

    proptest! {
        /// 2 回反転すると元に戻る（E13）。メタデータも戻る。
        #[test]
        fn flip_twice_is_identity(w in 1u32..600, h in 1u32..40, vertical in any::<bool>(), seed in any::<u64>()) {
            let axis = if vertical { FlipAxis::Vertical } else { FlipAxis::Horizontal };
            let params = serde_json::to_value(FlipParams { axis }).unwrap();
            let input = InputGen::Noise { w, h, fmt: PixelFormat::Rgb8, seed }.generate().unwrap();
            let once = render_value(Flip::ID, 1, params.clone(), input.clone()).unwrap();
            let twice = render_value(Flip::ID, 1, params, once).unwrap();
            prop_assert_eq!(twice, input);
        }
    }

    #[test]
    fn mirrors_each_pixel() {
        for (axis, vertical) in [(FlipAxis::Horizontal, false), (FlipAxis::Vertical, true)] {
            let input = InputGen::Noise {
                w: 300,
                h: 270,
                fmt: PixelFormat::GrayA8,
                seed: 8,
            }
            .generate()
            .unwrap();
            let out = render_value(
                Flip::ID,
                1,
                serde_json::to_value(FlipParams { axis }).unwrap(),
                input.clone(),
            )
            .unwrap();
            let (Value::Image(src), Value::Image(dst)) = (&input, &out) else {
                panic!()
            };
            for y in 0..270 {
                for x in 0..300 {
                    let (sx, sy) = if vertical { (x, 269 - y) } else { (299 - x, y) };
                    assert_eq!(
                        dst.px.pixel(x, y),
                        src.px.pixel(sx, sy),
                        "{axis:?} ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn sheet_round_trips_with_meta() {
        let params = serde_json::to_value(FlipParams {
            axis: FlipAxis::Horizontal,
        })
        .unwrap();
        let input = InputGen::Sheet {
            cols: 3,
            rows: 2,
            cell: 16,
        }
        .generate()
        .unwrap();
        let once = render_value(Flip::ID, 1, params.clone(), input.clone()).unwrap();
        assert_ne!(once, input);
        assert_eq!(render_value(Flip::ID, 1, params, once).unwrap(), input);
    }
}
