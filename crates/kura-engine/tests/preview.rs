//! プレビューの段の受け入れテスト（段 10。docs/editing.md §12.7・§17 の E32、ADR-154 (8)）。
//!
//! - level0_equals_render：段 0（求めたものと、落ちたもの）は `Engine::render` とビット単位で同じ。
//! - preview_deterministic：段 1〜3 の値は 1・3・16 スレッドのプールで同じ（照合のためにテストの中で取るだけで、ゴールデンには
//!   入れない）。範囲の描画も同じ。
//! - prefix_before_switch_is_exact：等倍でしか描けない操作（paint・trim）を含む鎖で、継ぎ目までの等倍の値が、全体の描画の
//!   同じノードとビット単位で同じ（計画のノードを共有し、キャッシュの当たりを描き直して照らす）。
//! - scaled_params_infer：Scaled・Skip の操作のすべての例で、段 1〜3 の引数が推論を通り、Px の引数が縮んでいる。
//! - preview_shape_close・preview_approximates：段 k の大きさは ⌈等倍/2^k⌉ から縮めたノードの数以内、写真らしい入力では
//!   等倍を縮めたものとの差が小さい（画素を動かす操作・色を混ぜない変換・box は最大 2 以内。グレー・bilinear・Lanczos は
//!   別の上限。PSNR 40 dB 以上）。
//! - choose_level_rules・random_chains_plan_or_fall_back（段 0 の計画が通るなら段 k の計画も通るか段 0 に落ちる）・
//!   level_values_are_not_for_saving（段の値は保存・解析・スポイト・ダイジェストに使えない）・
//!   level_plans_estimate_and_admit（見積もりと予算の照らしが段の計画にも効く）・seam_is_downscaled_once・
//!   seam_rescales_only_changed_tiles・skip_ops_write_metadata_in_level_coordinates・fallback_reasons・
//!   fallback_reasons_of_level_nodes（試験用の操作で、段の推論が通らない Scaled・解析を申告する Scaled・段の推論が通らない
//!   Skip の道を通す）。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::analysis::Need;
use kura_engine::analysis::image::opaque_bounds::{OpaqueBounds, OpaqueBoundsParams};
use kura_engine::budget::MemoryBudget;
use kura_engine::doc::{Graph, InputId, NodeId, OutputId};
use kura_engine::geom::{IPoint, IRect, Size};
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::op::{
    Applied, ApplyCx, DynOp, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, spec,
};
use kura_engine::render::area::{AreaRequest, SpoitAt};
use kura_engine::render::cache::{FingerprintedInput, NodeCache, Origin, RenderOptions};
use kura_engine::render::estimate::{CacheLease, CacheProbe, EstimateSpec, Source};
use kura_engine::render::preview::{
    LevelFallback, LevelHint, MAX_LEVEL, Preview, SEAM_NODE, choose_level, downscale, level_shape,
    level_size,
};
use kura_engine::render::region::Area;
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::shape::{Dim, Fullness, Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::pen;
use kura_engine::testing::preview::{LevelCounts, check_levels};
use kura_engine::testing::region::{below, random_chain, random_input};
use kura_engine::testing::source_of;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, EngineError, Issue, IssueCode, Issues, NoMonitor, Value, determinism};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn engine() -> Engine {
    Engine::new().unwrap()
}

fn pool(n: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .build()
        .unwrap()
}

/// 入力 1 つの線形の鎖。
fn chain(ops: &[(&str, serde_json::Value)]) -> Graph {
    let mut g = Graph::new_single();
    for (i, (op, p)) in ops.iter().enumerate() {
        g.push_linear(op, 1, p.clone(), 7 + i as u64).unwrap();
    }
    g
}

/// 入力を作り、元のハッシュの代わりのダイジェストと一緒に返す。
fn input(make: &InputGen) -> (Value, Digest) {
    (
        make.generate().unwrap(),
        source_of(std::slice::from_ref(make)).unwrap(),
    )
}

fn plain_inputs(v: &Value, source: Digest) -> BTreeMap<InputId, InputValue> {
    BTreeMap::from([(
        InputId(0),
        InputValue {
            value: v.clone(),
            source,
        },
    )])
}

fn fin(v: &Value, source: Digest) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(v.clone(), source).unwrap(),
    )])
}

fn compile(engine: &Engine, g: &Graph, v: &Value) -> Plan {
    engine
        .compile(g, &BTreeMap::from([(InputId(0), v.shape())]))
        .unwrap()
}

/// 筆 1 本の描画の引数。
fn stroke(points: &[(i64, i64)], size: u8, color: [u8; 4]) -> serde_json::Value {
    let pts = points
        .iter()
        .map(|(x, y)| IPoint { x: *x, y: *y })
        .collect();
    json!({ "actions": [pen(pts, size, false, color)] })
}

/// 写真らしい入力（Rgb8）。
fn photo(w: u32, h: u32) -> InputGen {
    InputGen::Photo { w, h, seed: 5 }
}

/// 段の計画をキャッシュの道で描いた出力 0 の値（ダイジェストなし）。
fn render_level(
    engine: &Engine,
    plan: &Plan,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
) -> Value {
    let mut cache = NodeCache::new(1 << 30);
    engine
        .render_cached(
            plan,
            inputs,
            &mut cache,
            &RenderOptions::default(),
            &NoMonitor,
        )
        .unwrap()
        .outputs
        .remove(0)
        .value
}

/// 写真の鎖の例（どれも段 1〜3 の計画になる）。
fn photo_chains() -> Vec<(&'static str, Graph)> {
    vec![
        (
            "flip",
            chain(&[("image.flip", json!({"axis": "horizontal"}))]),
        ),
        (
            "crop-rotate-lanczos",
            chain(&[
                (
                    "image.crop",
                    json!({"rect": {"x": 13, "y": 7, "w": 250, "h": 170}}),
                ),
                ("image.rotate90", json!({"turn": "cw"})),
                (
                    "image.resize",
                    json!({"width": {"px": 120}, "height": "auto", "filter": "lanczos3"}),
                ),
            ]),
        ),
        (
            "canvas-convert-meta",
            chain(&[
                (
                    "image.canvas",
                    json!({"rect": {"x": -9, "y": -5, "w": 330, "h": 230}, "fill": [10, 20, 30, 255]}),
                ),
                (
                    "image.convert",
                    json!({"format": "gray8", "colors": 256, "dither": "none", "matte": [255, 255, 255]}),
                ),
                (
                    "meta.set_grid",
                    json!({"grid": {"cellW": 32, "cellH": 16, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0}}),
                ),
                ("meta.set_pivot", json!({"pivot": {"x": 101, "y": 57}})),
            ]),
        ),
        (
            "paint-then-resize",
            chain(&[
                (
                    "paint.actions",
                    stroke(&[(3, 4), (200, 150)], 9, [255, 0, 0, 255]),
                ),
                (
                    "image.resize",
                    json!({"width": {"scale": {"num": 1, "den": 2}}, "height": "auto", "filter": "bilinear"}),
                ),
            ]),
        ),
    ]
}

/// 段 0（求めたもの）は `render` と同じ。段 0 に落ちた計画は等倍の計画の写しで、やはり `render` と同じ。
#[test]
fn level0_equals_render() {
    let engine = engine();
    let (v, src) = input(&photo(300, 200));
    for (name, g) in photo_chains() {
        let plan = compile(&engine, &g, &v);
        let want = engine
            .render(&plan, &plain_inputs(&v, src), &NoMonitor)
            .unwrap();
        let lp = engine.plan_level(&plan, 0).unwrap();
        assert_eq!(
            (lp.plan.level(), lp.seam, &lp.fallback),
            (0, None, &None),
            "{name}"
        );
        let mut cache = NodeCache::new(1 << 30);
        let opts = RenderOptions {
            digests: true,
            ..RenderOptions::default()
        };
        let got = engine
            .render_cached(&lp.plan, &fin(&v, src), &mut cache, &opts, &NoMonitor)
            .unwrap();
        assert_eq!(
            got.outputs[0].digest,
            Some(want.outputs[0].digest),
            "{name}"
        );
        assert_eq!(got.warnings, want.warnings, "{name}");
        let again = engine
            .render(&lp.plan, &plain_inputs(&v, src), &NoMonitor)
            .unwrap();
        assert_eq!(again.outputs[0].digest, want.outputs[0].digest, "{name}");
    }
    // 段 0 に落ちる計画（Indexed8 の値・フレームの列・音声）
    let fallbacks = [
        (
            InputGen::Noise {
                w: 40,
                h: 30,
                fmt: PixelFormat::Indexed8,
                seed: 3,
            },
            chain(&[(
                "palette.replace",
                json!({"entries": [{"index": 1, "color": [1, 2, 3, 255]}]}),
            )]),
        ),
        (
            InputGen::NoiseFrames {
                w: 20,
                h: 10,
                fmt: PixelFormat::Rgba8,
                count: 3,
                seed: 4,
            },
            chain(&[("image.flip", json!({"axis": "vertical"}))]),
        ),
        (
            InputGen::Sine {
                rate: 8_000,
                channels: 1,
                frames: 800,
                freq: 440,
                loop_range: None,
            },
            chain(&[("audio.gain", json!({"db": -3.0}))]),
        ),
    ];
    for (make, g) in fallbacks {
        let (v, src) = input(&make);
        let plan = compile(&engine, &g, &v);
        let want = engine
            .render(&plan, &plain_inputs(&v, src), &NoMonitor)
            .unwrap();
        for level in 1..=MAX_LEVEL {
            let lp = engine.plan_level(&plan, level).unwrap();
            assert!(lp.fallback.is_some(), "{make:?}");
            assert_eq!(lp.plan.level(), 0);
            let got = engine
                .render(&lp.plan, &plain_inputs(&v, src), &NoMonitor)
                .unwrap();
            assert_eq!(got.outputs[0].digest, want.outputs[0].digest);
        }
    }
}

/// 段 1〜3 の値は、1・3・16 スレッドのプールで同じ（キャッシュの道と範囲の描画）。
#[test]
fn preview_deterministic() {
    let engine = engine();
    let (v, src) = input(&photo(400, 280));
    let inputs = fin(&v, src);
    for (name, g) in photo_chains() {
        let plan = compile(&engine, &g, &v);
        for level in 1..=MAX_LEVEL {
            let lp = engine.plan_level(&plan, level).unwrap();
            assert_eq!(lp.fallback, None, "{name} {level}");
            assert_eq!(lp.plan.level(), level, "{name} {level}");
            let mut seen = Vec::new();
            for threads in [1, 3, 16] {
                let (cached, area) = pool(threads).install(|| {
                    let cached = render_level(&engine, &lp.plan, &inputs);
                    let mut cache = NodeCache::new(1 << 30);
                    let req = AreaRequest {
                        output: OutputId(0),
                        area: Area::rect(0, IRect::new(5, 3, 70, 40)),
                    };
                    let area = engine
                        .render_area(&lp.plan, &inputs, &mut cache, &req, None, &NoMonitor)
                        .unwrap();
                    (cached, area)
                });
                let rect = IRect::new(5, 3, 70, 40);
                let mut buf = vec![0u8; 70 * 40 * cached_bpp(&cached)];
                let r = clip(rect, &cached);
                let mut want = vec![0u8; r.w as usize * r.h as usize * cached_bpp(&cached)];
                area.value.read_rect(0, r, &mut want).unwrap();
                buf.truncate(want.len());
                image_of(&cached).px.read_rect(r, &mut buf).unwrap();
                assert_eq!(buf, want, "{name} {level} {threads}");
                seen.push(cached.digest().unwrap());
            }
            assert!(seen.windows(2).all(|w| w[0] == w[1]), "{name} {level}");
        }
    }
}

fn image_of(v: &Value) -> &kura_engine::value::Image {
    match v {
        Value::Image(img) => img,
        _ => panic!("画像ではありません"),
    }
}

fn cached_bpp(v: &Value) -> usize {
    image_of(v).px.bpp()
}

/// 矩形を値の中に切る。
fn clip(r: IRect, v: &Value) -> IRect {
    let img = image_of(v);
    let w = (r.right().min(i64::from(img.px.width())) - r.x).max(1) as u32;
    let h = (r.bottom().min(i64::from(img.px.height())) - r.y).max(1) as u32;
    IRect::new(r.x, r.y, w, h)
}

/// 等倍でしか描けない操作（paint・trim）を含む鎖で、継ぎ目までの等倍の値は全体の描画の同じノードとビット単位で同じ：
/// 段の計画は継ぎ目までのノードを等倍の計画と共有し（同じ指紋）、段の計画で描いたキャッシュに、継ぎ目までの等倍の文書が
/// 当たりを描き直して照らしても通る。
#[test]
fn prefix_before_switch_is_exact() {
    let engine = engine();
    let make = InputGen::PixelSprite {
        w: 300,
        h: 220,
        fmt: PixelFormat::Rgba8,
        seed: 9,
    };
    let (v, src) = input(&make);
    let ops = [
        ("image.flip", json!({"axis": "horizontal"})),
        (
            "paint.actions",
            stroke(&[(10, 10), (250, 190)], 5, [0, 0, 255, 255]),
        ),
        ("image.trim_alpha", json!({"threshold": 0, "mode": "union"})),
        (
            "image.resize",
            json!({"width": {"px": 100}, "height": "auto", "filter": "lanczos3"}),
        ),
        ("image.rotate90", json!({"turn": "ccw"})),
    ];
    let g = chain(&ops);
    let plan = compile(&engine, &g, &v);
    let inputs = fin(&v, src);
    let seam_id = g.nodes[2].id;
    for level in 1..=MAX_LEVEL {
        let lp = engine.plan_level(&plan, level).unwrap();
        assert_eq!(lp.fallback, None, "{level}");
        assert_eq!(lp.seam, Some(seam_id), "{level}");
        let full_fps = engine.fingerprints(&plan, &inputs).unwrap();
        let fps = engine.fingerprints(&lp.plan, &inputs).unwrap();
        for n in &g.nodes[..3] {
            assert!(lp.plan.shares_node(&plan, n.id), "{level} {}", n.id.0);
            assert_eq!(fps.nodes[&n.id], full_fps.nodes[&n.id], "{level}");
        }
        for n in &g.nodes[3..] {
            assert!(!lp.plan.shares_node(&plan, n.id), "{level}");
            assert_ne!(fps.nodes[&n.id], full_fps.nodes[&n.id], "{level}");
        }
        // 継ぎ目の縮小は派生の指紋
        assert!(fps.nodes.contains_key(&SEAM_NODE));
        let mut cache = NodeCache::new(1 << 30);
        engine
            .render_cached(
                &lp.plan,
                &inputs,
                &mut cache,
                &RenderOptions::default(),
                &NoMonitor,
            )
            .unwrap();
        // 継ぎ目までの文書（出力は継ぎ目）を、同じキャッシュで当たりを照らしながら描く
        let mut prefix = chain(&ops[..3]);
        prefix.outputs[0].from = kura_engine::doc::PortRef::Node { id: seam_id };
        let prefix_plan = compile(&engine, &prefix, &v);
        let want = engine
            .render(&prefix_plan, &plain_inputs(&v, src), &NoMonitor)
            .unwrap();
        let opts = RenderOptions {
            digests: true,
            verify_hits: true,
            ..RenderOptions::default()
        };
        let got = engine
            .render_cached(&prefix_plan, &inputs, &mut cache, &opts, &NoMonitor)
            .unwrap();
        assert_eq!(got.stats.hits, 3, "{level}");
        assert_eq!(got.stats.misses, 0, "{level}");
        assert_eq!(
            got.outputs[0].digest,
            Some(want.outputs[0].digest),
            "{level}"
        );
    }
}

/// 操作に渡す段の入力の形（持ち上げる操作のフレームの列は、フレームの画像の形。段 0 は等倍の形）。
fn level_op_shapes(op: &dyn DynOp, input: &Shape, level: u8) -> Vec<Shape> {
    let s = level_shape(input, level);
    if op.signature().lift == Lift::PerFrame
        && let Some(frame) = s.frame_image()
    {
        return vec![frame];
    }
    vec![s]
}

/// `orig` の Px の数（比の num・den を除く）が、段 `level` で ⌈|n|/2^level⌉ + 1 以下に縮んでいること。返すのは見た数。
fn px_shrunk(orig: &serde_json::Value, got: &serde_json::Value, level: u8, what: &str) -> usize {
    match (orig, got) {
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            let (a, b) = (a.as_i64().unwrap(), b.as_i64().unwrap());
            let bound = level_size(a.unsigned_abs(), level) + 1;
            assert!(
                b.unsigned_abs() <= bound,
                "{what}：段 {level} で {a} が {b} です（⌈|n|/2^k⌉+1 = {bound} 以下に縮めること）"
            );
            1
        }
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => a
            .iter()
            .filter(|(k, _)| k.as_str() != "num" && k.as_str() != "den")
            .filter_map(|(k, va)| b.get(k).map(|vb| px_shrunk(va, vb, level, what)))
            .sum(),
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => a
            .iter()
            .zip(b)
            .map(|(va, vb)| px_shrunk(va, vb, level, what))
            .sum(),
        _ => 0,
    }
}

/// Scaled・Skip の操作のすべての例で、段 1〜3 の引数が段の入力の形で推論を通り、Px の引数が縮んでいる（縮め忘れを捕まえる）。
#[test]
fn scaled_params_infer() {
    let engine = engine();
    let limits = engine.limits().clone();
    let cx = InferCx { limits: &limits };
    let mut ops_seen = Vec::new();
    let mut px_seen = 0;
    for op in engine.registry().current() {
        let schema = op.schema();
        for ex in op.examples().unwrap() {
            let params = op.parse(&ex.params).unwrap();
            let mode = op.preview(params.as_ref()).unwrap();
            if mode == Preview::FullOnly {
                continue;
            }
            if !ops_seen.contains(&op.id()) {
                ops_seen.push(op.id());
            }
            let shape = ex.inputs[0].generate().unwrap().shape();
            for level in 1..=MAX_LEVEL {
                let ins = level_op_shapes(op.as_ref(), &shape, level);
                let full = level_op_shapes(op.as_ref(), &shape, 0);
                let scaled = op
                    .preview_params(params.as_ref(), level, &ins, &full)
                    .unwrap();
                let what = format!("{} {} 段 {level}", op.id(), ex.name);
                if let Err(e) = op.infer(scaled.as_ref(), &ins, &cx) {
                    panic!("{what}：段の引数が推論を通りません（{e:?}）");
                }
                let got = op.params_json(scaled.as_ref()).unwrap();
                for spec in schema.params.iter().filter(|s| s.unit == Unit::Px) {
                    px_seen += px_shrunk(&ex.params[spec.key], &got[spec.key], level, &what);
                }
            }
        }
    }
    for want in [
        "image.crop",
        "image.flip",
        "image.rotate90",
        "image.canvas",
        "image.resize",
        "image.convert",
        "palette.replace",
        "meta.set_grid",
        "meta.set_pivot",
    ] {
        assert!(ops_seen.contains(&want), "{want} の例を照らしていません");
    }
    assert!(px_seen > 30, "{px_seen}");
}

/// 縮小しない（比が 1 以下の）操作だけのランダムな鎖（大きさの近さを見るため）。
fn shrinking_chain(r: &mut ChaCha8Rng, engine: &Engine, input: &Shape, len: usize) -> Graph {
    let mut g = Graph::new_single();
    let shapes = BTreeMap::from([(InputId(0), *input)]);
    for _ in 0..len {
        let now = engine.compile(&g, &shapes).unwrap().output_shapes()[0].1;
        let Shape::Image { w, h, .. } = now else {
            break;
        };
        let (w, h) = (w.max(), h.max());
        let (op, p) = match below(r, 7) {
            0 => {
                let x = below(r, w) as i64;
                let y = below(r, h) as i64;
                let rw = 1 + below(r, w - x as u64);
                let rh = 1 + below(r, h - y as u64);
                (
                    "image.crop",
                    json!({"rect": {"x": x, "y": y, "w": rw, "h": rh}}),
                )
            }
            1 => ("image.flip", json!({"axis": "vertical"})),
            2 => {
                let turn = ["cw", "ccw", "half"][below(r, 3) as usize];
                ("image.rotate90", json!({ "turn": turn }))
            }
            3 => {
                let x = below(r, 40) as i64 - 20;
                let y = below(r, 40) as i64 - 20;
                let rw = 1 + below(r, w + 30);
                let rh = 1 + below(r, h + 30);
                (
                    "image.canvas",
                    json!({"rect": {"x": x, "y": y, "w": rw, "h": rh}, "fill": null}),
                )
            }
            4 => {
                let width = match below(r, 3) {
                    0 => json!({"px": 1 + below(r, w)}),
                    1 => json!({"scale": {"num": 1 + below(r, 2), "den": 2 + below(r, 2)}}),
                    _ => json!("auto"),
                };
                let height = if width == json!("auto") {
                    json!({"px": 1 + below(r, h)})
                } else {
                    json!("auto")
                };
                let filter = ["nearest", "box", "bilinear", "lanczos3"][below(r, 4) as usize];
                (
                    "image.resize",
                    json!({"width": width, "height": height, "filter": filter}),
                )
            }
            5 => {
                let format = ["gray8", "rgba8", "rgb16", "grayA8"][below(r, 4) as usize];
                (
                    "image.convert",
                    json!({"format": format, "colors": 256, "dither": "none", "matte": [255, 255, 255]}),
                )
            }
            _ => (
                "meta.set_pivot",
                json!({"pivot": {"x": below(r, w + 1), "y": below(r, h + 1)}}),
            ),
        };
        let mut next = g.clone();
        next.push_linear(op, 1, p, r.next_u64()).unwrap();
        if engine.compile(&next, &shapes).is_ok() {
            g = next;
        }
    }
    g
}

/// 段 k の大きさの各軸は、⌈等倍/2^k⌉ から、縮めたノード（継ぎ目の後ろの有効なノード）の数以内。
#[test]
fn preview_shape_close() {
    let engine = engine();
    let mut r = determinism::rng(28);
    let mut checked = 0;
    for _ in 0..60 {
        let w = 1 + below(&mut r, 700) as u32;
        let h = 1 + below(&mut r, 500) as u32;
        let make = InputGen::Noise {
            w,
            h,
            fmt: PixelFormat::Rgb8,
            seed: r.next_u64(),
        };
        let shape = Shape::Image {
            w: Dim::Exact(u64::from(w)),
            h: Dim::Exact(u64::from(h)),
            fmt: PixelFormat::Rgb8,
        };
        let len = 1 + below(&mut r, 5) as usize;
        let g = shrinking_chain(&mut r, &engine, &shape, len);
        let plan = engine
            .compile(&g, &BTreeMap::from([(InputId(0), shape)]))
            .unwrap();
        let Shape::Image { w: fw, h: fh, .. } = plan.output_shapes()[0].1 else {
            continue;
        };
        for level in 1..=MAX_LEVEL {
            let lp = engine.plan_level(&plan, level).unwrap();
            assert_eq!(lp.fallback, None, "{g:?}");
            let scaled = g.nodes.iter().filter(|n| n.enabled).count() as u64;
            let Shape::Image { w: lw, h: lh, .. } = lp.plan.output_shapes()[0].1 else {
                panic!("段の出力が画像ではありません");
            };
            for (full, got) in [(fw, lw), (fh, lh)] {
                let want = level_size(full.max(), level);
                assert!(
                    got.max().abs_diff(want) <= scaled,
                    "段 {level}：{} と ⌈{}/2^k⌉ = {want} の差が {scaled} を超えます（{make:?} {g:?}）",
                    got.max(),
                    full.max()
                );
            }
            checked += 1;
        }
    }
    assert!(checked >= 150, "{checked}");
}

/// 写真らしい入力に、反転・回転・2^k に揃えた切り抜きとキャンバス・形式の変換（Indexed8 以外）・リサイズを当てると、段 1・2 の
/// 値と、等倍の値を縮めたもの（`downscale`）との差は小さい（近似の程度。testgen の Photo 640×424 で測った値を添える）：
/// - 画素を動かすだけの操作・色を混ぜない形式の変換・box は最大 2 以内（実測 0〜1）。
/// - グレーへの変換は符号化された値のまま輝度を取る（`pixels::luma`）ので、線形光で縮めるのと順を入れ替えると色の境で差が出る
///   （最大 8 で見張る。実測 5〜6・PSNR 54 dB）。
/// - bilinear・Lanczos は 2 画素より広い重みで、縮めてから当てるのと当ててから縮めるのとで鋭い境の値が違う（最大 32 で見張る。
///   実測 11〜19・PSNR 42.5〜48.7 dB）。
/// - どれも PSNR 40 dB 以上。
#[test]
fn preview_approximates() {
    let engine = engine();
    let (v, src) = input(&photo(640, 424));
    let inputs = fin(&v, src);
    let convert = |format: &str| {
        chain(&[(
            "image.convert",
            json!({"format": format, "colors": 256, "dither": "none", "matte": [255, 255, 255]}),
        )])
    };
    let cases = [
        (chain(&[("image.flip", json!({"axis": "horizontal"}))]), 2),
        (chain(&[("image.rotate90", json!({"turn": "cw"}))]), 2),
        (
            chain(&[(
                "image.crop",
                json!({"rect": {"x": 64, "y": 32, "w": 512, "h": 352}}),
            )]),
            2,
        ),
        (
            chain(&[(
                "image.canvas",
                json!({"rect": {"x": -32, "y": -16, "w": 704, "h": 456}, "fill": [40, 80, 120, 255]}),
            )]),
            2,
        ),
        (convert("rgba8"), 2),
        (convert("rgb8"), 2),
        (convert("gray8"), 8),
        (
            chain(&[(
                "image.resize",
                json!({"width": {"px": 320}, "height": "auto", "filter": "lanczos3"}),
            )]),
            32,
        ),
        (
            chain(&[(
                "image.resize",
                json!({"width": {"px": 320}, "height": "auto", "filter": "bilinear"}),
            )]),
            32,
        ),
        (
            chain(&[(
                "image.resize",
                json!({"width": {"px": 320}, "height": "auto", "filter": "box"}),
            )]),
            2,
        ),
    ];
    for (g, bound) in cases {
        let plan = compile(&engine, &g, &v);
        let full = engine
            .render(&plan, &plain_inputs(&v, src), &NoMonitor)
            .unwrap();
        let full = image_of(&full.outputs[0].value).px.clone();
        for level in 1..=2u8 {
            let lp = engine.plan_level(&plan, level).unwrap();
            assert_eq!(lp.fallback, None);
            let got = render_level(&engine, &lp.plan, &inputs);
            let got = image_of(&got).px.clone();
            let want = downscale(&full, level, &NoMonitor, None).unwrap();
            assert_eq!(
                (got.width(), got.height()),
                (want.width(), want.height()),
                "{g:?}"
            );
            assert_eq!(got.format(), want.format(), "{g:?}");
            let (a, b) = (got.to_rows().unwrap(), want.to_rows().unwrap());
            let diff = a
                .iter()
                .zip(&b)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
            let mse = a
                .iter()
                .zip(&b)
                .map(|(x, y)| {
                    let d = f64::from(x.abs_diff(*y));
                    d * d
                })
                .sum::<f64>()
                / a.len().max(1) as f64;
            let psnr = if mse == 0.0 {
                f64::INFINITY
            } else {
                10.0 * kura_engine::math::log10(255.0 * 255.0 / mse)
            };
            let what = format!("{} {} 段 {level}", g.nodes[0].op, g.nodes[0].params);
            eprintln!("{what}：差 {diff}・PSNR {psnr:.1} dB");
            assert!(diff <= bound, "{what}：差 {diff}");
            assert!(psnr >= 40.0, "{what}：PSNR {psnr:.1} dB");
        }
    }
}

/// 段の選び方：ドット絵の指定・Indexed8・フレームの列・音声・大きさの決まっていない形は 0。写真は、各軸が need を下回らない
/// 最も粗い段（上限 3）。
#[test]
fn choose_level_rules() {
    let img = |w: u64, h: u64, fmt| Shape::Image {
        w: Dim::Exact(w),
        h: Dim::Exact(h),
        fmt,
    };
    let need = |w, h| Size { w, h };
    let photo = img(6000, 4000, PixelFormat::Rgb8);
    let none = LevelHint::default();
    assert_eq!(choose_level(&photo, need(1500, 1000), none), 2);
    assert_eq!(choose_level(&photo, need(1501, 1000), none), 1);
    assert_eq!(choose_level(&photo, need(1500, 1001), none), 1);
    assert_eq!(choose_level(&photo, need(750, 500), none), 3);
    assert_eq!(choose_level(&photo, need(0, 0), none), MAX_LEVEL);
    assert_eq!(choose_level(&photo, need(3000, 2000), none), 1);
    assert_eq!(choose_level(&photo, need(3001, 10), none), 0);
    assert_eq!(choose_level(&photo, need(7000, 1), none), 0);
    // 奇数の大きさは天井で数える（⌈6001/4⌉ = 1501）
    assert_eq!(
        choose_level(&img(6001, 4001, PixelFormat::Rgba8), need(1501, 1001), none),
        2
    );
    assert_eq!(
        choose_level(&img(1, 1, PixelFormat::Gray8), need(1, 1), none),
        MAX_LEVEL
    );
    let art = LevelHint { pixel_art: true };
    assert_eq!(choose_level(&photo, need(10, 10), art), 0);
    assert_eq!(
        choose_level(&img(6000, 4000, PixelFormat::Indexed8), need(10, 10), none),
        0
    );
    let frames = Shape::Frames {
        count: Dim::Exact(3),
        w: Dim::Exact(4000),
        h: Dim::Exact(4000),
        fmt: PixelFormat::Rgba8,
        full: Fullness::Full,
    };
    assert_eq!(choose_level(&frames, need(10, 10), none), 0);
    let audio = Shape::Audio {
        rate: 44_100,
        channels: 2,
        frames: Dim::Exact(44_100),
    };
    assert_eq!(choose_level(&audio, need(10, 10), none), 0);
    let at_most = Shape::Image {
        w: Dim::AtMost(6000),
        h: Dim::Exact(4000),
        fmt: PixelFormat::Rgba8,
    };
    assert_eq!(choose_level(&at_most, need(10, 10), none), 0);
}

/// ランダムな文書（random_docs の生成器の鎖）で、段 0 の計画が通るなら、段 1〜3 の計画も通るか段 0 に落ちる。段の計画は
/// 等倍で描けるなら描け、キャッシュの道と範囲の描画で同じ値になる（`testing::preview::check_levels`）。段 1〜3 の値は
/// 1 と 16 スレッドで同じ。
#[test]
fn random_chains_plan_or_fall_back() {
    let engine = engine();
    let mut r = determinism::rng(29);
    let mut counts = LevelCounts::default();
    for case in 0..60 {
        let make = random_input(&mut r);
        let (v, src) = input(&make);
        let len = 1 + below(&mut r, 5) as usize;
        let g = random_chain(&engine, &mut r, &v.shape(), len, case % 3 == 0);
        let Ok(plan) = engine.compile(&g, &BTreeMap::from([(InputId(0), v.shape())])) else {
            continue;
        };
        let full_ok = engine
            .render(&plan, &plain_inputs(&v, src), &NoMonitor)
            .is_ok();
        let inputs = fin(&v, src);
        let one = pool(3)
            .install(|| check_levels(&engine, &plan, &inputs, full_ok, case))
            .unwrap_or_else(|e| panic!("{case}：{e}（{make:?} {g:?}）"));
        // 段の値はスレッドの数によらない（1 と 16 スレッド）
        for level in 1..=MAX_LEVEL {
            let lp = engine.plan_level(&plan, level).unwrap();
            if lp.fallback.is_some() || !full_ok {
                continue;
            }
            let a = pool(1).install(|| render_level(&engine, &lp.plan, &inputs));
            let b = pool(16).install(|| render_level(&engine, &lp.plan, &inputs));
            assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "{case} {level}");
        }
        counts.add(&one);
    }
    eprintln!("random_chains_plan_or_fall_back：{counts:?}");
    assert!(counts.drawn > 40, "{counts:?}");
    for kind in ["not-image", "indexed"] {
        assert!(
            counts.fallbacks.get(kind).copied().unwrap_or(0) > 0,
            "{kind} {counts:?}"
        );
    }
}

/// 段の値は保存・解析・スポイト・ダイジェストに使えない：保存・一括の道（`render`・`render_with_solver`）とダイジェストを取る
/// `render_cached`・`spoit` は段の計画を断り、範囲の描画の結果は由来 `Scaled`（等倍の値として取り出せない）。解析を申告する
/// ノード（trim）は継ぎ目より前に入り、解析は等倍の値で解く（等倍の計画と同じ鍵で当たる）。
#[test]
fn level_values_are_not_for_saving() {
    let engine = engine();
    let make = InputGen::PixelSprite {
        w: 200,
        h: 160,
        fmt: PixelFormat::Rgba8,
        seed: 2,
    };
    let (v, src) = input(&make);
    let g = chain(&[
        (
            "image.resize",
            json!({"width": {"px": 180}, "height": "auto", "filter": "bilinear"}),
        ),
        ("image.trim_alpha", json!({"threshold": 0, "mode": "union"})),
        ("image.flip", json!({"axis": "horizontal"})),
    ]);
    let plan = compile(&engine, &g, &v);
    let inputs = fin(&v, src);
    let lp = engine.plan_level(&plan, 2).unwrap();
    assert_eq!(lp.fallback, None);
    assert_eq!(lp.seam, Some(g.nodes[1].id));
    let unsupported = |r: Result<_, EngineError>, what: &str| match r {
        Err(EngineError::Unsupported(_)) => {}
        Err(e) => panic!("{what}：{e}"),
        Ok(_) => panic!("{what}：段の計画を断りませんでした"),
    };
    unsupported(
        engine
            .render(&lp.plan, &plain_inputs(&v, src), &NoMonitor)
            .map(|_| ()),
        "render",
    );
    unsupported(
        engine
            .render_with_solver(
                &lp.plan,
                &plain_inputs(&v, src),
                &NoMonitor,
                &kura_engine::analysis::Solve,
            )
            .map(|_| ()),
        "render_with_solver",
    );
    let mut cache = NodeCache::new(1 << 30);
    let digests = RenderOptions {
        digests: true,
        ..RenderOptions::default()
    };
    unsupported(
        engine
            .render_cached(&lp.plan, &inputs, &mut cache, &digests, &NoMonitor)
            .map(|_| ()),
        "render_cached（ダイジェスト）",
    );
    let at = SpoitAt {
        output: OutputId(0),
        frame: 0,
        x: 1,
        y: 1,
    };
    unsupported(
        engine
            .spoit(&lp.plan, &inputs, &mut cache, &at, &NoMonitor)
            .map(|_| ()),
        "spoit",
    );
    let req = AreaRequest {
        output: OutputId(0),
        area: Area::All,
    };
    let area = engine
        .render_area(&lp.plan, &inputs, &mut cache, &req, None, &NoMonitor)
        .unwrap();
    assert_eq!(area.value.origin(), Origin::Scaled);
    assert!(area.value.exact().is_none());
    // 解析：段の計画の継ぎ目の後ろには解析を申告するノードがない。等倍で描いたあとなら、段の計画は解析を解かずに当たる
    for id in lp.plan.node_ids() {
        let needs = lp.plan.node_needs(id).unwrap();
        if id == g.nodes[1].id {
            assert!(!needs.is_empty());
        } else {
            assert!(needs.is_empty(), "{}", id.0);
        }
    }
    let mut cache = NodeCache::new(1 << 30);
    engine
        .render_cached(
            &plan,
            &inputs,
            &mut cache,
            &RenderOptions::default(),
            &NoMonitor,
        )
        .unwrap();
    let r = engine
        .render_cached(
            &lp.plan,
            &inputs,
            &mut cache,
            &RenderOptions::default(),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!((r.stats.analysis_solved, r.stats.hits), (0, 2));
    assert!(r.outputs.iter().all(|o| o.digest.is_none()));
    assert_eq!(r.changed.level, 2);
}

/// 見積もりと予算の照らしが段の計画にも効く：継ぎ目の縮小のノードは段の値と box の作業領域を数え、キャッシュに当たる
/// ノードは 0。24MP で全体に効く鎖（反転・描いてから縮小）では、段の見積もりは等倍の見積もりより小さい。エディタの確保
/// （`admit_cached`）は通り、予算が足りなければ描く前に `Limit`。
#[test]
fn level_plans_estimate_and_admit() {
    let engine = engine();
    let shape = Shape::Image {
        w: Dim::Exact(6000),
        h: Dim::Exact(4000),
        fmt: PixelFormat::Rgb8,
    };
    for (name, g) in photo_chains() {
        if name != "flip" && name != "paint-then-resize" {
            continue;
        }
        let plan = engine
            .compile(&g, &BTreeMap::from([(InputId(0), shape)]))
            .unwrap();
        let lp = engine.plan_level(&plan, 2).unwrap();
        let spec = EstimateSpec::new(3).all_inputs(&plan, Source::Memory);
        let est = engine.estimate(&lp.plan, &spec, None).unwrap();
        let full = engine.estimate(&plan, &spec, None).unwrap();
        assert!(est.peak < full.peak, "{name}：{} ≥ {}", est.peak, full.peak);
    }
    let (v, src) = input(&photo(800, 600));
    let inputs = fin(&v, src);
    for (name, g) in photo_chains() {
        let plan = compile(&engine, &g, &v);
        let lp = engine.plan_level(&plan, 2).unwrap();
        let spec = EstimateSpec::new(3).all_inputs(&lp.plan, Source::Memory);
        let est = engine.estimate(&lp.plan, &spec, None).unwrap();
        let seam = est.nodes.iter().find(|n| n.id == SEAM_NODE).unwrap();
        let level_bytes = lp.plan.node_shape(SEAM_NODE).unwrap().value_bytes();
        assert_eq!(seam.value, level_bytes, "{name}");
        assert!(seam.work > 0, "{name}");
        // 入力・継ぎ目の値・出力は同時に生きる
        assert!(est.peak >= v.shape().value_bytes() + level_bytes, "{name}");
        // 温まったキャッシュでは当たるノードを数えない
        let mut cache = NodeCache::new(1 << 30);
        engine
            .render_cached(
                &lp.plan,
                &inputs,
                &mut cache,
                &RenderOptions::default(),
                &NoMonitor,
            )
            .unwrap();
        let warm = engine
            .estimate(
                &lp.plan,
                &spec,
                Some(CacheProbe {
                    cache: &cache,
                    inputs: &inputs,
                }),
            )
            .unwrap();
        assert!(warm.nodes.iter().all(|n| !n.hit || n.value == 0), "{name}");
        assert!(
            warm.nodes
                .iter()
                .all(|n| n.hit || !n_enabled(&lp.plan, n.id)),
            "{name}"
        );
        // エディタの確保
        let held_spec = EstimateSpec::new(3).all_inputs(&lp.plan, Source::Held);
        let budget = Arc::new(MemoryBudget::new(1 << 30));
        let mut cache = NodeCache::new(64 << 20);
        cache.register_external(&v);
        let mut lease = CacheLease::open(&budget, &mut cache, 64 << 20).unwrap();
        engine
            .admit_cached(&lp.plan, &held_spec, &inputs, &mut cache, &mut lease)
            .unwrap();
        engine
            .render_cached(
                &lp.plan,
                &inputs,
                &mut cache,
                &RenderOptions::default(),
                &NoMonitor,
            )
            .unwrap();
        lease.settle(&mut cache).unwrap();
        let tiny = Arc::new(MemoryBudget::new(v.shape().value_bytes() + 1024));
        let mut cache = NodeCache::new(0);
        let mut lease = CacheLease::open(&tiny, &mut cache, 0).unwrap();
        match engine.admit_cached(&lp.plan, &held_spec, &inputs, &mut cache, &mut lease) {
            Err(EngineError::Limit(_)) => {}
            other => panic!("{name}：{other:?}"),
        }
    }
}

/// 有効なノードなら true（外したノードは当たりを数えない）。
fn n_enabled(plan: &Plan, id: NodeId) -> bool {
    plan.node_inferred(id).is_some_and(|i| i.cost > 0)
}

/// 段の計画は継ぎ目の値（段の入力）を 1 度だけ縮め、後ろのノードの引数を変えても縮め直さない（キャッシュに当たる）。
#[test]
fn seam_is_downscaled_once() {
    let engine = engine();
    let (v, src) = input(&photo(640, 480));
    let inputs = fin(&v, src);
    let mut cache = NodeCache::new(1 << 30);
    for (i, x) in [3, 9, 27].iter().enumerate() {
        let g = chain(&[
            ("image.flip", json!({"axis": "vertical"})),
            (
                "image.crop",
                json!({"rect": {"x": x, "y": 5, "w": 400, "h": 300}}),
            ),
        ]);
        let plan = compile(&engine, &g, &v);
        let lp = engine.plan_level(&plan, 2).unwrap();
        assert_eq!(lp.seam, None);
        let r = engine
            .render_cached(
                &lp.plan,
                &inputs,
                &mut cache,
                &RenderOptions::default(),
                &NoMonitor,
            )
            .unwrap();
        // 1 回目は縮小・反転・切り抜きを描く。2 回目からは縮小と反転が当たり、切り抜きだけを描く
        let want = if i == 0 { (0, 3) } else { (2, 1) };
        assert_eq!((r.stats.hits, r.stats.misses), want, "{x}");
    }
}

/// 継ぎ目の値（等倍の描画）が一部だけ変わったら、継ぎ目の縮小は変わったタイルだけを縮め直す（筆を足したとき）。値は
/// まっさらなキャッシュで描いたもの・全体を縮め直したもの（`verify_hits`）と同じ。
#[test]
fn seam_rescales_only_changed_tiles() {
    let engine = engine();
    let (v, src) = input(&photo(900, 700));
    let inputs = fin(&v, src);
    let mut cache = NodeCache::new(1 << 30);
    let strokes = [
        vec![(10, 10), (60, 40)],
        vec![(500, 300), (520, 330)],
        vec![(800, 600), (850, 650)],
    ];
    for n in 1..=strokes.len() {
        let actions: Vec<_> = strokes[..n]
            .iter()
            .map(|p| {
                let pts = p.iter().map(|(x, y)| IPoint { x: *x, y: *y }).collect();
                pen(pts, 4, false, [250, 10, 10, 255])
            })
            .collect();
        let g = chain(&[
            ("paint.actions", json!({ "actions": actions })),
            ("image.flip", json!({"axis": "horizontal"})),
        ]);
        let plan = compile(&engine, &g, &v);
        let lp = engine.plan_level(&plan, 2).unwrap();
        assert_eq!(lp.seam, Some(g.nodes[0].id));
        let opts = RenderOptions {
            verify_hits: true,
            ..RenderOptions::default()
        };
        let r = engine
            .render_cached(&lp.plan, &inputs, &mut cache, &opts, &NoMonitor)
            .unwrap();
        // 2 本目からは、描画は差分の適用、継ぎ目の縮小は変わったタイルだけ
        let want = if n == 1 { (0, 0) } else { (1, 1) };
        assert_eq!((r.stats.appended, r.stats.rescaled), want, "{n}");
        let fresh = render_level(&engine, &lp.plan, &inputs);
        assert_eq!(
            r.outputs[0].value.digest().unwrap(),
            fresh.digest().unwrap(),
            "{n}"
        );
    }
}

/// Skip の操作（meta.*）は画素を描かずに、メタデータを段の座標で書く（docs/editing.md §12.7）：格子は 2^k で割り切れれば
/// 縮め、割り切れなければ消す。ピボットは段の座標に縮める。画素は前のノードの値をそのまま共有する。
#[test]
fn skip_ops_write_metadata_in_level_coordinates() {
    let engine = engine();
    let (v, src) = input(&photo(400, 280));
    let inputs = fin(&v, src);
    let (_, g) = photo_chains().remove(2);
    let plan = compile(&engine, &g, &v);
    for (level, grid, pivot) in [
        (1u8, Some((16, 8)), IPoint { x: 50, y: 28 }),
        (2, Some((8, 4)), IPoint { x: 25, y: 14 }),
        (3, Some((4, 2)), IPoint { x: 13, y: 7 }),
    ] {
        let lp = engine.plan_level(&plan, level).unwrap();
        assert_eq!(lp.fallback, None);
        let out = render_level(&engine, &lp.plan, &inputs);
        let meta = &image_of(&out).meta;
        assert_eq!(meta.grid.map(|g| (g.cell_w, g.cell_h)), grid, "{level}");
        assert_eq!(meta.pivot, Some(pivot), "{level}");
    }
    // 割り切れない格子は段では消える（等倍の値には残る）
    let odd = chain(&[(
        "meta.set_grid",
        json!({"grid": {"cellW": 6, "cellH": 6, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0}}),
    )]);
    let plan = compile(&engine, &odd, &v);
    let lp = engine.plan_level(&plan, 2).unwrap();
    let out = render_level(&engine, &lp.plan, &inputs);
    assert_eq!(image_of(&out).meta.grid, None);
    let full = engine
        .render(&plan, &plain_inputs(&v, src), &NoMonitor)
        .unwrap();
    assert!(image_of(&full.outputs[0].value).meta.grid.is_some());
}

/// 落ちる理由：継ぎ目の値が Indexed8（写真を Indexed8 にしてから番号の色を替える。palette.replace は Indexed8 しか受け付けない
/// ので、段の推論より先に継ぎ目の convert の値で落ちる）・線形でない文書。段の推論が通らない Scaled・解析を申告する Scaled・
/// 段の推論が通らない Skip は、今の登録表の操作では届かないので `fallback_reasons_of_level_nodes`（試験用の操作）で確かめる。
#[test]
fn fallback_reasons() {
    let engine = engine();
    let (v, _) = input(&photo(120, 80));
    let g = chain(&[
        (
            "image.convert",
            json!({"format": "indexed8", "colors": 16, "dither": "none", "matte": [255, 255, 255]}),
        ),
        (
            "palette.replace",
            json!({"entries": [{"index": 0, "color": [255, 255, 0, 255]}]}),
        ),
    ]);
    let plan = compile(&engine, &g, &v);
    let lp = engine.plan_level(&plan, 1).unwrap();
    assert_eq!(lp.fallback, Some(LevelFallback::Indexed(g.nodes[0].id)));
    // 2 つ目の出力のある文書は線形でない
    let mut two = chain(&[("image.flip", json!({"axis": "horizontal"}))]);
    let mut out = two.outputs[0].clone();
    out.id = OutputId(1);
    two.outputs.push(out);
    let plan = compile(&engine, &two, &v);
    assert_eq!(
        engine.plan_level(&plan, 1).unwrap().fallback,
        Some(LevelFallback::NotLinear)
    );
    // 段の上限を超える段・段の計画からの段の計画は断る
    let plan = compile(
        &engine,
        &chain(&[("image.flip", json!({"axis": "vertical"}))]),
        &v,
    );
    assert!(matches!(
        engine.plan_level(&plan, MAX_LEVEL + 1),
        Err(EngineError::Unsupported(_))
    ));
    let lp = engine.plan_level(&plan, 1).unwrap();
    assert!(matches!(
        engine.plan_level(&lp.plan, 1),
        Err(EngineError::Unsupported(_))
    ));
    // 別のエンジンの計画は内部の誤り
    let other = Engine::new().unwrap();
    assert!(matches!(
        other.plan_level(&plan, 1),
        Err(EngineError::Internal(_))
    ));
}

/// `test.level_probe` の引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeParams {
    /// 推論が通る入力の幅の下限（段の入力の幅がこれより狭いと推論が通らない）。
    min: u64,
    /// 段で `Skip`（偽なら `Scaled`）。
    skip: bool,
    /// 解析（image.opaque_bounds）を申告する。
    analysis: bool,
}

impl OpParams for ProbeParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "min",
                    ParamKind::Int {
                        min: 0,
                        max: 1 << 20,
                        step: 1,
                    },
                    Unit::Px,
                    json!(0),
                ),
                spec("skip", ParamKind::Bool, Unit::None, json!(false)),
                spec("analysis", ParamKind::Bool, Unit::None, json!(false)),
            ],
        }
    }
}

/// `test.level_probe`：値をそのまま返す。幅が `min` より狭い入力では推論が通らない（段の幅でだけ通らない形を作る）。段では
/// `skip` なら Skip、そうでなければ Scaled（引数は縮めない）。`analysis` なら解析を申告する。
struct LevelProbe;

impl Op for LevelProbe {
    const ID: &'static str = "test.level_probe";
    const VERSION: u16 = 1;
    const NEEDS: &'static [(&'static str, u16)] = &[("image.opaque_bounds", 1)];
    type Params = ProbeParams;
    fn signature() -> Signature {
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
    fn infer(p: &ProbeParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        let Shape::Image { w, .. } = inputs[0] else {
            return Err(Issues::none());
        };
        if w.max() < p.min {
            return Err(Issues::one(
                Issue::error(IssueCode::BadParams, format!("幅が {} より狭い", p.min)).param("min"),
            ));
        }
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: 1,
        })
    }
    fn needs(p: &ProbeParams, _inputs: &[Shape]) -> Result<Vec<Need>, Issues> {
        if !p.analysis {
            return Ok(Vec::new());
        }
        Ok(vec![Need::of::<OpaqueBounds>(
            0,
            &OpaqueBoundsParams { threshold: 0 },
        )?])
    }
    fn apply(
        _: &ProbeParams,
        inputs: &[&Value],
        _: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        Ok(Applied::value(inputs[0].clone()))
    }
    fn preview(p: &ProbeParams) -> Preview {
        if p.skip {
            Preview::Skip
        } else {
            Preview::Scaled
        }
    }
    fn examples() -> Vec<Example<ProbeParams>> {
        Vec::new()
    }
}

/// E32（§12.7）：段 0 に落ちる理由のうち、段の形で推論が通らない Scaled（`Infer`）と、解析を申告する Scaled（`Analysis`）。
/// 落ちた計画は等倍の計画の写しで、`render` と同じ値。Skip の操作は、段の形で推論が通らなければ段の計画で外したもの
/// （入力 0 の指紋・手間 0）になり、その操作のない鎖の段の計画と同じ値で描ける。今の登録表の操作ではこの道に届かない
/// （palette.replace は Indexed8 で先に落ち、set_grid・set_pivot の段の引数はいつも推論を通る）ので、試験用の操作で通す。
#[test]
fn fallback_reasons_of_level_nodes() {
    let mut registry = kura_engine::ops::registry().unwrap();
    registry.add_current::<LevelProbe>().unwrap();
    let engine = Engine::with_registry(registry, kura_engine::Limits::default());
    let make = InputGen::Noise {
        w: 120,
        h: 80,
        fmt: PixelFormat::Rgba8,
        seed: 31,
    };
    let (v, src) = input(&make);
    let inputs = fin(&v, src);
    let probe = |min: u64, skip: bool, analysis: bool| {
        (
            "test.level_probe",
            json!({ "min": min, "skip": skip, "analysis": analysis }),
        )
    };
    let flip = ("image.flip", json!({ "axis": "horizontal" }));
    // 段の形で推論が通らない Scaled（等倍の幅 120 なら通り、段 1 の 60 では通らない）
    let g = chain(&[flip.clone(), probe(100, false, false)]);
    let plan = compile(&engine, &g, &v);
    let lp = engine.plan_level(&plan, 1).unwrap();
    match &lp.fallback {
        Some(LevelFallback::Infer { node, issues }) => {
            assert_eq!(*node, g.nodes[1].id);
            assert!(!issues.0.is_empty());
        }
        other => panic!("段の推論で落ちていない: {other:?}"),
    }
    assert_eq!(lp.plan.level(), 0);
    let want = engine
        .render(&plan, &plain_inputs(&v, src), &NoMonitor)
        .unwrap();
    let got = render_level(&engine, &lp.plan, &inputs);
    assert_eq!(got, want.outputs[0].value);
    // 段の幅でも通る下限なら段の計画になる（落ちた理由は下限だけ）
    let ok = compile(
        &engine,
        &chain(&[flip.clone(), probe(60, false, false)]),
        &v,
    );
    let lp_ok = engine.plan_level(&ok, 1).unwrap();
    assert_eq!((lp_ok.fallback.is_none(), lp_ok.plan.level()), (true, 1));
    // 解析を申告する Scaled
    let g = chain(&[flip.clone(), probe(0, false, true)]);
    let plan = compile(&engine, &g, &v);
    assert_eq!(plan.node_needs(g.nodes[1].id).map(<[Need]>::len), Some(1));
    let lp = engine.plan_level(&plan, 2).unwrap();
    assert_eq!(lp.fallback, Some(LevelFallback::Analysis(g.nodes[1].id)));
    assert_eq!(lp.plan.level(), 0);
    let want = engine
        .render(&plan, &plain_inputs(&v, src), &NoMonitor)
        .unwrap();
    assert_eq!(
        render_level(&engine, &lp.plan, &inputs),
        want.outputs[0].value
    );
    // 段の形で推論が通らない Skip は、段の計画で外したもの
    let g = chain(&[flip.clone(), probe(100, true, false)]);
    let plan = compile(&engine, &g, &v);
    let without = compile(&engine, &chain(std::slice::from_ref(&flip)), &v);
    for level in 1..=MAX_LEVEL {
        let lp = engine.plan_level(&plan, level).unwrap();
        assert_eq!(
            (lp.fallback.is_none(), lp.plan.level()),
            (true, level),
            "{level}"
        );
        let fps = engine.fingerprints(&lp.plan, &inputs).unwrap();
        assert_eq!(
            fps.nodes[&g.nodes[1].id], fps.nodes[&g.nodes[0].id],
            "{level}：外したノードは入力 0 の指紋"
        );
        let base = engine.plan_level(&without, level).unwrap();
        assert_eq!(
            lp.plan.cost(),
            base.plan.cost(),
            "{level}：外したノードの手間は 0"
        );
        assert_eq!(
            render_level(&engine, &lp.plan, &inputs),
            render_level(&engine, &base.plan, &inputs),
            "{level}"
        );
    }
    // 等倍では外さない（推論が通る）
    assert_eq!(
        engine.plan_level(&plan, 0).unwrap().plan.cost(),
        without.cost() + 1
    );
}
