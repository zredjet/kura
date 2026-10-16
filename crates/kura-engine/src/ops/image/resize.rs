//! `image.resize` v1：大きさを変える。式は `imaging::resample`（最近傍は整数だけ、box・bilinear・Lanczos3 は線形光の浮動小数）。
//!
//! - 幅・高さは `Length`：画素（`{"px": n}`）・入力に対する比（`{"scale": {"num", "den"}}`。`round_half_even(in·num/den)`）・
//!   自動（`"auto"`。もう一方の軸の倍率で縦横比を保つ：`round_half_even(in·out_other/in_other)`）。どれも 1 以上にする。両方を自動にはできない。
//! - 最近傍は形式のまま（Indexed8 はパレットのまま）で、整数だけなのでゴールデンはすべてのターゲットで照らす（`numeric_for`）。
//!   平滑なフィルタは Indexed8 を Rgba8 にする（推論で分かるので、エディタはパレットが失われることを先に知らせられる）。
//! - 大きさが変わらない軸は混ぜない（重みは恒等）。ただし値は線形光の乗算済みを通るので、片方の軸だけ変わるときは
//!   α = 0 の画素の色は 0 になる。両方とも変わらないときは、混ぜずにそのまま通す（Indexed8 は Rgba8 に広げるだけ）。
//! - 平滑なフィルタは sRGB の伝達関数で混ぜる。ICC が sRGB に見えない画像は、そうみなしたことを警告する（`ColorSpaceAssumed`）。
//! - メタデータは倍率で写す（格子は割り切れるときだけ残る。§6）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::IssueCode;
use crate::error::{EngineError, Issues};
pub use crate::geom::Length;
use crate::geom::{Ratio, Size, div_round_half_even};
use crate::imaging::resample::{Filter, resize_nearest, resize_smooth, smooth_format};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::util::{bad_param, exact_size, image_of, param_issue};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Image, PixelFormat, Value};

/// フィルタ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResizeFilter {
    Nearest,
    Box,
    Bilinear,
    Lanczos3,
}

impl ResizeFilter {
    fn imaging(self) -> Filter {
        match self {
            Self::Nearest => Filter::Nearest,
            Self::Box => Filter::Box,
            Self::Bilinear => Filter::Bilinear,
            Self::Lanczos3 => Filter::Lanczos3,
        }
    }
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResizeParams {
    pub width: Length,
    pub height: Length,
    pub filter: ResizeFilter,
}

impl OpParams for ResizeParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec("width", ParamKind::Length, Unit::Px, json!({ "px": 1 })),
                spec("height", ParamKind::Length, Unit::Px, json!("auto")),
                spec(
                    "filter",
                    ParamKind::Enum {
                        values: &["nearest", "box", "bilinear", "lanczos3"],
                    },
                    Unit::None,
                    json!("nearest"),
                ),
            ],
        }
    }
}

/// リサイズ。
pub struct Resize;

/// 比の大きさの上限（倍率の分子・分母。極端な比で計算があふれないように）。
const MAX_RATIO_TERM: u32 = 1 << 16;

fn check_length(key: &str, l: Length) -> Result<(), Issues> {
    match l {
        Length::Px(0) => Err(bad_param(key, "大きさは 1 以上にします")),
        Length::Scale(r) if r.num == 0 || r.den == 0 => {
            Err(bad_param(key, "比の分子・分母は 1 以上にします"))
        }
        Length::Scale(r) if r.num > MAX_RATIO_TERM || r.den > MAX_RATIO_TERM => {
            Err(bad_param(key, "比が大きすぎます"))
        }
        _ => Ok(()),
    }
}

fn scaled(inn: u64, r: Ratio) -> u64 {
    let v =
        div_round_half_even(i128::from(inn) * i128::from(r.num), i128::from(r.den)).unwrap_or(1);
    v.max(1) as u64
}

/// 出力の大きさ（入力の大きさから）。
pub fn out_size(p: &ResizeParams, iw: u64, ih: u64) -> (u64, u64) {
    let direct = |l: Length, inn: u64| match l {
        Length::Px(n) => Some(u64::from(n)),
        Length::Scale(r) => Some(scaled(inn, r)),
        Length::Auto => None,
    };
    let keep = |inn: u64, out_other: u64, in_other: u64| {
        let v = div_round_half_even(
            i128::from(inn) * i128::from(out_other),
            i128::from(in_other.max(1)),
        )
        .unwrap_or(1);
        v.max(1) as u64
    };
    match (direct(p.width, iw), direct(p.height, ih)) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, keep(ih, w, iw)),
        (None, Some(h)) => (keep(iw, h, ih), h),
        // 推論で断っている
        (None, None) => (iw, ih),
    }
}

/// 出力の大きさの推論。入力の大きさが決まっていればそのまま計算する。決まっていない軸は上限を返す：
/// 比はその軸の入力の上限から（単調なので）、自動はもう一方の軸の長さから求めた上限（`max_dim` で切る。
/// 実際に `max_dim` を超えたら、実行で `Limit` にする）。
fn out_dim(p: &ResizeParams, w: Dim, h: Dim, max_dim: u64) -> (Dim, Dim) {
    if let (Some(iw), Some(ih)) = (w.exact(), h.exact()) {
        let (ow, oh) = out_size(p, iw, ih);
        return (Dim::Exact(ow), Dim::Exact(oh));
    }
    let bound_auto = |in_this: u64, other: Length| -> u64 {
        let b = match other {
            // もう一方の入力は 1 以上なので、in_this·n / in_other ≤ in_this·n
            Length::Px(n) => in_this.saturating_mul(u64::from(n)),
            // もう一方の出力は in_other·r + 1 以下なので、in_this·(r + 1) + 1 以下
            Length::Scale(r) => in_this
                .saturating_mul(u64::from(r.num.div_ceil(r.den.max(1))) + 1)
                .saturating_add(1),
            Length::Auto => in_this,
        };
        b.clamp(1, max_dim.max(1))
    };
    let axis = |l: Length, inn: Dim, other: Length| match l {
        Length::Px(n) => Dim::Exact(u64::from(n)),
        Length::Scale(r) => match inn.exact() {
            Some(n) => Dim::Exact(scaled(n, r)),
            None => Dim::AtMost(scaled(inn.max(), r)),
        },
        Length::Auto => Dim::AtMost(bound_auto(inn.max(), other)),
    };
    (axis(p.width, w, p.height), axis(p.height, h, p.width))
}

fn space(iw: u32, ih: u32, ow: u32, oh: u32) -> SpaceMap {
    SpaceMap::Scale {
        sx: Ratio { num: ow, den: iw },
        sy: Ratio { num: oh, den: ih },
        out: Size { w: ow, h: oh },
    }
}

impl Op for Resize {
    const ID: &'static str = "image.resize";
    const VERSION: u16 = 1;
    type Params = ResizeParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::PerFrame,
            numeric: Numeric::Float,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &ResizeParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Image { w, h, fmt } = inputs[0] else {
            return Err(Issues::none());
        };
        check_length("width", p.width)?;
        check_length("height", p.height)?;
        if p.width == Length::Auto && p.height == Length::Auto {
            return Err(bad_param("height", "幅と高さの両方を自動にはできません"));
        }
        let (ow, oh) = out_dim(p, w, h, cx.limits.max_dim);
        // 上限を超える大きさは、ここで断る（u32 に収まらない大きさも）。原因の軸を指す
        for (key, d) in [("width", ow), ("height", oh)] {
            if d.max() > cx.limits.max_dim {
                return Err(param_issue(key, IssueCode::TooLarge, "出力が大きすぎます"));
            }
        }
        let out_fmt = match p.filter {
            ResizeFilter::Nearest => fmt,
            _ => smooth_format(fmt),
        };
        let space = match (exact_size(w, h), exact_size(ow, oh)) {
            (Some(i), Some(o)) => space(i.w, i.h, o.w, o.h),
            _ => SpaceMap::Unknown,
        };
        let cost = ow.max().saturating_mul(oh.max());
        Ok(Inferred {
            shape: Shape::Image {
                w: ow,
                h: oh,
                fmt: out_fmt,
            },
            space,
            time: TimeMap::Identity,
            cost,
        })
    }

    fn apply(
        p: &ResizeParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let img = image_of(inputs[0], Self::ID)?;
        let (iw, ih) = (img.px.width(), img.px.height());
        let (ow, oh) = out_size(p, u64::from(iw), u64::from(ih));
        if ow > cx.limits.max_dim || oh > cx.limits.max_dim {
            return Err(EngineError::Limit("出力が大きすぎます".into()));
        }
        let (ow, oh) = (
            u32::try_from(ow).map_err(|_| EngineError::Limit("出力が大きすぎます".into()))?,
            u32::try_from(oh).map_err(|_| EngineError::Limit("出力が大きすぎます".into()))?,
        );
        let px = match p.filter {
            ResizeFilter::Nearest => resize_nearest(&img.px, ow, oh, cx.mon)?,
            // 大きさが変わらないときは混ぜずにそのまま通す（α = 0 の画素の色も保つ。Indexed8 は推論どおり Rgba8 に広げる）
            _ if (ow, oh) == (iw, ih) => {
                let fmt = smooth_format(img.px.format());
                crate::ops::image::convert::convert_raster(&img.px, fmt, [0, 0, 0], cx.mon)?
            }
            f => {
                // v1 は色の変換をしないので、sRGB でない ICC の画像は sRGB とみなして混ぜたことを知らせる（§5.2）
                if img
                    .meta
                    .icc
                    .as_deref()
                    .is_some_and(|icc| !crate::imaging::icc::is_srgb(icc))
                {
                    cx.warn(
                        crate::error::Issue::warning(
                            IssueCode::ColorSpaceAssumed,
                            "sRGB でない色空間の画像を、sRGB とみなして混ぜました",
                        )
                        .param("filter"),
                    );
                }
                resize_smooth(&img.px, ow, oh, f.imaging(), cx.mon)?
            }
        };
        Ok(Applied {
            value: Value::Image(Arc::new(Image {
                px,
                meta: img.meta.clone(),
            })),
            space: Some(space(iw, ih, ow, oh)),
            time: None,
        })
    }

    fn numeric_for(p: &ResizeParams) -> Numeric {
        if p.filter == ResizeFilter::Nearest {
            Numeric::Exact
        } else {
            Numeric::Float
        }
    }

    fn examples() -> Vec<Example<ResizeParams>> {
        let ex = |name, width, height, filter, input| Example {
            name,
            params: ResizeParams {
                width,
                height,
                filter,
            },
            inputs: vec![input],
        };
        let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
        let half = Ratio { num: 1, den: 2 };
        vec![
            ex(
                "nearest-4x-indexed",
                Length::Scale(Ratio { num: 4, den: 1 }),
                Length::Auto,
                ResizeFilter::Nearest,
                noise(16, 12, PixelFormat::Indexed8, 41),
            ),
            ex(
                "nearest-down-across-tiles",
                Length::Px(130),
                Length::Px(300),
                ResizeFilter::Nearest,
                noise(520, 600, PixelFormat::Rgba16, 42),
            ),
            ex(
                "nearest-sheet-keeps-grid",
                Length::Scale(Ratio { num: 2, den: 1 }),
                Length::Scale(Ratio { num: 2, den: 1 }),
                ResizeFilter::Nearest,
                InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 8,
                },
            ),
            ex(
                "box-half-rgba",
                Length::Scale(half),
                Length::Scale(half),
                ResizeFilter::Box,
                noise(64, 40, PixelFormat::Rgba8, 43),
            ),
            ex(
                "bilinear-up-gray16",
                Length::Px(23),
                Length::Auto,
                ResizeFilter::Bilinear,
                noise(10, 7, PixelFormat::Gray16, 44),
            ),
            ex(
                "lanczos-down-across-tiles",
                Length::Px(270),
                Length::Px(130),
                ResizeFilter::Lanczos3,
                noise(600, 300, PixelFormat::Rgb8, 45),
            ),
            ex(
                "lanczos-indexed-to-rgba",
                Length::Auto,
                Length::Px(9),
                ResizeFilter::Lanczos3,
                noise(12, 6, PixelFormat::Indexed8, 46),
            ),
            ex(
                "frames-per-frame",
                Length::Scale(Ratio { num: 3, den: 1 }),
                Length::Auto,
                ResizeFilter::Nearest,
                InputGen::NoiseFrames {
                    w: 5,
                    h: 4,
                    fmt: PixelFormat::Rgba8,
                    count: 2,
                    seed: 47,
                },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_from_lengths() {
        let p = |width, height| ResizeParams {
            width,
            height,
            filter: ResizeFilter::Nearest,
        };
        assert_eq!(out_size(&p(Length::Px(10), Length::Auto), 20, 7), (10, 4));
        assert_eq!(out_size(&p(Length::Px(10), Length::Auto), 20, 5), (10, 2));
        assert_eq!(
            out_size(
                &p(Length::Scale(Ratio { num: 1, den: 3 }), Length::Px(2)),
                10,
                9
            ),
            (3, 2)
        );
        assert_eq!(
            out_size(&p(Length::Auto, Length::Px(1)), 1_000, 1),
            (1_000, 1)
        );
        // 1 より小さくはならない
        assert_eq!(
            out_size(
                &p(Length::Scale(Ratio { num: 1, den: 100 }), Length::Auto),
                3,
                3
            ),
            (1, 1)
        );
    }

    #[test]
    fn params_are_validated() {
        let run = |v: serde_json::Value| {
            crate::testing::render_single(
                Resize::ID,
                1,
                v,
                &[InputGen::Noise {
                    w: 4,
                    h: 4,
                    fmt: PixelFormat::Rgb8,
                    seed: 0,
                }],
            )
        };
        assert!(run(json!({ "width": "auto", "height": "auto", "filter": "nearest" })).is_err());
        assert!(
            run(json!({ "width": { "px": 0 }, "height": "auto", "filter": "nearest" })).is_err()
        );
        assert!(
            run(json!({ "width": { "scale": { "num": 1, "den": 0 } }, "height": "auto", "filter": "box" }))
                .is_err()
        );
        assert!(
            run(json!({ "width": { "px": 100000 }, "height": "auto", "filter": "box" })).is_err()
        );
        assert!(run(json!({ "width": { "px": 8 }, "height": "auto", "filter": "box" })).is_ok());
    }
}
