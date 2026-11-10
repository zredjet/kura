//! `image.pad` v1：上下左右に余白を足す・削る（M14 段 1。相対の指定。画素は動かすだけなので、ビット単位で正確）。
//!
//! - 各辺は `{"px": n}`（負で削る。±2^31）か `{"scale": {"num", "den"}}`（その軸の入力の長さに対する比。上下は高さ、左右は幅。
//!   `round_half_even(長さ·num/den)`。分子は 0〜2^16、分母は 1〜2^16）。
//! - 出力は 幅 `w + left + right`・高さ `h + top + bottom`（どちらも 1 以上・`max_dim` 以下）。値・メタデータ・警告は、新しい
//!   キャンバスを入力の座標の矩形 `(−left, −top, 幅, 高さ)` にした `image.canvas` と同じ（E51。実装も canvas の部品を使う）。
//!   `fill` の意味も canvas と同じ（null は透明で、アルファのない形式はアルファ付きになる。余白が 0 でも）。
//! - 持ち上げはキャンバスいっぱいのフレームの列だけ。格子・ピボット・スライスは平行移動で写る（左・上を削ってはみ出たものは
//!   警告して落ちる）。
//! - 大きさの決まらない入力（`AtMost`）は、上限の長さで求めた大きさを上限（`max_dim` で切る）にして推論し、写し方は実行で
//!   決める。実際の大きさで 0 以下・上限を超えるなら、実行で断る。
//! - ドット絵の倍率にそろえる指定は持たない（拡大済みの絵に 1px 足すと格子がずれる。要るなら先に最近傍で縮める）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, IssueCode, Issues};
use crate::geom::{IPoint, IRect, Margin, RATIO_TERM_LIMIT, Ratio, coord_ok};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, expect_inputs, spec,
};
use crate::ops::image::canvas::{Canvas, CanvasParams, infer_format, space_of};
use crate::ops::util::{bad_param, image_of, param_issue};
use crate::render::preview::{Preview, scale_rect};
use crate::render::region::{Area, FrameTiles, Reach, exact_image_size};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::{InputGen, MetaGen};
use crate::value::{PixelFormat, Slice, Value};

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PadParams {
    pub top: Margin,
    pub right: Margin,
    pub bottom: Margin,
    pub left: Margin,
    /// 足した部分の色（ストレートの RGBA8）。null は透明。
    #[serde(default)]
    pub fill: Option<[u8; 4]>,
}

impl OpParams for PadParams {
    fn schema() -> ParamSchema {
        let side = |key| spec(key, ParamKind::Margin, Unit::Px, json!({ "px": 0 })).ratio();
        ParamSchema {
            params: vec![
                side("top"),
                side("right"),
                side("bottom"),
                side("left"),
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

/// 上下左右の余白。
pub struct Pad;

/// 余白の値の範囲（px は ±2^31、比の分母は 1 以上、項は 2^16 まで）。
fn check_margin(key: &str, m: Margin) -> Result<(), Issues> {
    match m {
        Margin::Px(n) if !coord_ok(n) => Err(bad_param(key, "余白が範囲の外です")),
        Margin::Scale(r) if r.den == 0 => Err(bad_param(key, "比の分母は 1 以上にします")),
        Margin::Scale(r) if r.num > RATIO_TERM_LIMIT || r.den > RATIO_TERM_LIMIT => {
            Err(bad_param(key, "比が大きすぎます"))
        }
        _ => Ok(()),
    }
}

fn check_params(p: &PadParams) -> Result<(), Issues> {
    check_margin("top", p.top)?;
    check_margin("right", p.right)?;
    check_margin("bottom", p.bottom)?;
    check_margin("left", p.left)
}

/// 1 つの軸：長さ `len` の前（左・上）と後（右・下）に余白を足した (前の余白, 新しい長さ)。0 以下は誤り（削りすぎた側の
/// 引数。同じなら前）、`max_dim` を超えるのも誤り（大きく足した側）。
fn axis(
    len: u64,
    (before_key, before): (&'static str, Margin),
    (after_key, after): (&'static str, Margin),
    max_dim: u64,
) -> Result<(i64, u64), Issues> {
    let px = |key: &str, m: Margin| {
        m.pixels(len)
            .ok_or_else(|| bad_param(key, "比が使えません"))
    };
    let (b, a) = (px(before_key, before)?, px(after_key, after)?);
    let (more, less) = if a > b {
        (after_key, before_key)
    } else {
        (before_key, after_key)
    };
    let new = i64::try_from(len)
        .ok()
        .and_then(|l| l.checked_add(b)?.checked_add(a))
        .ok_or_else(|| param_issue(more, IssueCode::TooLarge, "出力が大きすぎます"))?;
    if new < 1 {
        return Err(bad_param(
            less,
            "余白を削りすぎて、大きさが 0 以下になります",
        ));
    }
    let new = new as u64;
    if new > max_dim || new > u64::from(u32::MAX) {
        return Err(param_issue(more, IssueCode::TooLarge, "出力が大きすぎます"));
    }
    Ok((b, new))
}

/// 大きさ `w × h` の入力の新しいキャンバス（入力の座標）。
pub(crate) fn rect_for(p: &PadParams, w: u64, h: u64, max_dim: u64) -> Result<IRect, Issues> {
    check_params(p)?;
    let (left, nw) = axis(w, ("left", p.left), ("right", p.right), max_dim)?;
    let (top, nh) = axis(h, ("top", p.top), ("bottom", p.bottom), max_dim)?;
    // axis が u32 に収まることを確かめてある
    Ok(IRect::new(-left, -top, nw as u32, nh as u32))
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
fn exact_rect(p: &PadParams, inputs: &[Shape]) -> Option<IRect> {
    let s = inputs.first().and_then(exact_image_size)?;
    rect_for(p, u64::from(s.w), u64::from(s.h), u64::MAX).ok()
}

impl Op for Pad {
    const ID: &'static str = "image.pad";
    const VERSION: u16 = 1;
    type Params = PadParams;

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

    fn infer(p: &PadParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
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
        // 大きさが決まらない：新しい長さは入力の長さについて単調（比の分子は 0 以上）なので、上限の長さで求めた長さが上限
        // （`max_dim` で切る。実際に超えれば実行で断る）。それが 0 以下なら、どの大きさでも断る
        let cap = max_dim.max(1);
        let dim = |d: Dim, before, after| -> Result<Dim, Issues> {
            match d.exact() {
                Some(n) => Ok(Dim::Exact(axis(n, before, after, max_dim)?.1)),
                None => match axis(d.max(), before, after, u64::MAX) {
                    Ok((_, n)) => Ok(Dim::AtMost(n.min(cap))),
                    Err(e) if e.0.iter().all(|i| i.code == IssueCode::TooLarge) => {
                        Ok(Dim::AtMost(cap))
                    }
                    Err(e) => Err(e),
                },
            }
        };
        let ow = dim(w, ("left", p.left), ("right", p.right))?;
        let oh = dim(h, ("top", p.top), ("bottom", p.bottom))?;
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
        p: &PadParams,
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

    fn examples() -> Vec<Example<PadParams>> {
        let px = Margin::Px;
        let ex = |name, [top, right, bottom, left]: [Margin; 4], fill, input| Example {
            name,
            params: PadParams {
                top,
                right,
                bottom,
                left,
                fill,
            },
            inputs: vec![input],
        };
        let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
        let tenth = Margin::Scale(Ratio { num: 1, den: 10 });
        vec![
            ex(
                "grow-rgba-across-tiles",
                [px(7), px(13), px(0), px(30)],
                None,
                noise(300, 270, PixelFormat::Rgba8, 61),
            ),
            ex(
                "rgb-gains-alpha",
                [px(2), px(2), px(2), px(2)],
                None,
                noise(9, 9, PixelFormat::Rgb8, 62),
            ),
            ex(
                "shrink-negative",
                [px(-2), px(-3), px(-1), px(0)],
                None,
                noise(20, 12, PixelFormat::Rgba8, 63),
            ),
            // 15×10 の 1/10：左右は 1.5 → 2、上下は 1
            ex(
                "scale-half-even",
                [tenth, tenth, tenth, tenth],
                Some([128, 128, 128, 255]),
                noise(15, 10, PixelFormat::Gray8, 64),
            ),
            ex(
                "indexed-appends-color",
                [px(0), px(1), px(3), px(0)],
                Some([1, 2, 3, 255]),
                noise(5, 5, PixelFormat::Indexed8, 65),
            ),
            ex(
                "indexed-uses-clear-color",
                [px(1), px(1), px(1), px(1)],
                None,
                noise(5, 5, PixelFormat::Indexed8, 66),
            ),
            ex(
                "sheet-shifts-grid-and-pivot",
                [px(0), px(0), px(0), px(16)],
                None,
                InputGen::WithMeta {
                    base: Box::new(InputGen::Sheet {
                        cols: 4,
                        rows: 2,
                        cell: 16,
                    }),
                    meta: MetaGen {
                        slices: vec![Slice {
                            name: "body".into(),
                            rect: IRect::new(2, 1, 12, 14),
                            center: Some(IRect::new(4, 4, 8, 8)),
                            pivot: Some(IPoint { x: 6, y: 14 }),
                        }],
                        ..MetaGen::default()
                    },
                },
            ),
            ex(
                "frames-per-frame",
                [px(1), px(-2), px(3), px(4)],
                None,
                InputGen::NoiseFrames {
                    w: 9,
                    h: 8,
                    fmt: PixelFormat::GrayA8,
                    count: 3,
                    seed: 67,
                },
            ),
            ex(
                "gray16-with-color",
                [px(-1), px(2), px(4), px(-3)],
                Some([80, 80, 80, 255]),
                noise(8, 6, PixelFormat::Gray16, 68),
            ),
        ]
    }

    // 段 10：範囲・引数を変えたときの範囲・範囲の描画（計算した矩形の canvas と同じ）
    const REGION: bool = true;

    fn reach(p: &PadParams, inputs: &[Shape]) -> Reach {
        match exact_rect(p, inputs) {
            Some(rect) => Canvas::reach(&canvas(rect, p.fill), inputs),
            None => Reach::Global,
        }
    }

    fn param_change(old: &PadParams, new: &PadParams, inputs: &[Shape]) -> Area {
        match (exact_rect(old, inputs), exact_rect(new, inputs)) {
            (Some(a), Some(b)) => {
                Canvas::param_change(&canvas(a, old.fill), &canvas(b, new.fill), inputs)
            }
            _ => Area::All,
        }
    }

    fn effect(p: &PadParams, inputs: &[Shape]) -> Area {
        match exact_rect(p, inputs) {
            Some(rect) => Canvas::effect(&canvas(rect, p.fill), inputs),
            None => Area::All,
        }
    }

    fn apply_region(
        p: &PadParams,
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

    // 段 10：プレビューの段。等倍の矩形を canvas と同じく段に縮め（始まりは床・終わりは天井）、段の入力の大きさとの差を
    // 各辺の画素にする（canvas の段と同じ値）。大きさが決まらなければ、画素を ⌊n/2^k⌋ にし、比はそのまま。
    fn preview(_p: &PadParams) -> Preview {
        Preview::Scaled
    }

    fn preview_params(p: &PadParams, level: u8, inputs: &[Shape], full: &[Shape]) -> PadParams {
        if let (Some(rect), Some(stage)) = (
            exact_rect(p, full),
            inputs.first().and_then(exact_image_size),
        ) {
            let s = scale_rect(rect, level);
            return PadParams {
                top: Margin::Px(-s.y),
                right: Margin::Px(s.right() - i64::from(stage.w)),
                bottom: Margin::Px(s.bottom() - i64::from(stage.h)),
                left: Margin::Px(-s.x),
                fill: p.fill,
            };
        }
        let shrink = |m: Margin| match m {
            Margin::Px(n) => Margin::Px(n >> u32::from(level.min(62))),
            other => other,
        };
        PadParams {
            top: shrink(p.top),
            right: shrink(p.right),
            bottom: shrink(p.bottom),
            left: shrink(p.left),
            fill: p.fill,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;
    use crate::testing::render_value;

    fn params(t: i64, r: i64, b: i64, l: i64, fill: Option<[u8; 4]>) -> PadParams {
        PadParams {
            top: Margin::Px(t),
            right: Margin::Px(r),
            bottom: Margin::Px(b),
            left: Margin::Px(l),
            fill,
        }
    }

    fn run(input: Value, p: PadParams) -> Result<Value, EngineError> {
        render_value(Pad::ID, 1, serde_json::to_value(p).unwrap(), input)
    }

    /// 余白の矩形：左・上は負の位置、削る辺は大きさを減らす。比はその軸の長さから偶数への丸め。
    #[test]
    fn rects_from_margins() {
        let max = 1 << 20;
        assert_eq!(
            rect_for(&params(1, 2, 3, 4, None), 10, 20, max).unwrap(),
            IRect::new(-4, -1, 16, 24)
        );
        assert_eq!(
            rect_for(&params(-2, -3, -1, 0, None), 20, 12, max).unwrap(),
            IRect::new(0, 2, 17, 9)
        );
        let tenth = Margin::Scale(Ratio { num: 1, den: 10 });
        let p = PadParams {
            top: tenth,
            right: tenth,
            bottom: tenth,
            left: tenth,
            fill: None,
        };
        // 左右 15/10 = 1.5 → 2、上下 10/10 = 1
        assert_eq!(
            rect_for(&p, 15, 10, max).unwrap(),
            IRect::new(-2, -1, 19, 12)
        );
        // 0 以下は削りすぎた辺、上限は大きく足した辺
        let e = rect_for(&params(0, -15, 0, -6, None), 20, 12, max).unwrap_err();
        assert_eq!(e.0[0].param.as_deref(), Some("right"));
        assert_eq!(e.0[0].code, IssueCode::OutOfRange);
        let e = rect_for(&params(0, 0, 9, 0, None), 20, 12, 20).unwrap_err();
        assert_eq!(
            (e.0[0].param.as_deref(), e.0[0].code),
            (Some("bottom"), IssueCode::TooLarge)
        );
        assert!(rect_for(&params(0, 0, 0, -19, None), 20, 12, max).is_ok());
        assert!(rect_for(&params(0, 0, 0, -20, None), 20, 12, max).is_err());
    }

    /// 画素は canvas と同じに置く（透明で足すとアルファ付きになり、削ると元の画素の一部になる）。
    #[test]
    fn pixels_move_and_fill() {
        let input = InputGen::Noise {
            w: 4,
            h: 3,
            fmt: PixelFormat::Rgb8,
            seed: 1,
        }
        .generate()
        .unwrap();
        let Value::Image(src) = &input else { panic!() };
        let Value::Image(img) =
            run(input.clone(), params(2, 0, 1, 1, Some([9, 8, 7, 255]))).unwrap()
        else {
            panic!()
        };
        assert_eq!((img.px.width(), img.px.height()), (5, 6));
        assert_eq!(img.px.format(), PixelFormat::Rgb8);
        assert_eq!(img.px.pixel(0, 0), Some(&[9u8, 8, 7][..]));
        assert_eq!(img.px.pixel(1, 2), src.px.pixel(0, 0));
        let Value::Image(t) = run(input.clone(), params(0, 0, 0, 0, None)).unwrap() else {
            panic!()
        };
        assert_eq!(t.px.format(), PixelFormat::Rgba8);
        let Value::Image(c) = run(input.clone(), params(-1, -1, 0, -2, None)).unwrap() else {
            panic!()
        };
        assert_eq!((c.px.width(), c.px.height()), (1, 2));
        let s = src.px.pixel(2, 1).unwrap();
        assert_eq!(c.px.pixel(0, 0), Some(&[s[0], s[1], s[2], 255][..]));
    }

    /// 大きさの決まらない入力は、上限の長さで求めた上限を推論にし、写し方は実行で決める。上限でも 0 以下なら推論で断る。
    #[test]
    fn unknown_sizes_infer_bounds() {
        let limits = Limits::default();
        let cx = InferCx { limits: &limits };
        let input = [Shape::Image {
            w: Dim::AtMost(10),
            h: Dim::Exact(4),
            fmt: PixelFormat::Gray8,
        }];
        let got = Pad::infer(&params(1, 2, -1, -3, None), &input, &cx).unwrap();
        assert_eq!(
            got.shape,
            Shape::Image {
                w: Dim::AtMost(9),
                h: Dim::Exact(4),
                fmt: PixelFormat::GrayA8,
            }
        );
        assert_eq!(got.space, SpaceMap::Unknown);
        assert!(Pad::infer(&params(0, 0, 0, -10, None), &input, &cx).is_err());
        assert!(Pad::infer(&params(-4, 0, 0, 0, None), &input, &cx).is_err());
        // 上限は max_dim で切る（実際に超えれば実行で断る）
        let huge = Pad::infer(&params(0, 1 << 30, 0, 0, None), &input, &cx).unwrap();
        let Shape::Image { w, .. } = huge.shape else {
            panic!()
        };
        assert_eq!(w, Dim::AtMost(limits.max_dim));
    }
}
