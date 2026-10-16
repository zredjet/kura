//! `image.trim_alpha` v1：透明な余白を切り落とす（画素は動かすだけなので、ビット単位で正確）。
//!
//! - アルファが `threshold` 以下の画素を透明とみなし、残りを囲む最小の矩形で切る（`imaging::bounds::opaque_bounds`。
//!   段 8 の解析 `image.opaque_bounds` も同じ関数を使う。E26）。
//! - 全部が透明なら、左上の 1×1 にする（ADR-147）。`union` で合併と重ならないフレーム（全部が透明とみなす画素）も、
//!   そのフレームの左上の 1 画素を新しいキャンバスの左上に置く（パレットに透明な色がなくても、パレットを変えずに済む）。
//! - 透明を表せない形式（アルファのない形式）は何もしない。
//! - フレームの列は `mode` で決める：`union` はすべてのフレームの範囲の合併で、キャンバスごと切る（フレームの位置も詰める）。
//!   `perFrame` はキャンバスを保ち、各フレームをそれぞれの範囲に切って位置を持たせる（トリムしたスプライト）。
//! - 大きさは中身で決まるので、推論は `AtMost`、写し方は実行で決まる（`Unknown`）。フレームへの持ち上げはしない（§7.1）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::{IPoint, IRect, Size};
use crate::imaging::bounds::opaque_bounds;
use crate::imaging::pixels::can_be_transparent;
use crate::mapping::{IntAffine, SpaceMap, TimeMap};
use crate::monitor::Monitor;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::shape::{Dim, Fullness, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Frame, Frames, Image, PixelFormat, Raster, Value};

/// フレームの列の切り方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrimMode {
    /// すべてのフレームの範囲の合併で、キャンバスごと切る。
    Union,
    /// キャンバスを保ち、フレームごとに切る。
    PerFrame,
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrimParams {
    /// このアルファ以下の画素を透明とみなす（16 ビットは 257 倍と比べる）。
    pub threshold: u8,
    pub mode: TrimMode,
}

impl OpParams for TrimParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "threshold",
                    ParamKind::Int {
                        min: 0,
                        max: 254,
                        step: 1,
                    },
                    Unit::None,
                    json!(0),
                ),
                spec(
                    "mode",
                    ParamKind::Enum {
                        values: &["union", "perFrame"],
                    },
                    Unit::None,
                    json!("union"),
                ),
            ],
        }
    }
}

/// 透明な余白のトリム。
pub struct TrimAlpha;

/// 切る矩形（全部が透明なら左上の 1×1）。
pub fn trim_rect(px: &Raster, threshold: u8, mon: &dyn Monitor) -> Result<IRect, EngineError> {
    Ok(opaque_bounds(px, threshold, mon)?.unwrap_or(IRect::new(0, 0, 1, 1)))
}

fn crop(px: &Raster, r: IRect, mon: &dyn Monitor) -> Result<Raster, EngineError> {
    if r == IRect::new(0, 0, px.width(), px.height()) {
        return Ok(px.clone());
    }
    px.derive(r.w, r.h, mon, |t, out| {
        px.read_rect(IRect::new(r.x + t.x, r.y + t.y, t.w, t.h), out)
    })
}

fn union(a: Option<IRect>, b: IRect) -> IRect {
    match a {
        None => b,
        Some(a) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
            IRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
        }
    }
}

fn intersect(a: IRect, b: IRect) -> Option<IRect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (x0 < x1 && y0 < y1).then(|| IRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

impl Op for TrimAlpha {
    const ID: &'static str = "image.trim_alpha";
    const VERSION: u16 = 1;
    type Params = TrimParams;

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

    fn infer(p: &TrimParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let input = inputs[0];
        let (fmt, cost) = match input {
            Shape::Image { w, h, fmt } => (fmt, w.max().saturating_mul(h.max())),
            Shape::Frames {
                count, w, h, fmt, ..
            } => (
                fmt,
                w.max().saturating_mul(h.max()).saturating_mul(count.max()),
            ),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        if !can_be_transparent(fmt) {
            return Ok(Inferred {
                shape: input,
                space: SpaceMap::Identity,
                time: TimeMap::Identity,
                cost,
            });
        }
        let at_most = |d: Dim| Dim::AtMost(d.max());
        let (shape, space) = match (input, p.mode) {
            (Shape::Image { w, h, fmt }, _) => (
                Shape::Image {
                    w: at_most(w),
                    h: at_most(h),
                    fmt,
                },
                SpaceMap::Unknown,
            ),
            (
                Shape::Frames {
                    count,
                    w,
                    h,
                    fmt,
                    full,
                },
                TrimMode::Union,
            ) => (
                Shape::Frames {
                    count,
                    w: at_most(w),
                    h: at_most(h),
                    fmt,
                    // いっぱいのフレームは同じ矩形で切るので、いっぱいのまま
                    full: if full == Fullness::Full {
                        Fullness::Full
                    } else {
                        Fullness::Unknown
                    },
                },
                SpaceMap::Unknown,
            ),
            (
                Shape::Frames {
                    count, w, h, fmt, ..
                },
                TrimMode::PerFrame,
            ) => (
                Shape::Frames {
                    count,
                    w,
                    h,
                    fmt,
                    full: Fullness::Unknown,
                },
                SpaceMap::Identity,
            ),
            (Shape::Audio { .. }, _) => return Err(Issues::none()),
        };
        Ok(Inferred {
            shape,
            space,
            time: TimeMap::Identity,
            cost,
        })
    }

    fn apply(
        p: &TrimParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let input = inputs[0];
        let fmt = match input {
            Value::Image(img) => img.px.format(),
            Value::Frames(fr) => fr.first().px.format(),
            Value::Audio(_) => {
                return Err(EngineError::Internal(
                    "image.trim_alpha の入力が音声です".into(),
                ));
            }
        };
        if !can_be_transparent(fmt) {
            return Ok(Applied::value(input.clone()));
        }
        match input {
            Value::Image(img) => {
                let r = trim_rect(&img.px, p.threshold, cx.mon)?;
                let px = crop(&img.px, r, cx.mon)?;
                Ok(Applied {
                    value: Value::Image(Arc::new(Image {
                        px,
                        meta: img.meta.clone(),
                    })),
                    space: Some(SpaceMap::Affine {
                        m: IntAffine::translate(-r.x, -r.y),
                        out: Size { w: r.w, h: r.h },
                    }),
                    time: None,
                })
            }
            Value::Frames(fr) => match p.mode {
                TrimMode::Union => {
                    // キャンバスの座標で、各フレームの不透明な範囲の合併
                    let mut all: Option<IRect> = None;
                    for f in fr.frames() {
                        cx.check()?;
                        if let Some(b) = opaque_bounds(&f.px, p.threshold, cx.mon)? {
                            all = Some(union(
                                all,
                                IRect::new(f.offset.x + b.x, f.offset.y + b.y, b.w, b.h),
                            ));
                        }
                    }
                    let r = all.unwrap_or(IRect::new(0, 0, 1, 1));
                    let mut frames = Vec::with_capacity(fr.frames().len());
                    for f in fr.frames() {
                        cx.check()?;
                        let fr_rect =
                            IRect::new(f.offset.x, f.offset.y, f.px.width(), f.px.height());
                        // 合併の外に出る部分を切る。フレームが合併と重ならなければ（全部が透明とみなす画素）、
                        // そのフレームの左上の 1 画素を合併の左上に置く（パレットを変えない。単体の画像の規則と同じ）
                        let (px, offset) = match intersect(fr_rect, r) {
                            Some(i) => (
                                crop(
                                    &f.px,
                                    IRect::new(i.x - f.offset.x, i.y - f.offset.y, i.w, i.h),
                                    cx.mon,
                                )?,
                                IPoint {
                                    x: i.x - r.x,
                                    y: i.y - r.y,
                                },
                            ),
                            None => (
                                crop(&f.px, IRect::new(0, 0, 1, 1), cx.mon)?,
                                IPoint { x: 0, y: 0 },
                            ),
                        };
                        frames.push(Frame {
                            px,
                            offset,
                            duration_ms: f.duration_ms,
                        });
                    }
                    Ok(Applied {
                        value: Value::Frames(Arc::new(Frames::new(
                            frames,
                            Size { w: r.w, h: r.h },
                            fr.meta.clone(),
                            fr.loop_count,
                        )?)),
                        space: Some(SpaceMap::Affine {
                            m: IntAffine::translate(-r.x, -r.y),
                            out: Size { w: r.w, h: r.h },
                        }),
                        time: None,
                    })
                }
                TrimMode::PerFrame => {
                    let mut frames = Vec::with_capacity(fr.frames().len());
                    for f in fr.frames() {
                        cx.check()?;
                        let b = trim_rect(&f.px, p.threshold, cx.mon)?;
                        frames.push(Frame {
                            px: crop(&f.px, b, cx.mon)?,
                            offset: IPoint {
                                x: f.offset.x + b.x,
                                y: f.offset.y + b.y,
                            },
                            duration_ms: f.duration_ms,
                        });
                    }
                    Ok(Applied::value(Value::Frames(Arc::new(Frames::new(
                        frames,
                        fr.canvas(),
                        fr.meta.clone(),
                        fr.loop_count,
                    )?))))
                }
            },
            Value::Audio(_) => Err(EngineError::Internal(
                "image.trim_alpha の入力が音声です".into(),
            )),
        }
    }

    fn examples() -> Vec<Example<TrimParams>> {
        let union = TrimParams {
            threshold: 0,
            mode: TrimMode::Union,
        };
        vec![
            Example {
                name: "sprite-across-tiles",
                params: union.clone(),
                inputs: vec![InputGen::Sprite {
                    w: 600,
                    h: 300,
                    rect: IRect::new(250, 40, 300, 200),
                }],
            },
            Example {
                name: "sheet-keeps-grid-offset",
                params: union.clone(),
                inputs: vec![InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 8,
                }],
            },
            Example {
                name: "noise-threshold",
                params: TrimParams {
                    threshold: 200,
                    mode: TrimMode::Union,
                },
                inputs: vec![InputGen::Noise {
                    w: 40,
                    h: 30,
                    fmt: PixelFormat::GrayA16,
                    seed: 21,
                }],
            },
            Example {
                name: "rgb-is-untouched",
                params: union.clone(),
                inputs: vec![InputGen::Noise {
                    w: 7,
                    h: 5,
                    fmt: PixelFormat::Rgb8,
                    seed: 22,
                }],
            },
            Example {
                name: "indexed-clear-color",
                params: union.clone(),
                inputs: vec![InputGen::Noise {
                    w: 20,
                    h: 20,
                    fmt: PixelFormat::Indexed8,
                    seed: 23,
                }],
            },
            Example {
                name: "frames-union",
                params: union,
                inputs: vec![InputGen::NoiseFrames {
                    w: 12,
                    h: 10,
                    fmt: PixelFormat::Rgba8,
                    count: 3,
                    seed: 24,
                }],
            },
            Example {
                name: "frames-per-frame",
                params: TrimParams {
                    threshold: 128,
                    mode: TrimMode::PerFrame,
                },
                inputs: vec![InputGen::NoiseFrames {
                    w: 12,
                    h: 10,
                    fmt: PixelFormat::Rgba8,
                    count: 3,
                    seed: 25,
                }],
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    fn trim(input: Value, threshold: u8, mode: TrimMode) -> Value {
        render_value(
            TrimAlpha::ID,
            1,
            serde_json::to_value(TrimParams { threshold, mode }).unwrap(),
            input,
        )
        .unwrap()
    }

    #[test]
    fn trims_to_the_sprite_and_all_clear_becomes_one_pixel() {
        let sprite = InputGen::Sprite {
            w: 20,
            h: 10,
            rect: IRect::new(3, 2, 5, 4),
        }
        .generate()
        .unwrap();
        let Value::Image(img) = trim(sprite, 0, TrimMode::Union) else {
            panic!()
        };
        assert_eq!((img.px.width(), img.px.height()), (5, 4));
        let clear = Value::Image(Arc::new(Image {
            px: Raster::filled(9, 9, PixelFormat::Rgba8, None, &[1, 2, 3, 0]).unwrap(),
            meta: Default::default(),
        }));
        let Value::Image(one) = trim(clear, 0, TrimMode::Union) else {
            panic!()
        };
        assert_eq!(one.px.to_rows().unwrap(), [1, 2, 3, 0]);
    }

    fn frames() -> Value {
        // 10×8 のキャンバスに、不透明な点の位置が違う 2 枚
        let mk = |x: u32, y: u32| {
            let mut data = vec![0u8; 10 * 8 * 4];
            let i = ((y * 10 + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&[9, 9, 9, 255]);
            Raster::from_rows(10, 8, PixelFormat::Rgba8, None, &data).unwrap()
        };
        Value::Frames(Arc::new(
            Frames::full(vec![(mk(2, 1), 50), (mk(6, 5), 70)], Default::default(), 0).unwrap(),
        ))
    }

    #[test]
    fn frames_union_and_per_frame() {
        let Value::Frames(u) = trim(frames(), 0, TrimMode::Union) else {
            panic!()
        };
        assert_eq!(u.canvas(), Size { w: 5, h: 5 });
        assert!(u.is_full());
        assert_eq!(u.frames()[0].px.pixel(0, 0), Some(&[9u8, 9, 9, 255][..]));
        assert_eq!(u.frames()[1].px.pixel(4, 4), Some(&[9u8, 9, 9, 255][..]));
        let Value::Frames(f) = trim(frames(), 0, TrimMode::PerFrame) else {
            panic!()
        };
        assert_eq!(f.canvas(), Size { w: 10, h: 8 });
        assert_eq!(f.frames()[0].offset, IPoint { x: 2, y: 1 });
        assert_eq!(f.frames()[1].offset, IPoint { x: 6, y: 5 });
        assert_eq!(
            (f.frames()[1].px.width(), f.frames()[1].px.height()),
            (1, 1)
        );
        assert_eq!(f.frames()[1].duration_ms, 70);
        // トリムしたものをもう一度合併で切ると、位置を保ってキャンバスが詰まる
        let Value::Frames(again) = trim(Value::Frames(f), 0, TrimMode::Union) else {
            panic!()
        };
        assert_eq!(again.canvas(), Size { w: 5, h: 5 });
        assert_eq!(again.frames()[1].offset, IPoint { x: 4, y: 4 });
    }
}
