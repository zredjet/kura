//! 段 10 の EditSession のベンチ（docs/editing.md §16）。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_session -- --ignored --nocapture --test-threads=1`
//!
//! 測るもの：1 手の時間（筆の追記・引数の変更）と履歴のバイトの見積もり、5 万本の筆の文書での追記、to_graph・正規の JSON・
//! 開く時間、undo・redo。描画（実行器）は含めない。実メモリはアロケータを数えないと測れないので、ここでは測らない
//! （段 10 の報告には scratchpad の試作で数えた値を書いた）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::time::Instant;

use kura_engine::determinism;
use kura_engine::digest::Digest;
use kura_engine::doc::{Graph, NodeId};
use kura_engine::geom::IPoint;
use kura_engine::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use kura_engine::render::Engine;
use kura_engine::session::{CommitMode, Edit, EditSession, HistoryLimits};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

/// 1 歩 ±12px のランダムウォークの筆（`points` 点・大きさ `size`）。
fn stroke(r: &mut ChaCha8Rng, points: usize, size: u8, w: i64) -> serde_json::Value {
    let mut x = (r.next_u64() % w as u64) as i64;
    let mut y = (r.next_u64() % w as u64) as i64;
    let mut pts = Vec::with_capacity(points);
    for _ in 0..points {
        pts.push(IPoint { x, y });
        x = (x + (r.next_u64() % 25) as i64 - 12).clamp(0, w - 1);
        y = (y + (r.next_u64() % 25) as i64 - 12).clamp(0, w - 1);
    }
    serde_json::to_value(PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Rgba([(r.next_u32() % 256) as u8, 40, 200, 255])),
        blend: Blend::Over,
        brush: Brush {
            shape: BrushShape::Circle,
            size,
        },
        pixel_perfect: size == 1,
        frame: 0,
        points: PointList(pts),
    })
    .unwrap()
}

/// 切り抜き → 筆（`prior` 本・各 `points` 点）→ 反転の文書。
fn doc(r: &mut ChaCha8Rng, prior: usize, points: usize) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear(
        "image.crop",
        1,
        json!({ "rect": { "x": 0, "y": 0, "w": 4096, "h": 4096 } }),
        0,
    )
    .unwrap();
    let actions: Vec<_> = (0..prior).map(|_| stroke(r, points, 8, 4096)).collect();
    g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
        .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    g
}

struct Stats {
    p50: f64,
    p95: f64,
    max: f64,
}

/// 最近順位法の p50・p95 と最大（ms）。
fn stats(mut v: Vec<f64>) -> Stats {
    v.sort_by(f64::total_cmp);
    let rank = |p: f64| v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1];
    Stats {
        p50: rank(0.50),
        p95: rank(0.95),
        max: v[v.len() - 1],
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn open(engine: &Engine, g: &Graph) -> (EditSession, f64) {
    let t = Instant::now();
    let s = EditSession::open(engine, g, Digest::default(), HistoryLimits::default()).unwrap();
    (s, ms(t))
}

/// 筆 1 本の追記（500 点のランダムウォーク）の 1 手：時間と、履歴のバイトの見積もりの増え方。
#[test]
#[ignore]
fn bench_append_stroke() {
    let engine = Engine::new().unwrap();
    for (prior, points) in [(0usize, 50usize), (999, 50), (5_000, 50), (50_000, 40)] {
        let mut r = determinism::rng(0xbe9c + prior as u64);
        let g = doc(&mut r, prior, points);
        let doc_bytes = g.encoded_len().unwrap();
        let (mut s, open_ms) = open(&engine, &g);
        let strokes: Vec<_> = (0..210).map(|_| stroke(&mut r, 500, 8, 4096)).collect();
        let stroke_bytes = kura_engine::digest::canonical_json(&strokes[0])
            .unwrap()
            .len();
        let mut times = Vec::new();
        let before = s.history_bytes();
        for (i, st) in strokes.into_iter().enumerate() {
            let e = Edit::AppendItems {
                node: NodeId(2),
                field: "actions".into(),
                items: vec![st],
            };
            let t = Instant::now();
            let c = s.commit(&engine, e, CommitMode::Step).unwrap();
            let dt = ms(t);
            assert!(!c.noop);
            // 慣らし 10 回
            if i >= 10 {
                times.push(dt);
            }
        }
        let per_step = (s.history_bytes() - before) as f64 / 210.0;
        let t = Instant::now();
        for _ in 0..200 {
            s.undo();
        }
        for _ in 0..200 {
            s.redo();
        }
        let undo_redo_ns = ms(t) * 1e6 / 400.0;
        let st = stats(times);
        let t = Instant::now();
        let graph = s.head().to_graph();
        let to_graph = ms(t);
        let t = Instant::now();
        let bytes = s.head().to_canonical_json().unwrap();
        let canonical = ms(t);
        let t = Instant::now();
        let reference = graph.encoded_len().unwrap();
        let encoded_len = ms(t);
        assert_eq!(reference, s.head().encoded_len());
        assert_eq!(bytes.len(), reference);
        eprintln!(
            "筆 {prior} 本（{points} 点）の文書 {:.0} KB：開く {open_ms:.2} ms ／ 500 点の筆（{stroke_bytes} B）の追記 1 手 p50 {:.4} ・p95 {:.4}・最大 {:.4} ms（n=200）、\
             履歴の見積もり +{per_step:.0} B/手 ／ undo・redo {:.0} ns ／ to_graph {to_graph:.2} ms・正規の JSON {canonical:.2} ms\
             （参考：Graph::encoded_len {encoded_len:.2} ms、セッションの encoded_len は O(1)）",
            doc_bytes as f64 / 1024.0,
            st.p50,
            st.p95,
            st.max,
            undo_redo_ns,
        );
    }
}

/// 引数の変更の 1 手（切り抜きの矩形を Merge で続けて変える・Step で変える）。筆 1,000 本の文書で。
#[test]
#[ignore]
fn bench_set_param() {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(0x5e7);
    let g = doc(&mut r, 1_000, 50);
    for mode in [CommitMode::Merge, CommitMode::Step] {
        let (mut s, _) = open(&engine, &g);
        let before = s.history_bytes();
        let mut times = Vec::new();
        for i in 0..210u32 {
            let e = Edit::SetParam {
                node: NodeId(1),
                key: "rect".into(),
                value: json!({ "x": i % 50 + 1, "y": 0, "w": 2048, "h": 2048 }),
            };
            let t = Instant::now();
            s.commit(&engine, e, mode).unwrap();
            let dt = ms(t);
            if i >= 10 {
                times.push(dt);
            }
        }
        let per_step = (s.history_bytes() - before) as f64 / 210.0;
        let st = stats(times);
        // 仮の値（ドラッグ中）
        let mut prev = Vec::new();
        for i in 0..210u32 {
            let e = Edit::SetParam {
                node: NodeId(3),
                key: "axis".into(),
                value: json!(if i % 2 == 0 { "vertical" } else { "horizontal" }),
            };
            let t = Instant::now();
            s.preview(&engine, e).unwrap();
            let dt = ms(t);
            if i >= 10 {
                prev.push(dt);
            }
        }
        let pv = stats(prev);
        eprintln!(
            "引数の変更（{mode:?}、筆 1,000 本の文書）：1 手 p50 {:.4}・p95 {:.4}・最大 {:.4} ms、履歴の見積もり +{per_step:.0} B/手、手の数 {} ／ \
             preview p50 {:.4}・p95 {:.4} ms",
            st.p50,
            st.p95,
            st.max,
            s.history_len(),
            pv.p50,
            pv.p95,
        );
    }
}
