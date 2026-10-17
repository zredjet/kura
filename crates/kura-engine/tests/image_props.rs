//! 段 5 の画像の操作の受け入れテスト（docs/editing.md §17 の E10〜E15・E23）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use kura_engine::geom::{IPoint, IRect, Size};
use kura_engine::paint::{
    Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool, sample,
};
use kura_engine::testgen::InputGen;
use kura_engine::testing::{render_full, render_value, single_node_graph};
use kura_engine::value::{Frames, Grid, Image, ImageMeta, Palette, PixelFormat, Raster, Slice};
use kura_engine::{Digest, Engine, IssueCode, NoMonitor, Value};
use proptest::prelude::*;
use serde_json::json;

fn op(id: &str, params: serde_json::Value, input: Value) -> Value {
    render_value(id, 1, params, input).unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn image(v: &Value) -> &Arc<Image> {
    match v {
        Value::Image(img) => img,
        _ => panic!("画像ではありません"),
    }
}

fn noise(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Value {
    InputGen::Noise { w, h, fmt, seed }.generate().unwrap()
}

fn crop(input: Value, r: IRect) -> Value {
    op("image.crop", json!({ "rect": r }), input)
}

fn canvas(input: Value, r: IRect) -> Value {
    op("image.canvas", json!({ "rect": r, "fill": null }), input)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    /// E10：切り抜いてから同じ量だけキャンバスを広げると、元の範囲の外を透明にしたものになる。
    #[test]
    fn e10_crop_then_canvas(x in 0i64..20, y in 0i64..15, w in 1u32..20, h in 1u32..15, indexed: bool) {
        prop_assume!(x + i64::from(w) <= 30 && y + i64::from(h) <= 22);
        let fmt = if indexed { PixelFormat::Indexed8 } else { PixelFormat::Rgba8 };
        let input = noise(30, 22, fmt, 7);
        let src = image(&input).clone();
        let r = IRect::new(x, y, w, h);
        let back = canvas(crop(input, r), IRect::new(-x, -y, 30, 22));
        let out = image(&back);
        prop_assert_eq!((out.px.width(), out.px.height()), (30, 22));
        let pal = out.px.palette().map(|p| p.colors.clone());
        for py in 0..22u32 {
            for px in 0..30u32 {
                let inside = i64::from(px) >= x && i64::from(px) < x + i64::from(w)
                    && i64::from(py) >= y && i64::from(py) < y + i64::from(h);
                let got = out.px.pixel(px, py).unwrap();
                if inside {
                    prop_assert_eq!(got, src.px.pixel(px, py).unwrap());
                } else if let Some(pal) = &pal {
                    prop_assert_eq!(pal[usize::from(got[0])][3], 0);
                } else {
                    prop_assert_eq!(got, &[0u8, 0, 0, 0][..]);
                }
            }
        }
    }

    /// E11：透明な余白のトリムは 2 回しても同じ。外接矩形を含む範囲で切り抜いてからトリムしても同じ。
    #[test]
    fn e11_trim_is_idempotent(rx in 0i64..30, ry in 0i64..20, rw in 1u32..20, rh in 1u32..15, mx in 0i64..5, my in 0i64..5) {
        prop_assume!(rx + i64::from(rw) <= 40 && ry + i64::from(rh) <= 30);
        let input = InputGen::Sprite { w: 40, h: 30, rect: IRect::new(rx, ry, rw, rh) }.generate().unwrap();
        let trim = |v: Value| op("image.trim_alpha", json!({ "threshold": 0, "mode": "union" }), v);
        let once = trim(input.clone());
        prop_assert_eq!(trim(once.clone()), once.clone());
        let x0 = (rx - mx).max(0);
        let y0 = (ry - my).max(0);
        let x1 = (rx + i64::from(rw) + mx).min(40);
        let y1 = (ry + i64::from(rh) + my).min(30);
        let around = crop(input, IRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32));
        prop_assert_eq!(image(&trim(around)).px.clone(), image(&once).px.clone());
    }
}

/// E12：最近傍で k 倍してから 1/k にすると元に戻る。Indexed8 はパレットを保つ。
#[test]
fn e12_nearest_round_trip() {
    for (fmt, k) in [
        (PixelFormat::Indexed8, 3u32),
        (PixelFormat::Rgba16, 2),
        (PixelFormat::GrayA8, 5),
    ] {
        let input = noise(37, 23, fmt, 9);
        let up = op(
            "image.resize",
            json!({ "width": { "scale": { "num": k, "den": 1 } }, "height": "auto", "filter": "nearest" }),
            input.clone(),
        );
        assert_eq!(image(&up).px.width(), 37 * k);
        let down = op(
            "image.resize",
            json!({ "width": { "scale": { "num": 1, "den": k } }, "height": "auto", "filter": "nearest" }),
            up,
        );
        assert_eq!(image(&down).px, image(&input).px, "{fmt:?} {k}");
    }
}

/// E13：90° 回転を 4 回、反転を 2 回で元に戻る（メタデータも）。
#[test]
fn e13_rotations_and_flips_return() {
    let input = InputGen::Sheet {
        cols: 3,
        rows: 2,
        cell: 8,
    }
    .generate()
    .unwrap();
    let rot = |v: Value, turn: &str| op("image.rotate90", json!({ "turn": turn }), v);
    let mut v = input.clone();
    for _ in 0..4 {
        v = rot(v, "cw");
    }
    assert_eq!(v, input);
    assert_eq!(rot(rot(input.clone(), "cw"), "ccw"), input);
    assert_eq!(rot(rot(input.clone(), "half"), "half"), input);
    for axis in ["horizontal", "vertical"] {
        let f = |v: Value| op("image.flip", json!({ "axis": axis }), v);
        assert_eq!(f(f(input.clone())), input);
    }
}

/// E14：スプライトを分割してから同じ格子で結合すると元に戻る（画素と格子）。
#[test]
fn e14_split_then_pack() {
    let input = InputGen::Sheet {
        cols: 4,
        rows: 3,
        cell: 16,
    }
    .generate()
    .unwrap();
    let grid = image(&input).meta.grid.unwrap();
    let frames = op(
        "sprite.split_grid",
        json!({ "grid": grid, "durationMs": 100, "skipEmpty": false }),
        input.clone(),
    );
    let Value::Frames(fr) = frames.clone() else {
        panic!()
    };
    assert_eq!(fr.frames().len(), 12);
    let back = op(
        "sprite.pack_grid",
        json!({ "columns": 4, "gapX": 0, "gapY": 0, "fill": null }),
        frames,
    );
    assert_eq!(image(&back).px, image(&input).px);
    assert_eq!(image(&back).meta.grid, Some(grid));
    // 間隔のある格子でも、間隔を色で埋めれば元に戻る
    let gapped = op(
        "sprite.pack_grid",
        json!({ "columns": 3, "gapX": 2, "gapY": 1, "fill": [9, 9, 9, 255] }),
        op(
            "sprite.split_grid",
            json!({ "grid": grid, "durationMs": 100, "skipEmpty": false }),
            input,
        ),
    );
    let g2 = image(&gapped).meta.grid.unwrap();
    let again = op(
        "sprite.split_grid",
        json!({ "grid": g2, "durationMs": 100, "skipEmpty": false }),
        gapped,
    );
    let Value::Frames(a) = &again else { panic!() };
    assert_eq!(a.frames().len(), 12);
    assert_eq!(a.frames()[5].px, fr.frames()[5].px);
}

fn with_meta() -> Value {
    let Value::Image(img) = noise(64, 32, PixelFormat::Rgba8, 3) else {
        panic!()
    };
    let meta = ImageMeta {
        grid: Some(Grid {
            cell_w: 16,
            cell_h: 16,
            offset_x: 0,
            offset_y: 0,
            gap_x: 0,
            gap_y: 0,
        }),
        pivot: Some(IPoint { x: 8, y: 16 }),
        slices: vec![
            Slice {
                name: "a".into(),
                rect: IRect::new(2, 2, 10, 10),
                center: Some(IRect::new(4, 4, 6, 6)),
                pivot: Some(IPoint { x: 5, y: 10 }),
            },
            Slice {
                name: "far".into(),
                rect: IRect::new(50, 20, 10, 10),
                center: None,
                pivot: None,
            },
        ],
        dpi: Some((144, 144)),
        ..ImageMeta::default()
    };
    Value::Image(Arc::new(Image {
        px: img.px.clone(),
        meta,
    }))
}

/// E15：格子・ピボット・スライスが §6 の表のとおりに写る。範囲の外に出たものは警告付きで落ちる。
#[test]
fn e15_metadata_follows_the_space_maps() {
    let engine = Engine::new().unwrap();
    let run = |id: &str, params: serde_json::Value| {
        let g = single_node_graph(id, 1, params).unwrap();
        render_full(&engine, &g, with_meta(), Digest::default(), &NoMonitor).unwrap()
    };
    // 切り抜き：平行移動。外に出たスライスは警告付きで落ちる
    let r = run("image.crop", json!({ "rect": IRect::new(0, 0, 40, 32) }));
    let m = &image(&r.outputs[0].value).meta;
    assert_eq!(m.slices.len(), 1);
    assert_eq!(m.pivot, Some(IPoint { x: 8, y: 16 }));
    assert_eq!(m.dpi, Some((144, 144)));
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == IssueCode::MetadataDropped)
    );
    // キャンバスを左に広げる：右へずれる
    let r = run(
        "image.canvas",
        json!({ "rect": IRect::new(-4, 0, 68, 32), "fill": null }),
    );
    let m = &image(&r.outputs[0].value).meta;
    assert_eq!(m.pivot, Some(IPoint { x: 12, y: 16 }));
    assert_eq!(m.slices[0].rect, IRect::new(6, 2, 10, 10));
    assert_eq!(m.grid.map(|g| g.offset_x), Some(4));
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    // 時計回り：格子の縦横が入れ替わり、ピボットは (h − y, x)
    let r = run("image.rotate90", json!({ "turn": "cw" }));
    let m = &image(&r.outputs[0].value).meta;
    assert_eq!(m.pivot, Some(IPoint { x: 16, y: 8 }));
    assert_eq!(m.slices[0].rect, IRect::new(20, 2, 10, 10));
    assert_eq!(m.grid.map(|g| (g.cell_w, g.cell_h)), Some((16, 16)));
    // 最近傍の 2 倍：倍に写り、格子も残る。1/3 では格子が割り切れずに落ちる
    let r = run(
        "image.resize",
        json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
    );
    let m = &image(&r.outputs[0].value).meta;
    assert_eq!(m.pivot, Some(IPoint { x: 16, y: 32 }));
    assert_eq!(m.grid.map(|g| g.cell_w), Some(32));
    let r = run(
        "image.resize",
        json!({ "width": { "scale": { "num": 1, "den": 3 } }, "height": "auto", "filter": "nearest" }),
    );
    assert_eq!(image(&r.outputs[0].value).meta.grid, None);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == IssueCode::MetadataDropped)
    );
    // トリム：中身で決まる写し方（実行が返す平行移動）
    let sprite = InputGen::Sprite {
        w: 20,
        h: 20,
        rect: IRect::new(5, 6, 8, 8),
    }
    .generate()
    .unwrap();
    let Value::Image(s) = &sprite else { panic!() };
    let mut s = (**s).clone();
    s.meta.pivot = Some(IPoint { x: 9, y: 14 });
    let g = single_node_graph(
        "image.trim_alpha",
        1,
        json!({ "threshold": 0, "mode": "union" }),
    )
    .unwrap();
    let r = render_full(
        &engine,
        &g,
        Value::Image(Arc::new(s)),
        Digest::default(),
        &NoMonitor,
    )
    .unwrap();
    assert_eq!(
        image(&r.outputs[0].value).meta.pivot,
        Some(IPoint { x: 4, y: 8 })
    );
}

/// E23：スポイトが返す色とインデックスが、描画結果の画素と一致する。
#[test]
fn e23_sample_matches_the_rendered_pixels() {
    let pal = Palette {
        colors: vec![[0, 0, 0, 0], [10, 20, 30, 255], [200, 100, 50, 128]],
    };
    let px = Raster::from_rows(
        4,
        2,
        PixelFormat::Indexed8,
        Some(pal.clone()),
        &[0, 1, 2, 1, 2, 2, 0, 1],
    )
    .unwrap();
    let input = Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    }));
    let action = PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Index(2)),
        blend: Blend::Replace,
        brush: Brush {
            shape: BrushShape::Square,
            size: 1,
        },
        pixel_perfect: false,
        frame: 0,
        points: PointList(vec![IPoint { x: 0, y: 0 }]),
    };
    let out = op("paint.actions", json!({ "actions": [action] }), input);
    let img = image(&out);
    for y in 0..2u32 {
        for x in 0..4u32 {
            let s = sample(&out, 0, i64::from(x), i64::from(y)).unwrap();
            let raw = img.px.pixel(x, y).unwrap();
            assert_eq!(s.raw, raw);
            assert_eq!(s.index, Some(raw[0]));
            assert_eq!(s.rgba, pal.colors[usize::from(raw[0])]);
        }
    }
    assert_eq!(sample(&out, 0, 0, 0).unwrap().index, Some(2));
    assert!(sample(&out, 0, 4, 0).is_none());
    assert!(sample(&out, 1, 0, 0).is_none());
    // 16 ビットは四捨五入で 8 ビットに。トリムしたフレームはキャンバスの座標で拾う
    let px = Raster::from_rows(
        1,
        1,
        PixelFormat::Rgba16,
        None,
        &[128, 0, 129, 0, 255, 255, 0, 128],
    )
    .unwrap();
    let fr = Value::Frames(Arc::new(
        Frames::new(
            vec![kura_engine::value::Frame {
                px,
                offset: IPoint { x: 2, y: 1 },
                duration_ms: 10,
            }],
            Size { w: 3, h: 2 },
            ImageMeta::default(),
            0,
        )
        .unwrap(),
    ));
    assert_eq!(sample(&fr, 0, 2, 1).unwrap().rgba, [0, 1, 255, 128]);
    assert!(sample(&fr, 0, 0, 0).is_none());
}

fn indexed_value(w: u32, h: u32, colors: Vec<[u8; 4]>, data: &[u8]) -> Value {
    let px =
        Raster::from_rows(w, h, PixelFormat::Indexed8, Some(Palette { colors }), data).unwrap();
    Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    }))
}

/// E14（形式・格子・空のセル）：元の形式で塗れる fill を使えば、分割して結合すると元に戻る。
#[test]
fn e14_split_then_pack_across_formats_and_grids() {
    let grid = |offset_x, gap_x| json!({ "cellW": 4, "cellH": 3, "offsetX": offset_x, "offsetY": 0, "gapX": gap_x, "gapY": 0 });
    // (入力, 格子, 結合の fill)
    let cases: Vec<(Value, serde_json::Value, serde_json::Value)> = vec![
        // 透明な色のあるパレット：fill null（透明な色を使い、パレットは変わらない）
        (
            noise(16, 6, PixelFormat::Indexed8, 1),
            grid(0, 0),
            json!(null),
        ),
        // アルファのない形式：元の形式で塗れる色を指定する
        (
            noise(16, 6, PixelFormat::Rgb8, 2),
            grid(0, 0),
            json!([1, 2, 3, 255]),
        ),
        (
            noise(16, 6, PixelFormat::Gray16, 3),
            grid(0, 0),
            json!([9, 9, 9, 255]),
        ),
    ];
    for (input, g, fill) in cases {
        let frames = op(
            "sprite.split_grid",
            json!({ "grid": g, "durationMs": 100, "skipEmpty": false }),
            input.clone(),
        );
        let back = op(
            "sprite.pack_grid",
            json!({ "columns": 4, "gapX": 0, "gapY": 0, "fill": fill }),
            frames,
        );
        assert_eq!(image(&back).px, image(&input).px, "{g} {fill}");
    }
    // 左の余白と間隔のある格子：セルの画素は元の位置の画素と同じ
    let input = noise(20, 6, PixelFormat::Rgba8, 4);
    let Value::Frames(fr) = op(
        "sprite.split_grid",
        json!({ "grid": grid(2, 1), "durationMs": 100, "skipEmpty": false }),
        input.clone(),
    ) else {
        panic!()
    };
    assert_eq!(fr.frames().len(), 2 * 3);
    let src = image(&input);
    for (i, f) in fr.frames().iter().enumerate() {
        let (cx, cy) = ((i % 3) as u32, (i / 3) as u32);
        for y in 0..3 {
            for x in 0..4 {
                assert_eq!(f.px.pixel(x, y), src.px.pixel(2 + cx * 5 + x, cy * 3 + y));
            }
        }
    }
    // 透明な色のないパレットで fill null：塗る部分があれば透明な色が足される（パレットが変わることを知らせる規則）
    let opaque = indexed_value(
        8,
        3,
        vec![[1, 1, 1, 255], [2, 2, 2, 255]],
        &[
            0, 1, 0, 1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 1, 0, 1, 0, 0, 1, 0, 1,
        ],
    );
    let Value::Image(sheet) = op(
        "sprite.pack_grid",
        json!({ "columns": 1, "gapX": 0, "gapY": 1, "fill": null }),
        op(
            "sprite.split_grid",
            json!({ "grid": grid(0, 0), "durationMs": 100, "skipEmpty": false }),
            opaque,
        ),
    ) else {
        panic!()
    };
    assert_eq!(sheet.px.palette().unwrap().colors.len(), 3);
    assert_eq!(sheet.px.palette().unwrap().colors[2], [0, 0, 0, 0]);
}

/// pack_grid：トリムしたフレームは元の位置に戻り、空きは fill で塗る。アルファのない形式で fill null ならアルファ付きになる。
#[test]
fn pack_grid_places_trimmed_frames_and_fills() {
    let input = InputGen::TrimmedFrames {
        w: 6,
        h: 4,
        fmt: PixelFormat::Rgb8,
        count: 3,
        seed: 7,
    }
    .generate()
    .unwrap();
    let Value::Frames(fr) = &input else { panic!() };
    let out = op(
        "sprite.pack_grid",
        json!({ "columns": 2, "gapX": 1, "gapY": 0, "fill": null }),
        input.clone(),
    );
    let sheet = image(&out);
    assert_eq!(sheet.px.format(), PixelFormat::Rgba8);
    assert_eq!((sheet.px.width(), sheet.px.height()), (13, 8));
    for (i, f) in fr.frames().iter().enumerate() {
        let (ox, oy) = ((i % 2) as i64 * 7, (i / 2) as i64 * 4);
        for y in 0..f.px.height() {
            for x in 0..f.px.width() {
                let s = f.px.pixel(x, y).unwrap();
                let d = sheet
                    .px
                    .pixel((ox + f.offset.x) as u32 + x, (oy + f.offset.y) as u32 + y)
                    .unwrap();
                assert_eq!(d, &[s[0], s[1], s[2], 255][..]);
            }
        }
    }
    // 間隔（x = 6）と最後の行の空き（右下のセル）は透明
    assert_eq!(sheet.px.pixel(6, 0), Some(&[0u8, 0, 0, 0][..]));
    assert_eq!(sheet.px.pixel(12, 7), Some(&[0u8, 0, 0, 0][..]));
    assert_eq!(sheet.meta.grid.map(|g| (g.cell_w, g.gap_x)), Some((6, 1)));
}

/// 持ち上げ：大きさの決まらないフレームの列（合併のトリムのあと）にも、出力の大きさが入力で決まる操作
/// （90° 回転・自動の軸のリサイズ・キャンバス）は持ち上げられ、各フレームに当てた結果と同じになる。
#[test]
fn lifting_works_on_at_most_frames() {
    let engine = Engine::new().unwrap();
    let input = InputGen::NoiseFrames {
        w: 12,
        h: 10,
        fmt: PixelFormat::Rgba8,
        count: 3,
        seed: 8,
    }
    .generate()
    .unwrap();
    for tail in [
        json!(["image.rotate90", { "turn": "cw" }]),
        json!(["image.resize", { "width": "auto", "height": { "px": 5 }, "filter": "nearest" }]),
        json!(["image.canvas", { "rect": { "x": -1, "y": -1, "w": 9, "h": 9 }, "fill": null }]),
    ] {
        let mut g = kura_engine::doc::Graph::new_single();
        g.push_linear(
            "image.trim_alpha",
            1,
            json!({ "threshold": 250, "mode": "union" }),
            0,
        )
        .unwrap();
        g.push_linear(
            "image.resize",
            1,
            json!({ "width": { "px": 7 }, "height": "auto", "filter": "nearest" }),
            0,
        )
        .unwrap();
        g.push_linear(tail[0].as_str().unwrap(), 1, tail[1].clone(), 0)
            .unwrap();
        let out = kura_engine::testing::render_with(
            &engine,
            &g,
            input.clone(),
            Digest::default(),
            &NoMonitor,
        )
        .unwrap_or_else(|e| panic!("{tail}: {e}"));
        let Value::Frames(fr) = &out else { panic!() };
        assert_eq!(fr.frames().len(), 3, "{tail}");
        assert!(fr.is_full());
    }
}

/// 描くときのエラーには、どのノードかが付く（エディタが強調できるように）。
#[test]
fn render_errors_carry_the_node() {
    let engine = Engine::new().unwrap();
    let mut g = kura_engine::doc::Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    g.push_linear(
        "palette.replace",
        1,
        json!({ "entries": [{ "index": 20, "color": [0, 0, 0, 0] }] }),
        0,
    )
    .unwrap();
    let e = kura_engine::testing::render_with(
        &engine,
        &g,
        noise(4, 4, PixelFormat::Indexed8, 1),
        Digest::default(),
        &NoMonitor,
    )
    .unwrap_err();
    assert_eq!(e.node(), Some(kura_engine::NodeId(2)), "{e:?}");
    // 実行でしか分からない描画の誤りには、何番目の動作かも付く
    let stroke = |color: serde_json::Value| {
        json!({ "type": "stroke", "tool": "pen", "color": color, "blend": "replace",
                "brush": { "shape": "square", "size": 1 }, "pixelPerfect": false, "frame": 0,
                "points": PointList(vec![IPoint { x: 0, y: 0 }]).pack() })
    };
    let mut actions = vec![stroke(json!({ "index": 1 })); 299];
    actions.push(stroke(json!({ "index": 200 })));
    let e = render_value(
        "paint.actions",
        1,
        json!({ "actions": actions }),
        noise(4, 4, PixelFormat::Indexed8, 1),
    )
    .unwrap_err();
    assert!(e.to_string().contains("動作 299："), "{e}");
    // トリムしたフレームの列に持ち上げられない操作は、実行でノード付きで断る
    let mut g = kura_engine::doc::Graph::new_single();
    g.push_linear(
        "image.trim_alpha",
        1,
        json!({ "threshold": 100, "mode": "perFrame" }),
        0,
    )
    .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    // 透明な余白のあるフレーム（トリムで大きさが変わる。推論では分からないので、実行で断る）
    let Value::Image(sprite) = InputGen::Sprite {
        w: 6,
        h: 5,
        rect: IRect::new(1, 1, 3, 2),
    }
    .generate()
    .unwrap() else {
        panic!()
    };
    let input = Value::Frames(Arc::new(
        Frames::full(
            vec![(sprite.px.clone(), 40), (sprite.px.clone(), 60)],
            ImageMeta::default(),
            0,
        )
        .unwrap(),
    ));
    let e = kura_engine::testing::render_with(&engine, &g, input, Digest::default(), &NoMonitor)
        .unwrap_err();
    assert_eq!(e.node(), Some(kura_engine::NodeId(2)), "{e:?}");
}

/// 塗りつぶしの起点：範囲の外（±2^31 を超える）は読み込みで断り、トリムしたフレームの外は何もしない（あふれない）。
#[test]
fn fill_origin_outside_is_safe() {
    let frames = InputGen::TrimmedFrames {
        w: 8,
        h: 6,
        fmt: PixelFormat::Rgba8,
        count: 3,
        seed: 2,
    }
    .generate()
    .unwrap();
    let fill = |x: i64, frame: u32| {
        json!({ "actions": [{ "type": "fill", "at": { "x": x, "y": 0 }, "color": { "rgba": [1, 2, 3, 255] },
                  "tolerance": 0, "connectivity": "four", "contiguous": true, "frame": frame }] })
    };
    assert!(render_value("paint.actions", 1, fill(i64::MIN, 2), frames.clone()).is_err());
    assert!(render_value("paint.actions", 1, fill(-(1 << 31) - 1, 2), frames.clone()).is_err());
    // フレーム 2 は位置 (2, 2) にある。キャンバスの (0, 0) はフレームの外なので何もしない
    assert_eq!(
        render_value("paint.actions", 1, fill(-(1 << 31), 2), frames.clone()).unwrap(),
        frames
    );
    assert_eq!(
        render_value("paint.actions", 1, fill(0, 2), frames.clone()).unwrap(),
        frames
    );
}

/// トリムしたフレーム（位置が 0 でない）への描画：座標はキャンバスの座標で、フレームの外は描かない。
#[test]
fn painting_trimmed_frames_uses_canvas_coordinates() {
    let frames = InputGen::TrimmedFrames {
        w: 8,
        h: 6,
        fmt: PixelFormat::Rgba8,
        count: 3,
        seed: 5,
    }
    .generate()
    .unwrap();
    let Value::Frames(src) = &frames else {
        panic!()
    };
    let f2 = &src.frames()[2];
    assert_eq!(f2.offset, IPoint { x: 2, y: 2 });
    let points = PointList(vec![IPoint { x: 0, y: 0 }, IPoint { x: 7, y: 7 }]);
    let action = PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Rgba([9, 9, 9, 255])),
        blend: Blend::Replace,
        brush: Brush {
            shape: BrushShape::Square,
            size: 1,
        },
        pixel_perfect: false,
        frame: 2,
        points,
    };
    let Value::Frames(out) = op(
        "paint.actions",
        json!({ "actions": [action] }),
        frames.clone(),
    ) else {
        panic!()
    };
    let painted = &out.frames()[2].px;
    for y in 0..painted.height() {
        for x in 0..painted.width() {
            let on_line = x == y;
            let got = painted.pixel(x, y).unwrap();
            if on_line {
                assert_eq!(got, &[9u8, 9, 9, 255][..], "({x},{y})");
            } else {
                assert_eq!(got, f2.px.pixel(x, y).unwrap(), "({x},{y})");
            }
        }
    }
    // ほかのフレームは変わらない
    assert_eq!(out.frames()[0].px, src.frames()[0].px);
}

/// トリムの合併：合併と重ならないフレームは、透明な色のないパレットでも、そのフレームの左上の 1 画素になる（Internal にならない）。
#[test]
fn trim_union_keeps_palettes_without_clear_colors() {
    let pal = vec![[0, 0, 0, 100], [9, 9, 9, 200], [7, 7, 7, 255]];
    let px = |data: &[u8]| {
        Raster::from_rows(
            4,
            4,
            PixelFormat::Indexed8,
            Some(Palette {
                colors: pal.clone(),
            }),
            data,
        )
        .unwrap()
    };
    let mut a = vec![0u8; 16];
    a[0] = 2;
    let b = vec![0u8; 16];
    let input = Value::Frames(Arc::new(
        Frames::new(
            vec![
                kura_engine::value::Frame {
                    px: px(&a),
                    offset: IPoint { x: 0, y: 0 },
                    duration_ms: 10,
                },
                kura_engine::value::Frame {
                    px: px(&b),
                    offset: IPoint { x: 4, y: 4 },
                    duration_ms: 10,
                },
            ],
            Size { w: 8, h: 8 },
            ImageMeta::default(),
            0,
        )
        .unwrap(),
    ));
    let Value::Frames(out) = op(
        "image.trim_alpha",
        json!({ "threshold": 200, "mode": "union" }),
        input,
    ) else {
        panic!()
    };
    assert_eq!(out.canvas(), Size { w: 1, h: 1 });
    assert_eq!(out.frames()[1].px.to_rows().unwrap(), [0]);
    assert_eq!(out.frames()[1].px.palette().unwrap().colors, pal);
}

/// 平滑なリサイズで大きさが変わらないときはそのまま通す（α = 0 の色も保つ）。sRGB でない ICC は警告する。
#[test]
fn smooth_resize_same_size_and_icc_warning() {
    let data = [200u8, 100, 50, 0, 1, 2, 3, 255, 4, 5, 6, 128, 7, 8, 9, 0];
    let input = Value::Image(Arc::new(Image {
        px: Raster::from_rows(2, 2, PixelFormat::Rgba8, None, &data).unwrap(),
        meta: ImageMeta::default(),
    }));
    for filter in ["box", "bilinear", "lanczos3"] {
        let out = op(
            "image.resize",
            json!({ "width": { "px": 2 }, "height": { "px": 2 }, "filter": filter }),
            input.clone(),
        );
        assert_eq!(image(&out).px, image(&input).px, "{filter}");
    }
    let engine = Engine::new().unwrap();
    let Value::Image(img) = &input else { panic!() };
    let mut with_icc = (**img).clone();
    with_icc.meta.icc = Some(Arc::new(vec![0u8; 200]));
    let g = single_node_graph(
        "image.resize",
        1,
        json!({ "width": { "px": 1 }, "height": "auto", "filter": "box" }),
    )
    .unwrap();
    let r = render_full(
        &engine,
        &g,
        Value::Image(Arc::new(with_icc)),
        Digest::default(),
        &NoMonitor,
    )
    .unwrap();
    assert!(
        r.warnings
            .iter()
            .any(|w| w.code == IssueCode::ColorSpaceAssumed)
    );
    let r = render_full(&engine, &g, input, Digest::default(), &NoMonitor).unwrap();
    assert!(r.warnings.is_empty());
    // 持ち上げても、同じ警告はノードごとに 1 つ
    let Value::Image(base) = noise(3, 2, PixelFormat::Rgba8, 1) else {
        panic!()
    };
    let meta = ImageMeta {
        icc: Some(Arc::new(vec![0u8; 200])),
        ..Default::default()
    };
    let frames = Value::Frames(Arc::new(
        Frames::full(
            vec![
                (base.px.clone(), 10),
                (base.px.clone(), 20),
                (base.px.clone(), 30),
            ],
            meta,
            0,
        )
        .unwrap(),
    ));
    let r = render_full(&engine, &g, frames, Digest::default(), &NoMonitor).unwrap();
    assert_eq!(
        r.warnings
            .iter()
            .filter(|w| w.code == IssueCode::ColorSpaceAssumed)
            .count(),
        1
    );
}

/// 描画の手間：塗りつぶしは画像の画素の数で数え、上限を超える文書は推論で断る。
#[test]
fn paint_work_counts_fills_by_pixels() {
    let engine = Engine::new().unwrap();
    let fill = json!({ "type": "fill", "at": { "x": 0, "y": 0 }, "color": { "rgba": [1, 2, 3, 255] },
                       "tolerance": 0, "connectivity": "four", "contiguous": false, "frame": 0 });
    // 8192² の画像に塗りつぶし 17 回は 2^26 × 17 > 2^30
    let g = single_node_graph(
        "paint.actions",
        1,
        json!({ "actions": vec![fill.clone(); 17] }),
    )
    .unwrap();
    let shape = kura_engine::Shape::Image {
        w: kura_engine::Dim::Exact(8192),
        h: kura_engine::Dim::Exact(8192),
        fmt: PixelFormat::Rgba8,
    };
    let issues = engine
        .compile(
            &g,
            &std::collections::BTreeMap::from([(kura_engine::doc::InputId(0), shape)]),
        )
        .err()
        .unwrap();
    assert_eq!(issues.0[0].code, IssueCode::TooLarge);
    let g = single_node_graph("paint.actions", 1, json!({ "actions": vec![fill; 15] })).unwrap();
    assert!(
        engine
            .compile(
                &g,
                &std::collections::BTreeMap::from([(kura_engine::doc::InputId(0), shape)])
            )
            .is_ok()
    );
}

/// 差分の適用を、型を消した口（登録表の DynOp）から呼んでも、全体を当て直したものと同じ。
#[test]
fn appended_paint_through_the_registry() {
    let engine = Engine::new().unwrap();
    let op = engine
        .registry()
        .all()
        .into_iter()
        .find(|o| o.id() == "paint.actions")
        .unwrap();
    for ex in op.examples().unwrap() {
        let all = ex.params["actions"].as_array().unwrap().clone();
        let input = ex.inputs[0].generate().unwrap();
        let limits = kura_engine::Limits::default();
        let mut cx = kura_engine::op::ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
        let prev_raw = json!({ "actions": all[..all.len() / 2].to_vec() });
        let prev = op.parse(&prev_raw).unwrap();
        let full = op.parse(&ex.params).unwrap();
        let prev_out = op.apply(prev.as_ref(), &[&input], &mut cx).unwrap().value;
        let inc = op
            .apply_appended(prev.as_ref(), &prev_out, full.as_ref(), &mut cx)
            .unwrap()
            .unwrap()
            .value;
        let whole = op.apply(full.as_ref(), &[&input], &mut cx).unwrap().value;
        assert_eq!(inc, whole, "{}", ex.name);
    }
}
