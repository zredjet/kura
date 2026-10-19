//! 段 10 の範囲の描画と変わった範囲のベンチ（docs/editing.md §16）。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_region -- --ignored --nocapture`
//!
//! 判定の条件（ADR-154 (1)(c)）：プレビューのプールを 3 スレッドにして測り、4・16 スレッドも記録する。時間は表に出すだけで、
//! assert するのは正しさだけ（E30）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::collections::BTreeMap;
use std::time::Instant;

use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::geom::IRect;
use kura_engine::paint::PaintAction;
use kura_engine::render::area::AreaRequest;
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::delta::{OutputChange, refine};
use kura_engine::render::region::Area;
use kura_engine::render::{Engine, InputValue};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::{Stats, measure, pen, photo, walk_stroke};
use kura_engine::testing::region::check_region;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, NoMonitor};
use serde_json::json;

fn clock() -> impl Fn() -> u64 {
    let start = Instant::now();
    move || start.elapsed().as_nanos() as u64
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn show(label: &str, s: &Stats) {
    eprintln!(
        "{label}: p50 {:.3} ms / p95 {} / 最大 {:.3} ms（{} 回）",
        ms(s.p50),
        s.p95
            .map_or("-".to_string(), |v| format!("{:.3} ms", ms(v))),
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

/// 4096² の描画の文書で、筆 1 本を足したあとに変わったタイルの数と、判定（変わった範囲の推論 `Engine::delta` と、
/// タイルの Arc の同一性での絞り込み `refine`）の時間。`render_cached` 全体（ダイジェストなし）も測る。
#[test]
#[ignore]
fn bench_changed_tiles_after_a_stroke() {
    let engine = Engine::new().unwrap();
    let input = InputGen::Noise {
        w: 4096,
        h: 4096,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let fin = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest::default()).unwrap(),
    )]);
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    let doc = |a: &[PaintAction]| {
        let mut g = Graph::new_single();
        g.push_linear("paint.actions", 1, json!({ "actions": a }), 0)
            .unwrap();
        g
    };
    for threads in [3, 4, 16] {
        let pool = pool(threads);
        pool.install(|| {
            let mut actions: Vec<PaintAction> = (0..20)
                .map(|i| {
                    pen(
                        walk_stroke(i, 500, 4096, 4096, 3),
                        8,
                        false,
                        [200, 10, 10, 255],
                    )
                })
                .collect();
            let mut cache = NodeCache::new(4 << 30);
            let opts = RenderOptions::default();
            let mut plan = engine.compile(&doc(&actions), &shapes).unwrap();
            let mut prev = engine
                .render_cached(&plan, &fin, &mut cache, &opts, &NoMonitor)
                .unwrap()
                .outputs[0]
                .value
                .clone();
            let c = clock();
            let mut tiles = Vec::new();
            let mut judge = Vec::new();
            let render = measure(&c, 3, 20, |i| {
                actions.push(pen(
                    walk_stroke(100 + i as u64, 500, 4096, 4096, 3),
                    8,
                    false,
                    [10, 200, 10, 255],
                ));
                let next = engine.compile(&doc(&actions), &shapes)?;
                let got = engine.render_cached(&next, &fin, &mut cache, &opts, &NoMonitor)?;
                // 判定だけを測り直す（推論 ＋ 同一性の絞り込み）
                let t0 = c();
                let delta = engine.delta(&plan, &fin, &next, &fin)?;
                let change = refine(&prev, &got.outputs[0].value, &delta.outputs[0].1);
                judge.push(c() - t0);
                assert_eq!(change, got.changed.per_output[0].1);
                if let OutputChange::Tiles(t) = &change {
                    tiles.push(t.values().map(|t| t.count()).sum::<usize>());
                }
                prev = got.outputs[0].value.clone();
                plan = next;
                Ok(())
            })
            .unwrap();
            show(
                &format!("{threads} スレッド：render_cached（筆 1 本の追記、ダイジェストなし）"),
                &render,
            );
            let judge = Stats::from_samples(judge.split_off(3)).unwrap();
            show(
                &format!("{threads} スレッド：判定（delta ＋ refine）"),
                &judge,
            );
            eprintln!(
                "{threads} スレッド：変わったタイル（256 タイルのうち） {:?}",
                &tiles[3..]
            );
        });
    }
}

/// 24MP の写真に局所の操作の鎖（切り抜き → 反転 → 回転 → 平滑な縮小 → 形式の変換）を当て、1 タイル（256²）と画面 1 枚分
/// （1920×1080）の範囲の描画の時間（キャッシュは空。入力だけを持つ）。全体の描画とも比べる。
#[test]
#[ignore]
fn bench_region_on_a_photo() {
    let engine = Engine::new().unwrap();
    let input = photo(7).generate().unwrap();
    let mut g = Graph::new_single();
    g.push_linear(
        "image.crop",
        1,
        json!({ "rect": { "x": 40, "y": 30, "w": 5900, "h": 3900 } }),
        0,
    )
    .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    g.push_linear("image.rotate90", 1, json!({ "turn": "half" }), 0)
        .unwrap();
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 3, "den": 4 } }, "height": "auto", "filter": "bilinear" }),
        0,
    )
    .unwrap();
    g.push_linear(
        "image.convert",
        1,
        json!({ "format": "rgba8", "colors": 256, "dither": "none", "matte": [0, 0, 0] }),
        0,
    )
    .unwrap();
    let plan = engine
        .compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))
        .unwrap();
    let fin = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest::default()).unwrap(),
    )]);
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source: Digest::default(),
        },
    )]);
    let full = engine.render(&plan, &plain, &NoMonitor).unwrap();
    let out = full.outputs[0].value.clone();
    eprintln!("出力 {:?}", out.shape());
    let tile = Area::rect(0, IRect::new(2048, 1280, 256, 256));
    let screen = Area::rect(0, IRect::new(1000, 600, 1920, 1080));
    for threads in [3, 4, 16] {
        let pool = pool(threads);
        pool.install(|| {
            for (label, area) in [("1 タイル", &tile), ("1920×1080", &screen)] {
                let req = AreaRequest {
                    output: OutputId(0),
                    area: area.clone(),
                };
                let first = engine
                    .render_area(&plan, &fin, &mut NodeCache::new(0), &req, None, &NoMonitor)
                    .unwrap();
                check_region(&first.value, &out, area).unwrap();
                eprintln!("{threads} スレッド {label}：{:?}", first.stats);
                let c = clock();
                let s = measure(&c, 3, 20, |_| {
                    engine.render_area(&plan, &fin, &mut NodeCache::new(0), &req, None, &NoMonitor)
                })
                .unwrap();
                show(&format!("{threads} スレッド：範囲の描画 {label}"), &s);
            }
            let c = clock();
            let s = measure(&c, 1, 5, |_| engine.render(&plan, &plain, &NoMonitor)).unwrap();
            show(
                &format!("{threads} スレッド：全体の描画（ダイジェスト込み）"),
                &s,
            );
        });
    }
}
