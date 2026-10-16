//! `image.rotate90` v1：90° 単位の回転（画素は動かすだけなので、ビット単位で正確）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::{IRect, Size};
use crate::mapping::{IntAffine, SpaceMap, TimeMap};
use crate::monitor::Monitor;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::image_of;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Image, PixelFormat, Raster, Value};

/// 回す向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Turn {
    /// 時計回りに 90°。
    Cw,
    /// 反時計回りに 90°。
    Ccw,
    /// 180°。
    Half,
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotate90Params {
    pub turn: Turn,
}

impl OpParams for Rotate90Params {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "turn",
                ParamKind::Enum {
                    values: &["cw", "ccw", "half"],
                },
                Unit::None,
                json!("cw"),
            )],
        }
    }
}

/// 90° 単位の回転。
pub struct Rotate90;

fn out_size(turn: Turn, w: u32, h: u32) -> (u32, u32) {
    match turn {
        Turn::Half => (w, h),
        Turn::Cw | Turn::Ccw => (h, w),
    }
}

fn space(turn: Turn, w: u32, h: u32) -> SpaceMap {
    let (ow, oh) = out_size(turn, w, h);
    let m = match turn {
        Turn::Cw => IntAffine::rotate_cw(h),
        Turn::Ccw => IntAffine::rotate_ccw(w),
        Turn::Half => IntAffine {
            a: -1,
            b: 0,
            c: i64::from(w),
            d: 0,
            e: -1,
            f: i64::from(h),
        },
    };
    SpaceMap::Affine {
        m,
        out: Size { w: ow, h: oh },
    }
}

/// 回した画素（フレームの列からも使う）。
pub fn rotate_raster(px: &Raster, turn: Turn, mon: &dyn Monitor) -> Result<Raster, EngineError> {
    let (w, h, bpp) = (px.width(), px.height(), px.bpp());
    let (ow, oh) = out_size(turn, w, h);
    px.derive(ow, oh, mon, |t, out| {
        // 出力の (x', y') の元の画素：cw は (y', h−1−x')、ccw は (w−1−y', x')、half は (w−1−x', h−1−y')
        let src = match turn {
            Turn::Cw => IRect::new(t.y, i64::from(h) - t.right(), t.h, t.w),
            Turn::Ccw => IRect::new(i64::from(w) - t.bottom(), t.x, t.h, t.w),
            Turn::Half => IRect::new(
                i64::from(w) - t.right(),
                i64::from(h) - t.bottom(),
                t.w,
                t.h,
            ),
        };
        let mut buf = vec![0u8; src.w as usize * src.h as usize * bpp];
        px.read_rect(src, &mut buf)?;
        let src_row = src.w as usize * bpp;
        let out_row = t.w as usize * bpp;
        for oy in 0..t.h as usize {
            for ox in 0..t.w as usize {
                // 読んだ矩形の中の座標
                let (sx, sy) = match turn {
                    Turn::Cw => (oy, t.w as usize - 1 - ox),
                    Turn::Ccw => (t.h as usize - 1 - oy, ox),
                    Turn::Half => (t.w as usize - 1 - ox, t.h as usize - 1 - oy),
                };
                let s = sy * src_row + sx * bpp;
                let d = oy * out_row + ox * bpp;
                out[d..d + bpp].copy_from_slice(&buf[s..s + bpp]);
            }
        }
        Ok(())
    })
}

impl Op for Rotate90 {
    const ID: &'static str = "image.rotate90";
    const VERSION: u16 = 1;
    type Params = Rotate90Params;

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

    fn infer(p: &Rotate90Params, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Image { w, h, fmt } = inputs[0] else {
            return Err(Issues::none());
        };
        let (ow, oh) = match p.turn {
            Turn::Half => (w, h),
            Turn::Cw | Turn::Ccw => (h, w),
        };
        let space = match crate::ops::util::exact_size(w, h) {
            Some(s) => space(p.turn, s.w, s.h),
            None => SpaceMap::Unknown,
        };
        Ok(Inferred {
            shape: Shape::Image { w: ow, h: oh, fmt },
            space,
            time: TimeMap::Identity,
            cost: w.max().saturating_mul(h.max()),
        })
    }

    fn apply(
        p: &Rotate90Params,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let img = image_of(inputs[0], Self::ID)?;
        let px = rotate_raster(&img.px, p.turn, cx.mon)?;
        Ok(Applied {
            value: Value::Image(Arc::new(Image {
                px,
                meta: img.meta.clone(),
            })),
            space: Some(space(p.turn, img.px.width(), img.px.height())),
            time: None,
        })
    }

    fn examples() -> Vec<Example<Rotate90Params>> {
        let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
        vec![
            Example {
                name: "cw-across-tiles",
                params: Rotate90Params { turn: Turn::Cw },
                inputs: vec![noise(600, 270, PixelFormat::Rgba8, 31)],
            },
            Example {
                name: "ccw-rgb16",
                params: Rotate90Params { turn: Turn::Ccw },
                inputs: vec![noise(13, 7, PixelFormat::Rgb16, 32)],
            },
            Example {
                name: "half-indexed",
                params: Rotate90Params { turn: Turn::Half },
                inputs: vec![noise(9, 4, PixelFormat::Indexed8, 33)],
            },
            Example {
                name: "sheet-rotates-grid",
                params: Rotate90Params { turn: Turn::Cw },
                inputs: vec![InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 8,
                }],
            },
            Example {
                name: "frames-per-frame",
                params: Rotate90Params { turn: Turn::Ccw },
                inputs: vec![InputGen::NoiseFrames {
                    w: 6,
                    h: 4,
                    fmt: PixelFormat::GrayA8,
                    count: 2,
                    seed: 34,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    /// 3×2 の画像で、各向きの結果を手で書いた期待値と照らす（EXIF の向きの 6・8・3 と同じ）。
    #[test]
    fn small_image_by_hand() {
        let px = Raster::from_rows(3, 2, PixelFormat::Gray8, None, &[0, 1, 2, 3, 4, 5]).unwrap();
        let cases = [
            (Turn::Cw, (2, 3), [3, 0, 4, 1, 5, 2]),
            (Turn::Ccw, (2, 3), [2, 5, 1, 4, 0, 3]),
            (Turn::Half, (3, 2), [5, 4, 3, 2, 1, 0]),
        ];
        for (turn, size, want) in cases {
            let out = rotate_raster(&px, turn, &NoMonitor).unwrap();
            assert_eq!((out.width(), out.height()), size, "{turn:?}");
            assert_eq!(out.to_rows().unwrap(), want, "{turn:?}");
            // EXIF の向きを当てる関数と同じ結果
            let o = match turn {
                Turn::Cw => 6,
                Turn::Ccw => 8,
                Turn::Half => 3,
            };
            assert_eq!(
                out,
                crate::imaging::orient::orient(&px, o, &NoMonitor).unwrap()
            );
        }
    }

    /// タイルの境目をまたぐ大きさで、EXIF の向きの関数（画素ごとに写すもの）と一致する。
    #[test]
    fn matches_orientation_across_tiles() {
        let Value::Image(img) = InputGen::Noise {
            w: 300,
            h: 520,
            fmt: PixelFormat::Rgb8,
            seed: 3,
        }
        .generate()
        .unwrap() else {
            panic!()
        };
        for (turn, o) in [(Turn::Cw, 6), (Turn::Ccw, 8), (Turn::Half, 3)] {
            assert_eq!(
                rotate_raster(&img.px, turn, &NoMonitor).unwrap(),
                crate::imaging::orient::orient(&img.px, o, &NoMonitor).unwrap(),
                "{turn:?}"
            );
        }
    }
}
