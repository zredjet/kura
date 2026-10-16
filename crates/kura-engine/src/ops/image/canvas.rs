//! `image.canvas` v1：キャンバスの大きさを変える（広げる・狭める・ずらす。画素は動かすだけなので、ビット単位で正確）。
//!
//! - `rect` は新しいキャンバスを入力の座標で表したもの（左上は負でもよい）。入力の外の部分は `fill` で塗る。
//! - `fill` が null（透明）なら、アルファのない形式はアルファ付きの形式になる（広げる部分がなくても。形は引数と入力の形式だけで決まる）。
//!   Indexed8 は形式のまま、パレットの透明な色（なければ後ろに足す）で塗る。
//! - `fill` が色なら形式はそのまま。グレーの形式は灰色、アルファのない形式は不透明な色だけ。Indexed8 はパレットの同じ色
//!   （なければ後ろに足す）。パレットが 256 色でいっぱいで足せなければ `Unsupported`。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::{IRect, Size};
use crate::imaging::convert::convert_pixel;
use crate::imaging::pixels::{color_bytes, palette_clear, palette_index, with_alpha};
use crate::mapping::{IntAffine, SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{bad_param, image_of};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Image, Palette, PixelFormat, Raster, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasParams {
    /// 新しいキャンバス（入力の座標）。
    pub rect: IRect,
    /// 広げた部分の色（ストレートの RGBA8）。null は透明。
    #[serde(default)]
    pub fill: Option<[u8; 4]>,
}

impl OpParams for CanvasParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "rect",
                    ParamKind::Rect,
                    Unit::Px,
                    json!({ "x": 0, "y": 0, "w": 1, "h": 1 }),
                ),
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

/// キャンバスの大きさを変える。
pub struct Canvas;

/// 出力の形式。
pub fn out_format(fmt: PixelFormat, fill: Option<[u8; 4]>) -> PixelFormat {
    match fill {
        None => with_alpha(fmt),
        Some(_) => fmt,
    }
}

/// 塗る画素とパレット（Indexed8 はパレットの色を探すか足す）。
pub fn fill_pixel(
    fmt: PixelFormat,
    palette: Option<&Palette>,
    fill: Option<[u8; 4]>,
) -> Result<(Vec<u8>, Option<Palette>), EngineError> {
    if fmt == PixelFormat::Indexed8 {
        let p = palette.ok_or_else(|| EngineError::Internal("パレットがありません".into()))?;
        let found = match fill {
            None => palette_clear(p),
            Some(c) => palette_index(p, c),
        };
        let (p, i) = found.ok_or_else(|| {
            EngineError::Unsupported("パレットが 256 色でいっぱいなので、塗る色を足せません".into())
        })?;
        return Ok((vec![i], Some(p)));
    }
    let bytes = match fill {
        None => vec![0u8; fmt.bytes_per_pixel()],
        Some(c) => color_bytes(fmt, c).map_err(|m| EngineError::Invalid(bad_param("fill", m)))?,
    };
    Ok((bytes, None))
}

/// 入力 `px` を、入力の座標の矩形 `rect` のキャンバスに置いた画素（外は `fill`）。
pub fn place(
    px: &Raster,
    rect: IRect,
    fill: Option<[u8; 4]>,
    mon: &dyn crate::monitor::Monitor,
) -> Result<Raster, EngineError> {
    let in_fmt = px.format();
    let fmt = out_format(in_fmt, fill);
    let (fill_px, palette) = fill_pixel(fmt, px.palette().map(|p| p.as_ref()), fill)?;
    let palette = palette.or_else(|| px.palette().map(|p| (**p).clone()));
    let in_palette = px.palette().map(|p| p.colors.as_slice());
    let (iw, ih) = (i64::from(px.width()), i64::from(px.height()));
    let (in_bpp, bpp) = (px.bpp(), fmt.bytes_per_pixel());
    Raster::from_tile_fn(rect.w, rect.h, fmt, palette, mon, |t, out| {
        for p in out.chunks_exact_mut(bpp) {
            p.copy_from_slice(&fill_px);
        }
        // このタイルが入力と重なる部分（入力の座標）
        let (x0, y0) = (rect.x + t.x, rect.y + t.y);
        let (x1, y1) = (x0 + i64::from(t.w), y0 + i64::from(t.h));
        let (cx0, cy0, cx1, cy1) = (x0.max(0), y0.max(0), x1.min(iw), y1.min(ih));
        if cx0 >= cx1 || cy0 >= cy1 {
            return Ok(());
        }
        let cw = (cx1 - cx0) as u32;
        let mut row = vec![0u8; cw as usize * in_bpp];
        for y in cy0..cy1 {
            px.read_rect(IRect::new(cx0, y, cw, 1), &mut row)?;
            let dst_row = (y - y0) as usize * t.w as usize * bpp;
            let dst = dst_row + (cx0 - x0) as usize * bpp;
            if in_fmt == fmt {
                out[dst..dst + row.len()].copy_from_slice(&row);
            } else {
                for (i, s) in row.chunks_exact(in_bpp).enumerate() {
                    let o = dst + i * bpp;
                    convert_pixel(in_fmt, in_palette, s, fmt, [0, 0, 0], &mut out[o..o + bpp])?;
                }
            }
        }
        Ok(())
    })
}

impl Op for Canvas {
    const ID: &'static str = "image.canvas";
    const VERSION: u16 = 1;
    type Params = CanvasParams;

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

    fn infer(p: &CanvasParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Image { fmt, .. } = inputs[0] else {
            return Err(Issues::none());
        };
        if p.rect.w == 0 || p.rect.h == 0 {
            return Err(bad_param("rect", "キャンバスの大きさが 0 です"));
        }
        let out_fmt = out_format(fmt, p.fill);
        if let (Some(c), false) = (p.fill, out_fmt == PixelFormat::Indexed8) {
            color_bytes(out_fmt, c).map_err(|m| bad_param("fill", m))?;
        }
        Ok(Inferred {
            shape: Shape::Image {
                w: Dim::Exact(u64::from(p.rect.w)),
                h: Dim::Exact(u64::from(p.rect.h)),
                fmt: out_fmt,
            },
            space: SpaceMap::Affine {
                m: IntAffine::translate(-p.rect.x, -p.rect.y),
                out: Size {
                    w: p.rect.w,
                    h: p.rect.h,
                },
            },
            time: TimeMap::Identity,
            cost: u64::from(p.rect.w) * u64::from(p.rect.h),
        })
    }

    fn apply(
        p: &CanvasParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let img = image_of(inputs[0], Self::ID)?;
        let px = place(&img.px, p.rect, p.fill, cx.mon)?;
        Ok(Applied::value(Value::Image(Arc::new(Image {
            px,
            meta: img.meta.clone(),
        }))))
    }

    fn examples() -> Vec<Example<CanvasParams>> {
        vec![
            Example {
                name: "grow-rgba-across-tiles",
                params: CanvasParams {
                    rect: IRect::new(-30, -7, 330, 290),
                    fill: None,
                },
                inputs: vec![InputGen::Noise {
                    w: 300,
                    h: 270,
                    fmt: PixelFormat::Rgba8,
                    seed: 11,
                }],
            },
            Example {
                name: "rgb-gains-alpha",
                params: CanvasParams {
                    rect: IRect::new(-2, 3, 12, 5),
                    fill: None,
                },
                inputs: vec![InputGen::Noise {
                    w: 9,
                    h: 9,
                    fmt: PixelFormat::Rgb8,
                    seed: 12,
                }],
            },
            Example {
                name: "gray16-with-color",
                params: CanvasParams {
                    rect: IRect::new(4, -4, 10, 12),
                    fill: Some([80, 80, 80, 255]),
                },
                inputs: vec![InputGen::Noise {
                    w: 8,
                    h: 6,
                    fmt: PixelFormat::Gray16,
                    seed: 13,
                }],
            },
            Example {
                name: "indexed-uses-clear-color",
                params: CanvasParams {
                    rect: IRect::new(-1, -1, 7, 7),
                    fill: None,
                },
                inputs: vec![InputGen::Noise {
                    w: 5,
                    h: 5,
                    fmt: PixelFormat::Indexed8,
                    seed: 14,
                }],
            },
            Example {
                name: "indexed-appends-color",
                params: CanvasParams {
                    rect: IRect::new(0, 0, 6, 8),
                    fill: Some([1, 2, 3, 255]),
                },
                inputs: vec![InputGen::Noise {
                    w: 5,
                    h: 5,
                    fmt: PixelFormat::Indexed8,
                    seed: 15,
                }],
            },
            Example {
                name: "sheet-shifts-grid-and-pivot",
                params: CanvasParams {
                    rect: IRect::new(-16, 0, 80, 32),
                    fill: None,
                },
                inputs: vec![InputGen::Sheet {
                    cols: 4,
                    rows: 2,
                    cell: 16,
                }],
            },
            Example {
                name: "frames-per-frame",
                params: CanvasParams {
                    rect: IRect::new(-3, 1, 12, 6),
                    fill: None,
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 9,
                    h: 8,
                    fmt: PixelFormat::GrayA8,
                    count: 3,
                    seed: 16,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    fn run(input: Value, rect: IRect, fill: Option<[u8; 4]>) -> Result<Value, EngineError> {
        render_value(
            Canvas::ID,
            1,
            serde_json::to_value(CanvasParams { rect, fill }).unwrap(),
            input,
        )
    }

    #[test]
    fn grows_with_fill_and_keeps_pixels() {
        let input = InputGen::Noise {
            w: 4,
            h: 3,
            fmt: PixelFormat::Rgb8,
            seed: 1,
        }
        .generate()
        .unwrap();
        let Value::Image(src) = &input else { panic!() };
        let out = run(
            input.clone(),
            IRect::new(-1, -2, 7, 6),
            Some([9, 8, 7, 255]),
        )
        .unwrap();
        let Value::Image(img) = &out else { panic!() };
        assert_eq!(img.px.format(), PixelFormat::Rgb8);
        assert_eq!(img.px.pixel(0, 0), Some(&[9u8, 8, 7][..]));
        assert_eq!(img.px.pixel(6, 5), Some(&[9u8, 8, 7][..]));
        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(img.px.pixel(x + 1, y + 2), src.px.pixel(x, y));
            }
        }
        // 透明で広げるとアルファ付きになる
        let Value::Image(t) = run(input.clone(), IRect::new(-1, 0, 5, 3), None).unwrap() else {
            panic!()
        };
        assert_eq!(t.px.format(), PixelFormat::Rgba8);
        assert_eq!(t.px.pixel(0, 0), Some(&[0u8, 0, 0, 0][..]));
        let s = src.px.pixel(0, 0).unwrap();
        assert_eq!(t.px.pixel(1, 0), Some(&[s[0], s[1], s[2], 255][..]));
        // 半透明の色はアルファのない画像に置けない・灰色でない色はグレーに置けない
        assert!(run(input, IRect::new(0, 0, 5, 3), Some([1, 2, 3, 4])).is_err());
        let gray = InputGen::Noise {
            w: 2,
            h: 2,
            fmt: PixelFormat::Gray8,
            seed: 1,
        }
        .generate()
        .unwrap();
        assert!(run(gray, IRect::new(0, 0, 3, 3), Some([1, 2, 3, 255])).is_err());
    }

    #[test]
    fn indexed_palettes_gain_the_fill_color_only_when_needed() {
        let pal = Palette {
            colors: vec![[5, 5, 5, 255], [6, 6, 6, 255]],
        };
        let px = Raster::from_rows(2, 1, PixelFormat::Indexed8, Some(pal), &[1, 0]).unwrap();
        let input = Value::Image(Arc::new(Image {
            px,
            meta: Default::default(),
        }));
        let Value::Image(a) = run(input.clone(), IRect::new(0, 0, 3, 1), None).unwrap() else {
            panic!()
        };
        assert_eq!(a.px.palette().unwrap().colors.len(), 3);
        assert_eq!(a.px.to_rows().unwrap(), [1, 0, 2]);
        let Value::Image(b) =
            run(input.clone(), IRect::new(-1, 0, 3, 1), Some([6, 6, 6, 255])).unwrap()
        else {
            panic!()
        };
        assert_eq!(b.px.palette().unwrap().colors.len(), 2);
        assert_eq!(b.px.to_rows().unwrap(), [1, 1, 0]);
        let full = Palette {
            colors: vec![[7, 7, 7, 255]; 256],
        };
        let px = Raster::from_rows(1, 1, PixelFormat::Indexed8, Some(full), &[0]).unwrap();
        let input = Value::Image(Arc::new(Image {
            px,
            meta: Default::default(),
        }));
        // 描くときのエラーは、どのノードかを付けて返す
        let e = run(input, IRect::new(0, 0, 2, 1), None).unwrap_err();
        assert!(matches!(e.root(), EngineError::Unsupported(_)), "{e:?}");
        assert_eq!(e.node(), Some(crate::doc::NodeId(1)));
    }

    #[test]
    fn far_away_canvas_is_all_fill() {
        let input = InputGen::Noise {
            w: 3,
            h: 3,
            fmt: PixelFormat::Rgba8,
            seed: 2,
        }
        .generate()
        .unwrap();
        let Value::Image(img) = run(input, IRect::new(1_000_000, -1_000_000, 2, 2), None).unwrap()
        else {
            panic!()
        };
        assert_eq!(img.px.to_rows().unwrap(), [0u8; 16]);
    }
}
