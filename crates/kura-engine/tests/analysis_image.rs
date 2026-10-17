//! 画像の解析（`image.alpha`・`image.colors`・`image.pixel_scale`・`image.sprite_grid`）の性質テスト
//! （docs/editing.md §11）。
//!
//! - 素朴な参照実装（画像全体を 1 画素ずつ見る。タイルを使わない）との照合：ChaCha8 の乱数の画像 × 9 形式 ×
//!   大きさ（1×1・タイルの境をまたぐ 255〜257・600×300）、フレームの列（いっぱい・トリムしたもの）。タイルの分け方に
//!   依らないことも、これで確かめる（スレッド数に依らないことは `analysis_conformance.rs` の E1）。
//! - 例の値を直接確かめる（ゴールデンに凍結する前の正しさ）。
//! - 合成の検査集：最近傍で拡大したドット絵・切り抜き・JPEG を通したもの・平滑な拡大・1 点の書き足し、
//!   スプライトシート（試作の 12 種）・不規則な配置。
//! - 16384² の時間（`#[ignore]`）：`cargo test --release -p kura-engine --test analysis_image -- --ignored --nocapture`

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Arc;

use kura_engine::analysis::image::alpha::{Alpha, AlphaKind, AlphaOutput};
use kura_engine::analysis::image::colors::{Colors, ColorsOutput, PaletteUse};
use kura_engine::analysis::image::pixel_scale::{AxisScale, PixelScale, PixelScaleOutput};
use kura_engine::analysis::image::sprite_grid::{GridBackground, GridSource, SpriteGrid};
use kura_engine::analysis::{Analysis, NoParams};
use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::geom::{IPoint, IRect, Size};
use kura_engine::imaging::convert::convert_pixel;
use kura_engine::imaging::pixels::{channel_max, is_16, read_rgba};
use kura_engine::imaging::quantize::pack;
use kura_engine::ops::util::GridParam;
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use kura_engine::testing::render_value;
use kura_engine::value::{Frame, Frames, Image, Palette, PixelFormat, Raster};
use kura_engine::{Limits, NoMonitor, Value};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

fn engine() -> Engine {
    Engine::new().unwrap()
}

fn analyze<A: Analysis<Params = NoParams>>(v: &Value) -> A::Output {
    engine()
        .analyze::<A>(v, &NoParams {}, &NoMonitor)
        .unwrap_or_else(|e| panic!("{}: {e}", A::ID))
}

fn image(px: Raster) -> Value {
    Value::Image(Arc::new(Image {
        px,
        meta: Default::default(),
    }))
}

/// (画素, キャンバスの中の左上) の並びとキャンバス。
fn planes(v: &Value) -> (Size, Vec<(Raster, i64, i64)>) {
    match v {
        Value::Image(img) => (
            Size {
                w: img.px.width(),
                h: img.px.height(),
            },
            vec![(img.px.clone(), 0, 0)],
        ),
        Value::Frames(fr) => (
            fr.canvas(),
            fr.frames()
                .iter()
                .map(|f| (f.px.clone(), f.offset.x, f.offset.y))
                .collect(),
        ),
        Value::Audio(_) => panic!("音声"),
    }
}

fn pixels(px: &Raster) -> Vec<Vec<u8>> {
    px.to_rows()
        .unwrap()
        .chunks_exact(px.bpp())
        .map(<[u8]>::to_vec)
        .collect()
}

// ---- 素朴な参照実装 ----

fn naive_alpha(v: &Value) -> AlphaOutput {
    let (canvas, planes) = planes(v);
    let (mut t, mut p, mut o, mut u) = (0u64, 0u64, 0u64, 0u64);
    for (px, _, _) in &planes {
        let pal = px.palette().map(|p| p.colors.as_slice());
        let max = channel_max(px.format());
        for q in pixels(px) {
            let a = read_rgba(px.format(), pal, &q).unwrap()[3];
            if a == 0 {
                t += 1;
            } else if a == max {
                o += 1;
            } else {
                p += 1;
            }
        }
        u += u64::from(canvas.w) * u64::from(canvas.h)
            - u64::from(px.width()) * u64::from(px.height());
    }
    let kind = if p > 0 {
        AlphaKind::Partial
    } else if t + u > 0 {
        AlphaKind::Binary
    } else {
        AlphaKind::None
    };
    AlphaOutput {
        kind,
        transparent: t,
        partial: p,
        opaque: o,
        uncovered: u,
    }
}

fn naive_colors(v: &Value) -> ColorsOutput {
    let (_, planes) = planes(v);
    let mut set = BTreeSet::new();
    let mut exact = true;
    let mut used = BTreeSet::new();
    for (px, _, _) in &planes {
        let pal = px.palette().map(|p| p.colors.as_slice());
        for q in pixels(px) {
            let mut out = [0u8; 4];
            convert_pixel(
                px.format(),
                pal,
                &q,
                PixelFormat::Rgba8,
                [0, 0, 0],
                &mut out,
            )
            .unwrap();
            set.insert(pack(out));
            if is_16(px.format()) {
                exact &= q
                    .chunks_exact(2)
                    .all(|c| u16::from_le_bytes([c[0], c[1]]) % 257 == 0);
            }
            if px.format() == PixelFormat::Indexed8 {
                used.insert(q[0]);
            }
        }
    }
    let count = set.len().min(257) as u16;
    let clear = set.iter().filter(|k| *k & 0xff == 0).count();
    let visible = (set.len() - clear + usize::from(clear > 0)).min(257) as u16;
    let fmt = planes[0].0.format();
    ColorsOutput {
        count,
        colors: (set.len() <= 256).then(|| set.iter().map(|k| k.to_be_bytes()).collect()),
        visible,
        palette_lossless: set.len() <= 256 && exact,
        palette: (fmt == PixelFormat::Indexed8).then(|| PaletteUse {
            len: planes[0].0.palette().unwrap().colors.len() as u16,
            used: used.len() as u16,
        }),
    }
}

fn naive_pixel_scale(v: &Value) -> PixelScaleOutput {
    let (canvas, planes) = planes(v);
    let (mut ex, mut rx) = (BTreeSet::new(), BTreeSet::new());
    let (mut ey, mut ry) = (BTreeSet::new(), BTreeSet::new());
    for (px, ox, oy) in &planes {
        let pal = px.palette().map(|p| p.colors.as_slice());
        let fmt = px.format();
        let all = pixels(px);
        let (w, h) = (px.width() as usize, px.height() as usize);
        let look = |q: &[u8]| -> Option<[u32; 4]> {
            let c = read_rgba(fmt, pal, q).unwrap();
            // 透明はひとつの見た目（None）。Indexed8 は色で比べる
            (c[3] != 0).then_some(c)
        };
        let vis_eq = |a: &[u8], b: &[u8]| {
            if fmt == PixelFormat::Indexed8 || kura_engine::imaging::pixels::can_be_transparent(fmt)
            {
                a == b || look(a) == look(b) && (fmt == PixelFormat::Indexed8 || look(a).is_none())
            } else {
                a == b
            }
        };
        for y in 0..h {
            for x in 0..w {
                let p = &all[y * w + x];
                if x > 0 {
                    let l = &all[y * w + x - 1];
                    let gx = (ox + x as i64) as u32;
                    if p != l {
                        rx.insert(gx);
                    }
                    if !vis_eq(p, l) {
                        ex.insert(gx);
                    }
                }
                if y > 0 {
                    let u = &all[(y - 1) * w + x];
                    let gy = (oy + y as i64) as u32;
                    if p != u {
                        ry.insert(gy);
                    }
                    if !vis_eq(p, u) {
                        ey.insert(gy);
                    }
                }
            }
        }
    }
    let _ = canvas;
    let axis = |e: &BTreeSet<u32>, r: &BTreeSet<u32>| -> (Option<AxisScale>, bool) {
        if e.len() < 2 {
            return (None, r == e);
        }
        let first = *e.first().unwrap();
        let mut g = 0u32;
        for v in e {
            let (mut a, mut b) = (g, v - first);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            g = a;
        }
        let phase = first % g;
        (
            Some(AxisScale {
                k: g,
                phase,
                lines: e.len() as u32,
            }),
            r.iter().all(|v| v % g == phase),
        )
    };
    let (x, lx) = axis(&ex, &rx);
    let (y, ly) = axis(&ey, &ry);
    PixelScaleOutput {
        x,
        y,
        lossless: lx && ly,
    }
}

// ---- 乱数の画像 ----

/// 乱数の画素。`clear` 割は透明（アルファのある形式）、Indexed8 は重なる色・透明な色・使わない色を含む 12 色のパレット。
/// `levels` で値の種類を絞る（変わり目・色の数が意味を持つように）。
fn random_raster(
    r: &mut ChaCha8Rng,
    w: u32,
    h: u32,
    fmt: PixelFormat,
    levels: u32,
    palette: &Palette,
) -> Raster {
    let bpp = fmt.bytes_per_pixel();
    let mut data = Vec::with_capacity(w as usize * h as usize * bpp);
    let alpha_at: Option<usize> = match fmt {
        PixelFormat::GrayA8 => Some(1),
        PixelFormat::Rgba8 => Some(3),
        PixelFormat::GrayA16 => Some(2),
        PixelFormat::Rgba16 => Some(6),
        _ => None,
    };
    for _ in 0..w * h {
        if fmt == PixelFormat::Indexed8 {
            data.push((r.next_u32() % (palette.colors.len() as u32 - 2)) as u8);
            continue;
        }
        let mut p: Vec<u8> = (0..bpp)
            .map(|_| ((r.next_u32() % levels) * (255 / levels.max(1))) as u8)
            .collect();
        if let Some(at) = alpha_at {
            let sel = r.next_u32() % 4;
            let a: u16 = match sel {
                0 => 0,
                1 | 2 => u16::MAX,
                _ => (r.next_u32() % 65_536) as u16,
            };
            if is_16(fmt) {
                p[at..at + 2].copy_from_slice(&a.to_le_bytes());
            } else {
                p[at] = (a >> 8) as u8;
            }
        }
        data.extend_from_slice(&p);
    }
    let pal = (fmt == PixelFormat::Indexed8).then(|| palette.clone());
    Raster::from_rows(w, h, fmt, pal, &data).unwrap()
}

fn test_palette() -> Palette {
    Palette {
        colors: vec![
            [1, 2, 3, 255],
            [1, 2, 3, 255],
            [0, 0, 0, 0],
            [9, 9, 9, 0],
            [200, 10, 10, 128],
            [5, 6, 7, 255],
            [50, 60, 70, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [17, 17, 17, 1],
            // 使わない色（番号は len − 2 まで）
            [1, 1, 1, 0],
            [2, 2, 2, 7],
        ],
    }
}

/// 照らす値の一覧：9 形式 × いろいろな大きさの画像と、フレームの列。
fn random_values() -> Vec<Value> {
    let mut r = kura_engine::determinism::rng(800);
    let pal = test_palette();
    let mut out = Vec::new();
    let sizes = [
        (1, 1),
        (255, 3),
        (256, 2),
        (257, 5),
        (3, 257),
        (600, 300),
        (13, 9),
    ];
    for (i, fmt) in PixelFormat::ALL.iter().enumerate() {
        for (j, (w, h)) in sizes.iter().enumerate() {
            if (w * h > 10_000) && (i + j) % 3 != 0 {
                continue;
            }
            let levels = [2, 3, 256][(i + j) % 3];
            out.push(image(random_raster(&mut r, *w, *h, *fmt, levels, &pal)));
        }
        // フレームの列（トリムしたもの・いっぱいのもの）
        let frames = (0..3)
            .map(|k| Frame {
                px: random_raster(&mut r, 5 + k, 4, *fmt, 3, &pal),
                offset: IPoint {
                    x: i64::from(k),
                    y: i64::from(k % 2),
                },
                duration_ms: 30,
            })
            .collect();
        out.push(Value::Frames(Arc::new(
            Frames::new(frames, Size { w: 9, h: 6 }, Default::default(), 0).unwrap(),
        )));
        let full = (0..2)
            .map(|_| (random_raster(&mut r, 260, 3, *fmt, 2, &pal), 40))
            .collect();
        out.push(Value::Frames(Arc::new(
            Frames::full(full, Default::default(), 0).unwrap(),
        )));
    }
    out
}

#[test]
fn analyses_match_the_naive_references() {
    for (i, v) in random_values().iter().enumerate() {
        assert_eq!(analyze::<Alpha>(v), naive_alpha(v), "alpha {i}");
        assert_eq!(analyze::<Colors>(v), naive_colors(v), "colors {i}");
        assert_eq!(
            analyze::<PixelScale>(v),
            naive_pixel_scale(v),
            "pixel_scale {i}"
        );
    }
}

/// 例（ゴールデンに凍結するもの）はどれも素朴な参照実装と同じ：256・257 色ちょうどの境（アルファが 0 でない色だけ・
/// 0 の色だけ・両方・タイルをまたぐ和集合）、透明で RGB の違う画素の `lossless`、片方の軸だけ null など。
#[test]
fn examples_match_the_naive_references() {
    for ex in Alpha::examples() {
        let v = ex.input.generate().unwrap();
        assert_eq!(analyze::<Alpha>(&v), naive_alpha(&v), "alpha {}", ex.name);
    }
    for ex in Colors::examples() {
        let v = ex.input.generate().unwrap();
        assert_eq!(
            analyze::<Colors>(&v),
            naive_colors(&v),
            "colors {}",
            ex.name
        );
    }
    for ex in PixelScale::examples() {
        let v = ex.input.generate().unwrap();
        assert_eq!(
            analyze::<PixelScale>(&v),
            naive_pixel_scale(&v),
            "pixel_scale {}",
            ex.name
        );
    }
}

/// 例の値を直接確かめる（ゴールデンに凍結する前に）。
#[test]
fn example_values_are_right() {
    let gen_ = |g: InputGen| g.generate().unwrap();
    // pixel_scale
    let checker = gen_(InputGen::Checker {
        w: 24,
        h: 16,
        fmt: PixelFormat::Rgba8,
        cell: 4,
    });
    let s = analyze::<PixelScale>(&checker);
    assert_eq!(
        s.x,
        Some(AxisScale {
            k: 4,
            phase: 0,
            lines: 5
        })
    );
    assert_eq!(s.y.map(|a| (a.k, a.phase, a.lines)), Some((4, 0, 3)));
    assert!(s.lossless);
    let sprite = gen_(InputGen::Sprite {
        w: 600,
        h: 300,
        rect: IRect::new(250, 40, 300, 200),
    });
    let s = analyze::<PixelScale>(&sprite);
    assert_eq!(s.x.map(|a| (a.k, a.phase, a.lines)), Some((300, 250, 2)));
    assert_eq!(s.y.map(|a| (a.k, a.phase, a.lines)), Some((200, 40, 2)));
    let cropped = gen_(InputGen::Crop {
        base: Box::new(InputGen::Upscale {
            base: Box::new(InputGen::Noise {
                w: 10,
                h: 8,
                fmt: PixelFormat::Rgba8,
                seed: 52,
            }),
            kx: 4,
            ky: 4,
        }),
        rect: IRect::new(3, 1, 30, 25),
    });
    let s = analyze::<PixelScale>(&cropped);
    assert_eq!(s.x.map(|a| (a.k, a.phase)), Some((4, 1)));
    assert_eq!(s.y.map(|a| (a.k, a.phase)), Some((4, 3)));
    let clear = gen_(InputGen::Sprite {
        w: 9,
        h: 7,
        rect: IRect::new(20, 20, 1, 1),
    });
    let s = analyze::<PixelScale>(&clear);
    assert_eq!((s.x, s.y, s.lossless), (None, None, true));
    // colors
    let c = analyze::<Colors>(&sprite);
    assert_eq!(
        (c.count, c.visible, c.colors),
        (2, 2, Some(vec![[0, 0, 0, 0], [200, 80, 40, 255]]))
    );
    let c = analyze::<Colors>(&checker);
    assert_eq!(c.colors, Some(vec![[1, 1, 1, 1], [255, 255, 255, 255]]));
    let g16 = gen_(InputGen::Checker {
        w: 10,
        h: 10,
        fmt: PixelFormat::Gray16,
        cell: 2,
    });
    let c = analyze::<Colors>(&g16);
    assert_eq!(c.colors, Some(vec![[1, 1, 1, 255], [255, 255, 255, 255]]));
    assert!(c.palette_lossless);
    let noise = gen_(InputGen::Noise {
        w: 64,
        h: 64,
        fmt: PixelFormat::Rgba8,
        seed: 41,
    });
    let c = analyze::<Colors>(&noise);
    assert_eq!((c.count, c.colors, c.visible), (257, None, 257));
    let idx = gen_(InputGen::Noise {
        w: 20,
        h: 20,
        fmt: PixelFormat::Indexed8,
        seed: 42,
    });
    let c = analyze::<Colors>(&idx);
    assert_eq!(c.palette, Some(PaletteUse { len: 16, used: 16 }));
    assert_eq!((c.count, c.visible), (16, 16));
    // alpha
    let a = analyze::<Alpha>(&sprite);
    assert_eq!(
        a,
        AlphaOutput {
            kind: AlphaKind::Binary,
            transparent: 600 * 300 - 300 * 200,
            partial: 0,
            opaque: 300 * 200,
            uncovered: 0,
        }
    );
    let frames = gen_(InputGen::TrimmedFrames {
        w: 40,
        h: 30,
        fmt: PixelFormat::Rgba8,
        count: 4,
        seed: 35,
    });
    let a = analyze::<Alpha>(&frames);
    // フレーム i は (20 + i) × 15 を置く
    let covered: u64 = (0..4u64).map(|i| (20 + i) * 15).sum();
    assert_eq!(a.uncovered, 4 * 40 * 30 - covered);
    assert_eq!(a.transparent + a.partial + a.opaque, covered);
    assert_eq!(a.kind, AlphaKind::Partial);
    // sprite_grid
    let sheet = gen_(InputGen::SpriteSheet {
        cols: 5,
        rows: 2,
        cell_w: 10,
        cell_h: 10,
        gap_x: 4,
        gap_y: 4,
        margin_x: 4,
        margin_y: 4,
        pad_right: 0,
        pad_bottom: 0,
        background: Some([0, 128, 128, 255]),
        full: false,
        empty: vec![],
        seed: 62,
    });
    let g = analyze::<SpriteGrid>(&sheet);
    assert_eq!(
        g.background,
        GridBackground::Color {
            rgba: [0, 128, 128, 255]
        }
    );
    let best = g.best.unwrap();
    assert_eq!((best.cols, best.rows, best.filled), (5, 2, 10));
    assert_eq!(
        (
            best.grid.cell_w + best.grid.gap_x,
            best.grid.cell_h + best.grid.gap_y
        ),
        (14, 14)
    );
}

// ---- 合成の検査集：ドット絵（試作の synth.rs） ----

/// 乱数のドット絵：透明（か不透明な背景）の上に、パレットの色の点・矩形・市松のディザを置く。
fn pixel_art(w: u32, h: u32, ncolors: usize, seed: u64, opaque_bg: bool) -> Vec<u8> {
    let mut r = kura_engine::determinism::rng(seed);
    let palette: Vec<[u8; 4]> = (0..ncolors)
        .map(|_| {
            let v = r.next_u32().to_le_bytes();
            [v[0], v[1], v[2], 255]
        })
        .collect();
    let bg = if opaque_bg {
        [30, 30, 40, 255]
    } else {
        [0, 0, 0, 0]
    };
    let mut px: Vec<u8> = (0..w * h).flat_map(|_| bg).collect();
    for _ in 0..(w * h / 12).max(4) {
        let c = palette[(r.next_u32() as usize) % ncolors];
        let kind = r.next_u32() % 4;
        let (sw, sh) = match kind {
            0 => (1, 1),
            1 => (1 + r.next_u32() % 3, 1 + r.next_u32() % 3),
            _ => (1 + r.next_u32() % 8, 1 + r.next_u32() % 6),
        };
        let (x0, y0) = (r.next_u32() % w, r.next_u32() % h);
        for y in y0..(y0 + sh).min(h) {
            for x in x0..(x0 + sw).min(w) {
                if kind == 3 && (x + y) % 2 == 1 {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&c);
            }
        }
    }
    px
}

fn rgba(w: u32, h: u32, data: &[u8]) -> Value {
    image(Raster::from_rows(w, h, PixelFormat::Rgba8, None, data).unwrap())
}

fn resize(v: &Value, w: u32, h: u32, filter: &str) -> Value {
    render_value(
        "image.resize",
        1,
        json!({ "width": { "px": w }, "height": { "px": h }, "filter": filter }),
        v.clone(),
    )
    .unwrap()
}

fn crop(v: &Value, rect: IRect) -> Value {
    render_value("image.crop", 1, json!({ "rect": rect }), v.clone()).unwrap()
}

/// 不透明にして固定の JPEG のエンコーダで書いて読み直す。
fn jpeg(v: &Value, quality: u32) -> Value {
    let flat = render_value(
        "image.convert",
        1,
        json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
        v.clone(),
    )
    .unwrap();
    let codecs = CodecSet::pinned();
    let (bytes, _) = codecs
        .encode_bytes(&flat, "jpeg", &json!({ "quality": quality }), &NoMonitor)
        .unwrap();
    codecs
        .decode(
            &mut Cursor::new(bytes),
            "jpeg",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap()
        .0
}

/// 軸ごとの (倍率, 位相)。
type KPhase = Option<(u32, u32)>;

fn axes(s: &PixelScaleOutput) -> (KPhase, KPhase) {
    (s.x.map(|a| (a.k, a.phase)), s.y.map(|a| (a.k, a.phase)))
}

/// 試作の表（image.md §3.4）：最近傍の k 倍は k と位相 0、位相をずらした切り抜きは k と位相、JPEG（q85 以下）・
/// bilinear で拡大したもの・1 点の書き足しは倍率を失う（厳密な定義）。
#[test]
fn pixel_scale_of_synthetic_pixel_art() {
    for seed in [1u64, 2] {
        let (w, h) = (48, 40);
        let art = rgba(w, h, &pixel_art(w, h, 12, seed, seed == 2));
        let base = analyze::<PixelScale>(&art);
        assert_eq!(axes(&base), (Some((1, 0)), Some((1, 0))), "s{seed}");
        for (kx, ky) in [(2u32, 2u32), (3, 3), (4, 4), (6, 6), (8, 8), (2, 1)] {
            let up = resize(&art, w * kx, h * ky, "nearest");
            let s = analyze::<PixelScale>(&up);
            assert_eq!(
                axes(&s),
                (Some((kx, 0)), Some((ky, 0))),
                "s{seed} k{kx}x{ky}"
            );
            assert!(s.lossless);
            if kx > 1 {
                // 位相をずらして切り抜く（端の欠けたブロック）
                let (dx, dy) = (kx / 2 + 1, 1);
                let c = crop(
                    &up,
                    IRect::new(i64::from(dx), dy, w * kx - kx - 3, h * ky - 2),
                );
                let s = analyze::<PixelScale>(&c);
                let px = (kx - dx % kx) % kx;
                let py = (ky - 1 % ky) % ky;
                assert_eq!(
                    axes(&s),
                    (Some((kx, px)), Some((ky, py))),
                    "s{seed} k{kx} 切り抜き"
                );
                // bilinear で拡大すると倍率を失う
                let b = resize(&art, w * kx, h * ky, "bilinear");
                let s = analyze::<PixelScale>(&b);
                assert!(s.x.is_none_or(|a| a.k < kx), "s{seed} k{kx} bilinear {s:?}");
            }
            // JPEG（q85 以下）は倍率を失う（k = 8 の q95 は JPEG のブロックと重なって残ることがある）
            let j = jpeg(&up, 85);
            let s = analyze::<PixelScale>(&j);
            assert!(
                !(s.lossless && s.x.is_some_and(|a| a.k == kx) && kx > 1),
                "s{seed} k{kx} jpeg {s:?}"
            );
            if kx == 4 {
                assert_eq!(s.x.map(|a| a.k), Some(1), "s{seed} jpeg q85");
            }
        }
        // 1 点の書き足し（ブロックの内側）で倍率は 1 に落ちる
        let up = resize(&art, w * 4, h * 4, "nearest");
        let Value::Image(img) = &up else { panic!() };
        let mut data = img.px.to_rows().unwrap();
        let i = ((5 * w * 4 + 9) * 4) as usize;
        data[i..i + 4].copy_from_slice(&[1, 2, 254, 255]);
        let s = analyze::<PixelScale>(&rgba(w * 4, h * 4, &data));
        assert_eq!(axes(&s), (Some((1, 0)), Some((1, 0))), "s{seed} 1 点");
    }
}

/// フレームの列：フレームごとに比べた変わり目の合併。拡大したトリムしたフレームの列は k。
#[test]
fn pixel_scale_of_upscaled_frames() {
    let v = InputGen::Upscale {
        base: Box::new(InputGen::TrimmedFrames {
            w: 12,
            h: 8,
            fmt: PixelFormat::Rgba8,
            count: 3,
            seed: 55,
        }),
        kx: 3,
        ky: 2,
    }
    .generate()
    .unwrap();
    let s = analyze::<PixelScale>(&v);
    assert_eq!(axes(&s), (Some((3, 0)), Some((2, 0))));
    assert!(s.lossless);
}

// ---- 合成の検査集：スプライトシート（試作の grid.rs） ----

struct SheetSpec {
    cols: u32,
    rows: u32,
    cw: u32,
    ch: u32,
    gap: u32,
    margin: u32,
    extra_right: u32,
    bg: [u8; 4],
    /// 0 = 中央に小さめ（ばらつく）、1 = セルいっぱい、2 = 左上に寄せる、3 = 右端に接する
    content: u32,
    empty: Vec<(u32, u32)>,
}

fn make_sheet(s: &SheetSpec, seed: u64) -> Value {
    let mut r = kura_engine::determinism::rng(seed);
    let w = s.margin * 2 + s.cols * s.cw + (s.cols - 1) * s.gap + s.extra_right;
    let h = s.margin * 2 + s.rows * s.ch + (s.rows - 1) * s.gap;
    let mut px: Vec<u8> = (0..w * h).flat_map(|_| s.bg).collect();
    for j in 0..s.rows {
        for i in 0..s.cols {
            if s.empty.contains(&(i, j)) {
                continue;
            }
            let (x0, y0) = (s.margin + i * (s.cw + s.gap), s.margin + j * (s.ch + s.gap));
            let (sw, sh, ox, oy) = match s.content {
                1 => (s.cw, s.ch, 0, 0),
                2 => (
                    s.cw / 3 + r.next_u32() % (s.cw / 3),
                    s.ch / 3 + r.next_u32() % (s.ch / 3),
                    0,
                    0,
                ),
                3 => {
                    let sw = s.cw / 2 + r.next_u32() % (s.cw / 3);
                    let sh = s.ch / 2 + r.next_u32() % (s.ch / 3);
                    (sw, sh, s.cw - sw, (s.ch - sh) / 2)
                }
                _ => {
                    let sw = s.cw / 3 + r.next_u32() % (s.cw / 2);
                    let sh = s.ch / 3 + r.next_u32() % (s.ch / 2);
                    (sw, sh, (s.cw - sw) / 2 + r.next_u32() % 3, (s.ch - sh) / 2)
                }
            };
            for y in 0..sh {
                for x in 0..sw {
                    let (px_, py_) = (x0 + ox + x, y0 + oy + y);
                    if px_ >= x0 + s.cw || py_ >= y0 + s.ch {
                        continue;
                    }
                    let v = r.next_u32();
                    if s.content != 1 && v.is_multiple_of(11) {
                        continue;
                    }
                    let k = ((py_ * w + px_) * 4) as usize;
                    px[k..k + 4].copy_from_slice(&[(v >> 8) as u8 | 1, (v >> 16) as u8, 200, 255]);
                }
            }
        }
    }
    rgba(w, h, &px)
}

/// 試作の表（image.md §3.5）のシートで、いちばんの候補が期待の格子（横・縦の (offset, cell, gap)。None は分けない）。
#[test]
fn sprite_grid_finds_the_prototype_sheets() {
    let clear = [0u8, 0, 0, 0];
    let teal = [0u8, 128, 128, 255];
    let spec =
        |cols, rows, c, gap, margin, extra_right, bg, content, empty: Vec<(u32, u32)>| SheetSpec {
            cols,
            rows,
            cw: c,
            ch: c,
            gap,
            margin,
            extra_right,
            bg,
            content,
            empty,
        };
    type Axis = Option<(u32, u32, u32)>;
    let cases: Vec<(&str, SheetSpec, Axis, Axis)> = vec![
        (
            "RPG 48² 3×4",
            spec(3, 4, 48, 0, 0, 0, clear, 0, vec![]),
            Some((0, 48, 0)),
            Some((0, 48, 0)),
        ),
        (
            "RPG 32² 12×8（空きあり）",
            spec(12, 8, 32, 0, 0, 0, clear, 0, vec![(11, 7), (10, 7)]),
            Some((0, 32, 0)),
            Some((0, 32, 0)),
        ),
        (
            "タイル 16 間隔 1 余白 0",
            spec(10, 6, 16, 1, 0, 0, clear, 1, vec![]),
            Some((0, 16, 1)),
            Some((0, 16, 1)),
        ),
        (
            "タイル 16 間隔 1 余白 1",
            spec(10, 6, 16, 1, 1, 0, clear, 1, vec![]),
            Some((1, 16, 1)),
            Some((1, 16, 1)),
        ),
        (
            "タイル 16 間隔 2 余白 4（空きあり）",
            spec(8, 5, 16, 2, 4, 0, clear, 1, vec![(7, 4)]),
            Some((4, 16, 2)),
            Some((4, 16, 2)),
        ),
        (
            "左上に寄せた 32",
            spec(8, 4, 32, 0, 0, 0, clear, 2, vec![]),
            Some((0, 32, 0)),
            Some((0, 32, 0)),
        ),
        (
            "右端に接する 32",
            spec(8, 4, 32, 0, 0, 0, clear, 3, vec![]),
            Some((0, 32, 0)),
            Some((0, 32, 0)),
        ),
        (
            "横一列の帯 32 × 8",
            spec(8, 1, 32, 0, 0, 0, clear, 0, vec![]),
            Some((0, 32, 0)),
            None,
        ),
        (
            "横一列の帯（右端に接する）",
            spec(8, 1, 32, 0, 0, 0, clear, 3, vec![]),
            Some((0, 32, 0)),
            None,
        ),
        (
            "右に 32 の余り",
            spec(10, 3, 48, 0, 0, 32, clear, 0, vec![]),
            Some((0, 48, 0)),
            Some((0, 48, 0)),
        ),
    ];
    for (name, s, want_x, want_y) in &cases {
        for seed in [1u64, 2] {
            let v = make_sheet(s, seed);
            let out = analyze::<SpriteGrid>(&v);
            let best = out
                .best
                .unwrap_or_else(|| panic!("{name} s{seed}：見つからない"));
            let Value::Image(img) = &v else { panic!() };
            let (w, h) = (img.px.width(), img.px.height());
            let axis = |o: u32, c: u32, g: u32, len: u32| {
                if o == 0 && c == len && g == 0 {
                    None
                } else {
                    Some((o, c, g))
                }
            };
            let got = (
                axis(best.grid.offset_x, best.grid.cell_w, best.grid.gap_x, w),
                axis(best.grid.offset_y, best.grid.cell_h, best.grid.gap_y, h),
            );
            assert_eq!(got, (*want_x, *want_y), "{name} s{seed}: {best:?}");
            assert_eq!(best.source, GridSource::Gutter);
            let empties = s.empty.len() as u32;
            assert_eq!(best.filled, s.cols * s.rows - empties, "{name} s{seed}");
        }
    }
    // 不透明な背景（間隔の色も背景）：セルの縁は画素から決まらないので、周期 44 で 6 × 3 に分ける
    let v = make_sheet(&spec(6, 3, 40, 4, 4, 0, teal, 0, vec![]), 1);
    let out = analyze::<SpriteGrid>(&v);
    assert_eq!(out.background, GridBackground::Color { rgba: teal });
    let best = out.best.unwrap();
    assert_eq!(
        (
            best.grid.cell_w + best.grid.gap_x,
            best.grid.cell_h + best.grid.gap_y,
            best.cols,
            best.rows,
            best.filled
        ),
        (44, 44, 6, 3, 18)
    );
    // 溝のないタイル（間隔 0・セルいっぱい）と不規則な配置は見つけない
    let v = make_sheet(&spec(10, 6, 16, 0, 0, 0, clear, 1, vec![]), 1);
    assert_eq!(analyze::<SpriteGrid>(&v).best, None);
    let mut r = kura_engine::determinism::rng(3);
    let (w, h) = (300u32, 200u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for _ in 0..25 {
        let (sw, sh) = (5 + r.next_u32() % 30, 5 + r.next_u32() % 30);
        let (x0, y0) = (r.next_u32() % (w - sw), r.next_u32() % (h - sh));
        for y in y0..y0 + sh {
            for x in x0..x0 + sw {
                let k = ((y * w + x) * 4) as usize;
                px[k..k + 4].copy_from_slice(&[200, 10, 10, 255]);
            }
        }
    }
    let out = analyze::<SpriteGrid>(&rgba(w, h, &px));
    assert_eq!(out.best, None, "{out:?}");
}

/// 推定した格子は、どれも `split_grid` に渡せる形で、中身はすべてセルの中（16 ビット・Indexed8・グレーの形式でも）。
#[test]
fn sprite_grid_candidates_cover_all_content_in_every_format() {
    let base = InputGen::SpriteSheet {
        cols: 4,
        rows: 3,
        cell_w: 9,
        cell_h: 7,
        gap_x: 2,
        gap_y: 1,
        margin_x: 1,
        margin_y: 1,
        pad_right: 1,
        pad_bottom: 1,
        background: None,
        full: false,
        empty: vec![2],
        seed: 9,
    }
    .generate()
    .unwrap();
    for fmt in [
        PixelFormat::Rgba16,
        PixelFormat::GrayA8,
        PixelFormat::GrayA16,
        PixelFormat::Indexed8,
        PixelFormat::Rgba8,
    ] {
        let name = serde_json::to_value(fmt).unwrap();
        let v = render_value(
            "image.convert",
            1,
            json!({ "format": name, "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
            base.clone(),
        )
        .unwrap();
        let out = analyze::<SpriteGrid>(&v);
        assert_eq!(out.background, GridBackground::Transparent, "{fmt:?}");
        let best = out.best.unwrap_or_else(|| panic!("{fmt:?}"));
        assert_eq!((best.cols, best.rows, best.filled), (4, 3, 11), "{fmt:?}");
        let g: GridParam =
            serde_json::from_value(serde_json::to_value(best.grid).unwrap()).unwrap();
        assert_eq!(g, best.grid);
    }
    // 不透明にした（背景色の）シートも同じセルに分ける
    let flat = render_value(
        "image.convert",
        1,
        json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [9, 99, 199] }),
        base,
    )
    .unwrap();
    let out = analyze::<SpriteGrid>(&flat);
    assert_eq!(
        out.background,
        GridBackground::Color {
            rgba: [9, 99, 199, 255]
        }
    );
    let best = out.best.unwrap();
    assert_eq!((best.cols, best.rows, best.filled), (4, 3, 11));
}

/// 例の入力の作り方（`Upscale`・`Crop`・`SpriteSheet`）：同じ記述なら同じ値（スレッド数に依らない）、大きさと中身が
/// 記述どおり。
#[test]
fn new_input_generators_follow_their_description() {
    let gens = [
        InputGen::Upscale {
            base: Box::new(InputGen::Noise {
                w: 300,
                h: 7,
                fmt: PixelFormat::Indexed8,
                seed: 1,
            }),
            kx: 3,
            ky: 40,
        },
        InputGen::Crop {
            base: Box::new(InputGen::Noise {
                w: 600,
                h: 300,
                fmt: PixelFormat::Rgb16,
                seed: 2,
            }),
            rect: IRect::new(250, 7, 300, 280),
        },
        InputGen::SpriteSheet {
            cols: 7,
            rows: 5,
            cell_w: 40,
            cell_h: 30,
            gap_x: 3,
            gap_y: 1,
            margin_x: 2,
            margin_y: 5,
            pad_right: 4,
            pad_bottom: 0,
            background: Some([1, 2, 3, 255]),
            full: false,
            empty: vec![0, 34],
            seed: 3,
        },
    ];
    for g in &gens {
        let mut digests = Vec::new();
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            digests.push(pool.install(|| g.generate().unwrap().digest().unwrap()));
        }
        assert!(digests.windows(2).all(|w| w[0] == w[1]), "{g:?}");
    }
    // 拡大：各画素は元の (⌊x/kx⌋, ⌊y/ky⌋)
    let Value::Image(base) = (InputGen::Noise {
        w: 300,
        h: 7,
        fmt: PixelFormat::Indexed8,
        seed: 1,
    })
    .generate()
    .unwrap() else {
        panic!()
    };
    let Value::Image(up) = gens[0].generate().unwrap() else {
        panic!()
    };
    assert_eq!((up.px.width(), up.px.height()), (900, 280));
    let (b, u) = (base.px.to_rows().unwrap(), up.px.to_rows().unwrap());
    for y in [0usize, 39, 40, 279] {
        for x in [0usize, 2, 3, 899] {
            assert_eq!(u[y * 900 + x], b[(y / 40) * 300 + x / 3]);
        }
    }
    // 切り抜き
    let Value::Image(c) = gens[1].generate().unwrap() else {
        panic!()
    };
    assert_eq!((c.px.width(), c.px.height()), (300, 280));
    // シート：大きさ・地の色・描かないセル・中身はセルの縁の 1 画素の内側
    let Value::Image(s) = gens[2].generate().unwrap() else {
        panic!()
    };
    let (w, h) = (2 + 7 * 40 + 6 * 3 + 4, 5 + 5 * 30 + 4);
    assert_eq!((s.px.width(), s.px.height()), (w, h));
    let rows = s.px.to_rows().unwrap();
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        rows[i..i + 4].try_into().unwrap()
    };
    let bg = [1, 2, 3, 255];
    for n in 0..35u32 {
        let (i, j) = (n % 7, n / 7);
        let (x0, y0) = (2 + i * 43, 5 + j * 31);
        let mut any = false;
        for y in y0..y0 + 30 {
            for x in x0..x0 + 40 {
                let edge = x == x0 || y == y0 || x == x0 + 39 || y == y0 + 29;
                if at(x, y) != bg {
                    any = true;
                    assert!(!edge, "セル {n} の縁に中身");
                }
            }
        }
        assert_eq!(any, n != 0 && n != 34, "セル {n}");
    }
    // 間隔・余白は地の色
    for y in 0..h {
        assert_eq!(at(0, y), bg);
        assert_eq!(at(2 + 40, y), bg);
    }
}

/// 16384²（`max_pixels`）の時間。ふだんは回さない（release で）。
#[test]
#[ignore]
#[allow(clippy::print_stderr, clippy::disallowed_methods)]
fn bench_image_analyses_at_max_pixels() {
    use std::time::Instant;
    let n = 16_384u32;
    let e = engine();
    let time = |label: &str, v: &Value, id: &str| {
        let req = kura_engine::analysis::AnalysisRequest::new(id);
        let mut best = f64::INFINITY;
        for _ in 0..3 {
            let t = Instant::now();
            e.analyze_dyn(&req, v, &NoMonitor).unwrap();
            best = best.min(t.elapsed().as_secs_f64());
        }
        eprintln!("{id} {label}: {:.1} ms（3 回の最小）", best * 1e3);
    };
    // 256 色が画素ごとに変わる（色の最悪）・4 倍のドット絵・不透明な範囲の例と同じ形
    let mut r = kura_engine::determinism::rng(5);
    let pal: Vec<[u8; 4]> = (0..256).map(|_| r.next_u32().to_le_bytes()).collect();
    let hash = |x: u32, y: u32| {
        let v = kura_engine::determinism::splitmix64(u64::from(x) << 32 | u64::from(y));
        (v >> 32) as u32
    };
    let worst = image(
        Raster::from_tile_fn(n, n, PixelFormat::Rgba8, None, &NoMonitor, |t, out| {
            for (i, p) in out.chunks_exact_mut(4).enumerate() {
                let (x, y) = (t.x as u32 + i as u32 % t.w, t.y as u32 + i as u32 / t.w);
                p.copy_from_slice(&pal[(hash(x, y) & 255) as usize]);
            }
            Ok(())
        })
        .unwrap(),
    );
    let art = image(
        Raster::from_tile_fn(n, n, PixelFormat::Rgba8, None, &NoMonitor, |t, out| {
            for (i, p) in out.chunks_exact_mut(4).enumerate() {
                let (x, y) = (t.x as u32 + i as u32 % t.w, t.y as u32 + i as u32 / t.w);
                let v = ((x / 4) * 7 + (y / 4) * 13) % 9;
                p.copy_from_slice(&[v as u8 * 20, 3, 4, if v == 0 { 0 } else { 255 }]);
            }
            Ok(())
        })
        .unwrap(),
    );
    let sheet = InputGen::SpriteSheet {
        cols: 256,
        rows: 256,
        cell_w: 62,
        cell_h: 62,
        gap_x: 2,
        gap_y: 2,
        margin_x: 0,
        margin_y: 0,
        pad_right: 2,
        pad_bottom: 2,
        background: None,
        full: false,
        empty: vec![],
        seed: 1,
    }
    .generate()
    .unwrap();
    for (label, v) in [
        ("256 色が画素ごと", &worst),
        ("4 倍のドット絵", &art),
        ("256×256 のシート", &sheet),
    ] {
        for id in [
            "image.alpha",
            "image.colors",
            "image.pixel_scale",
            "image.sprite_grid",
        ] {
            time(label, v, id);
        }
    }
}
