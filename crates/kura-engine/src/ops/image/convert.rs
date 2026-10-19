//! `image.convert` v1：画素の形式を変える（深さ・グレー・アルファ・パレット）。整数だけで、式を凍結する。
//!
//! - Indexed8 以外へは 1 画素ずつ `imaging::convert::convert_pixel`（グレーは Rec. 709 の整数の式、アルファを外すときは
//!   下地 `matte` に重ねる、深さは 257 倍・四捨五入）。同じ形式ならそのまま。
//! - Indexed8 へは、RGBA8 にしてから（16 ビットは四捨五入で 8 ビットに）、すべての画素（フレームの列なら全フレーム）の
//!   色の表から `imaging::quantize` で `colors` 色以下のパレットを作り、`imaging::dither` で割り当てる。
//!   色の数が `colors` 以下ならディザはかけない（色がそのまま残る）。すでに Indexed8 でパレットが `colors` 色以下ならそのまま。
//! - フレームの列は 1 つのパレットにそろえる必要があるので、持ち上げずにこの操作が全フレームを扱う（位置はそのまま）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{EngineError, Issues};
use crate::geom::IRect;
use crate::imaging::convert::convert_pixel;
use crate::imaging::dither::{Dither, map_rgba8};
use crate::imaging::quantize::{
    histogram, map_with_lookup, merge_all, palette_for, palette_lookup,
};
use crate::mapping::{SpaceMap, TimeMap};
use crate::monitor::Monitor;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, Work, expect_inputs, spec,
};
use crate::ops::util::map_rasters;
use crate::render::preview::Preview;
use crate::render::region::{Area, FrameTiles, Reach, Tiles, map_rasters_region};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{Palette, PixelFormat, Raster, Value};

/// ディザ（引数の形）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DitherParam {
    None,
    Ordered,
    ErrorDiffusion,
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConvertParams {
    /// 変換先の形式。
    pub format: PixelFormat,
    /// Indexed8 にするときの色の数の上限（1〜256）。
    pub colors: u16,
    /// Indexed8 にするときのディザ。
    pub dither: DitherParam,
    /// アルファを外すときの下地の色。
    pub matte: [u8; 3],
}

impl OpParams for ConvertParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "format",
                    ParamKind::Enum {
                        values: &[
                            "gray8", "grayA8", "rgb8", "rgba8", "gray16", "grayA16", "rgb16",
                            "rgba16", "indexed8",
                        ],
                    },
                    Unit::None,
                    json!("rgba8"),
                ),
                spec(
                    "colors",
                    ParamKind::Int {
                        min: 1,
                        max: 256,
                        step: 1,
                    },
                    Unit::None,
                    json!(256),
                ),
                spec(
                    "dither",
                    ParamKind::Enum {
                        values: &["none", "ordered", "errorDiffusion"],
                    },
                    Unit::None,
                    json!("none"),
                ),
                spec(
                    "matte",
                    ParamKind::Color { alpha: false },
                    Unit::None,
                    json!([255, 255, 255]),
                ),
            ],
        }
    }
}

/// 形式の変換。
pub struct Convert;

fn rasters(v: &Value) -> Vec<&Raster> {
    match v {
        Value::Image(img) => vec![&img.px],
        Value::Frames(fr) => fr.frames().iter().map(|f| &f.px).collect(),
        Value::Audio(_) => Vec::new(),
    }
}

/// 1 枚の画素を Indexed8 以外の形式にする。
pub(crate) fn convert_raster(
    px: &Raster,
    fmt: PixelFormat,
    matte: [u8; 3],
    mon: &dyn Monitor,
) -> Result<Raster, EngineError> {
    if px.format() == fmt {
        return Ok(px.clone());
    }
    Raster::from_tile_fn(
        px.width(),
        px.height(),
        fmt,
        None,
        mon,
        convert_tile(px, fmt, matte),
    )
}

/// [`convert_raster`] の、`pick` のタイルだけを描いたもの（段 10。範囲の描画。ほかは `base` のタイルか無効なタイル）。
/// 同じ形式なら、選んだタイルを入力と共有する（全体の描画と同じ）。
pub(crate) fn convert_raster_picked(
    px: &Raster,
    fmt: PixelFormat,
    matte: [u8; 3],
    mon: &dyn Monitor,
    pick: &Tiles,
    base: Option<&Raster>,
) -> Result<Raster, EngineError> {
    if px.format() == fmt {
        return Raster::splice(px, pick, base);
    }
    Raster::from_tiles_picked(
        px.width(),
        px.height(),
        fmt,
        None,
        mon,
        pick,
        base,
        convert_tile(px, fmt, matte),
    )
}

/// 出力のタイル 1 枚を描く閉包（全体の描画と範囲の描画が同じものを使う）。
fn convert_tile(
    px: &Raster,
    fmt: PixelFormat,
    matte: [u8; 3],
) -> impl Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send + '_ {
    let (src_fmt, src_bpp, bpp) = (px.format(), px.bpp(), fmt.bytes_per_pixel());
    let palette = px.palette().map(|p| p.colors.as_slice());
    move |t, out| {
        let mut buf = vec![0u8; t.w as usize * t.h as usize * src_bpp];
        px.read_rect(t, &mut buf)?;
        for (s, d) in buf.chunks_exact(src_bpp).zip(out.chunks_exact_mut(bpp)) {
            convert_pixel(src_fmt, palette, s, fmt, matte, d)?;
        }
        Ok(())
    }
}

/// 1 フレームの画素の数とフレームの数（音声は 0）。
fn pixels_of(input: &Shape) -> (u64, u64) {
    match input {
        Shape::Image { w, h, .. } => (w.max().saturating_mul(h.max()), 1),
        Shape::Frames { count, w, h, .. } => (w.max().saturating_mul(h.max()), count.max()),
        Shape::Audio { .. } => (0, 0),
    }
}

/// 推論で上限と照らす Indexed8 への作業領域（段 4 からの式。変えると断る入力が変わるので凍結する）：1 フレームの RGBA と
/// 色の並び（8 バイト／画素）と、全フレームの色の表と併合の写し（最大で全画素の数 × 32 バイト）。
fn infer_work(input: &Shape) -> u64 {
    let (frame_px, frames) = pixels_of(input);
    let colors = frame_px.saturating_mul(frames).min(1 << 32);
    frame_px
        .saturating_mul(8)
        .saturating_add(colors.saturating_mul(32))
}

/// メモリの予算に使う Indexed8 への作業領域（`apply` が実際に持つものの上限。`infer_work` 以上）。違う色の数は最大で
/// 全画素の数（2³² で頭打ち）とみなす。
/// - 1 フレームあたり 16 バイト／画素：RGBA8 の写し 2 つ（`convert_raster` と `to_rows`。8）、色の並び（4）とその並べ替えの
///   作業（4）。
/// - 色あたり：色の表（`(u32, u64)` の 16 バイト。1 つずつ伸ばすので容量は最大 2 倍で 32）、メディアンカットの最初の写し（16）、
///   箱を分けるたびに右の半分を新しく持つ分（分け方の段ごとに 8。段の数は ⌈log2 colors⌉ ≤ 8）。
/// - フレームの列：フレームごとの表を併合する間の写し（色あたり 32）。
///
/// 実測（2048² の乱数の Rgb8 を 256 色・誤差拡散。values 調査と段 10 で同じ）は、入力 12 MiB に対してピーク 418〜419 MB
/// （入力と出力の値を引いた作業領域は 381 MiB）。この式は 512 MiB（約 1.3 倍）。`infer_work` は 160 MiB で、実測の約 1/2.4
/// しか見積もらない。
fn indexed_work(input: &Shape, colors: u16) -> u64 {
    let (frame_px, frames) = pixels_of(input);
    let distinct = frame_px.saturating_mul(frames).min(1 << 32);
    let levels = u64::from(u16::BITS - (colors.clamp(1, 256) - 1).leading_zeros());
    let per_color = 32 + 16 + 8 * levels + if frames > 1 { 32 } else { 0 };
    frame_px
        .saturating_mul(16)
        .saturating_add(distinct.saturating_mul(per_color))
}

/// RGBA8 の画素の並び（行を詰めたもの）。
fn rgba8_rows(px: &Raster, mon: &dyn Monitor) -> Result<Vec<u8>, EngineError> {
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    convert_raster(px, PixelFormat::Rgba8, [0, 0, 0], mon)?.to_rows()
}

impl Op for Convert {
    const ID: &'static str = "image.convert";
    const VERSION: u16 = 1;
    type Params = ConvertParams;

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

    fn infer(p: &ConvertParams, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        // Indexed8 へは作業領域が要る。上限（max_value_bytes）を超えるなら描く前に断る（式は `infer_work`。メモリの予算の
        // 見積もりは、実測に合わせた `work_bytes` で、こちらより大きい）
        if p.format == PixelFormat::Indexed8 {
            let work = infer_work(&inputs[0]);
            if work > cx.limits.max_value_bytes {
                return Err(crate::ops::util::param_issue(
                    "format",
                    crate::error::IssueCode::TooLarge,
                    "減色の作業領域が大きすぎます（画素が多すぎます）",
                ));
            }
        }
        let (shape, cost) = match inputs[0] {
            Shape::Image { w, h, .. } => (
                Shape::Image {
                    w,
                    h,
                    fmt: p.format,
                },
                w.max().saturating_mul(h.max()),
            ),
            Shape::Frames {
                count, w, h, full, ..
            } => (
                Shape::Frames {
                    count,
                    w,
                    h,
                    fmt: p.format,
                    full,
                },
                w.max().saturating_mul(h.max()).saturating_mul(count.max()),
            ),
            Shape::Audio { .. } => return Err(Issues::none()),
        };
        Ok(Inferred {
            shape,
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost,
        })
    }

    fn work_bytes(p: &ConvertParams, inputs: &[Shape], _out: &Shape) -> Work {
        // Indexed8 以外は 1 タイルずつ写すだけ（タイル 1 枚ぶんの一時の領域は数えない）
        match inputs.first() {
            Some(input) if p.format == PixelFormat::Indexed8 => {
                Work::shared(indexed_work(input, p.colors))
            }
            _ => Work::ZERO,
        }
    }

    fn apply(
        p: &ConvertParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let input = inputs[0];
        let mon = cx.mon;
        if p.format != PixelFormat::Indexed8 {
            let value = map_rasters(input, mon, |_, px| {
                convert_raster(px, p.format, p.matte, mon)
            })?;
            return Ok(Applied::value(value));
        }
        let colors = usize::from(p.colors.clamp(1, 256));
        let all = rasters(input);
        // すでに色の数が収まるパレットの画像はそのまま
        if all.iter().all(|px| {
            px.format() == PixelFormat::Indexed8
                && px.palette().is_some_and(|pal| pal.colors.len() <= colors)
        }) {
            return Ok(Applied::value(input.clone()));
        }
        // 色の表はフレームごとに作って併合する（全フレームの RGBA を同時に持たない。作業領域は 1 フレームぶん）
        let mut parts: Vec<Vec<(u32, u64)>> = Vec::with_capacity(all.len());
        for px in &all {
            let rgba = rgba8_rows(px, mon)?;
            parts.push(histogram(&rgba, mon)?);
        }
        // 木の形で併合する（フレームの数の 2 乗にならないように）
        let hist = merge_all(parts, mon)?;
        cx.check()?;
        let palette = palette_for(&hist, colors, mon)?;
        let exact = hist.len() <= colors;
        let dither = match p.dither {
            _ if exact => Dither::None,
            DitherParam::None => Dither::None,
            DitherParam::Ordered => Dither::Ordered,
            DitherParam::ErrorDiffusion => Dither::ErrorDiffusion,
        };
        let pal = Palette {
            colors: palette.clone(),
        };
        // 組織的ディザのしきい値はキャンバスの座標で引く（トリムしたフレームでも同じ位置に同じしきい値）
        let origins: Vec<(u64, u64)> = match input {
            Value::Frames(fr) => fr
                .frames()
                .iter()
                .map(|f| (f.offset.x as u64, f.offset.y as u64))
                .collect(),
            _ => vec![(0, 0)],
        };
        // いちばん近い色の表は 1 回だけ作る（フレームごとに作り直さない）
        let lookup = match dither {
            Dither::None => palette_lookup(&hist, &palette, mon)?,
            _ => Vec::new(),
        };
        let value = map_rasters(input, mon, |i, px| {
            // RGBA はフレームごとに作り直す（持っておかない）
            let rgba = rgba8_rows(px, mon)?;
            let idx = match dither {
                Dither::None => map_with_lookup(&rgba, &hist, &lookup, mon)?,
                d => map_rgba8(&rgba, px.width(), px.height(), origins[i], &palette, d, mon)?,
            };
            Raster::from_rows(
                px.width(),
                px.height(),
                PixelFormat::Indexed8,
                Some(pal.clone()),
                &idx,
            )
        })?;
        Ok(Applied::value(value))
    }

    fn examples() -> Vec<Example<ConvertParams>> {
        let ex = |name, format, colors, dither, input| Example {
            name,
            params: ConvertParams {
                format,
                colors,
                dither,
                matte: [255, 255, 255],
            },
            inputs: vec![input],
        };
        let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
        vec![
            ex(
                "rgba16-to-gray8-across-tiles",
                PixelFormat::Gray8,
                256,
                DitherParam::None,
                noise(300, 260, PixelFormat::Rgba16, 51),
            ),
            ex(
                "rgba-to-rgb-on-white",
                PixelFormat::Rgb8,
                256,
                DitherParam::None,
                noise(9, 7, PixelFormat::Rgba8, 52),
            ),
            ex(
                "indexed-to-grayA16",
                PixelFormat::GrayA16,
                256,
                DitherParam::None,
                noise(8, 5, PixelFormat::Indexed8, 53),
            ),
            ex(
                "rgb-to-16-colors",
                PixelFormat::Indexed8,
                16,
                DitherParam::None,
                noise(20, 15, PixelFormat::Rgb8, 54),
            ),
            ex(
                "rgba-to-8-colors-ordered",
                PixelFormat::Indexed8,
                8,
                DitherParam::Ordered,
                noise(70, 66, PixelFormat::Rgba8, 55),
            ),
            ex(
                "rgb-to-4-colors-error-diffusion",
                PixelFormat::Indexed8,
                4,
                DitherParam::ErrorDiffusion,
                noise(33, 21, PixelFormat::Rgb8, 56),
            ),
            ex(
                "sprite-keeps-exact-colors",
                PixelFormat::Indexed8,
                256,
                DitherParam::ErrorDiffusion,
                InputGen::Sprite {
                    w: 12,
                    h: 9,
                    rect: IRect::new(2, 2, 5, 4),
                },
            ),
            ex(
                "trimmed-frames-ordered-canvas-phase",
                PixelFormat::Indexed8,
                4,
                DitherParam::Ordered,
                InputGen::TrimmedFrames {
                    w: 16,
                    h: 12,
                    fmt: PixelFormat::Rgb8,
                    count: 3,
                    seed: 58,
                },
            ),
            ex(
                "frames-share-one-palette",
                PixelFormat::Indexed8,
                6,
                DitherParam::None,
                InputGen::NoiseFrames {
                    w: 10,
                    h: 8,
                    fmt: PixelFormat::Rgb8,
                    count: 3,
                    seed: 57,
                },
            ),
        ]
    }

    // 段 10：範囲（Indexed8 以外は画素ごと。Indexed8 は全フレームの色の表に依るので全体）と範囲の描画
    const REGION: bool = true;

    fn reach(p: &ConvertParams, _inputs: &[Shape]) -> Reach {
        if p.format == PixelFormat::Indexed8 {
            Reach::Global
        } else {
            Reach::Same
        }
    }

    fn effect(p: &ConvertParams, inputs: &[Shape]) -> Area {
        // 同じ形式（Indexed8 以外）なら、入力をそのまま返す
        let fmt = match inputs.first() {
            Some(Shape::Image { fmt, .. } | Shape::Frames { fmt, .. }) => *fmt,
            _ => return Area::All,
        };
        if fmt == p.format && fmt != PixelFormat::Indexed8 {
            Area::Empty
        } else {
            Area::All
        }
    }

    fn apply_region(
        p: &ConvertParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        // Indexed8 へは全体の色の表が要る（reach が Global なので、実行器は全体で描く）
        if p.format == PixelFormat::Indexed8 {
            return None;
        }
        let mon = cx.mon;
        Some(
            cx.check()
                .and_then(|()| {
                    map_rasters_region(inputs[0], tiles, base, |_, px, pick, b| {
                        convert_raster_picked(px, p.format, p.matte, mon, pick, b)
                    })
                })
                .map(Applied::value),
        )
    }

    // 段 10：プレビューの段。Indexed8 へは等倍だけ（パレットの番号の意味を等倍と同じに保つ。減色は全体に依る）
    fn preview(p: &ConvertParams) -> Preview {
        if p.format == PixelFormat::Indexed8 {
            Preview::FullOnly
        } else {
            Preview::Scaled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::render_value;

    fn run(input: Value, p: ConvertParams) -> Value {
        render_value(Convert::ID, 1, serde_json::to_value(p).unwrap(), input).unwrap()
    }

    fn params(format: PixelFormat, colors: u16, dither: DitherParam) -> ConvertParams {
        ConvertParams {
            format,
            colors,
            dither,
            matte: [255, 255, 255],
        }
    }

    #[test]
    fn few_colors_survive_exactly_and_round_trip() {
        let input = InputGen::Sprite {
            w: 6,
            h: 5,
            rect: IRect::new(1, 1, 3, 2),
        }
        .generate()
        .unwrap();
        let Value::Image(idx) = run(
            input.clone(),
            params(PixelFormat::Indexed8, 256, DitherParam::ErrorDiffusion),
        ) else {
            panic!()
        };
        assert_eq!(idx.px.palette().unwrap().colors.len(), 2);
        let back = run(
            Value::Image(idx.clone()),
            params(PixelFormat::Rgba8, 256, DitherParam::None),
        );
        let (Value::Image(a), Value::Image(b)) = (&input, &back) else {
            panic!()
        };
        assert_eq!(a.px, b.px);
        // すでに収まるパレットの画像はそのまま
        let again = run(
            Value::Image(idx.clone()),
            params(PixelFormat::Indexed8, 4, DitherParam::Ordered),
        );
        assert_eq!(again, Value::Image(idx));
    }

    #[test]
    fn frames_get_one_palette() {
        let input = InputGen::NoiseFrames {
            w: 6,
            h: 6,
            fmt: PixelFormat::Rgba8,
            count: 3,
            seed: 9,
        }
        .generate()
        .unwrap();
        let Value::Frames(fr) = run(
            input,
            params(PixelFormat::Indexed8, 5, DitherParam::ErrorDiffusion),
        ) else {
            panic!()
        };
        let pal = fr.first().px.palette().cloned();
        assert!(fr.frames().iter().all(|f| f.px.palette().cloned() == pal));
        assert!(pal.unwrap().colors.len() <= 5);
    }

    /// 予算の見積もり（work_bytes）は、推論で上限と照らす式以上。Indexed8 以外は 0。値の実測（2048² を 256 色）より大きい。
    #[test]
    fn work_covers_the_infer_formula_and_the_measurement() {
        use crate::shape::{Dim, Fullness};
        let image = |w: u64, h: u64| Shape::Image {
            w: Dim::Exact(w),
            h: Dim::AtMost(h),
            fmt: PixelFormat::Rgb8,
        };
        let shapes = [
            image(1, 1),
            image(300, 200),
            image(2048, 2048),
            image(32_768, 32_768),
            image(u64::MAX, u64::MAX),
            Shape::Frames {
                count: Dim::Exact(40),
                w: Dim::Exact(256),
                h: Dim::Exact(256),
                fmt: PixelFormat::Rgba8,
                full: Fullness::Full,
            },
        ];
        for s in &shapes {
            for colors in [1, 2, 16, 17, 256] {
                let p = params(PixelFormat::Indexed8, colors, DitherParam::ErrorDiffusion);
                let w = Convert::work_bytes(&p, std::slice::from_ref(s), s);
                assert_eq!(w.per_task, 0);
                assert!(w.shared >= infer_work(s), "{s:?} {colors}");
            }
            let p = params(PixelFormat::Gray8, 256, DitherParam::None);
            assert_eq!(
                Convert::work_bytes(&p, std::slice::from_ref(s), s),
                Work::ZERO
            );
        }
        // values 調査の実測：2048² の乱数の Rgb8 を 256 色・誤差拡散で、作業領域は約 380 MiB
        let p = params(PixelFormat::Indexed8, 256, DitherParam::ErrorDiffusion);
        let s = image(2048, 2048);
        let w = Convert::work_bytes(&p, std::slice::from_ref(&s), &s).shared;
        assert_eq!(w, 512 << 20);
        assert!(w >= 380 << 20);
    }
}
