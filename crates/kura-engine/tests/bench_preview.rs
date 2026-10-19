//! 段 10 のプレビューの段のベンチ（NFR-11。docs/editing.md §16）。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_preview -- --ignored --nocapture --test-threads 1`
//!
//! 判定の条件（ADR-154 (1)(c)）：プレビューのプールを 3 スレッドにして測り、4・16 スレッドも記録する。「エンジンの中」は
//! 編集（文書）を受けてから、プレビューの値と変わったタイルの一覧ができるまで：compile → plan_level → render_cached
//! （ダイジェストなし。結果の `changed` が変わったタイル）。毎回、前の文書でキャッシュを温めてから（測らない）、引数を 1 つ
//! 変えた文書を描く（変えたノードから後ろを描く）。時間は表に出すだけで、assert するのは正しさだけ（E30）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::collections::BTreeMap;
use std::time::Instant;

use kura_engine::doc::{Graph, InputId};
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::preview::downscale;
use kura_engine::render::{Engine, InputValue};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::{
    PHOTO_H, PHOTO_W, Stats, WARMUP, measure, pen, photo, walk_stroke,
};
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, NoMonitor, Value};
use serde_json::json;

fn clock() -> impl Fn() -> u64 {
    let start = Instant::now();
    move || start.elapsed().as_nanos() as u64
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn row(label: &str, s: &Stats) {
    eprintln!(
        "| {label} | {:.2} | {} | {:.2} | {} |",
        ms(s.p50),
        s.p95.map_or("-".to_string(), |v| format!("{:.2}", ms(v))),
        ms(s.max),
        s.n
    );
}

fn pool(n: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .build()
        .unwrap()
}

/// 測る回数（慣らしのあと）。
const RUNS: usize = 30;

fn chain(ops: &[(&str, serde_json::Value)]) -> Graph {
    let mut g = Graph::new_single();
    for (i, (op, p)) in ops.iter().enumerate() {
        g.push_linear(op, 1, p.clone(), i as u64 + 1).unwrap();
    }
    g
}

/// 1 回分：前の文書 `before` でキャッシュを温め（測らない）、`after` を段 `level` で描く時間（compile → plan_level →
/// render_cached）。返すのは時間と出力の値。`min_hits` は当たるはずのノードの数の下限（上流が温まっていることの確かめ）。
fn once(
    engine: &Engine,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
    before: &Graph,
    after: &Graph,
    level: u8,
    min_hits: u64,
    clock: &dyn Fn() -> u64,
) -> (u64, Value) {
    let shapes: BTreeMap<InputId, _> = inputs.iter().map(|(k, v)| (*k, v.value.shape())).collect();
    let opts = RenderOptions::default();
    let mut cache = NodeCache::new(4 << 30);
    let plan = engine.compile(before, &shapes).unwrap();
    let lp = engine.plan_level(&plan, level).unwrap();
    assert_eq!(lp.plan.level(), level, "{:?}", lp.fallback);
    engine
        .render_cached(&lp.plan, inputs, &mut cache, &opts, &NoMonitor)
        .unwrap();
    let start = clock();
    let plan = engine.compile(after, &shapes).unwrap();
    let lp = engine.plan_level(&plan, level).unwrap();
    let r = engine
        .render_cached(&lp.plan, inputs, &mut cache, &opts, &NoMonitor)
        .unwrap();
    let end = clock();
    assert!(r.stats.hits >= min_hits, "上流が当たっていません");
    assert!(r.outputs[0].digest.is_none());
    (end - start, r.outputs[0].value.clone())
}

/// NFR-11 の写真の経路：24MP（testgen の Photo）の鎖（切り抜き → 90° 回転 → Lanczos → キャンバス → 形式の変換）で、
/// 段 2（1/4）と段 1（1/2）で、1 つの操作の引数を変えて描き直す時間（3・4・16 スレッド）。
#[test]
#[ignore]
fn bench_photo_param_change() {
    let engine = Engine::new().unwrap();
    let input = photo(1).generate().unwrap();
    let inputs = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest::default()).unwrap(),
    )]);
    let crop = |x: i64| {
        (
            "image.crop",
            json!({"rect": {"x": x, "y": 80, "w": 5600, "h": 3800}}),
        )
    };
    let rot = |t: &str| ("image.rotate90", json!({ "turn": t }));
    let resize = |w: u32| {
        (
            "image.resize",
            json!({"width": {"px": w}, "height": "auto", "filter": "lanczos3"}),
        )
    };
    let canvas = |x: i64| {
        (
            "image.canvas",
            json!({"rect": {"x": x, "y": -50, "w": 3100, "h": 4521}, "fill": [255, 255, 255, 255]}),
        )
    };
    let convert = |f: &str| {
        (
            "image.convert",
            json!({"format": f, "colors": 256, "dither": "none", "matte": [255, 255, 255]}),
        )
    };
    let doc = |i: usize, alt: bool| {
        chain(&[
            crop(if i == 0 && alt { 104 } else { 100 }),
            rot(if i == 1 && alt { "ccw" } else { "cw" }),
            resize(if i == 2 && alt { 2990 } else { 3000 }),
            canvas(if i == 3 && alt { -46 } else { -50 }),
            convert(if i == 4 && alt { "rgba8" } else { "gray8" }),
        ])
    };
    let names = ["crop", "rotate90", "resize(lanczos)", "canvas", "convert"];
    eprintln!("load average: {}", load());
    eprintln!("| 段・操作・スレッド | p50 ms | p95 ms | 最大 ms | 回数 |");
    eprintln!("|---|---|---|---|---|");
    for level in [2u8, 1] {
        for threads in [3, 4, 16] {
            let p = pool(threads);
            for (i, name) in names.iter().enumerate() {
                let clock = clock();
                let mut samples = Vec::with_capacity(RUNS);
                let mut last: Option<Value> = None;
                for run in 0..WARMUP + RUNS {
                    let alt = run % 2 == 1;
                    let (ns, v) = p.install(|| {
                        once(
                            &engine,
                            &inputs,
                            &doc(i, !alt),
                            &doc(i, alt),
                            level,
                            i as u64 + 1,
                            &clock,
                        )
                    });
                    if run >= WARMUP {
                        samples.push(ns);
                    }
                    // 同じ文書は同じ値（決定的）
                    if run >= 2 && run % 2 == 0 {
                        assert_eq!(
                            last.as_ref().map(|l| l.digest().unwrap()),
                            Some(v.digest().unwrap())
                        );
                    }
                    if run % 2 == 0 {
                        last = Some(v);
                    }
                }
                let s = Stats::from_samples(samples).unwrap();
                row(&format!("段 {level}・{name}・{threads}"), &s);
            }
        }
    }
}

/// 引数の 2 つの値（偽・真）の文書を作る。
type DocFn = Box<dyn Fn(bool) -> Graph>;

/// 2048² のドット絵の段 0（等倍）：palette.replace・convert（Indexed8 へ）・canvas・最近傍の ×2・trim・split の引数を変えて
/// 描き直す時間（3・4・16 スレッド）。
#[test]
#[ignore]
fn bench_pixel_art_level0() {
    let engine = Engine::new().unwrap();
    let rgba = InputGen::PixelSprite {
        w: 2048,
        h: 2048,
        fmt: PixelFormat::Rgba8,
        seed: 7,
    }
    .generate()
    .unwrap();
    let indexed = InputGen::PixelSprite {
        w: 2048,
        h: 2048,
        fmt: PixelFormat::Indexed8,
        seed: 7,
    }
    .generate()
    .unwrap();
    let grid =
        |c: u32| json!({"cellW": c, "cellH": c, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0});
    let cases: Vec<(&str, &Value, DocFn)> = vec![
        (
            "palette.replace",
            &indexed,
            Box::new(|alt| {
                let c = if alt {
                    [10, 200, 30, 255]
                } else {
                    [200, 10, 30, 255]
                };
                chain(&[(
                    "palette.replace",
                    json!({"entries": [{"index": 1, "color": c}, {"index": 2, "color": [0, 0, 0, 0]}]}),
                )])
            }),
        ),
        (
            "convert(indexed8)",
            &rgba,
            Box::new(|alt| {
                chain(&[(
                    "image.convert",
                    json!({"format": "indexed8", "colors": if alt { 15 } else { 16 }, "dither": "none", "matte": [255, 255, 255]}),
                )])
            }),
        ),
        (
            "canvas",
            &rgba,
            Box::new(|alt| {
                chain(&[(
                    "image.canvas",
                    json!({"rect": {"x": if alt { 4 } else { 0 }, "y": 0, "w": 2100, "h": 2100}, "fill": null}),
                )])
            }),
        ),
        (
            "resize(nearest ×2)",
            &rgba,
            Box::new(|alt| {
                chain(&[(
                    "image.resize",
                    json!({"width": {"px": if alt { 4092 } else { 4096 }}, "height": "auto", "filter": "nearest"}),
                )])
            }),
        ),
        (
            "trim_alpha",
            &rgba,
            Box::new(|alt| {
                chain(&[(
                    "image.trim_alpha",
                    json!({"threshold": if alt { 1 } else { 0 }, "mode": "union"}),
                )])
            }),
        ),
        (
            "split_grid",
            &rgba,
            Box::new(move |alt| {
                chain(&[(
                    "sprite.split_grid",
                    json!({"grid": grid(if alt { 128 } else { 64 }), "durationMs": 100, "skipEmpty": false}),
                )])
            }),
        ),
    ];
    eprintln!("load average: {}", load());
    eprintln!("| 操作・スレッド（段 0・2048²） | p50 ms | p95 ms | 最大 ms | 回数 |");
    eprintln!("|---|---|---|---|---|");
    for threads in [3, 4, 16] {
        let p = pool(threads);
        for (name, input, doc) in &cases {
            let inputs = BTreeMap::from([(
                InputId(0),
                FingerprintedInput::new((*input).clone(), Digest::default()).unwrap(),
            )]);
            let clock = clock();
            let mut samples = Vec::with_capacity(RUNS);
            for run in 0..WARMUP + RUNS {
                let alt = run % 2 == 1;
                let (before, after) = (doc(!alt), doc(alt));
                let (ns, _) = p.install(|| once(&engine, &inputs, &before, &after, 0, 0, &clock));
                if run >= WARMUP {
                    samples.push(ns);
                }
            }
            let s = Stats::from_samples(samples).unwrap();
            row(&format!("{name}・{threads}"), &s);
        }
    }
    // 正しさ：段 0 のキャッシュの道は、キャッシュなしの道と同じ値
    for (_, input, doc) in &cases {
        let g = doc(false);
        let shapes = BTreeMap::from([(InputId(0), input.shape())]);
        let plan = engine.compile(&g, &shapes).unwrap();
        let want = engine
            .render(
                &plan,
                &BTreeMap::from([(
                    InputId(0),
                    InputValue {
                        value: (*input).clone(),
                        source: Digest::default(),
                    },
                )]),
                &NoMonitor,
            )
            .unwrap();
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new((*input).clone(), Digest::default()).unwrap(),
        )]);
        let mut cache = NodeCache::new(1 << 30);
        let got = engine
            .render_cached(
                &plan,
                &inputs,
                &mut cache,
                &RenderOptions::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(
            got.outputs[0].value.digest().unwrap(),
            want.outputs[0].digest
        );
    }
}

/// 継ぎ目の縮小：24MP を段 1〜3 に縮める時間（全体）と、24MP に筆を描いてから段 2 で描く鎖（描画 → 反転）で筆 1 本（500 点・
/// 8px）を足したときの時間（描画は差分の適用、継ぎ目の縮小は変わったタイルだけ。`CacheStats::rescaled`）。3・4・16 スレッド。
#[test]
#[ignore]
fn bench_seam_downscale_and_stroke() {
    let engine = Engine::new().unwrap();
    let input = photo(1).generate().unwrap();
    let Value::Image(img) = &input else {
        panic!("画像ではありません");
    };
    let inputs = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest::default()).unwrap(),
    )]);
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    eprintln!("load average: {}", load());
    eprintln!("| 場合・スレッド | p50 ms | p95 ms | 最大 ms | 回数 |");
    eprintln!("|---|---|---|---|---|");
    for threads in [3, 4, 16] {
        let p = pool(threads);
        for level in 1..=3u8 {
            let clock = clock();
            let s = p.install(|| {
                measure(&clock, WARMUP, RUNS, |_| {
                    downscale(&img.px, level, &NoMonitor, None)
                })
                .unwrap()
            });
            row(&format!("24MP を段 {level} に縮める・{threads}"), &s);
        }
        // 筆を足していく（毎回 1 本）。キャッシュは使い続ける（エディタと同じ）
        let mut actions = Vec::new();
        let mut cache = NodeCache::new(4 << 30);
        let mut samples = Vec::with_capacity(RUNS);
        let clock = clock();
        let mut rescaled = 0;
        for run in 0..WARMUP + RUNS {
            let pts = walk_stroke(run as u64, 500, PHOTO_W, PHOTO_H, 8);
            actions.push(pen(pts, 8, false, [250, 20, 20, 255]));
            let g = chain(&[
                ("paint.actions", json!({ "actions": actions })),
                ("image.flip", json!({"axis": "horizontal"})),
            ]);
            let (ns, r) = p.install(|| {
                let start = clock();
                let plan = engine.compile(&g, &shapes).unwrap();
                let lp = engine.plan_level(&plan, 2).unwrap();
                let r = engine
                    .render_cached(
                        &lp.plan,
                        &inputs,
                        &mut cache,
                        &RenderOptions::default(),
                        &NoMonitor,
                    )
                    .unwrap();
                (clock() - start, r)
            });
            if run >= WARMUP {
                samples.push(ns);
                rescaled += r.stats.rescaled;
            }
        }
        assert_eq!(
            rescaled, RUNS as u64,
            "継ぎ目の縮小が変わったタイルだけになっていません"
        );
        let s = Stats::from_samples(samples).unwrap();
        row(
            &format!("24MP の描画に筆 1 本（段 2・compile 込み）・{threads}"),
            &s,
        );
    }
}

/// 負荷の目安（`sysctl -n vm.loadavg`。取れなければ空）。
fn load() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
