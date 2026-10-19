//! 範囲と範囲の描画の受け入れテスト（段 10。docs/editing.md §12.5・§17 の E18・E23・E24、ADR-154 (4)・(6)）。
//!
//! - reach_is_sound：申告した依存（Reach）の外の入力を壊しても範囲の中は同じ。加えて、範囲の描画を持つ操作は、依存の外を
//!   無効なタイルにした入力で描いても無効なタイルを読まない（読めば誤りになる形なので、申告が小さすぎれば必ず見つかる）。
//! - region_equals_full_crop：選んだタイルは全体を描いたものと同じで、選ばないタイルは前の値と共有する（1・3・16 スレッド）。
//! - render_area_matches_full：ランダムな鎖で、範囲の描画が全体の描画と同じ（依存の外を読んで全体に落ちることもない）。
//! - spoit_via_region・cancel_in_region_and_patch・understated_reach_never_leaks。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::doc::{Dither, Graph, InputId, OutputId, QUANTIZE_VERSION, Quantize};
use kura_engine::error::Issues;
use kura_engine::geom::IRect;
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::monitor::CancelAfter;
use kura_engine::op::{
    Applied, ApplyCx, DynOp, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamSchema, PortSpec, Registry, Signature,
};
use kura_engine::render::area::{AreaRequest, SpoitAt};
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::region::{
    Area, Base, FrameTiles, PartialValue, Reach, Rects, Tiles, frames_of, tiles_in,
};
use kura_engine::render::view::ViewRequest;
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::shape::{Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::{
    below, check_region, check_same_inside, corrupt_outside, only_tiles, random_area, random_chain,
    random_input,
};
use kura_engine::testing::{render_with, single_node_graph, source_of};
use kura_engine::value::{Image, PixelFormat, Raster, TILE};
use kura_engine::{Digest, EngineError, Limits, NoMonitor, Value, determinism};
use rand_chacha::rand_core::Rng;
use serde_json::json;

fn engine() -> Engine {
    Engine::new().unwrap()
}

/// 操作に渡す入力の形（持ち上げる操作のフレームの列は、フレームの画像の形）。
fn op_shapes(op: &dyn DynOp, input: &Value) -> Vec<Shape> {
    let s = input.shape();
    if op.signature().lift == Lift::PerFrame
        && let Some(frame) = s.frame_image()
    {
        return vec![frame];
    }
    vec![s]
}

/// 登録されたすべての操作の例と、境界の場合（負の位置・入力の外だけのキャンバス・非整数の倍率・片方の軸だけのリサイズ・
/// 1 画素・タイルの境）。
fn cases(engine: &Engine) -> Vec<(Arc<dyn DynOp>, String, serde_json::Value, InputGen)> {
    let mut out = Vec::new();
    for op in engine.registry().current() {
        for ex in op.examples().unwrap() {
            out.push((
                op.clone(),
                ex.name.to_string(),
                ex.params,
                ex.inputs[0].clone(),
            ));
        }
    }
    let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
    let extra: Vec<(&str, serde_json::Value, InputGen)> = vec![
        (
            "image.canvas",
            json!({ "rect": { "x": -300, "y": -5, "w": 280, "h": 40 }, "fill": [1, 2, 3, 255] }),
            noise(257, 30, PixelFormat::Rgb8, 1),
        ),
        (
            "image.canvas",
            json!({ "rect": { "x": -3, "y": -260, "w": 600, "h": 600 }, "fill": null }),
            noise(513, 257, PixelFormat::Rgba8, 2),
        ),
        (
            "image.canvas",
            json!({ "rect": { "x": 200, "y": 10, "w": 300, "h": 270 }, "fill": null }),
            noise(513, 257, PixelFormat::Indexed8, 3),
        ),
        (
            "image.resize",
            json!({ "width": { "px": 197 }, "height": { "px": 271 }, "filter": "lanczos3" }),
            noise(300, 260, PixelFormat::Rgba8, 4),
        ),
        (
            "image.resize",
            json!({ "width": { "px": 611 }, "height": "auto", "filter": "nearest" }),
            noise(300, 120, PixelFormat::Rgb8, 5),
        ),
        (
            "image.resize",
            json!({ "width": { "px": 170 }, "height": { "scale": { "num": 1, "den": 1 } }, "filter": "bilinear" }),
            noise(300, 257, PixelFormat::Gray16, 6),
        ),
        (
            "image.resize",
            json!({ "width": { "px": 300 }, "height": { "px": 333 }, "filter": "box" }),
            noise(300, 200, PixelFormat::Rgba16, 7),
        ),
        (
            "image.resize",
            json!({ "width": { "px": 7 }, "height": { "px": 5 }, "filter": "lanczos3" }),
            noise(1, 1, PixelFormat::Rgba8, 8),
        ),
        (
            "image.resize",
            json!({ "width": { "px": 1 }, "height": { "px": 1 }, "filter": "bilinear" }),
            noise(270, 260, PixelFormat::Rgb8, 9),
        ),
        (
            "image.resize",
            json!({ "width": { "scale": { "num": 2, "den": 3 } }, "height": "auto", "filter": "nearest" }),
            noise(300, 260, PixelFormat::Indexed8, 10),
        ),
        (
            "image.crop",
            json!({ "rect": { "x": 255, "y": 255, "w": 2, "h": 2 } }),
            noise(513, 513, PixelFormat::Rgba8, 11),
        ),
        (
            "image.rotate90",
            json!({ "turn": "cw" }),
            noise(1, 1, PixelFormat::GrayA8, 12),
        ),
        (
            "image.rotate90",
            json!({ "turn": "ccw" }),
            noise(513, 257, PixelFormat::Rgb8, 13),
        ),
        (
            "image.flip",
            json!({ "axis": "horizontal" }),
            noise(257, 1, PixelFormat::Indexed8, 14),
        ),
        (
            "image.convert",
            json!({ "format": "rgba16", "colors": 256, "dither": "none", "matte": [1, 2, 3] }),
            noise(513, 300, PixelFormat::Indexed8, 15),
        ),
        (
            "image.convert",
            json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [9, 9, 9] }),
            InputGen::TrimmedFrames {
                w: 40,
                h: 30,
                fmt: PixelFormat::Rgba16,
                count: 3,
                seed: 16,
            },
        ),
        (
            "audio.resample",
            json!({ "rate": 48_000 }),
            InputGen::AudioNoise {
                rate: 44_100,
                channels: 2,
                frames: 5_000,
                seed: 17,
            },
        ),
        (
            "audio.trim",
            json!({ "start": 123, "end": 4_000 }),
            InputGen::AudioNoise {
                rate: 8_000,
                channels: 1,
                frames: 5_000,
                seed: 18,
            },
        ),
    ];
    for (id, params, make) in extra {
        let v = engine.registry().current_version(id).unwrap();
        let op = engine
            .registry()
            .all()
            .into_iter()
            .find(|o| o.id() == id && o.version() == v)
            .unwrap();
        out.push((op, format!("境界 {id}"), params, make));
    }
    out
}

/// 範囲の描画を直接呼ぶ（持ち上げる操作のフレームの列はフレームごと）。返すのは値の画素（フレームの順）。
fn region_pixels(
    op: &dyn DynOp,
    params: &serde_json::Value,
    input: &Value,
    tiles: &FrameTiles,
    base: Option<&Value>,
) -> Result<Vec<Raster>, EngineError> {
    let p = op.parse(params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
    let lifted = op.signature().lift == Lift::PerFrame && matches!(input, Value::Frames(_));
    if !lifted {
        let applied = op
            .apply_region(p.as_ref(), &[input], tiles, base, &mut cx)
            .expect("REGION の操作は Some を返す")?;
        return Ok(frames_of(&applied.value)
            .into_iter()
            .map(|(_, px, _)| px.clone())
            .collect());
    }
    let (Value::Frames(fr), base_fr) = (input, base) else {
        unreachable!()
    };
    let mut out = Vec::new();
    for (i, f) in fr.frames().iter().enumerate() {
        let single = Value::Image(Arc::new(Image {
            px: f.px.clone(),
            meta: fr.meta.clone(),
        }));
        let b = match base_fr {
            Some(Value::Frames(b)) => Some(Value::Image(Arc::new(Image {
                px: b.frames()[i].px.clone(),
                meta: fr.meta.clone(),
            }))),
            _ => None,
        };
        let t: FrameTiles = tiles
            .get(&(i as u32))
            .map(|t| BTreeMap::from([(0, t.clone())]))
            .unwrap_or_default();
        let applied = op
            .apply_region(p.as_ref(), &[&single], &t, b.as_ref(), &mut cx)
            .expect("REGION の操作は Some を返す")?;
        let Value::Image(img) = applied.value else {
            panic!("持ち上げた範囲の描画が画像を返さない")
        };
        out.push(img.px.clone());
    }
    Ok(out)
}

/// 全体を描いた画素（`apply` を直接。持ち上げる操作のフレームの列はフレームごと）。
fn apply_pixels(op: &dyn DynOp, params: &serde_json::Value, input: &Value) -> Vec<Raster> {
    let p = op.parse(params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
    match input {
        Value::Frames(fr) if op.signature().lift == Lift::PerFrame => fr
            .frames()
            .iter()
            .map(|f| {
                let single = Value::Image(Arc::new(Image {
                    px: f.px.clone(),
                    meta: fr.meta.clone(),
                }));
                let out = op.apply(p.as_ref(), &[&single], &mut cx).unwrap().value;
                frames_of(&out)[0].1.clone()
            })
            .collect(),
        _ => {
            let out = op.apply(p.as_ref(), &[input], &mut cx).unwrap().value;
            frames_of(&out)
                .into_iter()
                .map(|(_, px, _)| px.clone())
                .collect()
        }
    }
}

/// フレームごとの画素を値にする（前の値に使う）。持ち上げる操作のフレームの列はフレームの列に、ほかは `apply` の値のまま。
fn base_of(op: &dyn DynOp, params: &serde_json::Value, input: &Value) -> Value {
    let p = op.parse(params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
    match input {
        Value::Frames(fr) if op.signature().lift == Lift::PerFrame => {
            let px = apply_pixels(op, params, input);
            Value::Frames(Arc::new(
                kura_engine::value::Frames::full(
                    px.into_iter().map(|p| (p, 100)).collect(),
                    fr.meta.clone(),
                    0,
                )
                .unwrap(),
            ))
        }
        _ => op.apply(p.as_ref(), &[input], &mut cx).unwrap().value,
    }
}

/// タイルの集合を、キャンバスの座標の範囲にする（範囲の描画が描くタイルの矩形）。
fn tiles_area(v: &Value, tiles: &FrameTiles) -> Area {
    let mut map = BTreeMap::new();
    for (f, _, o) in frames_of(v) {
        if let Some(t) = tiles.get(&f) {
            map.insert(f, t.to_rects(o));
        }
    }
    Area::pixels(map)
}

fn holes_read(v: &Value) -> bool {
    frames_of(v).iter().any(|(_, px, _)| px.holes_read())
}

/// E18 を画像に広げたもの：依存の外を壊しても範囲の中は同じ。範囲の描画は依存の外の無効なタイルを読まない。
#[test]
fn reach_is_sound() {
    let engine = engine();
    let mut r = determinism::rng(16);
    let (mut corrupted, mut holed, mut global) = (0, 0, 0);
    for (op, name, params, make) in cases(&engine) {
        let input = make.generate().unwrap();
        let source = source_of(std::slice::from_ref(&make)).unwrap();
        let g = single_node_graph(op.id(), op.version(), params.clone()).unwrap();
        let full = render_with(&engine, &g, input.clone(), source, &NoMonitor)
            .unwrap_or_else(|e| panic!("{} {name}: {e}", op.id()));
        let p = op.parse(&params).unwrap();
        let reach = op
            .reach(p.as_ref(), &op_shapes(op.as_ref(), &input))
            .unwrap();
        if reach == Reach::Global {
            global += 1;
            continue;
        }
        // 大きな値は描き直すのに時間がかかるので回数を減らす（debug のビルドで数秒に収める）
        let big = full.shape().value_bytes() > 200_000;
        for _ in 0..if big { 2 } else { 4 } {
            let out_area = random_area(&mut r, &full);
            // 1. 依存の外を壊して描いても、範囲の中は同じ
            let need = reach.backward(&out_area.expand(&full.shape()));
            let broken = corrupt_outside(&input, &need, r.next_u64()).unwrap();
            let again = render_with(&engine, &g, broken, source, &NoMonitor)
                .unwrap_or_else(|e| panic!("{} {name}: 壊した入力で描けない: {e}", op.id()));
            check_same_inside(&full, &again, &out_area)
                .unwrap_or_else(|m| panic!("{} {name}（{out_area:?}）: {m}", op.id()));
            corrupted += 1;
            // 2. 範囲の描画は、描くタイルの逆写しの外（無効なタイル）を読まない
            if op.region() {
                let tiles = tiles_in(&full, &out_area, TILE);
                let need_t = reach.backward(&tiles_area(&full, &tiles).expand(&full.shape()));
                let partial = only_tiles(&input, &need_t).unwrap();
                let got = region_pixels(op.as_ref(), &params, &partial, &tiles, None)
                    .unwrap_or_else(|e| panic!("{} {name}: 範囲の描画が失敗: {e}", op.id()));
                assert!(
                    !holes_read(&partial),
                    "{} {name}: 依存の外（無効なタイル）を読んだ（{out_area:?}）",
                    op.id()
                );
                for ((f, want, _), px) in frames_of(&full).into_iter().zip(&got) {
                    for (tx, ty) in tiles
                        .get(&f)
                        .map(|t| t.iter().collect::<Vec<_>>())
                        .unwrap_or_default()
                    {
                        assert!(
                            !px.is_hole(tx, ty),
                            "{} {name}: 選んだタイルが無効",
                            op.id()
                        );
                        assert_eq!(
                            px.tile(tx, ty),
                            want.tile(tx, ty),
                            "{} {name}: フレーム {f} のタイル ({tx}, {ty}) が全体と違う",
                            op.id()
                        );
                    }
                }
                holed += 1;
            }
        }
    }
    // 空回りしていないこと
    assert!(
        corrupted > 250 && holed > 100 && global >= 5,
        "{corrupted} {holed} {global}"
    );
}

/// 範囲の描画で選んだタイルは全体を描いたものと同じ。選ばないタイルは前の値と共有し、前の値がなければ無効。
/// 1・3・16 スレッドで同じ。
#[test]
fn region_equals_full_crop() {
    let engine = engine();
    let pools: Vec<(usize, rayon::ThreadPool)> = [1, 3, 16]
        .into_iter()
        .map(|n| {
            (
                n,
                rayon::ThreadPoolBuilder::new()
                    .num_threads(n)
                    .build()
                    .unwrap(),
            )
        })
        .collect();
    let mut checked = 0;
    for (op, name, params, make) in cases(&engine) {
        if !op.region() {
            continue;
        }
        let input = make.generate().unwrap();
        let p = op.parse(&params).unwrap();
        if op
            .reach(p.as_ref(), &op_shapes(op.as_ref(), &input))
            .unwrap()
            == Reach::Global
        {
            // Global の引数では呼ばれない（convert の Indexed8 は None）
            continue;
        }
        let full = apply_pixels(op.as_ref(), &params, &input);
        // 前の値：入力をすべて壊して描いたもの（同じ形・形式・パレット）
        let other = corrupt_outside(&input, &Area::Empty, 99).unwrap();
        let base_value = Some(base_of(op.as_ref(), &params, &other));
        let shapes: Vec<(u32, u32)> = full.iter().map(|px| (px.width(), px.height())).collect();
        let sets: Vec<(&str, FrameTiles)> = {
            let mk = |f: &dyn Fn(u32, u32, &mut Tiles)| -> FrameTiles {
                shapes
                    .iter()
                    .enumerate()
                    .map(|(i, (w, h))| {
                        let mut t = Tiles::none(*w, *h, TILE);
                        f(*w, *h, &mut t);
                        (i as u32, t)
                    })
                    .collect()
            };
            vec![
                ("空", mk(&|_, _, _| {})),
                (
                    "1 枚",
                    mk(&|w, h, t| t.insert((w - 1) / TILE, (h - 1) / TILE)),
                ),
                (
                    "市松",
                    mk(&|_, _, t| {
                        for ty in 0..t.tiles_y() {
                            for tx in 0..t.tiles_x() {
                                if (tx + ty) % 2 == 0 {
                                    t.insert(tx, ty);
                                }
                            }
                        }
                    }),
                ),
                ("全体", mk(&|w, h, t| *t = Tiles::all(w, h, TILE))),
            ]
        };
        for (threads, pool) in &pools {
            pool.install(|| {
                // 1 スレッドは 4 通りすべて、3・16 スレッドは市松だけ（debug のビルドで数秒に収める）
                for (what, tiles) in sets.iter().filter(|(w, _)| *threads == 1 || *w == "市松") {
                    let got = region_pixels(op.as_ref(), &params, &input, tiles, None).unwrap();
                    let with_base = base_value.as_ref().map(|b| {
                        region_pixels(op.as_ref(), &params, &input, tiles, Some(b)).unwrap()
                    });
                    let base_px: Option<Vec<Raster>> = base_value.as_ref().map(|b| {
                        frames_of(b)
                            .into_iter()
                            .map(|(_, px, _)| px.clone())
                            .collect()
                    });
                    for (i, want) in full.iter().enumerate() {
                        let t = &tiles[&(i as u32)];
                        for ty in 0..want.tiles_y() {
                            for tx in 0..want.tiles_x() {
                                let picked = t.contains(tx, ty);
                                let at = format!(
                                    "{} {name} {what} {threads} スレッド ({tx}, {ty})",
                                    op.id()
                                );
                                if picked {
                                    assert_eq!(got[i].tile(tx, ty), want.tile(tx, ty), "{at}");
                                } else {
                                    assert!(
                                        got[i].is_hole(tx, ty),
                                        "{at}: 選ばないタイルが無効でない"
                                    );
                                }
                                if let (Some(wb), Some(bp)) = (&with_base, &base_px) {
                                    if picked {
                                        assert_eq!(wb[i].tile(tx, ty), want.tile(tx, ty), "{at}");
                                    } else {
                                        assert!(
                                            wb[i].shares_tile(&bp[i], tx, ty),
                                            "{at}: 前の値と共有しない"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    checked += 1;
                }
            });
        }
    }
    assert!(checked > 100, "{checked}");
}

/// 入力 1 つの描画の補助。
fn inputs_of(
    value: &Value,
) -> (
    BTreeMap<InputId, FingerprintedInput>,
    BTreeMap<InputId, InputValue>,
) {
    (
        BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(value.clone(), Digest::default()).unwrap(),
        )]),
        BTreeMap::from([(
            InputId(0),
            InputValue {
                value: value.clone(),
                source: Digest::default(),
            },
        )]),
    )
}

fn compile(engine: &Engine, g: &Graph, input: &Value) -> Option<Plan> {
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .ok()?;
    (plan.cost() <= 1 << 22).then_some(plan)
}

/// E18・E1：ランダムな鎖（2〜5 ノード）のランダムな範囲で、範囲の描画の有効なタイルが全体の描画と同じ。
/// 範囲の描画が依存の外を読んで全体に落ちることもない。全体に依る操作を挟んでも同じ。
#[test]
fn render_area_matches_full() {
    let engine = engine();
    let mut r = determinism::rng(18);
    let (mut checked, mut regional, mut full_nodes) = (0, 0, 0);
    for case in 0..60 {
        let make = random_input(&mut r);
        let input = make.generate().unwrap();
        let len = 2 + below(&mut r, 4) as usize;
        let g = random_chain(&engine, &mut r, &input.shape(), len, case % 3 == 0);
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let (fin, plain) = inputs_of(&input);
        let Ok(full) = engine.render(&plan, &plain, &NoMonitor) else {
            continue;
        };
        let out = &full.outputs[0].value;
        let mut cache = NodeCache::new([0, 1 << 20, 1 << 30][case % 3]);
        for _ in 0..3 {
            let area = random_area(&mut r, out);
            let req = AreaRequest {
                output: OutputId(0),
                area: area.clone(),
            };
            let got = engine
                .render_area(&plan, &fin, &mut cache, &req, None, &NoMonitor)
                .unwrap_or_else(|e| {
                    panic!(
                        "{case}: {e}\n{}",
                        String::from_utf8_lossy(&g.to_canonical_json().unwrap())
                    )
                });
            assert_eq!(got.stats.fallbacks, 0, "{case}: 依存の外を読んだ");
            check_region(&got.value, out, &area).unwrap_or_else(|m| {
                panic!(
                    "{case}: {m}\n{area:?}\n{}",
                    String::from_utf8_lossy(&g.to_canonical_json().unwrap())
                )
            });
            assert!(got.value.covers(&area), "{case}");
            assert!(got.value.exact().is_none() || got.value.is_complete());
            regional += got.stats.region_nodes;
            full_nodes += got.stats.full_nodes;
            checked += 1;
        }
    }
    assert!(
        checked > 100 && regional > 50 && full_nodes > 10,
        "{checked} {regional} {full_nodes}"
    );
}

/// 遠い座標の要求（段 10 の検証）：i64 の端に近い・負の大きな座標の `AreaRequest`・`ViewRequest`・`SpoitAt` で、
/// 座標の足し算があふれない（debug のビルドで止まらない）。画像の中は全体の描画と同じで、外は拾わない。遠い矩形が混ざって
/// 外接矩形に畳まれても（矩形が 64 を超える）、画像の中で頼んだタイルは描く。反転（Affine）と convert（Same）の鎖で。
#[test]
fn far_requests_are_clipped_to_the_canvas() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 300,
        h: 200,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let docs = [
        single_node_graph("image.flip", 1, json!({ "axis": "vertical" })).unwrap(),
        single_node_graph(
            "image.convert",
            1,
            json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
        )
        .unwrap(),
    ];
    let far = [
        IRect::new(i64::MAX - 100, 0, 50, 10),
        IRect::new(0, i64::MAX - 1, 10, 10),
        IRect::new(i64::MAX - 5, i64::MAX - 5, u32::MAX, u32::MAX),
        IRect::new(i64::MIN, i64::MIN, u32::MAX, u32::MAX),
        IRect::new(-(1 << 62), 5, u32::MAX, 3),
        IRect::new(290, 190, u32::MAX, u32::MAX),
        IRect::new(10, 10, 1, 1),
    ];
    for g in docs {
        let plan = compile(&engine, &g, &input).unwrap();
        let (fin, plain) = inputs_of(&input);
        let full = engine
            .render(&plan, &plain, &NoMonitor)
            .unwrap()
            .outputs
            .swap_remove(0)
            .value;
        let op = g.nodes[0].op.clone();
        let mut cache = NodeCache::new(0);
        for r in far {
            let area = Area::rect(0, r);
            let got = engine
                .render_area(
                    &plan,
                    &fin,
                    &mut cache,
                    &AreaRequest {
                        output: OutputId(0),
                        area: area.clone(),
                    },
                    None,
                    &NoMonitor,
                )
                .unwrap();
            check_region(&got.value, &full, &area).unwrap_or_else(|m| panic!("{op} {r:?}: {m}"));
            let view = engine
                .render_view(
                    &plan,
                    &fin,
                    &mut cache,
                    &ViewRequest {
                        output: OutputId(0),
                        area: area.clone(),
                        focus: None,
                    },
                    None,
                    &NoMonitor,
                )
                .unwrap();
            check_region(view.view.value(), &full, &area)
                .unwrap_or_else(|m| panic!("{op} {r:?}: {m}"));
            for (x, y) in [(r.x, r.y), (r.right(), r.bottom())] {
                let at = SpoitAt {
                    output: OutputId(0),
                    frame: 0,
                    x,
                    y,
                };
                let got = engine
                    .spoit(&plan, &fin, &mut cache, &at, &NoMonitor)
                    .unwrap()
                    .map(|s| s.raw);
                let want = kura_engine::paint::sample(&full, 0, x, y).map(|s| s.raw);
                assert_eq!(got, want, "{op} ({x}, {y})");
            }
        }
        // 遠い矩形 1 つと、画像の中の 64 個（畳むと幅が u32 に収まらない）
        let mut rects = Rects::new();
        rects.push(IRect::new(-(1 << 40), 0, 1, 1));
        for i in 0..64 {
            rects.push(IRect::new(i * 4, 0, 2, 2));
        }
        let area = Area::frame(0, rects);
        let got = engine
            .render_area(
                &plan,
                &fin,
                &mut cache,
                &AreaRequest {
                    output: OutputId(0),
                    area: area.clone(),
                },
                None,
                &NoMonitor,
            )
            .unwrap();
        let inside = Area::rect(0, IRect::new(0, 0, 256, 2));
        assert!(got.value.covers(&inside), "{op}: {:?}", got.stats);
        check_region(&got.value, &full, &inside).unwrap_or_else(|m| panic!("{op}: {m}"));
    }
}

/// 量子化した音声の出力（段 10 の検証）：範囲の描画・表示の経路も `render`・`render_cached` と同じく出力の量子化を当てる
/// （由来 Exact の値を量子化前のまま返さない）。ノードあり・なし（入力をそのまま量子化）、TPDF・ディザなし、キャッシュの
/// 上限 0・1 GiB（2 回目は量子化の指紋で当たる）。
#[test]
fn quantized_outputs_match_render() {
    let engine = engine();
    let input = InputGen::Sine {
        rate: 44_100,
        channels: 2,
        frames: 4_000,
        freq: 440,
        loop_range: None,
    }
    .generate()
    .unwrap();
    let source = Digest([7; 32]);
    let fin = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), source).unwrap(),
    )]);
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source,
        },
    )]);
    for (with_node, dither, bits) in [
        (true, Dither::Tpdf, 8),
        (false, Dither::Tpdf, 16),
        (true, Dither::None, 24),
    ] {
        let mut g = Graph::new_single();
        if with_node {
            g.push_linear("audio.gain", 1, json!({ "db": -3.0 }), 1)
                .unwrap();
        }
        g.outputs[0].quantize = Some(Quantize {
            v: QUANTIZE_VERSION,
            bits,
            dither,
            seed: 3,
        });
        let what = format!("ノード {with_node}・{dither:?}・{bits} ビット");
        let plan = engine
            .compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))
            .unwrap();
        let want = engine.render(&plan, &plain, &NoMonitor).unwrap().outputs[0].digest;
        for cap in [0, 1 << 30] {
            let mut cache = NodeCache::new(cap);
            for round in 0..2 {
                let area = engine
                    .render_area(
                        &plan,
                        &fin,
                        &mut cache,
                        &AreaRequest {
                            output: OutputId(0),
                            area: Area::All,
                        },
                        None,
                        &NoMonitor,
                    )
                    .unwrap();
                let view = engine
                    .render_view(
                        &plan,
                        &fin,
                        &mut cache,
                        &ViewRequest::whole(OutputId(0), None),
                        None,
                        &NoMonitor,
                    )
                    .unwrap();
                for (path, v) in [
                    ("render_area", &area.value),
                    ("render_view", view.view.value()),
                ] {
                    let got = v.exact().map(|v| v.digest().unwrap());
                    assert_eq!(got, Some(want), "{what}・上限 {cap}・{round} 回目：{path}");
                }
            }
        }
    }
}

/// E18 の続き：全体に依る操作（trim・Indexed8 への convert・塗りつぶし）を挟むと、その上流は全体で描く（逆写しが
/// 全体に落ちる）。下流の局所の操作は範囲だけを描く。
#[test]
fn global_ops_pull_the_whole_input() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 300,
        h: 270,
        fmt: PixelFormat::Rgba8,
        seed: 3,
    }
    .generate()
    .unwrap();
    let globals = [
        (
            "image.trim_alpha",
            json!({ "threshold": 3, "mode": "union" }),
        ),
        (
            "image.convert",
            json!({ "format": "indexed8", "colors": 16, "dither": "none", "matte": [0, 0, 0] }),
        ),
        (
            "paint.actions",
            json!({ "actions": [{ "type": "fill", "at": { "x": 3, "y": 3 }, "color": { "rgba": [1, 2, 3, 255] }, "tolerance": 0, "connectivity": "four", "contiguous": false, "frame": 0 }] }),
        ),
    ];
    for (op, params) in globals {
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
            .unwrap();
        g.push_linear(op, 1, params, 0).unwrap();
        g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
            .unwrap();
        let plan = compile(&engine, &g, &input).unwrap();
        let (fin, plain) = inputs_of(&input);
        let full = engine.render(&plan, &plain, &NoMonitor).unwrap();
        let area = Area::rect(0, IRect::new(10, 10, 3, 3));
        let got = engine
            .render_area(
                &plan,
                &fin,
                &mut NodeCache::new(0),
                &AreaRequest {
                    output: OutputId(0),
                    area: area.clone(),
                },
                None,
                &NoMonitor,
            )
            .unwrap();
        check_region(&got.value, &full.outputs[0].value, &area).unwrap();
        // 全体に依る操作とその上流の flip は全体。最後の flip は範囲だけ（trim は出力の大きさが中身で決まるので、
        // 下流も全体。後からの絞り込みは後の段）
        let (region, whole) = if op == "image.trim_alpha" {
            (0, 3)
        } else {
            (1, 2)
        };
        assert_eq!(
            (got.stats.region_nodes, got.stats.full_nodes),
            (region, whole),
            "{op}"
        );
        assert_eq!(got.stats.drawn_tiles, region, "{op}");
    }
}

/// E23：ランダムな点で 1×1 の範囲を描いて拾った色・番号は、全体を描いて paint::sample で拾ったものと同じ。
#[test]
fn spoit_via_region() {
    let engine = engine();
    let mut r = determinism::rng(23);
    let mut checked = 0;
    for case in 0..40 {
        let make = random_input(&mut r);
        let input = make.generate().unwrap();
        if matches!(input, Value::Audio(_)) {
            continue;
        }
        let len = 1 + below(&mut r, 4) as usize;
        let g = random_chain(&engine, &mut r, &input.shape(), len, case % 4 == 0);
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let (fin, plain) = inputs_of(&input);
        let Ok(full) = engine.render(&plan, &plain, &NoMonitor) else {
            continue;
        };
        let out = &full.outputs[0].value;
        let mut cache = NodeCache::new(1 << 24);
        let frames = frames_of(out);
        for _ in 0..6 {
            let frame = below(&mut r, frames.len().max(1) as u64 + 1) as u32;
            let (w, h) = match out {
                Value::Image(img) => (img.px.width(), img.px.height()),
                Value::Frames(fr) => (fr.canvas().w, fr.canvas().h),
                Value::Audio(_) => (1, 1),
            };
            let x = below(&mut r, u64::from(w) + 4) as i64 - 2;
            let y = below(&mut r, u64::from(h) + 4) as i64 - 2;
            let at = SpoitAt {
                output: OutputId(0),
                frame,
                x,
                y,
            };
            let got = engine
                .spoit(&plan, &fin, &mut cache, &at, &NoMonitor)
                .unwrap();
            assert_eq!(
                got,
                kura_engine::paint::sample(out, frame, x, y),
                "{case} {at:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}

/// E24：範囲の描画・継ぎはぎの途中で中止すると Canceled になり、キャッシュに部分の値も途中の値も残らない
/// （そのあと当たりを照らす描画がキャッシュなしと同じ）。
#[test]
fn cancel_in_region_and_patch() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 400,
        h: 300,
        fmt: PixelFormat::Rgba8,
        seed: 24,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "px": 501 }, "height": "auto", "filter": "bilinear" }),
        0,
    )
    .unwrap();
    g.push_linear("image.rotate90", 1, json!({ "turn": "cw" }), 0)
        .unwrap();
    let plan = compile(&engine, &g, &input).unwrap();
    let (fin, plain) = inputs_of(&input);
    let full = engine.render(&plan, &plain, &NoMonitor).unwrap();
    let out = full.outputs[0].value.clone();
    let prev = PartialValue::full(corrupt_outside(&out, &Area::Empty, 5).unwrap());
    let changed = Area::rect(0, IRect::new(100, 100, 300, 20));
    let mut canceled = 0;
    for k in 1..24 {
        for patch in [false, true] {
            let mut cache = NodeCache::new(1 << 30);
            let mon = CancelAfter::new(k);
            let base = Base {
                value: &prev,
                changed: &changed,
            };
            let req = AreaRequest {
                output: OutputId(0),
                area: Area::All,
            };
            let r = engine.render_area(&plan, &fin, &mut cache, &req, patch.then_some(&base), &mon);
            match r {
                Err(EngineError::Canceled) => canceled += 1,
                Ok(v) => {
                    if !patch {
                        check_region(&v.value, &out, &Area::All).unwrap();
                    }
                }
                Err(e) => panic!("{k}: {e}"),
            }
            // 残った項目は、どれも等倍で全体を描いたもの（照らしても違わない）
            let verify = RenderOptions {
                digests: true,
                verify_hits: true,
                ..RenderOptions::default()
            };
            let again = engine
                .render_cached(&plan, &fin, &mut cache, &verify, &NoMonitor)
                .unwrap();
            assert_eq!(again.outputs[0].digest, Some(full.outputs[0].digest), "{k}");
        }
    }
    assert!(canceled > 8, "{canceled}");
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShiftParams {
    dx: u32,
}

impl OpParams for ShiftParams {
    fn schema() -> ParamSchema {
        ParamSchema { params: Vec::new() }
    }
}

/// 出力 (x, y) = 入力 ((x + dx) mod 幅, y)。
fn shift_tile(
    px: &Raster,
    dx: u32,
) -> impl Fn(IRect, &mut [u8]) -> Result<(), EngineError> + Sync + Send + '_ {
    move |t, out| {
        let (w, bpp) = (px.width(), px.bpp());
        for (row, dst) in out.chunks_exact_mut(t.w as usize * bpp).enumerate() {
            for (c, d) in dst.chunks_exact_mut(bpp).enumerate() {
                let sx = (t.x as u32 + c as u32 + dx) % w;
                let mut b = vec![0u8; bpp];
                px.read_rect(IRect::new(i64::from(sx), t.y + row as i64, 1, 1), &mut b)?;
                d.copy_from_slice(&b);
            }
        }
        Ok(())
    }
}

fn shift_infer(inputs: &[Shape]) -> Result<Inferred, Issues> {
    Ok(Inferred {
        shape: inputs[0],
        space: SpaceMap::Identity,
        time: TimeMap::Identity,
        cost: 1,
    })
}

fn shift_sig() -> Signature {
    Signature {
        inputs: &[PortSpec {
            name: "image",
            accepts: &[ValueKind::Image],
        }],
        lift: Lift::None,
        numeric: Numeric::Exact,
        meta: MetaAccess::Map,
    }
}

fn shift_apply(
    p: &ShiftParams,
    inputs: &[&Value],
    cx: &ApplyCx<'_>,
) -> Result<Applied, EngineError> {
    let Value::Image(img) = inputs[0] else {
        return Err(EngineError::Internal("画像ではありません".into()));
    };
    let px = img.px.derive(
        img.px.width(),
        img.px.height(),
        cx.mon,
        shift_tile(&img.px, p.dx),
    )?;
    Ok(Applied::value(Value::Image(Arc::new(Image {
        px,
        meta: img.meta.clone(),
    }))))
}

/// 試験用：依存をわざと小さく（同じ位置と）申告し、範囲の描画を持たない操作（実際は横にずらす）。
struct UnderShift;

impl Op for UnderShift {
    const ID: &'static str = "test.under_shift";
    const VERSION: u16 = 1;
    type Params = ShiftParams;
    fn signature() -> Signature {
        shift_sig()
    }
    fn infer(_: &ShiftParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        shift_infer(inputs)
    }
    fn apply(
        p: &ShiftParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        shift_apply(p, inputs, cx)
    }
    fn examples() -> Vec<Example<ShiftParams>> {
        Vec::new()
    }
    fn reach(_: &ShiftParams, _: &[Shape]) -> Reach {
        Reach::Same
    }
}

/// 試験用：依存をわざと小さく申告し、範囲の描画は正直に（ずらした位置を）読む操作。
struct UnderShiftRegion;

impl Op for UnderShiftRegion {
    const ID: &'static str = "test.under_shift_region";
    const VERSION: u16 = 1;
    type Params = ShiftParams;
    const REGION: bool = true;
    fn signature() -> Signature {
        shift_sig()
    }
    fn infer(_: &ShiftParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        shift_infer(inputs)
    }
    fn apply(
        p: &ShiftParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        shift_apply(p, inputs, cx)
    }
    fn examples() -> Vec<Example<ShiftParams>> {
        Vec::new()
    }
    fn reach(_: &ShiftParams, _: &[Shape]) -> Reach {
        Reach::Same
    }
    fn apply_region(
        p: &ShiftParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        let Value::Image(img) = inputs[0] else {
            return Some(Err(EngineError::Internal("画像ではありません".into())));
        };
        let (w, h) = (img.px.width(), img.px.height());
        let pick = tiles
            .get(&0)
            .cloned()
            .unwrap_or_else(|| Tiles::none(w, h, TILE));
        let base = match base {
            Some(Value::Image(b)) => Some(&b.px),
            _ => None,
        };
        Some(
            img.px
                .derive_picked(w, h, cx.mon, &pick, base, shift_tile(&img.px, p.dx))
                .map(|px| {
                    Applied::value(Value::Image(Arc::new(Image {
                        px,
                        meta: img.meta.clone(),
                    })))
                }),
        )
    }
}

/// 由来 Exact だけを使う約束と無効なタイルの見張り（docs/editing.md §12.3・§12.5）：依存をわざと小さく申告する操作が
/// あっても、範囲の描画は無効なタイルを読んだことを見つけて全体で描き直す。継ぎはぎの値（前の値を使う）は古いタイルを
/// 残しうるが、キャッシュ・スポイト・解析には漏れず、`render_cached`・スポイト・解析は `render` と同じ。
#[test]
fn understated_reach_never_leaks() {
    let mut registry: Registry = kura_engine::ops::registry().unwrap();
    registry.add_current::<UnderShift>().unwrap();
    registry.add_current::<UnderShiftRegion>().unwrap();
    let engine = Engine::with_registry(registry, Limits::default());
    let input = InputGen::Noise {
        w: 600,
        h: 300,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let (fin, plain) = inputs_of(&input);
    let stroke = |x: i64| {
        json!({ "type": "stroke", "tool": "pen", "color": { "rgba": [255, 0, 0, 255] }, "blend": "replace",
                "brush": { "shape": "square", "size": 5 }, "pixelPerfect": false, "frame": 0,
                "points": kura_engine::paint::PointList(vec![kura_engine::geom::IPoint { x, y: 20 }]).pack() })
    };
    // A：範囲の描画を持たない操作（申告が小さすぎる）の下流で継ぎはぎすると、前の値の古いタイルが残る
    let doc = |n: usize| {
        let mut g = Graph::new_single();
        let actions: Vec<_> = (0..n).map(|i| stroke(20 + 40 * i as i64)).collect();
        g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
            .unwrap();
        g.push_linear("test.under_shift", 1, json!({ "dx": 300 }), 0)
            .unwrap();
        g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
            .unwrap();
        g
    };
    let (g1, g2) = (doc(1), doc(2));
    let (p1, p2) = (
        compile(&engine, &g1, &input).unwrap(),
        compile(&engine, &g2, &input).unwrap(),
    );
    let fresh = engine.render(&p2, &plain, &NoMonitor).unwrap();
    let want = &fresh.outputs[0].value;
    let mut cache = NodeCache::new(1 << 30);
    let first = engine
        .render_area(
            &p1,
            &fin,
            &mut cache,
            &AreaRequest {
                output: OutputId(0),
                area: Area::All,
            },
            None,
            &NoMonitor,
        )
        .unwrap();
    let delta = engine.delta(&p1, &fin, &p2, &fin).unwrap();
    let base = Base {
        value: &first.value,
        changed: &delta.outputs[0].1,
    };
    let patched = engine
        .render_area(
            &p2,
            &fin,
            &mut cache,
            &AreaRequest {
                output: OutputId(0),
                area: Area::All,
            },
            Some(&base),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(
        patched.value.origin(),
        kura_engine::render::cache::Origin::Patched
    );
    assert!(
        patched.value.exact().is_none(),
        "継ぎはぎの値を等倍の値として渡さない"
    );
    assert!(
        check_region(&patched.value, want, &Area::All).is_err(),
        "申告が小さすぎる操作の継ぎはぎは古いタイルを残す（試験が空回りしていない）"
    );
    // それでも、キャッシュ・スポイト・解析は正しい
    let verify = RenderOptions {
        digests: true,
        verify_hits: true,
        ..RenderOptions::default()
    };
    let cached = engine
        .render_cached(&p2, &fin, &mut cache, &verify, &NoMonitor)
        .unwrap();
    assert_eq!(cached.outputs[0].digest, Some(fresh.outputs[0].digest));
    let mut r = determinism::rng(1);
    for _ in 0..20 {
        let (x, y) = (below(&mut r, 600) as i64, below(&mut r, 300) as i64);
        let at = SpoitAt {
            output: OutputId(0),
            frame: 0,
            x,
            y,
        };
        assert_eq!(
            engine
                .spoit(&p2, &fin, &mut cache, &at, &NoMonitor)
                .unwrap(),
            kura_engine::paint::sample(want, 0, x, y)
        );
    }
    let req = kura_engine::AnalysisRequest {
        id: "image.colors".into(),
        v: None,
        params: json!({}),
    };
    assert_eq!(
        engine
            .analyze_dyn(&req, &cached.outputs[0].value, &NoMonitor)
            .unwrap(),
        engine.analyze_dyn(&req, want, &NoMonitor).unwrap()
    );
    // B：範囲の描画を持つ操作は、依存の外（無効なタイル）を読んだことを見つけて全体で描き直す（結果は正しい）
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    g.push_linear("test.under_shift_region", 1, json!({ "dx": 300 }), 0)
        .unwrap();
    let plan = compile(&engine, &g, &input).unwrap();
    let want = engine.render(&plan, &plain, &NoMonitor).unwrap().outputs[0]
        .value
        .clone();
    let area = Area::rect(0, IRect::new(5, 5, 2, 2));
    let got = engine
        .render_area(
            &plan,
            &fin,
            &mut NodeCache::new(0),
            &AreaRequest {
                output: OutputId(0),
                area: area.clone(),
            },
            None,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(got.stats.fallbacks, 1);
    check_region(&got.value, &want, &area).unwrap();
    let at = SpoitAt {
        output: OutputId(0),
        frame: 0,
        x: 5,
        y: 5,
    };
    assert_eq!(
        engine
            .spoit(&plan, &fin, &mut NodeCache::new(0), &at, &NoMonitor)
            .unwrap(),
        kura_engine::paint::sample(&want, 0, 5, 5)
    );
}

/// 範囲の描画を持つと申告した操作（REGION）は、Global でない引数の例で、必ず apply_region が Some を返す。持たない操作は None。
#[test]
fn region_declarations_match_the_implementation() {
    let engine = engine();
    let mut with = std::collections::BTreeSet::new();
    for (op, name, params, make) in cases(&engine) {
        let input = make.generate().unwrap();
        let p = op.parse(&params).unwrap();
        let reach = op
            .reach(p.as_ref(), &op_shapes(op.as_ref(), &input))
            .unwrap();
        let limits = Limits::default();
        let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
        let single = match &input {
            Value::Frames(fr) if op.signature().lift == Lift::PerFrame => {
                Value::Image(Arc::new(Image {
                    px: fr.frames()[0].px.clone(),
                    meta: fr.meta.clone(),
                }))
            }
            v => v.clone(),
        };
        let got = op.apply_region(p.as_ref(), &[&single], &BTreeMap::new(), None, &mut cx);
        if op.region() && reach != Reach::Global {
            assert!(got.is_some_and(|r| r.is_ok()), "{} {name}", op.id());
            with.insert(op.id());
        } else if !op.region() {
            assert!(got.is_none(), "{} {name}", op.id());
        }
    }
    assert_eq!(
        with.into_iter().collect::<Vec<_>>(),
        [
            "image.canvas",
            "image.convert",
            "image.crop",
            "image.flip",
            "image.resize",
            "image.rotate90",
            "meta.set_grid",
            "meta.set_pivot",
            "palette.replace",
        ]
    );
}
