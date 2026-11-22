//! `image.fit_canvas` v1：キャンバスを決めた大きさにし、中身を 9 点の基準に置く（M14 段 1。相対の指定。画素は動かすだけ）。
//!
//! - 大きさは各軸の `CanvasLength`：`{"px": n}`（1 以上）・`{"scale": 比}`（その軸の入力の長さの比を偶数への丸め。1 より小さく
//!   しない。項は 1〜2^16）・`"keep"`（入力のまま）・`{"multipleOf": n}`（n の倍数に切り上げ。n は 1〜2^16）・`"pow2"`（入力の
//!   長さ以上の最小の 2 の冪）・`"longest"`（入力の長い辺。両方に使えば正方形）。どちらも `max_dim` 以下。
//! - 中身の位置は `⌊(外 − 中)·a/2⌋`（a は基準の係数：左・上 0、中央 1、右・下 2）。割り切れない中央は中身が左・上へ 0.5px
//!   寄る（広げる余りの 1px は右・下に付き、狭める余りの 1px は左・上から削る）。
//! - `allowCrop` が偽（既定）なら、どちらかの軸で画像より小さくなる大きさは断る（一括で黙って切らない。入力の大きさが
//!   決まっていれば推論で、決まらなければ実行で）。`keep`・`multipleOf`・`pow2`・`longest` は縮まない。
//! - 値・メタデータ・警告は、計算した矩形の `image.canvas` と同じ（E51。実装も canvas の部品を使う）。`fill`・持ち上げ・写し方・
//!   範囲の描画も canvas と同じ。大きさの決まらない入力は、`px` の軸は決まった長さ、ほかは上限の長さで求めた上限（どれも長さに
//!   ついて単調。`max_dim` で切る）を推論にし、写し方は実行で決める。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, IssueCode, Issues};
use crate::geom::{
    ANCHOR_NAMES, Anchor, CanvasLength, IRect, RATIO_TERM_LIMIT, Ratio, anchor_offset,
};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::image::canvas::{Canvas, CanvasParams, infer_format, space_of};
use crate::ops::util::{bad_param, image_of, param_issue};
use crate::render::preview::{Preview, level_size, scale_len};
use crate::render::region::{Area, FrameTiles, Reach, exact_image_size};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FitCanvasParams {
    pub width: CanvasLength,
    pub height: CanvasLength,
    pub anchor: Anchor,
    /// 広げた部分の色（ストレートの RGBA8）。null は透明。
    #[serde(default)]
    pub fill: Option<[u8; 4]>,
    /// 画像より小さい大きさで切ってよいか（偽なら断る）。
    #[serde(default)]
    pub allow_crop: bool,
}

impl OpParams for FitCanvasParams {
    fn schema() -> ParamSchema {
        let len = |key| spec(key, ParamKind::CanvasLength, Unit::Px, json!("keep")).ratio();
        ParamSchema {
            params: vec![
                len("width"),
                len("height"),
                spec(
                    "anchor",
                    ParamKind::Enum {
                        values: ANCHOR_NAMES,
                    },
                    Unit::None,
                    json!("center"),
                ),
                spec(
                    "fill",
                    ParamKind::Color { alpha: true },
                    Unit::None,
                    json!(null),
                ),
                spec("allowCrop", ParamKind::Bool, Unit::None, json!(false)),
            ],
        }
    }
}

/// 大きさと 9 点の基準でキャンバスを決める。
pub struct FitCanvas;

/// 長さの値の範囲（px は 1 以上、比・倍数の項は 1〜2^16）。
fn check_length(key: &str, l: CanvasLength) -> Result<(), Issues> {
    match l {
        CanvasLength::Px(0) => Err(bad_param(key, "大きさは 1 以上にします")),
        CanvasLength::Scale(r) if r.num == 0 || r.den == 0 => {
            Err(bad_param(key, "比の分子・分母は 1 以上にします"))
        }
        CanvasLength::Scale(r) if r.num > RATIO_TERM_LIMIT || r.den > RATIO_TERM_LIMIT => {
            Err(bad_param(key, "比が大きすぎます"))
        }
        CanvasLength::MultipleOf(n) if n == 0 || n > RATIO_TERM_LIMIT => {
            Err(bad_param(key, "倍数は 1〜65,536 にします"))
        }
        _ => Ok(()),
    }
}

fn check_params(p: &FitCanvasParams) -> Result<(), Issues> {
    check_length("width", p.width)?;
    check_length("height", p.height)
}

/// 1 軸の長さ（この軸の入力 `len`・もう一方 `other`）。あふれる・`max_dim` を超えるなら誤り。
fn length(key: &str, l: CanvasLength, len: u64, other: u64, max_dim: u64) -> Result<u64, Issues> {
    let too_large = || param_issue(key, IssueCode::TooLarge, "出力が大きすぎます");
    let n = l.resolve(len, other).ok_or_else(too_large)?;
    if n > max_dim || n > u64::from(u32::MAX) {
        return Err(too_large());
    }
    Ok(n)
}

/// 大きさ `w × h` の入力の新しいキャンバス（入力の座標）。
pub(crate) fn rect_for(p: &FitCanvasParams, w: u64, h: u64, max_dim: u64) -> Result<IRect, Issues> {
    check_params(p)?;
    let nw = length("width", p.width, w, h, max_dim)?;
    let nh = length("height", p.height, h, w, max_dim)?;
    if !p.allow_crop {
        for (key, n, len) in [("width", nw, w), ("height", nh, h)] {
            if n < len {
                return Err(bad_param(key, "キャンバスが画像より小さいので切れます"));
            }
        }
    }
    let (ax, ay) = p.anchor.factors();
    let pos = |outer: u64, inner: u64, a: i64| {
        anchor_offset(i64::try_from(outer).ok()?, i64::try_from(inner).ok()?, a)
    };
    let (Some(ox), Some(oy)) = (pos(nw, w, ax), pos(nh, h, ay)) else {
        return Err(param_issue(
            "width",
            IssueCode::TooLarge,
            "出力が大きすぎます",
        ));
    };
    // length が u32 に収まることを確かめてある
    Ok(IRect::new(-ox, -oy, nw as u32, nh as u32))
}

/// 実行のときの誤り（上限は `Limit`、ほかは引数の誤り）。
fn apply_error(issues: Issues) -> EngineError {
    if issues.0.iter().any(|i| i.code == IssueCode::TooLarge) {
        EngineError::Limit("出力が大きすぎます".into())
    } else {
        EngineError::Invalid(issues)
    }
}

fn canvas(rect: IRect, fill: Option<[u8; 4]>) -> CanvasParams {
    CanvasParams { rect, fill }
}

/// 入力の大きさが決まっていれば、新しいキャンバス（範囲・プレビューの段の計算に使う。上限は見ない）。
fn exact_rect(p: &FitCanvasParams, inputs: &[Shape]) -> Option<IRect> {
    let s = inputs.first().and_then(exact_image_size)?;
    rect_for(p, u64::from(s.w), u64::from(s.h), u64::MAX).ok()
}

impl Op for FitCanvas {
    const ID: &'static str = "image.fit_canvas";
    const VERSION: u16 = 1;
    type Params = FitCanvasParams;

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

    fn infer(p: &FitCanvasParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Shape::Image { w, h, fmt } = inputs[0] else {
            return Err(Issues::none());
        };
        check_params(p)?;
        let max_dim = cx.limits.max_dim;
        if let (Some(w), Some(h)) = (w.exact(), h.exact()) {
            let rect = rect_for(p, w, h, max_dim)?;
            return Canvas::infer(&canvas(rect, p.fill), inputs, cx);
        }
        // 大きさが決まらない：長さが入力によらない（px）か、よる軸の入力が決まっていれば決まった長さ。ほかは上限の長さで
        // 求めた上限（どの種類も長さについて単調）。縮むかは、軸の長さと入力がどちらも決まっていればここで、ほかは実行で見る
        let dim = |key: &str, l: CanvasLength, this: Dim, other: Dim| -> Result<Dim, Issues> {
            let known = l.is_fixed()
                || (this.exact().is_some() && (!l.uses_other() || other.exact().is_some()));
            if known {
                let n = length(key, l, this.max(), other.max(), max_dim)?;
                if !p.allow_crop
                    && let Some(len) = this.exact()
                    && n < len
                {
                    return Err(bad_param(key, "キャンバスが画像より小さいので切れます"));
                }
                return Ok(Dim::Exact(n));
            }
            let cap = max_dim.max(1);
            let n = l.resolve(this.max(), other.max()).unwrap_or(cap);
            Ok(Dim::AtMost(n.clamp(1, cap)))
        };
        let ow = dim("width", p.width, w, h)?;
        let oh = dim("height", p.height, h, w)?;
        Ok(Inferred {
            shape: Shape::Image {
                w: ow,
                h: oh,
                fmt: infer_format(fmt, p.fill)?,
            },
            space: SpaceMap::Unknown,
            time: TimeMap::Identity,
            cost: ow.max().saturating_mul(oh.max()),
        })
    }

    fn apply(
        p: &FitCanvasParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let img = image_of(inputs[0], Self::ID)?;
        let rect = rect_for(
            p,
            u64::from(img.px.width()),
            u64::from(img.px.height()),
            cx.limits.max_dim,
        )
        .map_err(apply_error)?;
        let applied = Canvas::apply(&canvas(rect, p.fill), inputs, cx)?;
        Ok(Applied {
            space: Some(space_of(rect)),
            ..applied
        })
    }

    fn examples() -> Vec<Example<FitCanvasParams>> {
        let ex = |name, width, height, anchor, fill, allow_crop, input| Example {
            name,
            params: FitCanvasParams {
                width,
                height,
                anchor,
                fill,
                allow_crop,
            },
            inputs: vec![input],
        };
        let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
        let px = CanvasLength::Px;
        vec![
            // 7×5 を 10×8 の中央に：中身の左上は (1, 1)
            ex(
                "center-odd-grow",
                px(10),
                px(8),
                Anchor::Center,
                None,
                false,
                noise(7, 5, PixelFormat::Rgba8, 71),
            ),
            ex(
                "bottom-right-crop",
                px(16),
                px(16),
                Anchor::BottomRight,
                None,
                true,
                noise(20, 12, PixelFormat::Rgb8, 72),
            ),
            ex(
                "top-left-pow2",
                CanvasLength::Pow2,
                CanvasLength::Pow2,
                Anchor::TopLeft,
                Some([0, 0, 0, 255]),
                false,
                noise(300, 100, PixelFormat::Rgb8, 73),
            ),
            ex(
                "bottom-multiple-of-8",
                CanvasLength::MultipleOf(8),
                CanvasLength::MultipleOf(8),
                Anchor::Bottom,
                None,
                false,
                noise(13, 9, PixelFormat::GrayA8, 74),
            ),
            ex(
                "longest-square",
                CanvasLength::Longest,
                CanvasLength::Longest,
                Anchor::Center,
                Some([200, 200, 200, 255]),
                false,
                noise(12, 7, PixelFormat::Gray16, 75),
            ),
            ex(
                "keep-and-scale-left",
                CanvasLength::Keep,
                CanvasLength::Scale(Ratio { num: 3, den: 2 }),
                Anchor::Left,
                None,
                false,
                noise(9, 7, PixelFormat::Rgba16, 76),
            ),
            ex(
                "indexed-clear-color",
                px(8),
                px(6),
                Anchor::TopRight,
                None,
                false,
                noise(5, 5, PixelFormat::Indexed8, 77),
            ),
            ex(
                "frames-per-frame",
                px(12),
                CanvasLength::Keep,
                Anchor::Right,
                None,
                false,
                InputGen::NoiseFrames {
                    w: 9,
                    h: 8,
                    fmt: PixelFormat::GrayA8,
                    count: 3,
                    seed: 78,
                },
            ),
            ex(
                "sheet-meta",
                px(80),
                px(40),
                Anchor::Center,
                None,
                false,
                InputGen::Sheet {
                    cols: 4,
                    rows: 2,
                    cell: 16,
                },
            ),
        ]
    }

    // 段 10：範囲・引数を変えたときの範囲・範囲の描画（計算した矩形の canvas と同じ）
    const REGION: bool = true;

    fn reach(p: &FitCanvasParams, inputs: &[Shape]) -> Reach {
        match exact_rect(p, inputs) {
            Some(rect) => Canvas::reach(&canvas(rect, p.fill), inputs),
            None => Reach::Global,
        }
    }

    fn param_change(old: &FitCanvasParams, new: &FitCanvasParams, inputs: &[Shape]) -> Area {
        match (exact_rect(old, inputs), exact_rect(new, inputs)) {
            (Some(a), Some(b)) => {
                Canvas::param_change(&canvas(a, old.fill), &canvas(b, new.fill), inputs)
            }
            _ => Area::All,
        }
    }

    fn effect(p: &FitCanvasParams, inputs: &[Shape]) -> Area {
        match exact_rect(p, inputs) {
            Some(rect) => Canvas::effect(&canvas(rect, p.fill), inputs),
            None => Area::All,
        }
    }

    fn apply_region(
        p: &FitCanvasParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        Some((|| {
            cx.check()?;
            let img = image_of(inputs[0], Self::ID)?;
            let rect = rect_for(
                p,
                u64::from(img.px.width()),
                u64::from(img.px.height()),
                cx.limits.max_dim,
            )
            .map_err(apply_error)?;
            let applied = Canvas::apply_region(&canvas(rect, p.fill), inputs, tiles, base, cx)
                .ok_or_else(|| {
                EngineError::Internal("image.canvas が範囲の描画を返しませんでした".into())
            })??;
            Ok(Applied {
                space: Some(space_of(rect)),
                ..applied
            })
        })())
    }

    // 段 10：プレビューの段。等倍の入力の大きさが決まっていれば、等倍の出力の大きさの ⌈n/2^k⌉ を画素で指す。決まって
    // いなければ、画素の長さを ⌈n/2^k⌉ にし、ほかはそのまま。基準はそのままで、段の丸めで断らないよう切ってよいことにする。
    fn preview(_p: &FitCanvasParams) -> Preview {
        Preview::Scaled
    }

    fn preview_params(
        p: &FitCanvasParams,
        level: u8,
        _inputs: &[Shape],
        full: &[Shape],
    ) -> FitCanvasParams {
        let (width, height) = match exact_rect(p, full) {
            Some(rect) => {
                let px = |n: u32| {
                    CanvasLength::Px(
                        u32::try_from(level_size(u64::from(n), level)).unwrap_or(u32::MAX),
                    )
                };
                (px(rect.w), px(rect.h))
            }
            None => {
                let len = |l: CanvasLength| match l {
                    CanvasLength::Px(n) => CanvasLength::Px(scale_len(n, level)),
                    other => other,
                };
                (len(p.width), len(p.height))
            }
        };
        FitCanvasParams {
            width,
            height,
            anchor: p.anchor,
            fill: p.fill,
            allow_crop: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::IPoint;
    use crate::limits::Limits;

    /// 中身の左上（出力の座標）。
    fn content_origin(p: &FitCanvasParams, w: u64, h: u64) -> IPoint {
        let r = rect_for(p, w, h, u64::MAX).unwrap();
        IPoint { x: -r.x, y: -r.y }
    }

    fn params(width: CanvasLength, height: CanvasLength, anchor: Anchor) -> FitCanvasParams {
        FitCanvasParams {
            width,
            height,
            anchor,
            fill: None,
            allow_crop: true,
        }
    }

    /// 9 点の位置（7×5 を 10×8 に）と、狭めるときの位置（20×12 を 15×9 に）。
    #[test]
    fn origins_follow_the_anchor() {
        let px = CanvasLength::Px;
        let grow: Vec<(i64, i64)> = Anchor::ALL
            .iter()
            .map(|a| {
                let o = content_origin(&params(px(10), px(8), *a), 7, 5);
                (o.x, o.y)
            })
            .collect();
        assert_eq!(
            grow,
            [
                (0, 0),
                (1, 0),
                (3, 0),
                (0, 1),
                (1, 1),
                (3, 1),
                (0, 3),
                (1, 3),
                (3, 3)
            ]
        );
        let o = content_origin(&params(px(15), px(9), Anchor::Center), 20, 12);
        assert_eq!((o.x, o.y), (-3, -2));
        let o = content_origin(&params(px(15), px(9), Anchor::BottomRight), 20, 12);
        assert_eq!((o.x, o.y), (-5, -3));
    }

    /// 縮む大きさは `allowCrop` がなければ断る（どの軸か）。上限を超える・あふれる大きさも断る。
    #[test]
    fn crops_and_limits() {
        let mut p = params(CanvasLength::Px(19), CanvasLength::Keep, Anchor::Center);
        assert!(rect_for(&p, 20, 12, 100).is_ok());
        p.allow_crop = false;
        let e = rect_for(&p, 20, 12, 100).unwrap_err();
        assert_eq!(
            (e.0[0].param.as_deref(), e.0[0].code),
            (Some("width"), IssueCode::OutOfRange)
        );
        let p = params(CanvasLength::Keep, CanvasLength::Pow2, Anchor::Top);
        let e = rect_for(&p, 20, 65, 100).unwrap_err();
        assert_eq!(
            (e.0[0].param.as_deref(), e.0[0].code),
            (Some("height"), IssueCode::TooLarge)
        );
        assert_eq!(rect_for(&p, 20, 64, 100).unwrap(), IRect::new(0, 0, 20, 64));
        let p = params(CanvasLength::Pow2, CanvasLength::Keep, Anchor::Top);
        assert!(rect_for(&p, u64::MAX, 1, u64::MAX).is_err());
    }

    /// 大きさの決まらない入力：px の軸は決まった長さ、ほかは上限の長さからの上限。写し方は実行で決める。
    #[test]
    fn unknown_sizes_infer_bounds() {
        let limits = Limits::default();
        let cx = InferCx { limits: &limits };
        let input = [Shape::Image {
            w: Dim::AtMost(13),
            h: Dim::Exact(9),
            fmt: PixelFormat::Rgba8,
        }];
        let got = FitCanvas::infer(
            &params(
                CanvasLength::MultipleOf(8),
                CanvasLength::Px(20),
                Anchor::Center,
            ),
            &input,
            &cx,
        )
        .unwrap();
        assert_eq!(
            got.shape,
            Shape::Image {
                w: Dim::AtMost(16),
                h: Dim::Exact(20),
                fmt: PixelFormat::Rgba8,
            }
        );
        assert_eq!(got.space, SpaceMap::Unknown);
        // 高さは決まっているので、縮むなら推論で断る。長い辺は両方の軸による
        let mut p = params(CanvasLength::Keep, CanvasLength::Px(8), Anchor::Center);
        p.allow_crop = false;
        assert!(FitCanvas::infer(&p, &input, &cx).is_err());
        let got = FitCanvas::infer(
            &params(CanvasLength::Keep, CanvasLength::Longest, Anchor::Center),
            &input,
            &cx,
        )
        .unwrap();
        let Shape::Image { w, h, .. } = got.shape else {
            panic!()
        };
        assert_eq!((w, h), (Dim::AtMost(13), Dim::AtMost(13)));
    }
}
