//! E26 の画像の分：意味の対・対のない解析・偽の値の作り方（`tests/analysis_ops.rs` が読む）。
//! 画像の解析を足す班は、ここに行を足す。対の検査は行（`Pair`）を受け取り、行の版の操作を当て（`Pair::apply`）、
//! 解析は登録表の（ID, 版）で引く（`Pair::analyze`）。補助に使うほかの操作は今の版で当てる。

use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Arc;

use kura_engine::analysis::NoParams;
use kura_engine::analysis::image::alpha::{Alpha, AlphaKind, AlphaLoss};
use kura_engine::analysis::image::colors::Colors;
use kura_engine::analysis::image::opaque_bounds::{OpaqueBounds, OpaqueBoundsParams};
use kura_engine::analysis::image::pixel_scale::{AxisScale, PixelScale, PixelScaleOutput};
use kura_engine::analysis::image::sprite_grid::{GridBackground, SpriteGrid};
use kura_engine::codec::{CodecSet, caps_of};
use kura_engine::doc::DecodeOptions;
use kura_engine::geom::{IPoint, IRect, Size};
use kura_engine::imaging::bounds::opaque_bounds;
use kura_engine::imaging::convert::for_each_rgba8_key;
use kura_engine::imaging::pixels::{alpha_at_most, can_be_transparent};
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use kura_engine::value::{Frame, Frames, Image, Palette, PixelFormat, Raster};
use kura_engine::{Limits, NoMonitor, Value};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

use super::{FakeField, Pair, Perturb, apply_op, current_op, err};

/// 意味の対（操作の版・解析の版ごとの行）。申告のある行は、操作が読む欄（`reads`）を書く。
pub const PAIRS: &[Pair] = &[
    Pair {
        op: "image.trim_alpha",
        op_v: 1,
        analysis: "image.opaque_bounds",
        analysis_v: 1,
        // 1 枚の画像と union は合併、perFrame はフレームごとの矩形を読む（キャンバスの大きさは読まない）
        reads: &["union", "frames"],
        check: trim_cuts_the_opaque_bounds,
    },
    Pair {
        op: "image.convert",
        op_v: 1,
        analysis: "image.colors",
        analysis_v: 1,
        reads: &[],
        check: indexed_conversion_keeps_the_counted_colors,
    },
    Pair {
        op: "image.convert",
        op_v: 1,
        analysis: "image.alpha",
        analysis_v: 1,
        reads: &[],
        check: alpha_kind_predicts_what_formats_keep,
    },
    Pair {
        op: "image.resize",
        op_v: 1,
        analysis: "image.pixel_scale",
        analysis_v: 1,
        reads: &[],
        check: nearest_round_trip_follows_the_pixel_scale,
    },
    Pair {
        op: "sprite.split_grid",
        op_v: 1,
        analysis: "image.sprite_grid",
        analysis_v: 1,
        reads: &[],
        check: split_by_the_guessed_grid_keeps_every_pixel,
    },
];

/// 対のない解析（どの操作とも意味の対を持たないもの）と理由。
pub const UNPAIRED: &[(&str, &str)] = &[];

/// 偽の値の作り方（操作が申告する解析ごとに、結果の欄ごと。JSON の結果は欄の一覧が結果の欄と同じであること）。
pub const FAKES: &[(&str, &[FakeField])] = &[(
    "image.opaque_bounds",
    &[
        FakeField {
            field: "canvas",
            perturb: Perturb::Json(wider_canvas),
        },
        FakeField {
            field: "union",
            perturb: Perturb::Json(shrink_union),
        },
        FakeField {
            field: "frames",
            perturb: Perturb::Json(shrink_frames),
        },
    ],
)];

/// キャンバス全体の矩形。
fn canvas_rect(out: &serde_json::Value) -> serde_json::Value {
    json!({ "x": 0, "y": 0, "w": out["canvas"]["w"], "h": out["canvas"]["h"] })
}

/// 矩形を 1 画素小さくする（1×1 は null に、null はキャンバス全体に）。
fn shrink(r: &mut serde_json::Value, canvas: &serde_json::Value) {
    if r.is_null() {
        *r = canvas.clone();
        return;
    }
    let (w, h) = (r["w"].as_u64().unwrap(), r["h"].as_u64().unwrap());
    if w > 1 {
        r["w"] = json!(w - 1);
    } else if h > 1 {
        r["h"] = json!(h - 1);
    } else {
        *r = serde_json::Value::Null;
    }
}

fn shrink_union(out: &mut serde_json::Value) {
    let canvas = canvas_rect(out);
    shrink(&mut out["union"], &canvas);
}

fn shrink_frames(out: &mut serde_json::Value) {
    let canvas = canvas_rect(out);
    for f in out["frames"].as_array_mut().unwrap() {
        shrink(f, &canvas);
    }
}

fn wider_canvas(out: &mut serde_json::Value) {
    let w = out["canvas"]["w"].as_u64().unwrap();
    out["canvas"]["w"] = json!(w + 1);
}

/// 切る矩形（全部が透明なら左上の 1×1）。トリムが申告して受け取る値とは別に、`imaging::bounds` で直接求める（照合用）。
fn trim_rect(px: &Raster, threshold: u8) -> Result<IRect, String> {
    Ok(opaque_bounds(px, threshold, &NoMonitor)
        .map_err(err)?
        .unwrap_or(IRect::new(0, 0, 1, 1)))
}

fn read(px: &Raster, r: IRect) -> Vec<u8> {
    let mut out = vec![0u8; r.w as usize * r.h as usize * px.bpp()];
    px.read_rect(r, &mut out).unwrap();
    out
}

fn intersect(a: IRect, b: IRect) -> Option<IRect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (x0 < x1 && y0 < y1).then(|| IRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// トリム（しきい値 t・モード）の結果が、`image.opaque_bounds`（t）の矩形で切ったものと同じ。
fn check_trim(
    engine: &Engine,
    p: &Pair,
    input: &Value,
    threshold: u8,
    mode: &str,
) -> Result<(), String> {
    let out = p.apply(
        engine,
        json!({ "threshold": threshold, "mode": mode }),
        input,
    )?;
    let b = p.analyze::<OpaqueBounds>(engine, input, &OpaqueBoundsParams { threshold })?;
    let clear = IRect::new(0, 0, 1, 1);
    let ctx = format!("しきい値 {threshold}・{mode}");
    match (input, &out) {
        (Value::Image(img), Value::Image(o)) => {
            let full = IRect::new(0, 0, img.px.width(), img.px.height());
            if b.canvas
                != (Size {
                    w: full.w,
                    h: full.h,
                })
                || b.frames != vec![b.union]
            {
                return Err(format!("{ctx}：画像の結果の形が違う {b:?}"));
            }
            let r = if can_be_transparent(img.px.format()) {
                b.union.unwrap_or(clear)
            } else {
                if b.union != Some(full) {
                    return Err(format!("{ctx}：アルファのない形式の範囲が画像全体でない"));
                }
                full
            };
            if trim_rect(&img.px, threshold)? != r && can_be_transparent(img.px.format()) {
                return Err(format!("{ctx}：imaging::bounds の矩形と解析の矩形が違う"));
            }
            if (o.px.width(), o.px.height()) != (r.w, r.h)
                || o.px.to_rows().unwrap() != read(&img.px, r)
            {
                return Err(format!("{ctx}：切った画像が解析の矩形 {r:?} と違う"));
            }
        }
        (Value::Frames(fr), Value::Frames(o)) => {
            let canvas = fr.canvas();
            if b.canvas != canvas || b.frames.len() != fr.frames().len() {
                return Err(format!("{ctx}：フレームの列の結果の形が違う"));
            }
            if !can_be_transparent(fr.first().px.format()) {
                if b.union != Some(IRect::new(0, 0, canvas.w, canvas.h)) {
                    return Err(format!(
                        "{ctx}：アルファのない形式の合併がキャンバス全体でない"
                    ));
                }
                return if out == *input {
                    Ok(())
                } else {
                    Err(format!("{ctx}：アルファのない形式を変えた"))
                };
            }
            if mode == "union" {
                let r = b.union.unwrap_or(clear);
                if o.canvas() != (Size { w: r.w, h: r.h }) {
                    return Err(format!("{ctx}：キャンバスが解析の合併 {r:?} と違う"));
                }
                for (f, g) in fr.frames().iter().zip(o.frames()) {
                    let fr_rect = IRect::new(f.offset.x, f.offset.y, f.px.width(), f.px.height());
                    let (want_px, want_at) = match intersect(fr_rect, r) {
                        Some(i) => (
                            read(
                                &f.px,
                                IRect::new(i.x - f.offset.x, i.y - f.offset.y, i.w, i.h),
                            ),
                            (
                                IPoint {
                                    x: i.x - r.x,
                                    y: i.y - r.y,
                                },
                                (i.w, i.h),
                            ),
                        ),
                        None => (read(&f.px, clear), (IPoint { x: 0, y: 0 }, (1, 1))),
                    };
                    if (g.offset, (g.px.width(), g.px.height())) != want_at
                        || g.px.to_rows().unwrap() != want_px
                    {
                        return Err(format!("{ctx}：フレームが合併で切ったものと違う"));
                    }
                }
            } else {
                if o.canvas() != canvas {
                    return Err(format!("{ctx}：フレームごとのトリムでキャンバスが変わった"));
                }
                for ((f, g), want) in fr.frames().iter().zip(o.frames()).zip(&b.frames) {
                    let r = want.unwrap_or(IRect::new(f.offset.x, f.offset.y, 1, 1));
                    let local = IRect::new(r.x - f.offset.x, r.y - f.offset.y, r.w, r.h);
                    if g.offset != (IPoint { x: r.x, y: r.y })
                        || (g.px.width(), g.px.height()) != (r.w, r.h)
                        || g.px.to_rows().unwrap() != read(&f.px, local)
                    {
                        return Err(format!(
                            "{ctx}：フレームが解析のフレームごとの矩形 {r:?} と違う"
                        ));
                    }
                }
            }
        }
        _ => return Err(format!("{ctx}：値の種類が変わった")),
    }
    Ok(())
}

/// 乱数の画素：8 割は透明、残りはアルファも乱数（形式ごとのアルファの位置に書く）。
fn sparse(r: &mut rand_chacha::ChaCha8Rng, w: u32, h: u32, fmt: PixelFormat) -> Raster {
    let bpp = fmt.bytes_per_pixel();
    let palette = (fmt == PixelFormat::Indexed8).then(|| Palette {
        colors: (0..8u8)
            .map(|i| {
                [
                    i * 30,
                    0,
                    255 - i * 30,
                    [0, 1, 7, 128, 200, 254, 255, 0][i as usize],
                ]
            })
            .collect(),
    });
    let mut data = vec![0u8; w as usize * h as usize * bpp];
    for p in data.chunks_exact_mut(bpp) {
        for b in p.iter_mut() {
            *b = r.next_u32() as u8;
        }
        let clear = r.next_u32() % 10 < 8;
        match fmt {
            PixelFormat::Rgba8 => p[3] = if clear { 0 } else { p[3] },
            PixelFormat::GrayA8 => p[1] = if clear { 0 } else { p[1] },
            PixelFormat::Rgba16 => {
                if clear {
                    p[6..8].fill(0);
                }
            }
            PixelFormat::GrayA16 => {
                if clear {
                    p[2..4].fill(0);
                }
            }
            PixelFormat::Indexed8 => p[0] = if clear { 0 } else { p[0] % 8 },
            _ => {}
        }
    }
    Raster::from_rows(w, h, fmt, palette, &data).unwrap()
}

fn trim_cuts_the_opaque_bounds(engine: &Engine, p: &Pair) -> Result<(), String> {
    // 操作の例
    for ex in p.examples(engine)? {
        let input = ex.inputs[0].generate().map_err(err)?;
        let t = ex.params["threshold"].as_u64().ok_or("threshold")? as u8;
        let mode = ex.params["mode"].as_str().ok_or("mode")?.to_string();
        check_trim(engine, p, &input, t, &mode).map_err(|e| format!("{}: {e}", ex.name))?;
    }
    // 乱数の画像とフレームの列（形式・大きさ・タイルの境・しきい値・全部が透明）
    let mut r = kura_engine::determinism::rng(26);
    let formats = [
        PixelFormat::Rgba8,
        PixelFormat::GrayA8,
        PixelFormat::GrayA16,
        PixelFormat::Rgba16,
        PixelFormat::Indexed8,
        PixelFormat::Rgb8,
        PixelFormat::Gray8,
    ];
    for case in 0..140u32 {
        let fmt = formats[case as usize % formats.len()];
        let (w, h) = if case % 23 == 0 {
            (257 + r.next_u32() % 20, 1 + r.next_u32() % 4)
        } else {
            (1 + r.next_u32() % 40, 1 + r.next_u32() % 30)
        };
        let t = [0u8, 1, 7, 128, 200, 254][case as usize % 6];
        let mut px = sparse(&mut r, w, h, fmt);
        if case % 17 == 0 && can_be_transparent(fmt) {
            // 全部が透明
            let clear = vec![0u8; w as usize * h as usize * fmt.bytes_per_pixel()];
            px = Raster::from_rows(w, h, fmt, px.palette().map(|p| (**p).clone()), &clear).unwrap();
        }
        let img = Value::Image(Arc::new(Image {
            px,
            meta: Default::default(),
        }));
        check_trim(engine, p, &img, t, "union").map_err(|e| format!("画像 {case}: {e}"))?;
        // トリムしたフレームの列（位置と大きさが違う）
        let (cw, ch) = (w + 6, h + 4);
        let count = 1 + r.next_u32() % 4;
        // Indexed8 のパレットは sparse のどの呼び出しでも同じ（フレームの列の形式・パレットがそろう）
        let frames = (0..count)
            .map(|_| {
                let (fw, fh) = (1 + r.next_u32() % w, 1 + r.next_u32() % h);
                Frame {
                    px: sparse(&mut r, fw, fh, fmt),
                    offset: IPoint {
                        x: i64::from(r.next_u32() % (cw - fw + 1)),
                        y: i64::from(r.next_u32() % (ch - fh + 1)),
                    },
                    duration_ms: 40,
                }
            })
            .collect();
        let fr = Value::Frames(Arc::new(
            Frames::new(frames, Size { w: cw, h: ch }, Default::default(), 0).unwrap(),
        ));
        for mode in ["union", "perFrame"] {
            check_trim(engine, p, &fr, t, mode).map_err(|e| format!("フレーム {case}: {e}"))?;
        }
    }
    Ok(())
}

// ---- 段 8 の画像の解析の対（image.colors・image.alpha・image.pixel_scale・image.sprite_grid） ----

/// 補助に使う操作 1 つ（今の版）を、渡したエンジンで値に当てる。
fn op(
    engine: &Engine,
    id: &str,
    params: serde_json::Value,
    input: &Value,
) -> Result<Value, String> {
    apply_op(engine, id, current_op(engine, id)?, params, input)
}

fn image(px: Raster) -> Value {
    Value::Image(Arc::new(Image {
        px,
        meta: Default::default(),
    }))
}

fn rasters(v: &Value) -> Vec<&Raster> {
    match v {
        Value::Image(img) => vec![&img.px],
        Value::Frames(fr) => fr.frames().iter().map(|f| &f.px).collect(),
        Value::Audio(_) => Vec::new(),
    }
}

/// 画素ごとの 8 ビットの色（`image.convert` の色）。
fn keys8(px: &Raster) -> Vec<u32> {
    let mut out = Vec::new();
    for_each_rgba8_key(
        px.format(),
        px.palette().map(|p| p.colors.as_slice()),
        &px.to_rows().unwrap(),
        |k| {
            out.push(k);
            true
        },
    )
    .unwrap();
    out
}

/// 行の版の解析の例の入力（登録表で引く）。
fn examples(engine: &Engine, p: &Pair) -> Result<Vec<Value>, String> {
    Ok(p.analysis_inputs(engine)?
        .into_iter()
        .map(|(_, v)| v)
        .collect())
}

/// 生の値が `m` 通りまでの乱数の画素（Indexed8 は `m` 色まで・重なる色と透明な色を含むパレット）。
fn limited(
    r: &mut ChaCha8Rng,
    w: u32,
    h: u32,
    fmt: PixelFormat,
    m: usize,
    palette: &Palette,
) -> Raster {
    let bpp = fmt.bytes_per_pixel();
    let patterns: Vec<Vec<u8>> = (0..m.max(1))
        .map(|_| (0..bpp).map(|_| r.next_u32() as u8).collect())
        .collect();
    let mut data = Vec::with_capacity(w as usize * h as usize * bpp);
    for _ in 0..w * h {
        if fmt == PixelFormat::Indexed8 {
            data.push((r.next_u32() as usize % palette.colors.len()) as u8);
        } else {
            data.extend_from_slice(&patterns[r.next_u32() as usize % patterns.len()]);
        }
    }
    let pal = (fmt == PixelFormat::Indexed8).then(|| palette.clone());
    Raster::from_rows(w, h, fmt, pal, &data).unwrap()
}

fn random_palette(r: &mut ChaCha8Rng, m: usize) -> Palette {
    let mut colors: Vec<[u8; 4]> = (0..m.clamp(1, 256))
        .map(|_| r.next_u32().to_le_bytes())
        .collect();
    // 重なる色・透明な色
    if colors.len() > 3 {
        colors[1] = colors[0];
        colors[2][3] = 0;
    }
    Palette { colors }
}

/// `image.convert`（Indexed8）↔ `image.colors`：256 色以下なら、convert のパレット＝解析の色の列（pack の小さい順）で、
/// 各画素の 8 ビットの色が変わらない（減色しない）。色の数を超える上限なら、パレットはその数以下。Indexed8 にして
/// 元の形式に戻すと元に戻るのは `paletteLossless` のときだけ。数えた色は、素朴に数えた集合と同じ。
fn indexed_conversion_keeps_the_counted_colors(engine: &Engine, p: &Pair) -> Result<(), String> {
    let mut values = examples(engine, p)?;
    let mut r = kura_engine::determinism::rng(261);
    for case in 0..72u32 {
        let fmt = PixelFormat::ALL[case as usize % PixelFormat::ALL.len()];
        let m = [1usize, 2, 5, 17, 255, 256, 257, 300][(case / 9) as usize % 8];
        let (w, h) = if case % 11 == 0 {
            (257 + r.next_u32() % 9, 2)
        } else {
            (1 + r.next_u32() % 30, 1 + r.next_u32() % 20)
        };
        let pal = random_palette(&mut r, m);
        values.push(image(limited(&mut r, w, h, fmt, m, &pal)));
        if case % 4 == 0 {
            // トリムしたフレームの列（同じパレット）
            let frames = (0..3)
                .map(|i| Frame {
                    px: limited(&mut r, (w / 2).max(1), (h / 2).max(1), fmt, m, &pal),
                    offset: IPoint {
                        x: i64::from(i % 2),
                        y: i64::from(i % 3),
                    },
                    duration_ms: 50,
                })
                .collect();
            values.push(Value::Frames(Arc::new(
                Frames::new(frames, Size { w: w + 2, h: h + 3 }, Default::default(), 0)
                    .map_err(err)?,
            )));
        }
    }
    for (vi, v) in values.iter().enumerate() {
        let ctx = |m: &str| format!("値 {vi}：{m}");
        let c = p.analyze::<Colors>(engine, v, &NoParams {})?;
        let ins = rasters(v);
        let fmt = ins[0].format();
        // 素朴に数えた 8 ビットの色の集合
        let set: BTreeSet<u32> = ins.iter().flat_map(|px| keys8(px)).collect();
        let want_count = set.len().min(257) as u16;
        if c.count != want_count {
            return Err(ctx(&format!("色の数 {} と素朴な数 {want_count}", c.count)));
        }
        let want_colors: Option<Vec<[u8; 4]>> =
            (set.len() <= 256).then(|| set.iter().map(|k| k.to_be_bytes()).collect());
        if c.colors != want_colors {
            return Err(ctx("色の列が素朴な集合と違う"));
        }
        let mut ns = vec![256u16, c.count.min(256), c.count.saturating_sub(1).max(1)];
        ns.dedup();
        for n in ns {
            let out = p.apply(
                engine,
                json!({ "format": "indexed8", "colors": n, "dither": "none", "matte": [255, 255, 255] }),
                v,
            )?;
            let outs = rasters(&out);
            let pal = outs[0]
                .palette()
                .ok_or("出力にパレットがない")?
                .colors
                .clone();
            if fmt == PixelFormat::Indexed8 {
                let in_len = ins[0].palette().map_or(0, |p| p.colors.len());
                if in_len <= usize::from(n)
                    && out.digest().map_err(err)? != v.digest().map_err(err)?
                {
                    return Err(ctx("色の数が収まる Indexed8 を convert が変えた"));
                }
                continue;
            }
            if c.count <= n {
                if Some(&pal) != c.colors.as_ref() {
                    return Err(ctx(&format!(
                        "上限 {n}：convert のパレットが解析の色の列と違う"
                    )));
                }
                for (a, b) in ins.iter().zip(&outs) {
                    if keys8(a) != keys8(b) {
                        return Err(ctx(&format!("上限 {n}：convert が色を変えた（減色した）")));
                    }
                }
                let again = p.analyze::<Colors>(engine, &out, &NoParams {})?;
                if (again.count, &again.colors) != (c.count, &c.colors) {
                    return Err(ctx("convert の出力の色の数・列が入力と違う"));
                }
            } else {
                let again = p.analyze::<Colors>(engine, &out, &NoParams {})?;
                if pal.len() > usize::from(n) || again.count > n {
                    return Err(ctx(&format!("上限 {n}：減色したパレットが上限を超える")));
                }
            }
        }
        if c.count <= 256 && fmt != PixelFormat::Indexed8 {
            let idx = p.apply(
                engine,
                json!({ "format": "indexed8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
                v,
            )?;
            let fmt_name = serde_json::to_value(fmt).map_err(err)?;
            let back = p.apply(
                engine,
                json!({ "format": fmt_name, "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
                &idx,
            )?;
            let same = back.digest().map_err(err)? == v.digest().map_err(err)?;
            if same != c.palette_lossless {
                return Err(ctx(&format!(
                    "Indexed8 の往復で元に戻る（{same}）と paletteLossless（{}）が違う",
                    c.palette_lossless
                )));
            }
        }
    }
    Ok(())
}

/// `image.alpha` ↔ 形式の `MetaCaps.alpha`：`loss_in` の予告どおりに書き出しで透明が残る・失われる。
/// convert でアルファのない形式にするとアルファは none、8 ビットから 16 ビットへは数が変わらない。
fn alpha_kind_predicts_what_formats_keep(engine: &Engine, p: &Pair) -> Result<(), String> {
    let mut values: Vec<Value> = examples(engine, p)?
        .into_iter()
        .filter(|v| matches!(v, Value::Image(_)))
        .collect();
    let mut r = kura_engine::determinism::rng(262);
    for fmt in [
        PixelFormat::Rgba8,
        PixelFormat::GrayA8,
        PixelFormat::Indexed8,
        PixelFormat::Rgb8,
    ] {
        for m in [1usize, 3, 40] {
            let pal = random_palette(&mut r, m);
            values.push(image(limited(&mut r, 9, 7, fmt, m, &pal)));
        }
    }
    // none・binary・partial の Rgba8（アルファだけ違う）
    for alphas in [[255u8, 255], [0, 255], [0, 128]] {
        let data: Vec<u8> = (0..12)
            .flat_map(|i| [i as u8 * 9, 7, 200, alphas[i % 2]])
            .collect();
        values.push(image(
            Raster::from_rows(4, 3, PixelFormat::Rgba8, None, &data).map_err(err)?,
        ));
    }
    let codecs = CodecSet::pinned();
    for (vi, v) in values.iter().enumerate() {
        let ctx = |m: &str| format!("値 {vi}：{m}");
        let a = p.analyze::<Alpha>(engine, v, &NoParams {})?;
        let fmt = rasters(v)[0].format();
        // convert：アルファのない形式は none、8 → 16 ビットとアルファ付きの 8 ビットどうしは数が同じ
        for to in PixelFormat::ALL {
            if to == PixelFormat::Indexed8 {
                continue;
            }
            let name = serde_json::to_value(to).map_err(err)?;
            let out = p.apply(
                engine,
                json!({ "format": name, "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
                v,
            )?;
            let b = p.analyze::<Alpha>(engine, &out, &NoParams {})?;
            let keeps = to.has_alpha() && (!is16(fmt) || is16(to));
            if !to.has_alpha() && b.kind != AlphaKind::None {
                return Err(ctx(&format!("{to:?} に変えてもアルファがある")));
            }
            if keeps && (b.transparent, b.partial, b.opaque) != (a.transparent, a.partial, a.opaque)
            {
                return Err(ctx(&format!("{to:?} に変えると透明の数が変わった")));
            }
        }
        // 書き出し：予告（loss_in）と、書いて読み直した値の透明が合う
        if is16(fmt) {
            continue;
        }
        for format in ["png", "gif", "webp", "jpeg"] {
            let loss = a.loss_in(caps_of(format).alpha);
            let encoded = codecs.encode_bytes(v, format, &json!({}), &NoMonitor);
            let bytes = match encoded {
                Ok((bytes, _)) => bytes,
                // JPEG は透明を持てない画像を断る（予告は Removed）
                Err(_) if loss == Some(AlphaLoss::Removed) => continue,
                Err(e) => return Err(ctx(&format!("{format} に書けない（予告 {loss:?}）: {e}"))),
            };
            if loss == Some(AlphaLoss::Removed) {
                return Err(ctx(&format!("{format}：透明が失われる予告なのに書けた")));
            }
            let (back, _) = codecs
                .decode(
                    &mut Cursor::new(bytes),
                    format,
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .map_err(err)?;
            let b = p.analyze::<Alpha>(engine, &back, &NoParams {})?;
            match loss {
                None if format == "jpeg" => {
                    if b.kind != AlphaKind::None {
                        return Err(ctx("jpeg を読み直すと透明がある"));
                    }
                }
                None => {
                    if (b.transparent, b.partial) != (a.transparent, a.partial) {
                        return Err(ctx(&format!(
                            "{format}：失わない予告なのに透明の数が変わった"
                        )));
                    }
                }
                Some(AlphaLoss::Binarized) => {
                    if b.partial != 0 || b.kind == AlphaKind::Partial {
                        return Err(ctx(&format!(
                            "{format}：2 値になる予告なのに半透明が残った"
                        )));
                    }
                }
                Some(AlphaLoss::Removed) => {}
            }
        }
    }
    Ok(())
}

fn is16(fmt: PixelFormat) -> bool {
    kura_engine::imaging::pixels::is_16(fmt)
}

/// 倍率を k 倍にした軸（最近傍で k 倍に上げると、変わり目の位置が k 倍になる）。
fn scaled_axis(a: Option<AxisScale>, k: u32) -> Option<AxisScale> {
    a.map(|a| AxisScale {
        k: a.k * k,
        phase: a.phase * k,
        lines: a.lines,
    })
}

/// 格子に合わせて切り抜き（位相から k の倍数の長さ）、最近傍で 1/k にして k 倍に戻すと元と同じか。
/// 格子に合わせると空になるときは真（照らすものがない）。
fn round_trip_restores(
    engine: &Engine,
    p: &Pair,
    v: &Value,
    ps: &PixelScaleOutput,
) -> Result<bool, String> {
    let Value::Image(img) = v else {
        return Err("画像ではない".into());
    };
    let (w, h) = (img.px.width(), img.px.height());
    let (kx, px) = ps.x.map_or((1, 0), |a| (a.k, a.phase));
    let (ky, py) = ps.y.map_or((1, 0), |a| (a.k, a.phase));
    let (cw, ch) = ((w - px) / kx * kx, (h - py) / ky * ky);
    if cw == 0 || ch == 0 {
        return Ok(true);
    }
    let cut = op(
        engine,
        "image.crop",
        json!({ "rect": { "x": px, "y": py, "w": cw, "h": ch } }),
        v,
    )?;
    let resize = |out_w: u32, out_h: u32, input: &Value| {
        p.apply(
            engine,
            json!({ "width": { "px": out_w }, "height": { "px": out_h }, "filter": "nearest" }),
            input,
        )
    };
    let down = resize(cw / kx, ch / ky, &cut)?;
    let up = resize(cw, ch, &down)?;
    let rows = |v: &Value| rasters(v)[0].to_rows().unwrap();
    Ok(rows(&up) == rows(&cut))
}

/// lossless なら、格子に合わせて切り抜き 1/k → k 倍に戻すとビット単位で同じ。
fn round_trip(engine: &Engine, p: &Pair, v: &Value, ps: &PixelScaleOutput) -> Result<(), String> {
    if round_trip_restores(engine, p, v, ps)? {
        Ok(())
    } else {
        Err(format!("倍率・位相 {ps:?} で 1/k → k 倍に戻らない"))
    }
}

/// `image.resize`（最近傍）↔ `image.pixel_scale`：k 倍に上げた画像の倍率と位相は元の k 倍（変わり目の数と lossless は
/// 同じ）。lossless なら、格子に合わせて切り抜き 1/k → k 倍に戻すとビット単位で同じ（切り抜いて位相がずれても）。
/// k ≥ 2 で上げたあとブロックの中の透明な画素の RGB だけを変えると、倍率と位相は同じで lossless は偽、1/k → k 倍に
/// 戻すと元に戻らない。
fn nearest_round_trip_follows_the_pixel_scale(engine: &Engine, p: &Pair) -> Result<(), String> {
    let mut bases: Vec<Value> = [
        PixelFormat::Rgba8,
        PixelFormat::Indexed8,
        PixelFormat::Rgb8,
        PixelFormat::GrayA16,
        PixelFormat::Gray8,
    ]
    .iter()
    .enumerate()
    .map(|(i, fmt)| {
        InputGen::Noise {
            w: 9,
            h: 7,
            fmt: *fmt,
            seed: 270 + i as u64,
        }
        .generate()
        .unwrap()
    })
    .collect();
    bases.extend(examples(engine, p)?.into_iter().filter(
        |v| matches!(v, Value::Image(img) if img.px.width() <= 64 && img.px.height() <= 64),
    ));
    // 透明な画素の RGB がばらばらの絵。透明な画素は縦にも横にも隣り合わない（i % 3 == 0）ので、見た目の変わり目と
    // 生の値の変わり目は同じ（倍率 1・lossless は真）。左上 (0, 0) は透明
    let mut r = kura_engine::determinism::rng(263);
    let data: Vec<u8> = (0..8 * 6)
        .flat_map(|i| {
            let v = r.next_u32().to_le_bytes();
            if i % 3 == 0 {
                [v[0], v[1], v[2], 0]
            } else {
                [v[0] | 1, 9, 9, 255]
            }
        })
        .collect();
    let dotted = image(Raster::from_rows(8, 6, PixelFormat::Rgba8, None, &data).map_err(err)?);
    bases.push(dotted.clone());
    for (bi, base) in bases.iter().enumerate() {
        let Value::Image(img) = base else { continue };
        let (w, h) = (img.px.width(), img.px.height());
        let ps0 = p.analyze::<PixelScale>(engine, base, &NoParams {})?;
        for (kx, ky) in [(1u32, 1u32), (2, 2), (3, 2), (1, 4), (5, 3)] {
            let ctx = |m: &str| format!("元 {bi}・倍率 ({kx}, {ky})：{m}");
            let up = p.apply(
                engine,
                json!({ "width": { "px": w * kx }, "height": { "px": h * ky }, "filter": "nearest" }),
                base,
            )?;
            let ps = p.analyze::<PixelScale>(engine, &up, &NoParams {})?;
            let want = PixelScaleOutput {
                x: scaled_axis(ps0.x, kx),
                y: scaled_axis(ps0.y, ky),
                lossless: ps0.lossless,
            };
            if ps != want {
                return Err(ctx(&format!("倍率が元の k 倍でない：{ps:?}（元 {ps0:?}）")));
            }
            if ps.lossless {
                round_trip(engine, p, &up, &ps).map_err(|e| ctx(&e))?;
            }
            // 切り抜いて位相をずらす
            let Value::Image(ui) = &up else { continue };
            let (uw, uh) = (ui.px.width(), ui.px.height());
            if uw > 3 && uh > 3 {
                let rect = IRect::new(1, 2 % i64::from(uh - 1), uw - 2, uh - 2);
                let cut = op(engine, "image.crop", json!({ "rect": rect }), &up)?;
                let pc = p.analyze::<PixelScale>(engine, &cut, &NoParams {})?;
                if pc.lossless {
                    round_trip(engine, p, &cut, &pc).map_err(|e| ctx(&e))?;
                }
            }
        }
    }
    // lossless が偽になる絵：透明な画素の多い絵を k ≥ 2 で上げ、左上の透明なブロックの中の 1 画素
    // （(min(1, kx − 1), min(1, ky − 1))）の RGB だけを変える。見た目は変わらないので倍率と位相は上げたものと同じ。
    // 生の値の変わり目がブロックの中（格子の外）にできるので lossless は偽で、1/k → k 倍に戻すと元に戻らない
    let (w, h) = (8u32, 6u32);
    let ps0 = p.analyze::<PixelScale>(engine, &dotted, &NoParams {})?;
    if !ps0.lossless {
        return Err(format!("透明な画素の多い絵の lossless が偽：{ps0:?}"));
    }
    for (kx, ky) in [(2u32, 2u32), (3, 2), (1, 4), (5, 3)] {
        let ctx = |m: &str| format!("透明な画素の RGB を変えた絵・倍率 ({kx}, {ky})：{m}");
        let up = p.apply(
            engine,
            json!({ "width": { "px": w * kx }, "height": { "px": h * ky }, "filter": "nearest" }),
            &dotted,
        )?;
        let clean = p.analyze::<PixelScale>(engine, &up, &NoParams {})?;
        if !clean.lossless {
            return Err(ctx(&format!("上げただけの絵の lossless が偽：{clean:?}")));
        }
        let Value::Image(ui) = &up else {
            return Err(ctx("画像ではない"));
        };
        let (uw, uh) = (ui.px.width(), ui.px.height());
        let mut rows = ui.px.to_rows().map_err(err)?;
        let (ox, oy) = (1.min(kx - 1), 1.min(ky - 1));
        let at = (oy as usize * uw as usize + ox as usize) * 4;
        if rows[at + 3] != 0 {
            return Err(ctx("変える画素が透明でない"));
        }
        rows[at] ^= 0x55;
        rows[at + 1] ^= 0xaa;
        let changed =
            image(Raster::from_rows(uw, uh, PixelFormat::Rgba8, None, &rows).map_err(err)?);
        let ps = p.analyze::<PixelScale>(engine, &changed, &NoParams {})?;
        if (ps.x, ps.y) != (clean.x, clean.y) {
            return Err(ctx(&format!(
                "透明な画素の RGB で倍率が変わった：{ps:?}（上げただけ {clean:?}）"
            )));
        }
        if ps.lossless {
            return Err(ctx(&format!(
                "ブロックの中で生の値が違うのに lossless が真：{ps:?}"
            )));
        }
        if round_trip_restores(engine, p, &changed, &ps)? {
            return Err(ctx("lossless が偽なのに 1/k → k 倍で元に戻った"));
        }
    }
    Ok(())
}

/// 中身の画素の数（`background` の空の決め方で。色の背景は四隅の生の値と比べる）。
fn content_pixels(px: &Raster, background: &GridBackground, corner: &[u8]) -> u64 {
    let rows = px.to_rows().unwrap();
    let pal = px.palette().map(|p| p.colors.as_slice());
    rows.chunks_exact(px.bpp())
        .filter(|p| match background {
            GridBackground::Transparent => !alpha_at_most(px.format(), pal, p, 0),
            _ => *p != corner,
        })
        .count() as u64
}

/// `sprite.split_grid` ↔ `image.sprite_grid`：推定した格子（いちばんの候補とほかの候補）はそのまま `split_grid` に
/// 渡せて、中身の画素を 1 つも落とさない。透明が空なら `skipEmpty` で残るセルの数＝ `filled`。`pack_grid` で並べた
/// シートからは、並べた格子を当てて元のフレームに戻る。
fn split_by_the_guessed_grid_keeps_every_pixel(engine: &Engine, p: &Pair) -> Result<(), String> {
    let mut sheets = examples(engine, p)?;
    let mut r = kura_engine::determinism::rng(264);
    for case in 0..40u64 {
        let n = |r: &mut ChaCha8Rng, lo: u32, hi: u32| lo + r.next_u32() % (hi - lo + 1);
        let (cols, rows) = (n(&mut r, 1, 6), n(&mut r, 1, 4));
        let empty = (0..cols * rows)
            .filter(|_| r.next_u32().is_multiple_of(5))
            .collect();
        let margin = n(&mut r, 0, 4);
        sheets.push(
            InputGen::SpriteSheet {
                cols,
                rows,
                cell_w: n(&mut r, 3, 20),
                cell_h: n(&mut r, 3, 20),
                gap_x: n(&mut r, 0, 3),
                gap_y: n(&mut r, 0, 3),
                margin_x: margin,
                margin_y: n(&mut r, 0, 4),
                pad_right: if case % 2 == 0 {
                    margin
                } else {
                    n(&mut r, 0, 4)
                },
                pad_bottom: n(&mut r, 0, 4),
                background: (case % 3 == 0).then_some([0, 128, 128, 255]),
                full: case % 4 == 1,
                empty,
                seed: case,
            }
            .generate()
            .map_err(err)?,
        );
    }
    for (si, sheet) in sheets.iter().enumerate() {
        let ctx = |m: &str| format!("シート {si}：{m}");
        let Value::Image(img) = sheet else { continue };
        let out = p.analyze::<SpriteGrid>(engine, sheet, &NoParams {})?;
        let mut corner = vec![0u8; img.px.bpp()];
        img.px
            .read_rect(IRect::new(0, 0, 1, 1), &mut corner)
            .map_err(err)?;
        let content = content_pixels(&img.px, &out.background, &corner);
        for g in out.best.iter().chain(&out.alternatives) {
            let split = p.apply(
                engine,
                json!({ "grid": g.grid, "durationMs": 100, "skipEmpty": false }),
                sheet,
            )?;
            let cells = rasters(&split);
            if cells.len() != g.cols as usize * g.rows as usize {
                return Err(ctx(&format!(
                    "セルの数 {} が cols × rows と違う",
                    cells.len()
                )));
            }
            let kept: u64 = cells
                .iter()
                .map(|px| content_pixels(px, &out.background, &corner))
                .sum();
            if kept != content {
                return Err(ctx(&format!(
                    "格子 {:?} で分けると中身が {content} から {kept} になった",
                    g.grid
                )));
            }
            if out.background == GridBackground::Transparent {
                let skipped = p.apply(
                    engine,
                    json!({ "grid": g.grid, "durationMs": 100, "skipEmpty": true }),
                    sheet,
                )?;
                if rasters(&skipped).len() != g.filled.max(1) as usize {
                    return Err(ctx("skipEmpty で残るセルの数が filled と違う"));
                }
            }
        }
    }
    // pack_grid で並べたシート（間隔・最後の行の空き・透明の地）→ 並べた格子 → 元のフレーム
    for (seed, columns, gx, gy) in [(1u64, 3u32, 2u32, 3u32), (2, 2, 1, 1), (3, 5, 4, 2)] {
        let frames = InputGen::NoiseFrames {
            w: 12,
            h: 10,
            fmt: PixelFormat::Rgba8,
            count: 5,
            seed,
        }
        .generate()
        .map_err(err)?;
        let sheet = op(
            engine,
            "sprite.pack_grid",
            json!({ "columns": columns, "gapX": gx, "gapY": gy, "fill": null }),
            &frames,
        )?;
        let out = p.analyze::<SpriteGrid>(engine, &sheet, &NoParams {})?;
        let best = out.best.ok_or("並べたシートの格子が見つからない")?;
        let rows = 5u32.div_ceil(columns);
        let want = kura_engine::ops::util::GridParam {
            cell_w: 12,
            cell_h: 10,
            offset_x: 0,
            offset_y: 0,
            gap_x: if columns > 1 { gx } else { 0 },
            gap_y: if rows > 1 { gy } else { 0 },
        };
        if best.grid != want || best.filled != 5 {
            return Err(format!(
                "並べた格子 {want:?} でなく {:?}（filled {}）",
                best.grid, best.filled
            ));
        }
        let back = p.apply(
            engine,
            json!({ "grid": best.grid, "durationMs": 100, "skipEmpty": true }),
            &sheet,
        )?;
        let (a, b) = (rasters(&frames), rasters(&back));
        if a.len() != b.len()
            || a.iter()
                .zip(&b)
                .any(|(x, y)| x.to_rows().unwrap() != y.to_rows().unwrap())
        {
            return Err(format!(
                "並べたシートを推定した格子で分けても元のフレームに戻らない（{seed}）"
            ));
        }
    }
    Ok(())
}
