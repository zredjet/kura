//! E51：相対の指定の操作（M14 段 1。docs/editing.md §7.4・§17）。
//!
//! - `image.pad`・`image.fit_canvas` は、入力の形から計算した矩形の `image.canvas` と、値・メタデータ・警告・推論した形が
//!   ビット単位で同じ（例のすべてと、ChaCha8 のランダムな大きさ・形式（Indexed8 を含む）・余白・比・9 点・長さの種類）。
//! - `meta.anchor_pivot` は、計算した点の `meta.set_pivot` と同じ。
//! - 余白 m と −m を続けると元の画素に戻る、9 点の位置は床の規則、`allowCrop` がなければ縮む大きさは断る、メタデータは
//!   平行移動で写り、はみ出たものは警告で落ちる、0・上限を超える・あふれる大きさは断る。
//!
//! 矩形と点は、このファイルの中で操作の実装とは別に書いた式（参照の式）で求める。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::doc::{Graph, InputId};
use kura_engine::geom::{IPoint, IRect};
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::region::below;
use kura_engine::testing::{render_full, single_node_graph};
use kura_engine::value::{PixelFormat, Slice};
use kura_engine::{
    Digest, Dim, Engine, EngineError, IssueCode, Issues, Limits, NoMonitor, Rendered, Shape, Value,
    determinism,
};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::{Value as J, json};

fn engine() -> Engine {
    Engine::new().unwrap()
}

// ===== 参照の式（操作の実装とは別に書く） =====

/// 最も近い整数（ちょうど半分なら偶数へ）。d > 0。
fn round_half_even(n: i128, d: i128) -> i128 {
    let (q, r) = (n.div_euclid(d), n.rem_euclid(d));
    if 2 * r > d || (2 * r == d && q % 2 != 0) {
        q + 1
    } else {
        q
    }
}

/// 余白 1 辺の画素の数（長さ `len` の軸）。
fn ref_margin(m: &J, len: i64) -> i64 {
    if let Some(px) = m.get("px") {
        return px.as_i64().unwrap();
    }
    let s = &m["scale"];
    let (num, den) = (s["num"].as_i64().unwrap(), s["den"].as_i64().unwrap());
    round_half_even(i128::from(len) * i128::from(num), i128::from(den)) as i64
}

/// `image.pad` の新しいキャンバス（入力の座標）。大きさが 1 より小さければ None。
fn ref_pad_rect(p: &J, w: i64, h: i64) -> Option<IRect> {
    let (t, r) = (ref_margin(&p["top"], h), ref_margin(&p["right"], w));
    let (b, l) = (ref_margin(&p["bottom"], h), ref_margin(&p["left"], w));
    let (nw, nh) = (w + l + r, h + t + b);
    (nw >= 1 && nh >= 1).then(|| IRect::new(-l, -t, nw as u32, nh as u32))
}

/// キャンバスの長さ 1 辺。
fn ref_length(v: &J, this: i64, other: i64) -> i64 {
    match v {
        J::String(s) if s == "keep" => this,
        J::String(s) if s == "pow2" => {
            let mut p = 1;
            while p < this {
                p *= 2;
            }
            p
        }
        J::String(s) if s == "longest" => this.max(other),
        _ if v.get("px").is_some() => v["px"].as_i64().unwrap(),
        _ if v.get("multipleOf").is_some() => {
            let n = v["multipleOf"].as_i64().unwrap();
            (this + n - 1) / n * n
        }
        _ => {
            let s = &v["scale"];
            let (num, den) = (s["num"].as_i64().unwrap(), s["den"].as_i64().unwrap());
            (round_half_even(i128::from(this) * i128::from(num), i128::from(den)) as i64).max(1)
        }
    }
}

/// 9 点の基準の係数（左・上 0、中央 1、右・下 2）。
fn ref_factors(anchor: &str) -> (i64, i64) {
    let names = [
        "topLeft",
        "top",
        "topRight",
        "left",
        "center",
        "right",
        "bottomLeft",
        "bottom",
        "bottomRight",
    ];
    let i = names.iter().position(|n| *n == anchor).unwrap() as i64;
    (i % 3, i / 3)
}

/// `image.fit_canvas` の新しいキャンバス。`allowCrop` がなく画像より小さければ None。
fn ref_fit_rect(p: &J, w: i64, h: i64) -> Option<IRect> {
    let (nw, nh) = (
        ref_length(&p["width"], w, h),
        ref_length(&p["height"], h, w),
    );
    if !p["allowCrop"].as_bool().unwrap_or(false) && (nw < w || nh < h) {
        return None;
    }
    let (ax, ay) = ref_factors(p["anchor"].as_str().unwrap());
    let ox = ((nw - w) * ax).div_euclid(2);
    let oy = ((nh - h) * ay).div_euclid(2);
    Some(IRect::new(-ox, -oy, nw as u32, nh as u32))
}

/// `meta.anchor_pivot` の点。
fn ref_pivot(p: &J, w: i64, h: i64) -> IPoint {
    let (ax, ay) = ref_factors(p["anchor"].as_str().unwrap());
    let off = &p["offset"];
    IPoint {
        x: (w * ax).div_euclid(2) + off["x"].as_i64().unwrap(),
        y: (h * ay).div_euclid(2) + off["y"].as_i64().unwrap(),
    }
}

// ===== 描く補助 =====

/// 画像の大きさ（フレームの列はキャンバス）。
fn size_of(v: &Value) -> (i64, i64) {
    match v {
        Value::Image(img) => (i64::from(img.px.width()), i64::from(img.px.height())),
        Value::Frames(fr) => (i64::from(fr.canvas().w), i64::from(fr.canvas().h)),
        Value::Audio(_) => panic!("音声"),
    }
}

fn shapes(v: &Value) -> BTreeMap<InputId, Shape> {
    BTreeMap::from([(InputId(0), v.shape())])
}

/// 操作 1 つの文書を描く（推論した出力の形も返す）。
fn run(engine: &Engine, op: &str, params: J, input: &Value) -> Result<(Shape, Rendered), String> {
    let g = single_node_graph(op, 1, params).unwrap();
    let plan = engine
        .compile(&g, &shapes(input))
        .map_err(|e| e.to_string())?;
    let shape = plan.output_shapes()[0].1;
    let out = render_full(engine, &g, input.clone(), Digest::default(), &NoMonitor)
        .map_err(|e| e.to_string())?;
    Ok((shape, out))
}

/// 推論で断られた指摘（通れば None）。
fn compile_issues(engine: &Engine, g: &Graph, input: &Value) -> Option<Issues> {
    engine.compile(g, &shapes(input)).err()
}

/// 2 つの結果が、推論した形・値・ダイジェスト・警告まで同じ（どちらも誤りなら同じ文言）。
fn assert_same(
    what: &str,
    a: Result<(Shape, Rendered), String>,
    b: Result<(Shape, Rendered), String>,
) {
    match (a, b) {
        (Ok((sa, ra)), Ok((sb, rb))) => {
            assert_eq!(sa, sb, "{what}：推論した形");
            assert_eq!(
                ra.outputs[0].digest, rb.outputs[0].digest,
                "{what}：ダイジェスト"
            );
            assert_eq!(ra.outputs[0].value, rb.outputs[0].value, "{what}：値");
            assert_eq!(ra.warnings, rb.warnings, "{what}：警告");
        }
        (Err(a), Err(b)) => assert_eq!(a, b, "{what}：誤り"),
        (a, b) => panic!(
            "{what}：片方だけが通りました（{:?} / {:?}）",
            a.map(|r| r.0),
            b.map(|r| r.0)
        ),
    }
}

fn canvas(rect: IRect, fill: &J) -> J {
    json!({ "rect": rect, "fill": fill })
}

fn pivot(pt: IPoint) -> J {
    json!({ "pivot": pt })
}

// ===== ランダムな引数と入力 =====

fn random_format(r: &mut ChaCha8Rng) -> PixelFormat {
    PixelFormat::ALL[below(r, PixelFormat::ALL.len() as u64) as usize]
}

/// 画像・フレームの列・シート（格子とピボット）・スライス付きのシート。`trimmed` ならトリムしたフレームの列も。
fn random_input(r: &mut ChaCha8Rng, trimmed: bool) -> InputGen {
    let big = below(r, 8) == 0;
    let side = |r: &mut ChaCha8Rng| {
        if big {
            1 + below(r, 300) as u32
        } else {
            1 + below(r, 40) as u32
        }
    };
    let (w, h) = (side(r), side(r));
    match below(r, if trimmed { 6 } else { 5 }) {
        0 | 1 => InputGen::Noise {
            w,
            h,
            fmt: random_format(r),
            seed: r.next_u64(),
        },
        2 => InputGen::NoiseFrames {
            w: w.min(40),
            h: h.min(40),
            fmt: random_format(r),
            count: 1 + below(r, 3) as u32,
            seed: r.next_u64(),
        },
        3 => InputGen::Sheet {
            cols: 1 + below(r, 4) as u32,
            rows: 1 + below(r, 3) as u32,
            cell: 1 + below(r, 20) as u32,
        },
        4 => {
            let cell = 2 + below(r, 16) as u32;
            InputGen::WithMeta {
                base: Box::new(InputGen::Sheet {
                    cols: 2,
                    rows: 2,
                    cell,
                }),
                meta: MetaGen {
                    slices: vec![Slice {
                        name: "s".into(),
                        rect: IRect::new(1, 1, cell, cell),
                        center: None,
                        pivot: Some(IPoint { x: 1, y: 2 }),
                    }],
                    ..MetaGen::default()
                },
            }
        }
        _ => InputGen::TrimmedFrames {
            w: 4 + w.min(40),
            h: 4 + h.min(40),
            fmt: random_format(r),
            count: 1 + below(r, 3) as u32,
            seed: r.next_u64(),
        },
    }
}

fn random_fill(r: &mut ChaCha8Rng) -> J {
    match below(r, 4) {
        0 | 1 => J::Null,
        // 灰色の不透明（どの形式でも塗れる）
        2 => {
            let v = below(r, 256);
            json!([v, v, v, 255])
        }
        _ => json!([below(r, 256), below(r, 256), below(r, 256), below(r, 256)]),
    }
}

fn random_ratio(r: &mut ChaCha8Rng, min_num: u64) -> J {
    json!({ "num": min_num + below(r, 5), "den": 1 + below(r, 12) })
}

fn random_margin(r: &mut ChaCha8Rng, len: i64) -> J {
    match below(r, 4) {
        0 => json!({ "scale": random_ratio(r, 0) }),
        // 削る（長さを超えることもある）
        1 => json!({ "px": -(below(r, len as u64 + 2) as i64) }),
        _ => json!({ "px": below(r, 24) }),
    }
}

fn random_length(r: &mut ChaCha8Rng, len: i64) -> J {
    match below(r, 7) {
        0 => json!({ "px": 1 + below(r, len as u64 * 2 + 4) }),
        1 => json!({ "scale": random_ratio(r, 1) }),
        2 => json!("keep"),
        3 => json!({ "multipleOf": 1 + below(r, 20) }),
        4 => json!("pow2"),
        5 => json!("longest"),
        _ => json!({ "px": 1 + below(r, 8) }),
    }
}

fn random_anchor(r: &mut ChaCha8Rng) -> &'static str {
    [
        "topLeft",
        "top",
        "topRight",
        "left",
        "center",
        "right",
        "bottomLeft",
        "bottom",
        "bottomRight",
    ][below(r, 9) as usize]
}

// ===== E51 =====

/// 例のすべてで、pad・fit_canvas は計算した矩形の canvas と、anchor_pivot は計算した点の set_pivot と同じ。
#[test]
fn examples_equal_canvas_and_set_pivot() {
    let engine = engine();
    let mut seen = BTreeMap::new();
    for op in engine.registry().current() {
        let id = op.id();
        if !matches!(id, "image.pad" | "image.fit_canvas" | "meta.anchor_pivot") {
            continue;
        }
        for ex in op.examples().unwrap() {
            let input = ex.inputs[0].generate().unwrap();
            let (w, h) = size_of(&input);
            let what = format!("{id} {}", ex.name);
            let want = match id {
                "image.pad" => {
                    let rect = ref_pad_rect(&ex.params, w, h).unwrap();
                    run(
                        &engine,
                        "image.canvas",
                        canvas(rect, &ex.params["fill"]),
                        &input,
                    )
                }
                "image.fit_canvas" => {
                    let rect = ref_fit_rect(&ex.params, w, h).unwrap();
                    run(
                        &engine,
                        "image.canvas",
                        canvas(rect, &ex.params["fill"]),
                        &input,
                    )
                }
                _ => run(
                    &engine,
                    "meta.set_pivot",
                    pivot(ref_pivot(&ex.params, w, h)),
                    &input,
                ),
            };
            assert!(want.is_ok(), "{what}：見本の例が描けません");
            assert_same(&what, run(&engine, id, ex.params.clone(), &input), want);
            *seen.entry(id).or_insert(0) += 1;
        }
    }
    assert_eq!(
        seen.keys().copied().collect::<Vec<_>>(),
        ["image.fit_canvas", "image.pad", "meta.anchor_pivot"]
    );
    assert!(seen.values().all(|n| *n >= 4), "{seen:?}");
}

/// ランダムな大きさ・形式・余白・比・長さ・9 点で、pad・fit_canvas は canvas と同じ。矩形が作れない（大きさが 0 以下・
/// `allowCrop` なしで縮む）ものは推論で断る。
#[test]
fn random_pad_and_fit_canvas_equal_canvas() {
    let engine = engine();
    let mut r = determinism::rng(0x0e51_0001);
    let (mut same, mut refused, mut cropped, mut indexed) = (0, 0, 0, 0);
    for i in 0..500 {
        let make = random_input(&mut r, false);
        let input = make.generate().unwrap();
        let (w, h) = size_of(&input);
        let fill = random_fill(&mut r);
        let pad = i % 2 == 0;
        let (op, params, rect) = if pad {
            let p = json!({
                "top": random_margin(&mut r, h),
                "right": random_margin(&mut r, w),
                "bottom": random_margin(&mut r, h),
                "left": random_margin(&mut r, w),
                "fill": fill,
            });
            let rect = ref_pad_rect(&p, w, h);
            ("image.pad", p, rect)
        } else {
            let p = json!({
                "width": random_length(&mut r, w),
                "height": random_length(&mut r, h),
                "anchor": random_anchor(&mut r),
                "fill": fill,
                "allowCrop": below(&mut r, 2) == 0,
            });
            let rect = ref_fit_rect(&p, w, h);
            ("image.fit_canvas", p, rect)
        };
        let what = format!("{i}：{op} {params} {make:?}");
        let Some(rect) = rect else {
            let g = single_node_graph(op, 1, params.clone()).unwrap();
            let issues =
                compile_issues(&engine, &g, &input).unwrap_or_else(|| panic!("{what}：通りました"));
            assert!(
                issues.0.iter().all(|i| i.code == IssueCode::OutOfRange),
                "{what}：{issues}"
            );
            refused += 1;
            continue;
        };
        if rect.w < w as u32 || rect.h < h as u32 {
            cropped += 1;
        }
        if matches!(
            make,
            InputGen::Noise {
                fmt: PixelFormat::Indexed8,
                ..
            }
        ) {
            indexed += 1;
        }
        assert_same(
            &what,
            run(&engine, op, params, &input),
            run(&engine, "image.canvas", canvas(rect, &fill), &input),
        );
        same += 1;
    }
    assert!(
        same > 300 && refused > 20 && cropped > 20 && indexed > 10,
        "same {same}, refused {refused}, cropped {cropped}, indexed {indexed}"
    );
}

/// anchor_pivot は、画像（フレームの列はキャンバス。トリムしたフレームも）の 9 点とずれで求めた点の set_pivot と同じ。
/// 画像の外になる点は推論で断る（引数 `offset`）。
#[test]
fn random_anchor_pivot_equals_set_pivot() {
    let engine = engine();
    let mut r = determinism::rng(0x0e51_0002);
    let (mut same, mut refused) = (0, 0);
    for i in 0..300 {
        let make = random_input(&mut r, true);
        let input = make.generate().unwrap();
        let (w, h) = size_of(&input);
        let off = |r: &mut ChaCha8Rng, n: i64| below(r, n as u64 + 7) as i64 - (n / 2 + 3);
        let p = json!({
            "anchor": random_anchor(&mut r),
            "offset": { "x": off(&mut r, w), "y": off(&mut r, h) },
        });
        let pt = ref_pivot(&p, w, h);
        let what = format!("{i}：{p} {make:?}");
        if !(0..=w).contains(&pt.x) || !(0..=h).contains(&pt.y) {
            let g = single_node_graph("meta.anchor_pivot", 1, p).unwrap();
            let issues =
                compile_issues(&engine, &g, &input).unwrap_or_else(|| panic!("{what}：通りました"));
            assert!(
                issues.0.iter().all(
                    |i| i.code == IssueCode::OutOfRange && i.param.as_deref() == Some("offset")
                ),
                "{what}：{issues}"
            );
            refused += 1;
            continue;
        }
        assert_same(
            &what,
            run(&engine, "meta.anchor_pivot", p, &input),
            run(&engine, "meta.set_pivot", pivot(pt), &input),
        );
        same += 1;
    }
    assert!(same > 80 && refused > 30, "same {same}, refused {refused}");
}

/// 余白 m（0 以上）を足してから −m を足すと、元の画素に戻る：どの形式でも、元を全体の矩形の canvas（透明）に通したものと
/// 同じ（アルファのない形式はアルファ付きになるだけ）。アルファのある形式（Indexed8 を除く）は元とビット単位で同じ。
#[test]
fn pad_then_negative_pad_restores() {
    let engine = engine();
    let mut r = determinism::rng(0x0e51_0003);
    for i in 0..120 {
        let fmt = random_format(&mut r);
        let input = InputGen::Noise {
            w: 1 + below(&mut r, 40) as u32,
            h: 1 + below(&mut r, 40) as u32,
            fmt,
            seed: r.next_u64(),
        }
        .generate()
        .unwrap();
        let (w, h) = size_of(&input);
        let m: Vec<i64> = (0..4).map(|_| below(&mut r, 20) as i64).collect();
        let side = |s: i64| json!({ "px": s });
        let mut g = Graph::new_single();
        let grow = json!({ "top": side(m[0]), "right": side(m[1]), "bottom": side(m[2]), "left": side(m[3]), "fill": null });
        let back = json!({ "top": side(-m[0]), "right": side(-m[1]), "bottom": side(-m[2]), "left": side(-m[3]), "fill": null });
        g.push_linear("image.pad", 1, grow, 1).unwrap();
        g.push_linear("image.pad", 1, back, 2).unwrap();
        let out = render_full(&engine, &g, input.clone(), Digest::default(), &NoMonitor)
            .unwrap_or_else(|e| panic!("{i} {fmt:?} {m:?}：{e}"));
        let want = run(
            &engine,
            "image.canvas",
            canvas(IRect::new(0, 0, w as u32, h as u32), &J::Null),
            &input,
        )
        .unwrap()
        .1;
        assert_eq!(
            out.outputs[0].value, want.outputs[0].value,
            "{i} {fmt:?} {m:?}"
        );
        if fmt.has_alpha() && fmt != PixelFormat::Indexed8 {
            assert_eq!(out.outputs[0].value, input, "{i} {fmt:?} {m:?}");
        }
    }
}

/// 9 点の位置は ⌊(外 − 中)·a/2⌋：広げる余りの 1px は右・下に、狭める余りの 1px は左・上から削る（中身は左・上へ寄る）。
#[test]
fn fit_canvas_positions_follow_the_floor_rule() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 7,
        h: 5,
        fmt: PixelFormat::Rgba8,
        seed: 3,
    }
    .generate()
    .unwrap();
    let Value::Image(src) = &input else { panic!() };
    // 7×5 を 10×8 の中央に：中身の左上は (1, 1)（余りの 1px は右・下）
    let fit = |w: J, h: J, anchor: &str, crop: bool| {
        let p =
            json!({ "width": w, "height": h, "anchor": anchor, "fill": null, "allowCrop": crop });
        let (_, out) = run(&engine, "image.fit_canvas", p, &input).unwrap();
        let Value::Image(img) = &out.outputs[0].value else {
            panic!()
        };
        img.clone()
    };
    let img = fit(json!({"px": 10}), json!({"px": 8}), "center", false);
    assert_eq!((img.px.width(), img.px.height()), (10, 8));
    assert_eq!(img.px.pixel(1, 1), src.px.pixel(0, 0));
    assert_eq!(img.px.pixel(7, 5), src.px.pixel(6, 4));
    assert_eq!(img.px.pixel(0, 0), Some(&[0u8, 0, 0, 0][..]));
    assert_eq!(img.px.pixel(8, 6), Some(&[0u8, 0, 0, 0][..]));
    // 右下・左上・偶数の差（7×5 を 9×7）
    let img = fit(json!({"px": 10}), json!({"px": 8}), "bottomRight", false);
    assert_eq!(img.px.pixel(3, 3), src.px.pixel(0, 0));
    let img = fit(json!({"px": 9}), json!({"px": 7}), "center", false);
    assert_eq!(img.px.pixel(1, 1), src.px.pixel(0, 0));
    let img = fit(json!({"px": 9}), json!({"px": 7}), "topLeft", false);
    assert_eq!(img.px.pixel(0, 0), src.px.pixel(0, 0));
    // 7×5 を 4×2 の中央に切る：x は ⌊−3/2⌋ = −2、y は ⌊−3/2⌋ = −2（左・上から 2、右・下から 1 削る）
    let img = fit(json!({"px": 4}), json!({"px": 2}), "center", true);
    assert_eq!((img.px.width(), img.px.height()), (4, 2));
    assert_eq!(img.px.pixel(0, 0), src.px.pixel(2, 2));
    assert_eq!(img.px.pixel(3, 1), src.px.pixel(5, 3));
    // 右へ寄せて切る：x は −3
    let img = fit(json!({"px": 4}), json!("keep"), "right", true);
    assert_eq!(img.px.pixel(0, 0), src.px.pixel(3, 0));
}

/// `allowCrop` がなければ、画像より小さくなる大きさは推論で断る（大きさの決まらない入力は実行で断る）。そのまま・倍数・
/// 2 の冪・長い辺は縮まないので、いつも通る。
#[test]
fn shrinking_without_allow_crop_is_refused() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 20,
        h: 12,
        fmt: PixelFormat::Rgb8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let p = |w: J, h: J, crop: bool| json!({ "width": w, "height": h, "anchor": "center", "fill": null, "allowCrop": crop });
    for (w, h, key) in [
        (json!({"px": 19}), json!("keep"), "width"),
        (json!("keep"), json!({"px": 11}), "height"),
        (
            json!({"scale": {"num": 1, "den": 2}}),
            json!("keep"),
            "width",
        ),
    ] {
        let g = single_node_graph("image.fit_canvas", 1, p(w.clone(), h.clone(), false)).unwrap();
        let issues = compile_issues(&engine, &g, &input).expect("通りました");
        assert_eq!(issues.0[0].code, IssueCode::OutOfRange, "{issues}");
        assert_eq!(issues.0[0].param.as_deref(), Some(key), "{issues}");
        assert!(run(&engine, "image.fit_canvas", p(w, h, true), &input).is_ok());
    }
    let mut r = determinism::rng(0x0e51_0004);
    for _ in 0..200 {
        let len = |r: &mut ChaCha8Rng| {
            [
                json!("keep"),
                json!("pow2"),
                json!("longest"),
                json!({"multipleOf": 1 + below(r, 9)}),
            ][below(r, 4) as usize]
                .clone()
        };
        let input = InputGen::Noise {
            w: 1 + below(&mut r, 50) as u32,
            h: 1 + below(&mut r, 50) as u32,
            fmt: PixelFormat::Gray8,
            seed: 1,
        }
        .generate()
        .unwrap();
        let params = json!({ "width": len(&mut r), "height": len(&mut r), "anchor": random_anchor(&mut r), "fill": [9, 9, 9, 255], "allowCrop": false });
        assert!(
            run(&engine, "image.fit_canvas", params.clone(), &input).is_ok(),
            "{params}"
        );
    }
    // 大きさの決まらない入力（透明な余白を切った画像。中身は 10×8）：推論は通り、実行で断る
    let sprite = InputGen::Sprite {
        w: 20,
        h: 20,
        rect: IRect::new(2, 3, 10, 8),
    }
    .generate()
    .unwrap();
    let doc = |params: J| {
        let mut g = Graph::new_single();
        g.push_linear(
            "image.trim_alpha",
            1,
            json!({ "threshold": 0, "mode": "union" }),
            1,
        )
        .unwrap();
        g.push_linear("image.fit_canvas", 1, params, 2).unwrap();
        g
    };
    let g = doc(p(json!({"px": 6}), json!("keep"), false));
    let plan = engine.compile(&g, &shapes(&sprite)).unwrap();
    assert!(
        matches!(
            plan.output_shapes()[0].1,
            Shape::Image {
                w: Dim::Exact(6),
                h: Dim::AtMost(_),
                ..
            }
        ),
        "{:?}",
        plan.output_shapes()
    );
    let e = render_full(&engine, &g, sprite.clone(), Digest::default(), &NoMonitor).unwrap_err();
    assert!(
        matches!(e.root(), EngineError::Invalid(i) if i.0[0].code == IssueCode::OutOfRange),
        "{e:?}"
    );
    let g = doc(p(json!({"px": 6}), json!("keep"), true));
    let out = render_full(&engine, &g, sprite.clone(), Digest::default(), &NoMonitor).unwrap();
    assert_eq!(size_of(&out.outputs[0].value), (6, 8));
    // 余白も同じ：上限の長さでは 1 以上だが、実際の長さで 0 以下になる大きさは実行で断る
    let mut g = Graph::new_single();
    g.push_linear(
        "image.trim_alpha",
        1,
        json!({ "threshold": 0, "mode": "union" }),
        1,
    )
    .unwrap();
    g.push_linear(
        "image.pad",
        1,
        json!({ "top": {"px": 0}, "right": {"px": 0}, "bottom": {"px": 0}, "left": {"px": -12}, "fill": null }),
        2,
    )
    .unwrap();
    assert!(engine.compile(&g, &shapes(&sprite)).is_ok());
    let e = render_full(&engine, &g, sprite, Digest::default(), &NoMonitor).unwrap_err();
    assert!(matches!(e.root(), EngineError::Invalid(_)), "{e:?}");
}

/// 格子・ピボット・スライスは平行移動で写り、はみ出たものは警告付きで落ちる（canvas と同じ写し方）。
#[test]
fn metadata_moves_by_translation() {
    let engine = engine();
    let sheet = InputGen::WithMeta {
        base: Box::new(InputGen::Sheet {
            cols: 4,
            rows: 2,
            cell: 16,
        }),
        meta: MetaGen {
            slices: vec![Slice {
                name: "head".into(),
                rect: IRect::new(2, 1, 12, 10),
                center: None,
                pivot: Some(IPoint { x: 6, y: 9 }),
            }],
            ..MetaGen::default()
        },
    }
    .generate()
    .unwrap();
    let side = |n: i64| json!({ "px": n });
    let pad = |t: i64, r: i64, b: i64, l: i64| json!({ "top": side(t), "right": side(r), "bottom": side(b), "left": side(l), "fill": null });
    let (_, out) = run(&engine, "image.pad", pad(0, 0, 0, 16), &sheet).unwrap();
    let Value::Image(img) = &out.outputs[0].value else {
        panic!()
    };
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(img.meta.grid.unwrap().offset_x, 16);
    assert_eq!(img.meta.pivot, Some(IPoint { x: 24, y: 16 }));
    assert_eq!(img.meta.slices[0].rect, IRect::new(18, 1, 12, 10));
    // 上を 12 削ると、スライスがはみ出て落ちる（警告）
    let (_, out) = run(&engine, "image.pad", pad(-12, 0, 0, 0), &sheet).unwrap();
    assert!(!out.warnings.is_empty());
    let Value::Image(img) = &out.outputs[0].value else {
        panic!()
    };
    assert!(img.meta.slices.is_empty());
    assert_eq!(img.meta.pivot, Some(IPoint { x: 8, y: 4 }));
    assert_same(
        "上を削る",
        run(&engine, "image.pad", pad(-12, 0, 0, 0), &sheet),
        run(
            &engine,
            "image.canvas",
            canvas(IRect::new(0, 12, 64, 20), &J::Null),
            &sheet,
        ),
    );
    // fit_canvas も同じ（64×32 を 80×40 の右下に）
    let p = json!({ "width": {"px": 80}, "height": {"px": 40}, "anchor": "bottomRight", "fill": null, "allowCrop": false });
    let (_, out) = run(&engine, "image.fit_canvas", p, &sheet).unwrap();
    let Value::Image(img) = &out.outputs[0].value else {
        panic!()
    };
    assert_eq!(img.meta.pivot, Some(IPoint { x: 24, y: 24 }));
    assert_eq!(img.meta.slices[0].rect, IRect::new(18, 9, 12, 10));
}

/// 0 の大きさ・上限を超える大きさ・範囲の外の値・使えない比と倍数は断る（どの引数かを付ける）。
#[test]
fn bad_sizes_are_refused() {
    let engine = engine();
    let small = Engine::with_limits(Limits {
        max_dim: 64,
        ..Limits::default()
    })
    .unwrap();
    let input = InputGen::Noise {
        w: 20,
        h: 12,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let pad = |t: J, r: J, b: J, l: J| json!({ "top": t, "right": r, "bottom": b, "left": l, "fill": null });
    let fit = |w: J, h: J| json!({ "width": w, "height": h, "anchor": "center", "fill": null, "allowCrop": true });
    let z = json!({"px": 0});
    let cases: Vec<(&Engine, &str, J, IssueCode, &str)> = vec![
        // 幅が 0・負になる
        (
            &engine,
            "image.pad",
            pad(z.clone(), z.clone(), z.clone(), json!({"px": -20})),
            IssueCode::OutOfRange,
            "left",
        ),
        (
            &engine,
            "image.pad",
            pad(z.clone(), json!({"px": -15}), z.clone(), json!({"px": -6})),
            IssueCode::OutOfRange,
            "right",
        ),
        (
            &engine,
            "image.pad",
            pad(json!({"px": -13}), z.clone(), z.clone(), z.clone()),
            IssueCode::OutOfRange,
            "top",
        ),
        // 両辺を同じだけ削りすぎたら前（左・上）
        (
            &engine,
            "image.pad",
            pad(z.clone(), json!({"px": -10}), z.clone(), json!({"px": -10})),
            IssueCode::OutOfRange,
            "left",
        ),
        (
            &engine,
            "image.pad",
            pad(json!({"px": -6}), z.clone(), json!({"px": -6}), z.clone()),
            IssueCode::OutOfRange,
            "top",
        ),
        // 上限を超える
        (
            &small,
            "image.pad",
            pad(z.clone(), json!({"px": 45}), z.clone(), z.clone()),
            IssueCode::TooLarge,
            "right",
        ),
        (
            &small,
            "image.pad",
            pad(
                z.clone(),
                z.clone(),
                json!({"scale": {"num": 5, "den": 1}}),
                z.clone(),
            ),
            IssueCode::TooLarge,
            "bottom",
        ),
        // 座標の範囲の外・使えない比
        (
            &engine,
            "image.pad",
            pad(
                z.clone(),
                z.clone(),
                z.clone(),
                json!({"px": (1i64 << 31) + 1}),
            ),
            IssueCode::OutOfRange,
            "left",
        ),
        (
            &engine,
            "image.pad",
            pad(z.clone(), z.clone(), z.clone(), json!({"px": i64::MIN})),
            IssueCode::OutOfRange,
            "left",
        ),
        (
            &engine,
            "image.pad",
            pad(
                json!({"scale": {"num": 1, "den": 0}}),
                z.clone(),
                z.clone(),
                z.clone(),
            ),
            IssueCode::OutOfRange,
            "top",
        ),
        (
            &engine,
            "image.pad",
            pad(
                json!({"scale": {"num": 65_537, "den": 1}}),
                z.clone(),
                z.clone(),
                z.clone(),
            ),
            IssueCode::OutOfRange,
            "top",
        ),
        (
            &engine,
            "image.pad",
            pad(
                z.clone(),
                z.clone(),
                z.clone(),
                json!({"scale": {"num": 1, "den": 65_537}}),
            ),
            IssueCode::OutOfRange,
            "left",
        ),
        // キャンバスの長さ
        (
            &engine,
            "image.fit_canvas",
            fit(z.clone(), json!("keep")),
            IssueCode::OutOfRange,
            "width",
        ),
        (
            &engine,
            "image.fit_canvas",
            fit(json!("keep"), json!({"multipleOf": 0})),
            IssueCode::OutOfRange,
            "height",
        ),
        (
            &engine,
            "image.fit_canvas",
            fit(json!({"multipleOf": 65_537}), json!("keep")),
            IssueCode::OutOfRange,
            "width",
        ),
        (
            &engine,
            "image.fit_canvas",
            fit(json!({"scale": {"num": 0, "den": 1}}), json!("keep")),
            IssueCode::OutOfRange,
            "width",
        ),
        (
            &engine,
            "image.fit_canvas",
            fit(json!({"scale": {"num": 1, "den": 0}}), json!("keep")),
            IssueCode::OutOfRange,
            "width",
        ),
        (
            &engine,
            "image.fit_canvas",
            fit(json!("keep"), json!({"scale": {"num": 70_000, "den": 1}})),
            IssueCode::OutOfRange,
            "height",
        ),
        (
            &small,
            "image.fit_canvas",
            fit(json!({"px": 65}), json!("keep")),
            IssueCode::TooLarge,
            "width",
        ),
        (
            &small,
            "image.fit_canvas",
            fit(json!({"multipleOf": 65_536}), json!("keep")),
            IssueCode::TooLarge,
            "width",
        ),
        (
            &engine,
            "image.fit_canvas",
            fit(json!({"px": 40_000}), json!("keep")),
            IssueCode::TooLarge,
            "width",
        ),
        // ピボットが画像の外
        (
            &engine,
            "meta.anchor_pivot",
            json!({ "anchor": "bottomRight", "offset": {"x": 1, "y": 0} }),
            IssueCode::OutOfRange,
            "offset",
        ),
        (
            &engine,
            "meta.anchor_pivot",
            json!({ "anchor": "topLeft", "offset": {"x": 0, "y": -1} }),
            IssueCode::OutOfRange,
            "offset",
        ),
        (
            &engine,
            "meta.anchor_pivot",
            json!({ "anchor": "center", "offset": {"x": (1i64 << 31) + 1, "y": 0} }),
            IssueCode::OutOfRange,
            "offset",
        ),
    ];
    for (eng, op, params, code, key) in cases {
        let g = single_node_graph(op, 1, params.clone()).unwrap();
        let issues =
            compile_issues(eng, &g, &input).unwrap_or_else(|| panic!("{op} {params}：通りました"));
        assert_eq!(issues.0[0].code, code, "{op} {params}：{issues}");
        assert_eq!(
            issues.0[0].param.as_deref(),
            Some(key),
            "{op} {params}：{issues}"
        );
    }
    // 境ちょうどは通る（幅 1・上限ちょうど・画像の端のピボット）
    for (eng, op, params) in [
        (
            &engine,
            "image.pad",
            pad(z.clone(), z.clone(), z.clone(), json!({"px": -19})),
        ),
        (
            &small,
            "image.pad",
            pad(z.clone(), json!({"px": 44}), z.clone(), z.clone()),
        ),
        (
            &small,
            "image.fit_canvas",
            fit(json!({"px": 64}), json!("pow2")),
        ),
        (
            &engine,
            "meta.anchor_pivot",
            json!({ "anchor": "bottomRight", "offset": {"x": 0, "y": 0} }),
        ),
        (
            &engine,
            "meta.anchor_pivot",
            json!({ "anchor": "topLeft", "offset": {"x": 20, "y": 12} }),
        ),
    ] {
        assert!(
            run(eng, op, params.clone(), &input).is_ok(),
            "{op} {params}"
        );
    }
}
