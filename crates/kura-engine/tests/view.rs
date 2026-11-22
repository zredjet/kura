//! エディタの表示の経路の受け入れテスト（段 10。docs/editing.md §12.8・§17 の E31・E21・E4、ADR-154 (1)(b)・(7)）。
//!
//! - view_equals_render_on_random_edit_sequences：ランダムな文書・編集の列（表示する範囲もときどき動かす）で、表示の経路
//!   （`Engine::render_view`）の値の有効なタイルが `render` の同じタイルとビット単位で同じ。報告する変わったタイルは、前後を
//!   全体で描いた差（中身の変わったタイル）を含む。
//! - view_redraws_only_changed_tiles_after_strokes：筆のあとに最近傍・canvas・convert が続く鎖と、上流に塗りつぶしがある鎖で、
//!   セッションの手（筆・塗りつぶし・取り消し・やり直し・引数・有効の切り替え）ごとに同じことを確かめる。筆では描画のノードが
//!   差分の適用で描かれ、後ろの範囲を持つノードは変わったタイルだけを描く。1・3・16 スレッドで同じ値・同じ報告。
//! - view_at_levels_equals_the_level_render：プレビューの段の計画（段 1・2）でも、値が段の計画の全体の描画と同じで、報告が
//!   差を含む。筆では描画のノードが差分の適用で描かれ、継ぎ目の縮小と後ろのノードは要るタイルだけを描く。段を替えると全体。
//! - view_follows_a_moving_viewport：表示する範囲を動かすと、新しく見えたタイルだけを描いて報告する。
//! - view_keeps_only_the_input_stamps：前回の表示は入力の値を保たず（指紋と元だけ）、入力を差し替えて手放せば残らない。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::doc::{Graph, InputId, NodeId, OutputId};
use kura_engine::geom::{IPoint, IRect};
use kura_engine::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use kura_engine::render::cache::{FingerprintedInput, NodeCache, Origin, RenderOptions};
use kura_engine::render::delta::OutputChange;
use kura_engine::render::region::{Area, Tiles, VIEW_TILE, frames_of, tiles_in};
use kura_engine::render::view::{View, ViewRender, ViewRequest};
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::session::{CommitMode, Edit};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::cases::{fill_item, flat_canvas, stroke_item};
use kura_engine::testing::bench::path::{EditorPath, Step, bulk_render, plain_inputs};
use kura_engine::testing::bench::pen;
use kura_engine::testing::region::{
    below, check_contains, check_region, palette_change_cases, random_area, random_chain,
    random_input, random_step_or_input,
};
use kura_engine::value::{PixelFormat, TILE};
use kura_engine::{Digest, Issue, NoMonitor, Value, determinism};
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

fn inputs_of(value: &Value) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(value.clone(), Digest::default()).unwrap(),
    )])
}

fn compile(engine: &Engine, g: &Graph, input: &Value) -> Option<Plan> {
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .ok()?;
    (plan.cost() <= 1 << 22).then_some(plan)
}

/// 手間の上限なしの計画（決まった文書）。
fn compile_any(engine: &Engine, g: &Graph, input: &Value) -> Plan {
    engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .unwrap()
}

/// 短い筆（2 点・30px。i ごとに位置を変え、左上の [20, 230) に収める：1 枚の画像の 1 タイルに入る）。
fn short_stroke(i: u64, size: u8) -> serde_json::Value {
    let x = 20 + (i * 37 % 180) as i64;
    let y = 20 + (i * 61 % 180) as i64;
    let points = vec![
        IPoint { x, y },
        IPoint {
            x: x + 30,
            y: y + 7,
        },
    ];
    serde_json::to_value(pen(
        points,
        size,
        size == 1,
        [250, (i % 256) as u8, 20, 255],
    ))
    .unwrap()
}

/// キャッシュなしの全体の描画（正）。
fn render(engine: &Engine, plan: &Plan, input: &Value) -> Option<(Value, Vec<Issue>)> {
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source: Digest::default(),
        },
    )]);
    let mut r = engine.render(plan, &plain, &NoMonitor).ok()?;
    Some((r.outputs.swap_remove(0).value, r.warnings))
}

fn json_of(g: &Graph) -> String {
    String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned()
}

/// メタデータ（と、フレームの表示時間・繰り返し）が同じなら true。
fn same_meta(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Image(x), Value::Image(y)) => x.meta == y.meta,
        (Value::Frames(x), Value::Frames(y)) => {
            x.meta == y.meta
                && x.loop_count == y.loop_count
                && x.frames().len() == y.frames().len()
                && x.frames()
                    .iter()
                    .zip(y.frames())
                    .all(|(f, g)| f.duration_ms == g.duration_ms)
        }
        (Value::Audio(x), Value::Audio(y)) => x.meta == y.meta,
        _ => false,
    }
}

/// 表示の経路の報告が、前後を全体で描いた値 `prev`・`next` の差のうち、表示する範囲 `area` に掛かるタイルをすべて含むか。
/// 形・形式・パレット・位置・フレームの数が変われば `All`。メタデータが変われば `meta_changed`。
fn assert_reported(prev: &Value, next: &Value, view: &ViewRender, area: &Area, what: &str) {
    assert_eq!(
        TILE, VIEW_TILE,
        "値のタイルと配信のタイルの大きさが同じ前提"
    );
    if !same_meta(prev, next) {
        assert!(
            view.meta_changed,
            "{what}: メタデータが変わったのに報告しない"
        );
    }
    match &view.changed {
        OutputChange::All => {}
        OutputChange::Samples(s) => {
            check_contains(prev, next, &Area::samples(s.clone()))
                .unwrap_or_else(|m| panic!("{what}: 報告した区間の外が変わった：{m}"));
        }
        OutputChange::Tiles(map) => {
            let (fa, fb) = (frames_of(prev), frames_of(next));
            assert_eq!(
                fa.len(),
                fb.len(),
                "{what}: フレームの数が変わったのに All でない"
            );
            let need = tiles_in(next, area, TILE);
            for ((_, a, ao), (f, b, bo)) in fa.iter().zip(&fb) {
                assert!(
                    ao == bo
                        && a.width() == b.width()
                        && a.height() == b.height()
                        && a.format() == b.format()
                        && a.palette() == b.palette(),
                    "{what}: フレーム {f} の形・位置が変わったのに All でない"
                );
                for ty in 0..b.tiles_y() {
                    for tx in 0..b.tiles_x() {
                        let shown = need.get(f).is_some_and(|t| t.contains(tx, ty));
                        if shown && a.tile(tx, ty) != b.tile(tx, ty) {
                            assert!(
                                map.get(f).is_some_and(|t| t.contains(tx, ty)),
                                "{what}: 中身の変わったタイル（フレーム {f}・{tx},{ty}）を報告していない"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// 報告したタイルの数（All は None）。
fn reported(change: &OutputChange) -> Option<usize> {
    match change {
        OutputChange::Tiles(map) => Some(map.values().map(Tiles::count).sum()),
        _ => None,
    }
}

/// 全体を表示する要求（出力 0）。
fn whole() -> ViewRequest {
    ViewRequest::whole(OutputId(0), None)
}

/// ランダムな文書（画像・パレット・フレームの列・トリムしたフレームの列・シート・音声）とランダムな手の列（1 手、構造の手、
/// 2〜3 手をまとめたもの、入力の値だけの変更）で、表示の経路の値が `render` と同じタイルを持ち、報告が差を含む。表示する範囲は
/// ときどきランダムな矩形にする。キャッシュの上限は 0・1 MiB・1 GiB。継ぎはぎと差分の適用が実際に効いていることも数える。
#[test]
fn view_equals_render_on_random_edit_sequences() {
    let c = view_cases(&engine(), 61, 60);
    assert!(
        c.steps > 100 && c.patched > 10 && c.reused > 20 && c.appended > 0 && c.partial > 0,
        "{c:?}"
    );
    for k in ["multi", "input", "remove", "move", "add", "replace", "seed"] {
        assert!(c.kinds.get(k).copied().unwrap_or(0) > 0, "{k}: {c:?}");
    }
}

/// 表示の経路のランダムな列を、段 10 の検証で報告の抜け（前回は無効だったタイルを、古い実体を共有しているので報告しない。
/// 段 10 の検証の D3）を見つけた種でも回す（数の断言はしない。種ごとにスレッドを分けて回す）。
#[test]
fn view_equals_render_on_found_seeds() {
    // テストの時間を縮めるためだけのスレッド（描画の値には効かない）
    #[allow(clippy::disallowed_methods)]
    std::thread::scope(|sc| {
        let runs: Vec<_> = [1001, 1020]
            .into_iter()
            .map(|seed| sc.spawn(move || view_cases(&engine(), seed, 60)))
            .collect();
        for run in runs {
            if let Err(e) = run.join() {
                std::panic::resume_unwind(e);
            }
        }
    });
}

/// ランダムな列の数（継ぎはぎ・差分の適用・範囲だけの値が実際に起きたかを見る）。
#[derive(Debug, Default)]
struct ViewCounts {
    steps: u64,
    /// 由来が Patched の表示。
    patched: u64,
    /// 継ぎはぎで共有したタイル。
    reused: u64,
    /// 差分の適用で描いたノード。
    appended: u64,
    /// 由来が Partial の表示。
    partial: u64,
    /// 手の種類ごとの数。
    kinds: BTreeMap<&'static str, u64>,
}

/// `view_equals_render_on_random_edit_sequences` の本体：種 `seed` で `cases` 個の文書を作り、手ごとに照らす。
fn view_cases(engine: &Engine, seed: u64, cases: usize) -> ViewCounts {
    let mut r = determinism::rng(seed);
    let mut c = ViewCounts::default();
    for case in 0..cases {
        let make = random_input(&mut r);
        let mut input = make.generate().unwrap();
        let len = 2 + below(&mut r, 4) as usize;
        let mut g = random_chain(engine, &mut r, &input.shape(), len, case % 4 == 0);
        let Some(plan) = compile(engine, &g, &input) else {
            continue;
        };
        let fin = inputs_of(&input);
        let mut cache = NodeCache::new([0, 1 << 20, 1 << 30][case % 3]);
        let Some((mut want, warnings)) = render(engine, &plan, &input) else {
            continue;
        };
        let first = engine
            .render_view(&plan, &fin, &mut cache, &whole(), None, &NoMonitor)
            .unwrap();
        check_region(first.view.value(), &want, &Area::All).unwrap();
        assert_eq!(first.changed, OutputChange::All);
        assert!(first.meta_changed);
        assert_eq!(
            first.warnings, warnings,
            "{case}: 前回なしの全体の表示の警告"
        );
        let mut prev: View = first.view;
        for _ in 0..5 {
            // 1 手・2〜3 手をまとめた手（前回の表示を飛ばす）・入力の値だけの変更
            let Some((next, kind, next_input)) =
                random_step_or_input(engine, &mut r, &g, &make, &input)
            else {
                break;
            };
            let Some(next_plan) = compile(engine, &next, &next_input) else {
                break;
            };
            let Some((next_want, warnings)) = render(engine, &next_plan, &next_input) else {
                break;
            };
            let fin = inputs_of(&next_input);
            // ときどき表示する範囲を動かす（前回の表示の有効なタイルと重なるとは限らない）
            let area = if below(&mut r, 3) == 0 {
                random_area(&mut r, &next_want)
            } else {
                Area::All
            };
            let req = ViewRequest {
                output: OutputId(0),
                area: area.clone(),
                focus: None,
            };
            let got = engine
                .render_view(&next_plan, &fin, &mut cache, &req, Some(&prev), &NoMonitor)
                .unwrap();
            let what = format!(
                "種 {seed}・{case} {kind}\n前 {}\n後 {}",
                json_of(&g),
                json_of(&next)
            );
            check_region(got.view.value(), &next_want, &area)
                .unwrap_or_else(|m| panic!("{what}: {m}"));
            assert_reported(&want, &next_want, &got, &area, &what);
            assert_eq!((got.area.fallbacks, got.area.rebased), (0, 0), "{what}");
            // 警告は全体の描画の部分列（順も同じ）
            let mut it = warnings.iter();
            assert!(
                got.warnings.iter().all(|w| it.any(|x| x == w)),
                "{what}: 警告 {:?} が {warnings:?} の部分列でない",
                got.warnings
            );
            // 表示の値は保存・解析に使えない（全体を描いたときだけ等倍の値）
            match got.view.value().origin() {
                Origin::Patched => {
                    c.patched += 1;
                    c.reused += got.area.reused_tiles;
                    assert!(got.view.value().exact().is_none());
                }
                Origin::Partial => c.partial += 1,
                _ => {}
            }
            c.appended += got.stats.appended;
            c.steps += 1;
            *c.kinds.entry(kind).or_default() += 1;
            (g, want, prev, input) = (next, next_want, got.view, next_input);
        }
    }
    c
}

/// パレットの変化（段 10 の検証の D2）：Indexed8 の上流でパレットだけが変わり（palette.replace の引数・外す・入れる、
/// Indexed8 への convert の引数）、後ろに canvas（広げる）・反転・最近傍の拡大が続く文書でも、表示の経路が描け（前回の表示の
/// パレットが違っても誤りにならない）、値が `render` と同じで、報告が全体。キャッシュの上限は 0 と 1 GiB。
#[test]
fn view_handles_palette_changes_upstream() {
    let engine = engine();
    for (what, a, b, input) in palette_change_cases().unwrap() {
        let fin = inputs_of(&input);
        let (pa, pb) = (
            compile_any(&engine, &a, &input),
            compile_any(&engine, &b, &input),
        );
        let (before, _) = render(&engine, &pa, &input).unwrap();
        let (want, _) = render(&engine, &pb, &input).unwrap();
        for cap in [0, 1 << 30] {
            let mut cache = NodeCache::new(cap);
            let first = engine
                .render_view(&pa, &fin, &mut cache, &whole(), None, &NoMonitor)
                .unwrap();
            let got = engine
                .render_view(
                    &pb,
                    &fin,
                    &mut cache,
                    &whole(),
                    Some(&first.view),
                    &NoMonitor,
                )
                .unwrap_or_else(|e| panic!("{what}（上限 {cap}）: {e}"));
            check_region(got.view.value(), &want, &Area::All)
                .unwrap_or_else(|m| panic!("{what}（上限 {cap}）: {m}"));
            assert_reported(&before, &want, &got, &Area::All, &what);
            assert_eq!(got.changed, OutputChange::All, "{what}（上限 {cap}）");
            assert_eq!(got.inferred, Area::All, "{what}（上限 {cap}）");
            assert_eq!(
                (got.area.fallbacks, got.area.rebased),
                (0, 0),
                "{what}（上限 {cap}）"
            );
        }
    }
}

/// 表示の値から配信に要るものを取る口（段 10 の検証の X4）：継ぎはぎ・範囲だけの表示（`exact()` が None）でも、形式・
/// パレット・メタデータ（格子・ピボット・スライス）・フレームの表示時間と繰り返し・音声のメタデータ（ループ点）を取れ、
/// `render` の値と同じ。
#[test]
fn view_values_expose_what_delivery_needs() {
    let engine = engine();
    let stroke = |i: i64| PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Index(1)),
        blend: Blend::Replace,
        brush: Brush {
            shape: BrushShape::Circle,
            size: 3,
        },
        pixel_perfect: false,
        frame: 0,
        points: PointList(vec![
            IPoint {
                x: 20 + i * 9,
                y: 20,
            },
            IPoint {
                x: 40 + i * 9,
                y: 31,
            },
        ]),
    };
    // Indexed8 の描画 → パレットの差し替え → ピボット → 格子 → 反転（筆を足すと継ぎはぎ。ピボットを変えるとメタデータだけ）
    let indexed = InputGen::Noise {
        w: 600,
        h: 300,
        fmt: PixelFormat::Indexed8,
        seed: 65,
    }
    .generate()
    .unwrap();
    let doc = |strokes: i64, pivot: i64| {
        let mut g = Graph::new_single();
        let actions: Vec<PaintAction> = (0..strokes).map(stroke).collect();
        g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
            .unwrap();
        g.push_linear(
            "palette.replace",
            1,
            json!({ "entries": [{ "index": 1, "color": [250, 10, 20, 255] }] }),
            0,
        )
        .unwrap();
        g.push_linear(
            "meta.set_pivot",
            1,
            json!({ "pivot": { "x": pivot, "y": 2 } }),
            0,
        )
        .unwrap();
        g.push_linear(
            "meta.set_grid",
            1,
            json!({ "grid": { "cellW": 16, "cellH": 8, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 } }),
            0,
        )
        .unwrap();
        g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
            .unwrap();
        g
    };
    let fin = inputs_of(&indexed);
    let mut cache = NodeCache::new(1 << 30);
    let mut prev: Option<View> = None;
    let steps = [
        (1, 3, Area::All),
        (2, 3, Area::All),
        (2, 7, Area::All),
        (3, 7, Area::rect(0, IRect::new(300, 0, 10, 10))),
    ];
    for (i, (strokes, pivot, area)) in steps.into_iter().enumerate() {
        let plan = compile_any(&engine, &doc(strokes, pivot), &indexed);
        let (want, _) = render(&engine, &plan, &indexed).unwrap();
        let req = ViewRequest {
            output: OutputId(0),
            area: area.clone(),
            focus: None,
        };
        let got = engine
            .render_view(&plan, &fin, &mut cache, &req, prev.as_ref(), &NoMonitor)
            .unwrap();
        let v = got.view.value();
        check_region(v, &want, &area).unwrap();
        let Value::Image(img) = &want else {
            panic!("画像でない")
        };
        assert_eq!(v.format(), Some(PixelFormat::Indexed8), "{i}");
        assert_eq!(v.palette(), img.px.palette().map(|p| p.as_ref()), "{i}");
        assert_eq!(v.image_meta(), Some(&img.meta), "{i}");
        assert!(img.meta.pivot.is_some() && img.meta.grid.is_some());
        assert_eq!(v.audio_meta(), None);
        assert_eq!(v.frame_duration_ms(0), None);
        assert_eq!(v.loop_count(), None);
        match i {
            0 => assert_eq!(v.origin(), Origin::Exact),
            1 => assert_eq!(v.origin(), Origin::Patched, "{:?}", got.area),
            2 => assert!(got.meta_changed),
            _ => assert_eq!(v.origin(), Origin::Patched, "{:?}", got.area),
        }
        if i > 0 {
            assert!(v.exact().is_none(), "{i}");
        }
        prev = Some(got.view);
    }
    // フレームの列：表示時間と繰り返し
    let frames = InputGen::NoiseFrames {
        w: 40,
        h: 30,
        fmt: PixelFormat::Rgba8,
        count: 3,
        seed: 66,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    let plan = compile_any(&engine, &g, &frames);
    let (want, _) = render(&engine, &plan, &frames).unwrap();
    let area = Area::rect(1, IRect::new(0, 0, 4, 4));
    let got = engine
        .render_view(
            &plan,
            &inputs_of(&frames),
            &mut NodeCache::new(0),
            &ViewRequest {
                output: OutputId(0),
                area: area.clone(),
                focus: None,
            },
            None,
            &NoMonitor,
        )
        .unwrap();
    let v = got.view.value();
    assert_eq!(v.origin(), Origin::Partial);
    let Value::Frames(fr) = &want else {
        panic!("フレームの列でない")
    };
    for (k, f) in fr.frames().iter().enumerate() {
        assert_eq!(
            v.frame_duration_ms(k as u32),
            Some(f.duration_ms),
            "フレーム {k}"
        );
    }
    assert_eq!(v.frame_duration_ms(3), None);
    assert_eq!(v.loop_count(), Some(fr.loop_count));
    assert_eq!(v.image_meta(), Some(&fr.meta));
    assert_eq!(v.format(), Some(PixelFormat::Rgba8));
    assert_eq!(v.palette(), None);
    // 音声：ループ点
    let audio = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 5_000,
        seed: 67,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear(
        "audio.set_loop",
        1,
        json!({ "loop": { "start": 100, "end": 900 } }),
        0,
    )
    .unwrap();
    let plan = compile_any(&engine, &g, &audio);
    let (want, _) = render(&engine, &plan, &audio).unwrap();
    let got = engine
        .render_view(
            &plan,
            &inputs_of(&audio),
            &mut NodeCache::new(0),
            &whole(),
            None,
            &NoMonitor,
        )
        .unwrap();
    let Value::Audio(a) = &want else {
        panic!("音声でない")
    };
    let v = got.view.value();
    assert_eq!(v.audio_meta(), Some(&a.meta));
    assert!(a.meta.loop_range.is_some());
    assert_eq!(
        (v.format(), v.image_meta(), v.loop_count()),
        (None, None, None)
    );
}

/// 筆の文書の鎖 1 つ（入力・文書・筆を足すノード・その後ろに範囲を持つノードが続くか）。
struct Chain {
    name: &'static str,
    graph: Graph,
    input: Value,
    /// 筆を足すノード（編集中のノード）。
    focus: NodeId,
    /// 後ろの範囲を持つノード（引数の変更と有効の切り替えを当てる。値はその引数の 2 つ目）。
    tail: Option<(NodeId, &'static str, serde_json::Value)>,
    /// 塗りつぶしを足す上流の描画のノード（なければ None）。
    upstream: Option<NodeId>,
}

const SIDE: u32 = 300;

fn chains() -> Vec<Chain> {
    let noise = InputGen::Noise {
        w: SIDE,
        h: SIDE,
        fmt: PixelFormat::Rgba8,
        seed: 62,
    }
    .generate()
    .unwrap();
    let flat = flat_canvas(SIDE).unwrap();
    let mut out = Vec::new();
    // 筆 → 最近傍 ×4（300² → 1200²。25 タイル）
    let mut g = Graph::new_single();
    let paint = g
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    let tail = g
        .push_linear(
            "image.resize",
            1,
            json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
            0,
        )
        .unwrap();
    out.push(Chain {
        name: "筆 → 最近傍 ×4",
        graph: g,
        input: flat.clone(),
        focus: paint,
        tail: Some((tail, "width", json!({ "scale": { "num": 3, "den": 1 } }))),
        upstream: None,
    });
    // 筆 → canvas（四方に 200 広げて色で埋める）→ 最近傍 ×2
    let mut g = Graph::new_single();
    let paint = g
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    let canvas = g
        .push_linear(
            "image.canvas",
            1,
            json!({ "rect": { "x": -200, "y": -200, "w": SIDE + 400, "h": SIDE + 400 }, "fill": [10, 20, 30, 255] }),
            0,
        )
        .unwrap();
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        0,
    )
    .unwrap();
    out.push(Chain {
        name: "筆 → canvas → 最近傍 ×2",
        graph: g,
        input: noise.clone(),
        focus: paint,
        tail: Some((canvas, "fill", json!([200, 100, 50, 255]))),
        upstream: None,
    });
    // 筆 → convert（RGB8。下地の色）
    let mut g = Graph::new_single();
    let paint = g
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    let convert = g
        .push_linear(
            "image.convert",
            1,
            json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
            0,
        )
        .unwrap();
    out.push(Chain {
        name: "筆 → convert",
        graph: g,
        input: noise.clone(),
        focus: paint,
        tail: Some((convert, "matte", json!([0, 0, 0]))),
        upstream: None,
    });
    // 上流に塗りつぶしがある鎖：描画（塗りつぶし）→ 描画（筆）→ 最近傍 ×2 → canvas
    let mut g = Graph::new_single();
    let fill = g
        .push_linear(
            "paint.actions",
            1,
            json!({ "actions": [fill_item(1, 0, false).unwrap()] }),
            0,
        )
        .unwrap();
    let paint = g
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        0,
    )
    .unwrap();
    let canvas = g
        .push_linear(
            "image.canvas",
            1,
            json!({ "rect": { "x": 0, "y": 0, "w": 2 * SIDE + 100, "h": 2 * SIDE }, "fill": null }),
            0,
        )
        .unwrap();
    out.push(Chain {
        name: "塗りつぶし → 筆 → 最近傍 ×2 → canvas",
        graph: g,
        input: noise.clone(),
        focus: paint,
        tail: Some((canvas, "fill", json!([1, 2, 3, 255]))),
        upstream: Some(fill),
    });
    // 同じ描画のノードの先頭に塗りつぶし → 最近傍 ×4
    let mut g = Graph::new_single();
    let paint = g
        .push_linear(
            "paint.actions",
            1,
            json!({ "actions": [fill_item(2, 0, true).unwrap()] }),
            0,
        )
        .unwrap();
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        0,
    )
    .unwrap();
    out.push(Chain {
        name: "塗りつぶしのある描画 → 最近傍 ×4",
        graph: g,
        input: flat,
        focus: paint,
        tail: None,
        upstream: None,
    });
    out
}

fn append(node: NodeId, item: serde_json::Value) -> Step {
    Step::Commit(
        Edit::AppendItems {
            node,
            field: "actions".into(),
            items: vec![item],
        },
        CommitMode::Step,
    )
}

/// 手の種類（数え方を変えるため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stroke,
    Other,
}

fn steps_of(c: &Chain) -> Vec<(Step, Kind, &'static str)> {
    let mut steps = Vec::new();
    for (i, size) in [1u8, 8, 32, 8, 1].into_iter().enumerate() {
        steps.push((
            append(c.focus, short_stroke(100 + i as u64, size)),
            Kind::Stroke,
            "筆",
        ));
    }
    // 長い筆（500 点の歩き。タイルをまたぐ）
    steps.push((
        append(c.focus, stroke_item(150, 8, SIDE).unwrap()),
        Kind::Other,
        "長い筆",
    ));
    steps.push((
        append(c.focus, fill_item(9, 0, false).unwrap()),
        Kind::Other,
        "塗りつぶし",
    ));
    steps.push((
        append(c.focus, short_stroke(200, 8)),
        Kind::Stroke,
        "塗りつぶしのあとの筆",
    ));
    steps.push((Step::Undo, Kind::Other, "取り消し"));
    steps.push((Step::Undo, Kind::Other, "取り消し"));
    steps.push((Step::Redo, Kind::Other, "やり直し"));
    if let Some(up) = c.upstream {
        steps.push((
            append(up, fill_item(5, 0, true).unwrap()),
            Kind::Other,
            "上流の塗りつぶし",
        ));
        steps.push((
            append(c.focus, short_stroke(300, 8)),
            Kind::Stroke,
            "上流を変えたあとの筆",
        ));
    }
    if let Some((tail, key, value)) = &c.tail {
        steps.push((
            Step::Commit(
                Edit::SetParam {
                    node: *tail,
                    key: (*key).into(),
                    value: value.clone(),
                },
                CommitMode::Step,
            ),
            Kind::Other,
            "後ろの引数",
        ));
        steps.push((
            Step::Commit(
                Edit::SetEnabled {
                    node: *tail,
                    enabled: false,
                },
                CommitMode::Step,
            ),
            Kind::Other,
            "後ろを外す",
        ));
        steps.push((
            append(c.focus, short_stroke(400, 8)),
            Kind::Stroke,
            "外したあとの筆",
        ));
        steps.push((
            Step::Commit(
                Edit::SetEnabled {
                    node: *tail,
                    enabled: true,
                },
                CommitMode::Step,
            ),
            Kind::Other,
            "後ろを入れる",
        ));
        steps.push((
            append(c.focus, short_stroke(500, 32)),
            Kind::Stroke,
            "入れたあとの筆",
        ));
        steps.push((Step::Undo, Kind::Other, "取り消し"));
    }
    steps
}

/// 2 つまとめて当てる手（`steps_of` のあと。間の表示を飛ばす）：筆を 2 本、上流の塗りつぶしと筆、後ろを外す・入れると筆、
/// 取り消しを 2 つ。
fn pairs_of(c: &Chain) -> Vec<(&'static str, [Step; 2])> {
    let mut out = vec![(
        "筆を 2 本",
        [
            append(c.focus, short_stroke(600, 8)),
            append(c.focus, short_stroke(601, 32)),
        ],
    )];
    if let Some(up) = c.upstream {
        out.push((
            "上流の塗りつぶしと筆",
            [
                append(up, fill_item(7, 0, false).unwrap()),
                append(c.focus, short_stroke(602, 8)),
            ],
        ));
    }
    if let Some((tail, _, _)) = &c.tail {
        let enabled = |on: bool| {
            Step::Commit(
                Edit::SetEnabled {
                    node: *tail,
                    enabled: on,
                },
                CommitMode::Step,
            )
        };
        out.push((
            "後ろを外すと筆",
            [enabled(false), append(c.focus, short_stroke(603, 8))],
        ));
        out.push((
            "後ろを入れると筆",
            [enabled(true), append(c.focus, short_stroke(604, 1))],
        ));
    }
    out.push(("取り消しを 2 つ", [Step::Undo, Step::Undo]));
    out
}

/// 筆のあとに範囲を持つ操作が続く鎖・上流に塗りつぶしがある鎖で、セッションの手ごとに表示の値が一括の経路（別のエンジン・
/// キャッシュなし）と同じタイルを持ち、報告が差を含む。筆は描画のノードが差分の適用で描かれ、後ろが範囲を持つなら継ぎはぎ
/// （描き直したタイルは一部）。1・3・16 スレッドで報告も同じ。
#[test]
fn view_redraws_only_changed_tiles_after_strokes() {
    for c in chains() {
        let mut reports: Vec<Vec<OutputChange>> = Vec::new();
        for threads in [1usize, 3, 16] {
            let pool = pool(threads);
            let engine = engine();
            let bulk = engine_for_bulk();
            let inputs = BTreeMap::from([(
                InputId(0),
                FingerprintedInput::new(c.input.clone(), Digest([2; 32])).unwrap(),
            )]);
            let plain = plain_inputs(&inputs);
            let mut ed =
                EditorPath::open(&engine, &c.graph, inputs, 1 << 30, Some(c.focus)).unwrap();
            let want_of = |ed: &EditorPath| {
                bulk_render(&bulk, &ed.document().unwrap(), &plain, &NoMonitor)
                    .unwrap()
                    .outputs
                    .swap_remove(0)
                    .value
            };
            let first = pool
                .install(|| ed.render_view(&engine, &NoMonitor))
                .unwrap();
            let mut want = want_of(&ed);
            check_region(first.rendered.view.value(), &want, &Area::All).unwrap();
            let mut report = Vec::new();
            let (mut patched_strokes, mut strokes) = (0, 0);
            for (step, kind, label) in steps_of(&c) {
                let what = format!("{}（{threads} スレッド）{label}", c.name);
                let got = pool
                    .install(|| ed.show(&engine, step, 0))
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                let next = want_of(&ed);
                let view = &got.rendered;
                check_region(view.view.value(), &next, &Area::All)
                    .unwrap_or_else(|m| panic!("{what}: {m}"));
                assert_reported(&want, &next, view, &Area::All, &what);
                assert_eq!((view.area.fallbacks, view.area.rebased), (0, 0), "{what}");
                if kind == Kind::Stroke {
                    strokes += 1;
                    assert_eq!(view.stats.appended, 1, "{what}: 差分の適用で描いていない");
                    // 筆 1 本で変わるのは数枚（全体を作り直していない）
                    let total: u32 = frames_of(&next)
                        .iter()
                        .map(|(_, px, _)| px.tiles_x() * px.tiles_y())
                        .sum();
                    let n = reported(&view.changed).unwrap_or(usize::MAX);
                    if view.view.value().origin() == Origin::Patched {
                        patched_strokes += 1;
                        assert!(view.area.reused_tiles > 0, "{what}: {:?}", view.area);
                        assert!((n as u32) < total, "{what}: {n} / {total} 枚を報告した");
                    }
                }
                report.push(view.changed.clone());
                want = next;
            }
            // 手を 2 つ当ててから描く（前回の表示を飛ばす。1 回の推論で 1〜2 つのノードが変わる）
            for (label, pair) in pairs_of(&c) {
                let what = format!("{}（{threads} スレッド）{label}", c.name);
                for step in pair {
                    ed.apply(&engine, step)
                        .unwrap_or_else(|e| panic!("{what}: {e}"));
                }
                let got = pool
                    .install(|| ed.render_preview(&engine, 0))
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                let next = want_of(&ed);
                let view = &got.rendered;
                check_region(view.view.value(), &next, &Area::All)
                    .unwrap_or_else(|m| panic!("{what}: {m}"));
                assert_reported(&want, &next, view, &Area::All, &what);
                assert_eq!((view.area.fallbacks, view.area.rebased), (0, 0), "{what}");
                report.push(view.changed.clone());
                want = next;
            }
            // 後ろに範囲を持つノードがある鎖は、筆のたびに継ぎはぎする（外した間は描画が出力）
            let expect = strokes - usize::from(c.tail.is_some());
            assert!(
                patched_strokes >= expect,
                "{}: 継ぎはぎした筆 {patched_strokes} / {strokes}",
                c.name
            );
            reports.push(report);
        }
        assert!(
            reports.windows(2).all(|w| w[0] == w[1]),
            "{}: スレッドの数で報告が違う",
            c.name
        );
    }
}

fn engine_for_bulk() -> Engine {
    Engine::new().unwrap()
}

/// プレビューの段の計画（段 1・2）でも、表示の経路の値が段の計画の全体の描画（別のエンジン・キャッシュなし）と同じで、
/// 報告が差を含む。筆では描画のノードが差分の適用で描かれ、継ぎ目の縮小と後ろのノードは要るタイルだけを描いて継ぎはぎする。
/// 段を替えると全体。
#[test]
fn view_at_levels_equals_the_level_render() {
    let photo = InputGen::Photo {
        w: 900,
        h: 640,
        seed: 63,
    }
    .generate()
    .unwrap();
    // 写真の鎖（切り抜き → Lanczos3 50% → グレー）の切り抜きを動かす
    let mut g = Graph::new_single();
    let crop = g
        .push_linear(
            "image.crop",
            1,
            json!({ "rect": { "x": 0, "y": 0, "w": 860, "h": 600 } }),
            0,
        )
        .unwrap();
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 1, "den": 2 } }, "height": "auto", "filter": "lanczos3" }),
        0,
    )
    .unwrap();
    g.push_linear(
        "image.convert",
        1,
        json!({ "format": "gray8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
        0,
    )
    .unwrap();
    let rect = |i: u32| {
        Step::Preview(Edit::SetParam {
            node: crop,
            key: "rect".into(),
            value: json!({ "x": i * 3, "y": i, "w": 860 - i * 5, "h": 600 }),
        })
    };
    // (手, 段, 短い筆か)
    let photo_steps: Vec<(Step, u8, bool)> = vec![
        (rect(1), 2, false),
        (rect(2), 2, false),
        (Step::CommitPreview(CommitMode::Step), 2, false),
        (rect(3), 1, false),
        (rect(4), 1, false),
        (rect(5), 2, false),
        (Step::Undo, 2, false),
    ];
    // 描画（等倍でしか描けない）→ 最近傍 ×2 → convert（RGB8）：継ぎ目は描画の出力。1600×1200 → 3200×2400（段 2 で
    // 800×600 の 4×3 タイル）
    let big = InputGen::Photo {
        w: 1600,
        h: 1200,
        seed: 64,
    }
    .generate()
    .unwrap();
    let mut g2 = Graph::new_single();
    let paint = g2
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    g2.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        0,
    )
    .unwrap();
    g2.push_linear(
        "image.convert",
        1,
        json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
        0,
    )
    .unwrap();
    let mut paint_steps: Vec<(Step, u8, bool)> = (0..4)
        .map(|i| (append(paint, short_stroke(600 + i, 8)), 2, true))
        .collect();
    paint_steps.push((append(paint, stroke_item(650, 8, 1600).unwrap()), 2, false));
    paint_steps.push((Step::Undo, 2, false));
    paint_steps.push((append(paint, short_stroke(700, 32)), 1, true));
    paint_steps.push((append(paint, short_stroke(701, 8)), 1, true));
    for (graph, input, focus, steps) in [
        (&g, &photo, crop, photo_steps),
        (&g2, &big, paint, paint_steps),
    ] {
        let engine = engine();
        let bulk = engine_for_bulk();
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input.clone(), Digest([3; 32])).unwrap(),
        )]);
        let shapes = BTreeMap::from([(InputId(0), input.shape())]);
        let level_want = |ed: &EditorPath, level: u8| {
            let g = bulk.load(&ed.document().unwrap()).unwrap();
            let plan = bulk.compile(&g, &shapes).unwrap();
            let lp = bulk.plan_level(&plan, level).unwrap();
            assert_eq!(lp.plan.level(), level, "段 {level} の計画にできない");
            let mut cold = NodeCache::new(0);
            bulk.render_cached(
                &lp.plan,
                &inputs,
                &mut cold,
                &RenderOptions::default(),
                &NoMonitor,
            )
            .unwrap()
            .outputs
            .swap_remove(0)
            .value
        };
        let mut ed =
            EditorPath::open(&engine, graph, inputs.clone(), 1 << 30, Some(focus)).unwrap();
        let mut level = 2;
        let first = ed.render_preview(&engine, level).unwrap();
        assert_eq!(first.level, level);
        let mut want = level_want(&ed, level);
        check_region(first.rendered.view.value(), &want, &Area::All).unwrap();
        assert_eq!(first.rendered.view.value().origin(), Origin::Scaled);
        for (i, (step, next_level, is_stroke)) in steps.into_iter().enumerate() {
            let switched = next_level != level;
            let got = ed.show(&engine, step, next_level).unwrap();
            let what = format!("{} 手目（段 {level} → {next_level}）", i + 1);
            assert_eq!(got.level, next_level, "{what}");
            let next = level_want(&ed, next_level);
            let view = &got.rendered;
            check_region(view.view.value(), &next, &Area::All)
                .unwrap_or_else(|m| panic!("{what}: {m}"));
            assert_eq!(view.view.value().origin(), Origin::Scaled, "{what}");
            assert!(view.view.value().exact().is_none(), "{what}");
            if switched {
                assert_eq!(
                    view.changed,
                    OutputChange::All,
                    "{what}: 段を替えたのに全体でない"
                );
            } else {
                assert_reported(&want, &next, view, &Area::All, &what);
            }
            if is_stroke && !switched {
                assert_eq!(view.stats.appended, 1, "{what}: 差分の適用で描いていない");
                assert!(view.area.reused_tiles > 0, "{what}: {:?}", view.area);
                assert!(view.area.region_nodes >= 3, "{what}: {:?}", view.area);
            }
            (want, level) = (next, next_level);
        }
    }
}

/// 表示する範囲を動かすと、新しく見えたタイルだけを描いて報告する。筆は範囲の中の変わったタイルだけを描き直す。
#[test]
fn view_follows_a_moving_viewport() {
    let engine = engine();
    let input = flat_canvas(1024).unwrap();
    let mut actions = vec![stroke_item(1, 8, 1024).unwrap()];
    let doc = |a: &[serde_json::Value]| {
        let mut g = Graph::new_single();
        g.push_linear("paint.actions", 1, json!({ "actions": a }), 0)
            .unwrap();
        g.push_linear(
            "image.resize",
            1,
            json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
            0,
        )
        .unwrap();
        g
    };
    let fin = inputs_of(&input);
    let mut cache = NodeCache::new(1 << 30);
    let view_at = |x: i64, y: i64| ViewRequest {
        output: OutputId(0),
        area: Area::rect(0, IRect::new(x, y, 512, 512)),
        focus: None,
    };
    // 2048² の出力（8×8 タイル）の左上 2×2 タイル
    let plan = compile_any(&engine, &doc(&actions), &input);
    let (mut want, _) = render(&engine, &plan, &input).unwrap();
    let first = engine
        .render_view(&plan, &fin, &mut cache, &view_at(0, 0), None, &NoMonitor)
        .unwrap();
    check_region(first.view.value(), &want, &view_at(0, 0).area).unwrap();
    assert_eq!(first.view.value().origin(), Origin::Partial);
    assert_eq!(first.view.value().valid(0).unwrap().count(), 4);
    // 動かすだけ：変わりうる範囲は空。新しく見えたタイル（3×3 − 4 = 5 枚）だけを描いて報告する
    let moved = engine
        .render_view(
            &plan,
            &fin,
            &mut cache,
            &view_at(128, 128),
            Some(&first.view),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(moved.inferred, Area::Empty);
    check_region(moved.view.value(), &want, &view_at(128, 128).area).unwrap();
    assert_eq!(moved.area.reused_tiles, 4, "{:?}", moved.area);
    assert_eq!(reported(&moved.changed), Some(5), "{:?}", moved.changed);
    assert!(!moved.meta_changed);
    // 筆を足す：範囲の中の差を報告する
    let mut prev = moved.view;
    let mut r = determinism::rng(64);
    for i in 0..4 {
        actions.push(stroke_item(10 + i, 8, 1024).unwrap());
        let next = compile_any(&engine, &doc(&actions), &input);
        let (next_want, _) = render(&engine, &next, &input).unwrap();
        let x = 256 * below(&mut r, 3) as i64;
        let req = view_at(x, 256);
        let got = engine
            .render_view(&next, &fin, &mut cache, &req, Some(&prev), &NoMonitor)
            .unwrap();
        check_region(got.view.value(), &next_want, &req.area).unwrap();
        assert_reported(&want, &next_want, &got, &req.area, &format!("筆 {i}"));
        assert_eq!(got.stats.appended, 1);
        (want, prev) = (next_want, got.view);
    }
    // 別の出力の前回は使わない（全体）
    let other = ViewRequest {
        output: OutputId(1),
        area: Area::All,
        focus: None,
    };
    let mut g = doc(&actions);
    let mut out1 = g.outputs[0].clone();
    out1.id = OutputId(1);
    g.outputs.push(out1);
    let p2 = compile_any(&engine, &g, &input);
    let got = engine
        .render_view(&p2, &fin, &mut cache, &other, Some(&prev), &NoMonitor)
        .unwrap();
    assert_eq!(got.changed, OutputChange::All);
    check_region(got.view.value(), &want, &Area::All).unwrap();
}

/// 前回の表示が持つもの（段 10 の確認。§12.8）：前回の表示（`View`）は入力の値を保たず、入力の指紋と元だけを持つ
/// （`render_cached` の前回と同じ）。入力を差し替えて手放せば、古い入力の値は前回の表示に残らない。同じ入力なら何も
/// 変わらず、差し替えれば値は `render` と同じで、報告は前後の差を含む。
#[test]
fn view_keeps_only_the_input_stamps() {
    let engine = engine();
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    let noise = |seed: u64| {
        InputGen::Noise {
            w: 300,
            h: 200,
            fmt: PixelFormat::Rgba8,
            seed,
        }
        .generate()
        .unwrap()
    };
    let old = noise(3);
    let Value::Image(img) = &old else {
        panic!("1 枚の画像")
    };
    let weak = std::sync::Arc::downgrade(img);
    let plan = compile_any(&engine, &g, &old);
    let mut cache = NodeCache::new(0);
    let first = engine
        .render_view(
            &plan,
            &inputs_of(&old),
            &mut cache,
            &whole(),
            None,
            &NoMonitor,
        )
        .unwrap();
    let same = engine
        .render_view(
            &plan,
            &inputs_of(&old),
            &mut cache,
            &whole(),
            Some(&first.view),
            &NoMonitor,
        )
        .unwrap();
    assert!(same.changed.is_empty(), "{:?}", same.changed);
    let (before, _) = render(&engine, &plan, &old).unwrap();
    // 入力を差し替えて古い値を手放すと、前回の表示（first・same）には残らない
    let new = noise(4);
    drop(old);
    assert!(
        weak.upgrade().is_none(),
        "前回の表示が古い入力の値を保っている"
    );
    let next = engine
        .render_view(
            &plan,
            &inputs_of(&new),
            &mut cache,
            &whole(),
            Some(&same.view),
            &NoMonitor,
        )
        .unwrap();
    let (want, _) = render(&engine, &plan, &new).unwrap();
    check_region(next.view.value(), &want, &Area::All).unwrap();
    assert_reported(&before, &want, &next, &Area::All, "入力の差し替え");
    drop((first, same));
}
