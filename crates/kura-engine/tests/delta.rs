//! 変わった範囲の受け入れテスト（段 10。docs/editing.md §12.6・§17 の E31、ADR-154 (6)・(7)）。
//!
//! - inferred_delta_contains_actual：ランダムな文書にランダムな編集を当て、前後を全体で描いた差が推論の範囲に入る。
//! - local_edits_do_not_fall_to_all：局所の編集の推論が全体に落ちない（締まり具合の見張り）。
//! - incremental_equals_fresh：前の値と推論した差で継ぎはぎした値が、毎回まっさらに描いたものとタイルごとに同じ。
//! - changed_tiles_by_identity：render_cached の報告する変わったタイルは、中身の変わったタイルを含み、推論の範囲の中にある。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};

use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::geom::IRect;
use kura_engine::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use kura_engine::render::area::AreaRequest;
use kura_engine::render::cache::{FingerprintedInput, NodeCache, Origin, RenderOptions};
use kura_engine::render::delta::{OutputChange, refine};
use kura_engine::render::region::{Area, Base, PartialValue, Tiles, VIEW_TILE, frames_of};
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::{
    STEP_KINDS, below, check_contains, check_region, palette_change_cases, random_chain,
    random_input, random_step, random_step_or_input,
};
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, Issue, NoMonitor, Value, determinism};
use serde_json::json;

fn engine() -> Engine {
    Engine::new().unwrap()
}

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

fn render(engine: &Engine, plan: &Plan, input: &Value) -> Option<(Value, Vec<Issue>)> {
    let (_, plain) = inputs_of(input);
    let mut r = engine.render(plan, &plain, &NoMonitor).ok()?;
    Some((r.outputs.swap_remove(0).value, r.warnings))
}

fn json_of(g: &Graph) -> String {
    String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned()
}

fn stroke(frame: u32, x: i64, y: i64, size: u8) -> PaintAction {
    PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Rgba([250, 10, 20, 255])),
        blend: Blend::Replace,
        brush: Brush {
            shape: BrushShape::Circle,
            size,
        },
        pixel_perfect: false,
        frame,
        points: PointList(vec![
            kura_engine::geom::IPoint { x, y },
            kura_engine::geom::IPoint {
                x: x + 30,
                y: y + 7,
            },
        ]),
    }
}

/// E31：ランダムな文書（画像・パレット・フレームの列・トリムしたフレームの列・シート・音声。音声はときどき出力を
/// 量子化）にランダムな手（引数の引き直し・描画への追記・途中の動作の差し替え・有効の切り替え・フェードの長さ・ループ点・
/// メタデータ・canvas の fill だけ・種だけ・構造の手（置き換え・消す・動かす・足す）、2〜3 手をまとめたもの、入力の値だけの
/// 変更）を当て、前後を全体で描いた差の画素・標本が推論の範囲に入る。形が変われば全体。
#[test]
fn inferred_delta_contains_actual() {
    let engine = engine();
    let (kinds, checked, partial) = infer_cases(&engine, 19, 200);
    for k in STEP_KINDS.iter().chain(&["input"]) {
        assert!(
            kinds.get(k).copied().unwrap_or(0) > 0,
            "{k} の編集が 1 回も起きていません：{kinds:?}"
        );
    }
    assert!(
        checked > 200 && partial > 80,
        "{checked} {partial} {kinds:?}"
    );
}

/// E31 を、段 10 の検証で推論の抜け（上流のパレットだけの変化が canvas などの後ろで画素の矩形に化ける）を
/// 見つけた種でも回す（数の断言はしない。種ごとにスレッドを分けて回す）。
#[test]
fn inferred_delta_contains_actual_on_found_seeds() {
    // テストの時間を縮めるためだけのスレッド（描画の値には効かない）
    #[allow(clippy::disallowed_methods)]
    std::thread::scope(|sc| {
        let runs: Vec<_> = [1003, 1007, 1014, 1022, 1036, 1038, 1040]
            .into_iter()
            .map(|seed| sc.spawn(move || infer_cases(&engine(), seed, 140)))
            .collect();
        for run in runs {
            if let Err(e) = run.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
}

/// E31 の本体：種 `seed` で `cases` 個の文書を作り、編集ごとに前後を全体で描いた差が推論に入るかを確かめる。返すのは
/// (編集の種類ごとの数, 照らした数, 全体でなかった数)。
fn infer_cases(
    engine: &Engine,
    seed: u64,
    cases: usize,
) -> (BTreeMap<&'static str, usize>, usize, usize) {
    let mut r = determinism::rng(seed);
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut checked, mut partial) = (0, 0);
    for case in 0..cases {
        let make = random_input(&mut r);
        let mut input = make.generate().unwrap();
        let len = 1 + below(&mut r, 4) as usize;
        let mut g = random_chain(engine, &mut r, &input.shape(), len, case % 3 == 0);
        let Some(mut plan) = compile(engine, &g, &input) else {
            continue;
        };
        let (mut fin, _) = inputs_of(&input);
        let Some((mut before, _)) = render(engine, &plan, &input) else {
            continue;
        };
        for _ in 0..3 {
            let Some((next, kind, next_input)) =
                random_step_or_input(engine, &mut r, &g, &make, &input)
            else {
                break;
            };
            let Some(next_plan) = compile(engine, &next, &next_input) else {
                break;
            };
            let Some((after, _)) = render(engine, &next_plan, &next_input) else {
                break;
            };
            let (next_fin, _) = inputs_of(&next_input);
            let delta = engine.delta(&plan, &fin, &next_plan, &next_fin).unwrap();
            let area = &delta.outputs[0].1;
            check_contains(&before, &after, area).unwrap_or_else(|m| {
                panic!(
                    "種 {seed}・{case} {kind}: {m}\n前 {}\n後 {}\n推論 {area:?}",
                    json_of(&g),
                    json_of(&next)
                )
            });
            *kinds.entry(kind).or_default() += 1;
            checked += 1;
            if !area.is_all() {
                partial += 1;
            }
            (g, plan, before, input, fin) = (next, next_plan, after, next_input, next_fin);
        }
    }
    (kinds, checked, partial)
}

/// パレットの変化の決まった場合（段 10 の検証）：Indexed8 の上流でパレットだけが変わる（palette.replace の引数の
/// 変更・外す・入れる、Indexed8 への convert の引数の変更）と、後ろの canvas（広げる）・反転・最近傍の拡大の出力のパレットも
/// 変わるので、推論は全体（入力を写した画素の矩形に絞らない）。
#[test]
fn palette_changes_upstream_stay_all() {
    let engine = engine();
    for (what, a, b, input) in palette_change_cases().unwrap() {
        let (fin, _) = inputs_of(&input);
        let (pa, pb) = (
            compile(&engine, &a, &input).unwrap(),
            compile(&engine, &b, &input).unwrap(),
        );
        let (va, _) = render(&engine, &pa, &input).unwrap();
        let (vb, _) = render(&engine, &pb, &input).unwrap();
        let area = engine.delta(&pa, &fin, &pb, &fin).unwrap().outputs[0]
            .1
            .clone();
        check_contains(&va, &vb, &area).unwrap_or_else(|m| panic!("{what}: {m}\n推論 {area:?}"));
        assert!(area.is_all(), "{what}: {area:?}");
    }
}

/// 継ぎはぎの守り（段 10 の検証）：推論が小さすぎて、パレットの違う前の値を継ぎはぎしようとしても、範囲の描画が
/// 前の値の形式・パレットの違いで断ったら、前の値なしで描き直す（誤りにしない。値は render と同じ）。
#[test]
fn render_area_redraws_without_a_base_of_another_palette() {
    let engine = engine();
    let all = AreaRequest {
        output: OutputId(0),
        area: Area::All,
    };
    for (what, a, b, input) in palette_change_cases().unwrap() {
        let (fin, _) = inputs_of(&input);
        let (pa, pb) = (
            compile(&engine, &a, &input).unwrap(),
            compile(&engine, &b, &input).unwrap(),
        );
        let (va, _) = render(&engine, &pa, &input).unwrap();
        let (vb, _) = render(&engine, &pb, &input).unwrap();
        if frames_of(&va)[0].1.palette() == frames_of(&vb)[0].1.palette() {
            continue;
        }
        // 古い推論の形（入力を写した矩形だけ）を、わざと前の値の変わりうる範囲として渡す
        let narrow = Area::rect(0, IRect::new(0, 0, 1, 1));
        let prev = PartialValue::full(va.clone());
        let base = Base {
            value: &prev,
            changed: &narrow,
        };
        let mut cache = NodeCache::new(0);
        let got = engine
            .render_area(&pb, &fin, &mut cache, &all, Some(&base), &NoMonitor)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        check_region(&got.value, &vb, &Area::All).unwrap_or_else(|m| panic!("{what}: {m}"));
        assert_ne!(got.value.origin(), Origin::Patched, "{what}");
        assert_eq!(got.stats.rebased, 1, "{what}: {:?}", got.stats);
        assert_eq!(got.stats.fallbacks, 0, "{what}");
    }
}

/// E31 の決まった場合（フレームの次元。docs/editing.md §12.5）：フレーム 0 以外の描画、トリムしたフレームの列への描画と
/// 形式の変換、入力の値が変わったとき（全体）。
#[test]
fn delta_handles_frames_and_inputs() {
    let engine = engine();
    let inputs = [
        InputGen::NoiseFrames {
            w: 300,
            h: 260,
            fmt: PixelFormat::Rgba8,
            count: 3,
            seed: 1,
        },
        InputGen::TrimmedFrames {
            w: 300,
            h: 270,
            fmt: PixelFormat::Rgba8,
            count: 3,
            seed: 2,
        },
    ];
    for make in inputs {
        let input = make.generate().unwrap();
        let (fin, _) = inputs_of(&input);
        let doc = |actions: Vec<PaintAction>, fmt: &str| {
            let mut g = Graph::new_single();
            g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
                .unwrap();
            g.push_linear(
                "image.convert",
                1,
                json!({ "format": fmt, "colors": 256, "dither": "none", "matte": [0, 0, 0] }),
                0,
            )
            .unwrap();
            g
        };
        let a = doc(vec![stroke(2, 10, 10, 3)], "rgb16");
        let b = doc(vec![stroke(2, 10, 10, 3), stroke(1, 200, 150, 5)], "rgb16");
        let (pa, pb) = (
            compile(&engine, &a, &input).unwrap(),
            compile(&engine, &b, &input).unwrap(),
        );
        let (va, _) = render(&engine, &pa, &input).unwrap();
        let (vb, _) = render(&engine, &pb, &input).unwrap();
        let delta = engine.delta(&pa, &fin, &pb, &fin).unwrap();
        let area = &delta.outputs[0].1;
        check_contains(&va, &vb, area).unwrap();
        // 変わったのはフレーム 1 の筆の範囲だけ
        let Area::Pixels(map) = area else {
            panic!("{area:?}")
        };
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), vec![1], "{make:?}");
        // 入力の値が変われば全体
        let other = InputGen::NoiseFrames {
            w: 300,
            h: 260,
            fmt: PixelFormat::Rgba8,
            count: 3,
            seed: 9,
        }
        .generate()
        .unwrap();
        if other.shape() == input.shape() {
            let (fo, _) = inputs_of(&other);
            assert!(
                engine.delta(&pa, &fin, &pa, &fo).unwrap().outputs[0]
                    .1
                    .is_all()
            );
        }
        assert!(
            engine.delta(&pa, &fin, &pa, &fin).unwrap().outputs[0]
                .1
                .is_empty()
        );
    }
}

/// §12.6 の「規則・段・エンジン・元が違えば全体」（段 10 の検証）：入力の値（指紋）が同じで元だけが違う（元は種と
/// TPDF のディザに入る）・別のエンジンの計画・段の違う計画は、どのノード・出力も全体。元の違いは render_cached の報告にも出る。
/// 規則の違う計画は作れない（文書の規則は 1〜RULES で、RULES は 1）。
#[test]
fn delta_falls_to_all_for_other_sources_engines_or_levels() {
    let engine = engine();
    let all_of = |d: &kura_engine::render::delta::Delta| {
        d.nodes.values().all(Area::is_all) && d.outputs.iter().all(|(_, a)| a.is_all())
    };
    // 元だけが違う：量子化（8 ビット・TPDF）した音声
    let audio = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 4_000,
        seed: 1,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -3.0 }), 0)
        .unwrap();
    g.outputs[0].quantize = Some(kura_engine::doc::Quantize {
        v: kura_engine::doc::QUANTIZE_VERSION,
        bits: 8,
        dither: kura_engine::doc::Dither::Tpdf,
        seed: 1,
    });
    let plan = compile(&engine, &g, &audio).unwrap();
    let with_source = |s: u8| {
        BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(audio.clone(), Digest([s; 32])).unwrap(),
        )])
    };
    let (a, b) = (with_source(1), with_source(2));
    assert_eq!(a[&InputId(0)].fingerprint, b[&InputId(0)].fingerprint);
    let d = engine.delta(&plan, &a, &plan, &b).unwrap();
    assert!(all_of(&d), "{d:?}");
    assert!(
        engine.delta(&plan, &a, &plan, &a).unwrap().outputs[0]
            .1
            .is_empty()
    );
    let mut cache = NodeCache::new(1 << 30);
    let opts = RenderOptions::default();
    let ra = engine
        .render_cached(&plan, &a, &mut cache, &opts, &NoMonitor)
        .unwrap();
    let rb = engine
        .render_cached(&plan, &b, &mut cache, &opts, &NoMonitor)
        .unwrap();
    let (va, vb) = (&ra.outputs[0].value, &rb.outputs[0].value);
    assert_ne!(
        va.digest().unwrap(),
        vb.digest().unwrap(),
        "元で TPDF のディザが変わる前提"
    );
    check_contains(va, vb, &d.outputs[0].1).unwrap();
    assert!(
        !rb.changed.per_output[0].1.is_empty(),
        "{:?}",
        rb.changed.per_output[0].1
    );
    // 別のエンジンの計画どうし（同じ文書）
    let image = InputGen::Noise {
        w: 600,
        h: 400,
        fmt: PixelFormat::Rgba8,
        seed: 2,
    }
    .generate()
    .unwrap();
    let (fin, _) = inputs_of(&image);
    let mut g = Graph::new_single();
    g.push_linear(
        "paint.actions",
        1,
        json!({ "actions": [stroke(0, 10, 10, 3)] }),
        0,
    )
    .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    let other = Engine::new().unwrap();
    let (p1, p2) = (
        compile(&engine, &g, &image).unwrap(),
        compile(&other, &g, &image).unwrap(),
    );
    assert!(
        engine.delta(&p1, &fin, &p1, &fin).unwrap().outputs[0]
            .1
            .is_empty()
    );
    assert!(all_of(&engine.delta(&p1, &fin, &p2, &fin).unwrap()));
    // 段の違う計画（等倍 → 段 1）
    let lp = engine.plan_level(&p1, 1).unwrap();
    assert_eq!(lp.plan.level(), 1, "{:?}", lp.fallback);
    assert!(all_of(&engine.delta(&p1, &fin, &lp.plan, &fin).unwrap()));
    assert!(
        engine
            .delta(&lp.plan, &fin, &lp.plan, &fin)
            .unwrap()
            .outputs[0]
            .1
            .is_empty()
    );
}

/// 持ち上げる操作の引数を変えたときの範囲（§12.6 の末尾）：フレームの列（キャンバスいっぱい）に持ち上げた
/// canvas の fill だけを変えると、1 フレームの範囲（入力の外の縁）が全フレームに広がる（全体には落ちない）。
#[test]
fn lifted_param_change_spans_all_frames() {
    let engine = engine();
    let input = InputGen::NoiseFrames {
        w: 40,
        h: 30,
        fmt: PixelFormat::Rgba8,
        count: 3,
        seed: 4,
    }
    .generate()
    .unwrap();
    let (fin, _) = inputs_of(&input);
    let doc = |fill: serde_json::Value| {
        let mut g = Graph::new_single();
        g.push_linear(
            "image.canvas",
            1,
            json!({ "rect": { "x": -5, "y": -4, "w": 50, "h": 40 }, "fill": fill }),
            0,
        )
        .unwrap();
        g
    };
    for (a, b) in [
        (json!([1, 2, 3, 255]), json!([9, 9, 9, 255])),
        (json!(null), json!([9, 9, 9, 255])),
    ] {
        let (pa, pb) = (
            compile(&engine, &doc(a.clone()), &input).unwrap(),
            compile(&engine, &doc(b.clone()), &input).unwrap(),
        );
        let (va, _) = render(&engine, &pa, &input).unwrap();
        let (vb, _) = render(&engine, &pb, &input).unwrap();
        let area = engine.delta(&pa, &fin, &pb, &fin).unwrap().outputs[0]
            .1
            .clone();
        check_contains(&va, &vb, &area).unwrap_or_else(|m| panic!("{a} → {b}: {m}\n推論 {area:?}"));
        let Area::Pixels(map) = &area else {
            panic!("{a} → {b}: 全体に落ちた・画素の範囲でない：{area:?}")
        };
        assert_eq!(
            map.keys().copied().collect::<Vec<_>>(),
            vec![0, 1, 2],
            "{a} → {b}"
        );
        // 入力を写した内側（5..45 × 4..34）は含まない
        assert!(!area.contains_pixel(1, 20, 15), "{a} → {b}: {area:?}");
    }
}

/// 締まり具合の見張り：描画への追記・キャンバスの fill だけの変更・フェードの長さ・メタデータ・ループ点の推論が
/// 全体に落ちない。描画だけの文書で、推論のタイルの数が「実際に変わったタイル＋筆の範囲のタイル」を超えない。
#[test]
fn local_edits_do_not_fall_to_all() {
    let engine = engine();
    let image = InputGen::Noise {
        w: 1024,
        h: 768,
        fmt: PixelFormat::Rgba8,
        seed: 20,
    }
    .generate()
    .unwrap();
    let audio = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 20_000,
        seed: 20,
    }
    .generate()
    .unwrap();
    let single = |op: &str, p: serde_json::Value| {
        let mut g = Graph::new_single();
        g.push_linear(op, 1, p, 0).unwrap();
        g
    };
    let pairs: Vec<(&str, Graph, Graph, &Value)> = vec![
        (
            "追記",
            single(
                "paint.actions",
                json!({ "actions": [stroke(0, 10, 10, 4)] }),
            ),
            single(
                "paint.actions",
                json!({ "actions": [stroke(0, 10, 10, 4), stroke(0, 600, 500, 9)] }),
            ),
            &image,
        ),
        (
            "fill だけ",
            single(
                "image.canvas",
                json!({ "rect": { "x": -20, "y": -10, "w": 1100, "h": 800 }, "fill": [1, 2, 3, 255] }),
            ),
            single(
                "image.canvas",
                json!({ "rect": { "x": -20, "y": -10, "w": 1100, "h": 800 }, "fill": [9, 9, 9, 255] }),
            ),
            &image,
        ),
        (
            "フェードの長さ",
            single(
                "audio.fade",
                json!({ "fadeIn": 100, "fadeOut": 0, "curve": "linear" }),
            ),
            single(
                "audio.fade",
                json!({ "fadeIn": 300, "fadeOut": 0, "curve": "linear" }),
            ),
            &audio,
        ),
        (
            "格子",
            single(
                "meta.set_grid",
                json!({ "grid": { "cellW": 16, "cellH": 16, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 } }),
            ),
            single(
                "meta.set_grid",
                json!({ "grid": { "cellW": 32, "cellH": 16, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 } }),
            ),
            &image,
        ),
        (
            "ピボット",
            single("meta.set_pivot", json!({ "pivot": { "x": 1, "y": 2 } })),
            single("meta.set_pivot", json!({ "pivot": { "x": 5, "y": 2 } })),
            &image,
        ),
        (
            "ループ点",
            single(
                "audio.set_loop",
                json!({ "loop": { "start": 10, "end": 900 } }),
            ),
            single(
                "audio.set_loop",
                json!({ "loop": { "start": 20, "end": 900 } }),
            ),
            &audio,
        ),
    ];
    for (what, a, b, input) in pairs {
        let (fin, _) = inputs_of(input);
        let (pa, pb) = (
            compile(&engine, &a, input).unwrap(),
            compile(&engine, &b, input).unwrap(),
        );
        let area = engine.delta(&pa, &fin, &pb, &fin).unwrap().outputs[0]
            .1
            .clone();
        assert!(!area.is_all(), "{what}: 全体に落ちた");
        let (va, _) = render(&engine, &pa, input).unwrap();
        let (vb, _) = render(&engine, &pb, input).unwrap();
        check_contains(&va, &vb, &area).unwrap_or_else(|m| panic!("{what}: {m}"));
        match what {
            "格子" | "ピボット" | "ループ点" => {
                assert!(area.is_empty(), "{what}: {area:?}")
            }
            "フェードの長さ" => assert_eq!(area, Area::span(0, 300)),
            _ => {}
        }
    }
    // 外して同じになる操作（入力と同じ矩形・形式のキャンバス、同じ形式への変換）は、外しても入れても何も変わらない
    for (op, p) in [
        (
            "image.canvas",
            json!({ "rect": { "x": 0, "y": 0, "w": 1024, "h": 768 }, "fill": null }),
        ),
        (
            "image.convert",
            json!({ "format": "rgba8", "colors": 256, "dither": "none", "matte": [0, 0, 0] }),
        ),
    ] {
        let a = single(op, p);
        let mut b = a.clone();
        b.nodes[0].enabled = false;
        let (fin, _) = inputs_of(&image);
        let (pa, pb) = (
            compile(&engine, &a, &image).unwrap(),
            compile(&engine, &b, &image).unwrap(),
        );
        assert!(
            engine.delta(&pa, &fin, &pb, &fin).unwrap().outputs[0]
                .1
                .is_empty(),
            "{op}"
        );
    }
    // 描画だけの文書：推論のタイル ≤ 実際に変わったタイル＋筆の範囲のタイル
    let mut r = determinism::rng(20);
    let mut actions: Vec<PaintAction> = Vec::new();
    let (fin, _) = inputs_of(&image);
    for _ in 0..12 {
        let prev = single("paint.actions", json!({ "actions": actions }));
        let s = stroke(
            0,
            below(&mut r, 1000) as i64,
            below(&mut r, 760) as i64,
            1 + below(&mut r, 20) as u8,
        );
        actions.push(s.clone());
        let next = single("paint.actions", json!({ "actions": actions }));
        let (pa, pb) = (
            compile(&engine, &prev, &image).unwrap(),
            compile(&engine, &next, &image).unwrap(),
        );
        let area = engine.delta(&pa, &fin, &pb, &fin).unwrap().outputs[0]
            .1
            .clone();
        let (va, _) = render(&engine, &pa, &image).unwrap();
        let (vb, _) = render(&engine, &pb, &image).unwrap();
        let inferred = view_tiles(&vb, &area);
        let actual = changed_tiles(&va, &vb);
        let PaintAction::Stroke { points, brush, .. } = &s else {
            unreachable!()
        };
        let clip = IRect::new(0, 0, 1024, 768);
        let bounds = kura_engine::paint::stroke::stroke_bounds(&points.0, *brush, clip)
            .map_or(Area::Empty, |b| Area::rect(0, b));
        let brush_tiles = view_tiles(&vb, &bounds);
        assert!(
            inferred <= actual + brush_tiles,
            "{inferred} > {actual} + {brush_tiles}"
        );
    }
}

/// 範囲に重なる配信のタイルの数。
fn view_tiles(v: &Value, area: &Area) -> usize {
    frames_of(v)
        .into_iter()
        .map(|(f, px, o)| match area.frame_rects(f) {
            Some(r) => Tiles::covering(px.width(), px.height(), VIEW_TILE, &r, o).count(),
            None => 0,
        })
        .sum()
}

/// 中身の変わった配信のタイル（フレームごと）。
fn changed_set(a: &Value, b: &Value) -> BTreeMap<u32, BTreeSet<(u32, u32)>> {
    let mut out = BTreeMap::new();
    for ((_, p, _), (f, q, _)) in frames_of(a).into_iter().zip(frames_of(b)) {
        let set: BTreeSet<(u32, u32)> = (0..q.tiles_y())
            .flat_map(|ty| (0..q.tiles_x()).map(move |tx| (tx, ty)))
            .filter(|(tx, ty)| p.tile(*tx, *ty) != q.tile(*tx, *ty))
            .collect();
        if !set.is_empty() {
            out.insert(f, set);
        }
    }
    out
}

fn changed_tiles(a: &Value, b: &Value) -> usize {
    changed_set(a, b).values().map(BTreeSet::len).sum()
}

/// E4・E21・E1：編集の列の 1 手ごとに、前の値と推論した差で継ぎはぎして描いた値が、毎回まっさらに描いたものと
/// タイルごとに同じ。警告は全体の描画の部分列で、前の値がなければ同じ（順も）。
#[test]
fn incremental_equals_fresh() {
    let engine = engine();
    let mut r = determinism::rng(21);
    let (mut steps, mut reused) = (0, 0);
    for case in 0..30 {
        let make = random_input(&mut r);
        let input = make.generate().unwrap();
        if matches!(input, Value::Audio(_)) {
            continue;
        }
        let len = 2 + below(&mut r, 4) as usize;
        let mut g = random_chain(&engine, &mut r, &input.shape(), len, case % 4 == 0);
        let Some(mut plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let (fin, _) = inputs_of(&input);
        let mut cache = NodeCache::new([0, 1 << 20, 1 << 30][case % 3]);
        let all = AreaRequest {
            output: OutputId(0),
            area: Area::All,
        };
        let Ok(first) = engine.render_area(&plan, &fin, &mut cache, &all, None, &NoMonitor) else {
            continue;
        };
        let Some((want, warnings)) = render(&engine, &plan, &input) else {
            continue;
        };
        check_region(&first.value, &want, &Area::All).unwrap();
        assert_eq!(
            first.warnings, warnings,
            "{case}: 前の値なしの全体の範囲の描画の警告"
        );
        let mut prev = first.value;
        for _ in 0..4 {
            let Some((next, kind)) = random_step(&engine, &mut r, &g, &input.shape()) else {
                break;
            };
            let Some(next_plan) = compile(&engine, &next, &input) else {
                break;
            };
            let Some((want, warnings)) = render(&engine, &next_plan, &input) else {
                break;
            };
            let delta = engine.delta(&plan, &fin, &next_plan, &fin).unwrap();
            let base = Base {
                value: &prev,
                changed: &delta.outputs[0].1,
            };
            let got = engine
                .render_area(&next_plan, &fin, &mut cache, &all, Some(&base), &NoMonitor)
                .unwrap();
            check_region(&got.value, &want, &Area::All).unwrap_or_else(|m| {
                panic!(
                    "{case} {kind}: {m}\n前 {}\n後 {}",
                    json_of(&g),
                    json_of(&next)
                )
            });
            assert_eq!((got.stats.fallbacks, got.stats.rebased), (0, 0));
            // 警告は全体の描画の部分列（順も同じ）
            let mut it = warnings.iter();
            assert!(
                got.warnings.iter().all(|w| it.any(|x| x == w)),
                "{case} {kind}: 警告 {:?} が {warnings:?} の部分列でない",
                got.warnings
            );
            if got.value.origin() == Origin::Patched {
                reused += got.stats.reused_tiles;
            }
            steps += 1;
            (g, plan, prev) = (next, next_plan, got.value);
        }
    }
    assert!(steps > 60 && reused > 10, "{steps} {reused}");
    // 決まった鎖（描画 → 反転 → 最近傍の拡大 → 切り抜き）への追記：毎回継ぎはぎし、共有したタイルが多い
    let input = InputGen::Noise {
        w: 700,
        h: 520,
        fmt: PixelFormat::Rgba8,
        seed: 21,
    }
    .generate()
    .unwrap();
    let (fin, _) = inputs_of(&input);
    let doc = |a: &[PaintAction]| {
        let mut g = Graph::new_single();
        g.push_linear("paint.actions", 1, json!({ "actions": a }), 0)
            .unwrap();
        g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
            .unwrap();
        g.push_linear(
            "image.resize",
            1,
            json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
            0,
        )
        .unwrap();
        g.push_linear(
            "image.crop",
            1,
            json!({ "rect": { "x": 100, "y": 50, "w": 1200, "h": 900 } }),
            0,
        )
        .unwrap();
        g
    };
    let mut actions = vec![stroke(0, 50, 50, 4)];
    let mut plan = compile(&engine, &doc(&actions), &input).unwrap();
    let mut cache = NodeCache::new(1 << 30);
    let all = AreaRequest {
        output: OutputId(0),
        area: Area::All,
    };
    let mut prev = engine
        .render_area(&plan, &fin, &mut cache, &all, None, &NoMonitor)
        .unwrap()
        .value;
    for _ in 0..5 {
        actions.push(stroke(
            0,
            below(&mut r, 650) as i64,
            below(&mut r, 500) as i64,
            6,
        ));
        let next = compile(&engine, &doc(&actions), &input).unwrap();
        let (want, _) = render(&engine, &next, &input).unwrap();
        let delta = engine.delta(&plan, &fin, &next, &fin).unwrap();
        let base = Base {
            value: &prev,
            changed: &delta.outputs[0].1,
        };
        let got = engine
            .render_area(&next, &fin, &mut cache, &all, Some(&base), &NoMonitor)
            .unwrap();
        check_region(&got.value, &want, &Area::All).unwrap();
        assert_eq!(got.value.origin(), Origin::Patched);
        // 1200×900 の出力は 5×4 = 20 タイル。筆 1 本で描き直すのは数枚
        assert!(got.stats.reused_tiles >= 12, "{:?}", got.stats);
        assert!(got.value.exact().is_none());
        (plan, prev) = (next, got.value);
    }
}

/// 描いたあとの絞り込み（段 10 の検証）：前の値で無効だったタイルは、今の値と同じ実体（`Arc`）を指していても共有と
/// みなさない（無効にしたタイルは実体を残すので、前回の表示の外で無効だったタイルが、今回同じ実体で有効になることがある）。
#[test]
fn refine_reports_tiles_that_were_holes() {
    let v = InputGen::Noise {
        w: 600,
        h: 300,
        fmt: PixelFormat::Rgba8,
        seed: 3,
    }
    .generate()
    .unwrap();
    let Value::Image(img) = &v else {
        panic!("画像でない")
    };
    let mut hole = Tiles::none(600, 300, kura_engine::value::TILE);
    hole.insert(1, 0);
    let holed = Value::Image(std::sync::Arc::new(kura_engine::value::Image {
        px: img.px.invalidate(&hole),
        meta: img.meta.clone(),
    }));
    assert!(img.px.shares_tile(frames_of(&holed)[0].1, 1, 0));
    // 前回（無効が 1 枚）→ 今回（すべて有効・同じ実体）：無効だったタイルだけを報告する
    let OutputChange::Tiles(map) = refine(&holed, &v, &Area::All) else {
        panic!("タイルの報告でない")
    };
    assert_eq!(map.len(), 1);
    assert_eq!(map[&0].iter().collect::<Vec<_>>(), vec![(1, 0)]);
    // 同じ値どうしは何も報告しない
    assert!(refine(&v, &v, &Area::All).is_empty());
}

/// E31（docs/editing.md §12.6）：render_cached の報告する変わったタイル（手元の値の Arc の同一性で絞ったもの）は、
/// 中身の変わったタイルを含み、推論の範囲の中にある。キャッシュの上限が 0・小さくても同じ（前の出力の値はキャッシュの外で
/// 持つ）。
#[test]
fn changed_tiles_by_identity() {
    let engine = engine();
    let image = InputGen::Noise {
        w: 1024,
        h: 1024,
        fmt: PixelFormat::Rgba8,
        seed: 22,
    }
    .generate()
    .unwrap();
    let (fin, _) = inputs_of(&image);
    let opts = RenderOptions::default();
    for cap in [0u64, 300 << 10, 1 << 30] {
        let mut cache = NodeCache::new(cap);
        let mut r = determinism::rng(cap);
        let mut actions = vec![stroke(0, 100, 100, 5)];
        let doc = |a: &[PaintAction]| {
            let mut g = Graph::new_single();
            g.push_linear("paint.actions", 1, json!({ "actions": a }), 0)
                .unwrap();
            g
        };
        let mut plan = compile(&engine, &doc(&actions), &image).unwrap();
        let first = engine
            .render_cached(&plan, &fin, &mut cache, &opts, &NoMonitor)
            .unwrap();
        assert_eq!(
            first.changed.per_output[0].1,
            OutputChange::All,
            "前回がなければ全体"
        );
        assert!(first.changed.meta_changed);
        let mut before = first.outputs[0].value.clone();
        for _ in 0..6 {
            actions.push(stroke(
                0,
                below(&mut r, 1000) as i64,
                below(&mut r, 1000) as i64,
                3,
            ));
            let next = compile(&engine, &doc(&actions), &image).unwrap();
            let got = engine
                .render_cached(&next, &fin, &mut cache, &opts, &NoMonitor)
                .unwrap();
            let after = got.outputs[0].value.clone();
            let inferred = engine.delta(&plan, &fin, &next, &fin).unwrap().outputs[0]
                .1
                .clone();
            let OutputChange::Tiles(reported) = &got.changed.per_output[0].1 else {
                panic!("{:?}", got.changed.per_output[0].1)
            };
            let actual = changed_set(&before, &after);
            for (f, set) in &actual {
                for (tx, ty) in set {
                    assert!(
                        reported.get(f).is_some_and(|t| t.contains(*tx, *ty)),
                        "上限 {cap}：中身の変わったタイル ({tx}, {ty}) を報告していない"
                    );
                }
            }
            for (f, t) in reported {
                let Some(rects) = inferred.frame_rects(*f) else {
                    panic!("推論にないフレーム {f}")
                };
                let guess = Tiles::covering(
                    1024,
                    1024,
                    VIEW_TILE,
                    &rects,
                    kura_engine::geom::IPoint { x: 0, y: 0 },
                );
                assert!(
                    t.is_subset_of(&guess),
                    "上限 {cap}：推論の外のタイルを報告した"
                );
            }
            // 1024² は 16 タイル。筆 1 本で変わるのは数枚だけ
            let n: usize = reported.values().map(Tiles::count).sum();
            assert!(n <= 4, "上限 {cap}：{n} 枚");
            assert!(!got.changed.meta_changed);
            (plan, before) = (next, after);
        }
        // メタデータだけを変えると、画素は何も変わらず meta_changed
        let mut g = doc(&actions);
        g.push_linear(
            "meta.set_pivot",
            1,
            json!({ "pivot": { "x": 3, "y": 3 } }),
            0,
        )
        .unwrap();
        let p1 = compile(&engine, &g, &image).unwrap();
        engine
            .render_cached(&p1, &fin, &mut cache, &opts, &NoMonitor)
            .unwrap();
        g.nodes[1].params = json!({ "pivot": { "x": 4, "y": 3 } });
        let p2 = compile(&engine, &g, &image).unwrap();
        let got = engine
            .render_cached(&p2, &fin, &mut cache, &opts, &NoMonitor)
            .unwrap();
        assert!(got.changed.meta_changed);
        assert!(
            got.changed.per_output[0].1.is_empty(),
            "{:?}",
            got.changed.per_output[0].1
        );
        // 入力の値が変われば全体を作り直す（同一性で比べても全部が新しい）
        let other = InputGen::Noise {
            w: 1024,
            h: 1024,
            fmt: PixelFormat::Rgba8,
            seed: 23,
        }
        .generate()
        .unwrap();
        let (fo, _) = inputs_of(&other);
        let got = engine
            .render_cached(&p2, &fo, &mut cache, &opts, &NoMonitor)
            .unwrap();
        match &got.changed.per_output[0].1 {
            OutputChange::All => {}
            OutputChange::Tiles(t) => assert_eq!(t[&0].count(), 16),
            other => panic!("{other:?}"),
        }
    }
}

/// 継ぎはぎの値の由来は Patched で、等倍の値として取り出せない（保存・解析に使わない）。前の値の形が違えば継ぎはぎしない。
#[test]
fn patched_values_are_marked() {
    let engine = engine();
    let image = InputGen::Noise {
        w: 600,
        h: 300,
        fmt: PixelFormat::Rgb8,
        seed: 4,
    }
    .generate()
    .unwrap();
    let (fin, _) = inputs_of(&image);
    let doc = |axis: &str| {
        let mut g = Graph::new_single();
        g.push_linear(
            "paint.actions",
            1,
            json!({ "actions": [stroke(0, 10, 10, 3)] }),
            0,
        )
        .unwrap();
        g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
            .unwrap();
        g
    };
    let (pa, pb) = (
        compile(&engine, &doc("horizontal"), &image).unwrap(),
        compile(&engine, &doc("horizontal"), &image).unwrap(),
    );
    let mut cache = NodeCache::new(1 << 30);
    let all = AreaRequest {
        output: OutputId(0),
        area: Area::All,
    };
    let first = engine
        .render_area(&pa, &fin, &mut cache, &all, None, &NoMonitor)
        .unwrap();
    assert_eq!(
        first.value.origin(),
        Origin::Exact,
        "全体を全体で描いた値は Exact"
    );
    let delta = engine.delta(&pa, &fin, &pb, &fin).unwrap();
    let base = Base {
        value: &first.value,
        changed: &delta.outputs[0].1,
    };
    let again = engine
        .render_area(&pb, &fin, &mut cache, &all, Some(&base), &NoMonitor)
        .unwrap();
    // 何も変わっていないので、出力のノードはキャッシュに当たる（継ぎはぎもしない）
    assert_eq!(again.value.origin(), Origin::Exact);
    // 前の値が違う形なら継ぎはぎしない
    let small = PartialValue::full(
        InputGen::Noise {
            w: 10,
            h: 10,
            fmt: PixelFormat::Rgb8,
            seed: 5,
        }
        .generate()
        .unwrap(),
    );
    let base = Base {
        value: &small,
        changed: &Area::Empty,
    };
    let mut cold = NodeCache::new(0);
    let got = engine
        .render_area(&pb, &fin, &mut cold, &all, Some(&base), &NoMonitor)
        .unwrap();
    assert_ne!(got.value.origin(), Origin::Patched);
    // 縦に反転を変える：前の値を使うと Patched になり、exact() は None
    let pc = compile(&engine, &doc("vertical"), &image).unwrap();
    let mut cold = NodeCache::new(0);
    let p0 = engine
        .render_area(&pa, &fin, &mut cold, &all, None, &NoMonitor)
        .unwrap();
    let delta = engine.delta(&pa, &fin, &pc, &fin).unwrap();
    assert!(delta.outputs[0].1.is_all());
    let base = Base {
        value: &p0.value,
        changed: &delta.outputs[0].1,
    };
    let got = engine
        .render_area(&pc, &fin, &mut cold, &all, Some(&base), &NoMonitor)
        .unwrap();
    assert_eq!(got.stats.reused_tiles, 0);
    assert!(got.value.exact().is_none() || got.value.origin() == Origin::Exact);
}
