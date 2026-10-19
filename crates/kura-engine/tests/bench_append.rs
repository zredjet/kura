//! 段 10 の差分の経路のベンチ（NFR-12。docs/editing.md §16）。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_append -- --ignored --nocapture --test-threads=1`
//!
//! 測るのは、セッションに筆 1 本（500 点）を足してから、差分の計画（`Engine::compile_snapshot`）とキャッシュの描画
//! （`Engine::render_cached`。差分の適用・ダイジェストなし）が終わるまで。セッションの手（commit）は別に測る。エンコード・
//! 変わったタイルの一覧・IPC は含めない。3 スレッドと 16 スレッドのプールで回す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::collections::BTreeMap;
use std::time::Instant;

use kura_engine::doc::{Graph, InputId, NodeId};
use kura_engine::paint::PaintAction;
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::session::{CommitMode, Edit, EditSession, HistoryLimits};
use kura_engine::testgen::InputGen;
use kura_engine::testing::bench::{pen, walk_stroke};
use kura_engine::value::{PixelFormat, Value};
use kura_engine::{Digest, NoMonitor};
use serde_json::{Value as J, json};

const SIDE: u32 = 4096;
const SAMPLES: usize = 30;
const WARMUP: usize = 5;

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
}

/// 単色の画像（全面がつながった色。塗りつぶしの最悪に近い）。
fn flat(side: u32) -> Value {
    InputGen::Checker {
        w: side,
        h: side,
        fmt: PixelFormat::Rgba8,
        cell: 1 << 15,
    }
    .generate()
    .unwrap()
}

/// 既にある筆（50 点・1 歩 ±12px）。5 万本でもノードの手間の上限（`MAX_PAINT_WORK`）に収まるよう、大きさは 4px。
fn prior(n: usize, side: u32) -> Vec<J> {
    (0..n)
        .map(|i| {
            let pts = walk_stroke(1_000 + i as u64, 50, side, side, 12);
            serde_json::to_value(pen(pts, 4, false, [(i % 251) as u8, 40, 200, 255])).unwrap()
        })
        .collect()
}

/// 足す筆 1 本（500 点・1 歩 ±12px）。
fn stroke(seed: u64, size: u8, side: u32) -> J {
    let pts = walk_stroke(seed, 500, side, side, 12);
    serde_json::to_value(pen(
        pts,
        size,
        size == 1,
        [200, (seed % 256) as u8, 30, 255],
    ))
    .unwrap()
}

fn ms(ns: u128) -> f64 {
    ns as f64 / 1e6
}

/// 最近順位法の p50・p95 と最大（ms）。
fn summary(mut v: Vec<u128>) -> String {
    v.sort();
    let rank = |p: usize| v[(v.len() * p).div_ceil(100).max(1) - 1];
    format!(
        "p50 {:.2}・p95 {:.2}・最大 {:.2} ms",
        ms(rank(50)),
        ms(rank(95)),
        ms(*v.last().unwrap())
    )
}

/// エディタの経路の状態（セッション・入力・キャッシュ・前の計画）。
struct Editor {
    engine: Engine,
    session: EditSession,
    inputs: BTreeMap<InputId, FingerprintedInput>,
    shapes: BTreeMap<InputId, kura_engine::Shape>,
    cache: NodeCache,
    plan: Plan,
    paint: NodeId,
}

impl Editor {
    fn open(g: &Graph, input: Value, paint: NodeId) -> Self {
        let engine = Engine::new().unwrap();
        let session =
            EditSession::open(&engine, g, Digest([1; 32]), HistoryLimits::default()).unwrap();
        let shapes = BTreeMap::from([(InputId(0), input.shape())]);
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input, Digest([2; 32])).unwrap(),
        )]);
        let t = Instant::now();
        let plan = engine
            .compile_snapshot(session.current(), &shapes, None)
            .unwrap();
        let compile_ms = ms(t.elapsed().as_nanos());
        let mut cache = NodeCache::new(1 << 30);
        let t = Instant::now();
        engine
            .render_cached(&plan, &inputs, &mut cache, &opts(paint), &NoMonitor)
            .unwrap();
        eprintln!(
            "  開いて最初の計画 {compile_ms:.1} ms・最初の描画 {:.1} ms（計画の手間 {}）",
            ms(t.elapsed().as_nanos()),
            plan.cost()
        );
        Self {
            engine,
            session,
            inputs,
            shapes,
            cache,
            plan,
            paint,
        }
    }

    /// 1 手を確定し、差分の計画とキャッシュの描画をする（セッションの手・差分の計画・計画＋描画の時間と、差分の適用で
    /// 描いたノードの数を返す）。
    fn step(&mut self, items: Vec<J>) -> (u128, u128, u128, u64) {
        let t = Instant::now();
        self.session
            .commit(
                &self.engine,
                Edit::AppendItems {
                    node: self.paint,
                    field: "actions".into(),
                    items,
                },
                CommitMode::Step,
            )
            .unwrap();
        let commit = t.elapsed().as_nanos();
        let t = Instant::now();
        let plan = self
            .engine
            .compile_snapshot(self.session.current(), &self.shapes, Some(&self.plan))
            .unwrap();
        let compile = t.elapsed().as_nanos();
        let out = self
            .engine
            .render_cached(
                &plan,
                &self.inputs,
                &mut self.cache,
                &opts(self.paint),
                &NoMonitor,
            )
            .unwrap();
        let engine = t.elapsed().as_nanos();
        let appended = out.stats.appended;
        drop(out);
        self.plan = plan;
        (commit, compile, engine, appended)
    }

    /// 取り消して、取り消した文書の計画と描画にする（測らない。前の値はキャッシュに当たる）。
    fn undo(&mut self) {
        self.session.undo().unwrap();
        let plan = self
            .engine
            .compile_snapshot(self.session.current(), &self.shapes, Some(&self.plan))
            .unwrap();
        let out = self
            .engine
            .render_cached(
                &plan,
                &self.inputs,
                &mut self.cache,
                &opts(self.paint),
                &NoMonitor,
            )
            .unwrap();
        assert!(out.stats.hits >= 1);
        self.plan = plan;
    }

    /// 今の文書をキャッシュなしで全体を描いたものと、エディタの経路の値が同じか（ダイジェストで照らす）。
    fn check(&mut self) {
        let snap = self.session.current().clone();
        let plan = self.engine.compile(&snap.to_graph(), &self.shapes).unwrap();
        let values: BTreeMap<InputId, InputValue> = self
            .inputs
            .iter()
            .map(|(k, v)| {
                (
                    *k,
                    InputValue {
                        value: v.value.clone(),
                        source: v.source,
                    },
                )
            })
            .collect();
        let want = self.engine.render(&plan, &values, &NoMonitor).unwrap();
        let got = self
            .engine
            .render_cached(
                &self.plan,
                &self.inputs,
                &mut self.cache,
                &RenderOptions {
                    digests: true,
                    ..opts(self.paint)
                },
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(got.outputs[0].digest, Some(want.outputs[0].digest));
    }
}

fn opts(focus: NodeId) -> RenderOptions {
    RenderOptions {
        digests: false,
        focus: Some(focus),
        verify_hits: false,
    }
}

/// 筆の文書（切り抜き → 描画 → 後ろの操作）。後ろの操作がなければ描画が鎖の最後。
fn paint_doc(prior: &[J], tail: Option<(&str, J)>) -> (Graph, NodeId) {
    let mut g = Graph::new_single();
    let paint = g
        .push_linear("paint.actions", 1, json!({ "actions": prior }), 0)
        .unwrap();
    if let Some((op, params)) = tail {
        g.push_linear(op, 1, params, 0).unwrap();
    }
    (g, paint)
}

/// 測る：`SAMPLES` 回（慣らし `WARMUP` 回）、`make(i)` の要素を足す。
fn run(label: &str, ed: &mut Editor, pool: &rayon::ThreadPool, make: &dyn Fn(usize) -> Vec<J>) {
    run_with(label, ed, pool, make, false);
}

/// 測る（`reset` なら、1 手ごとに取り消して同じ文書に足す。手間の上限に届かないように）。
fn run_with(
    label: &str,
    ed: &mut Editor,
    pool: &rayon::ThreadPool,
    make: &dyn Fn(usize) -> Vec<J>,
    reset: bool,
) {
    let (mut commits, mut compiles, mut engines) = (Vec::new(), Vec::new(), Vec::new());
    let mut appended = 0;
    for i in 0..WARMUP + SAMPLES {
        let items = make(i);
        let (c, p, e, a) = pool.install(|| ed.step(items));
        if reset {
            pool.install(|| ed.undo());
        }
        if i >= WARMUP {
            commits.push(c);
            compiles.push(p);
            engines.push(e);
            appended += a;
        }
    }
    eprintln!(
        "  {label}：エンジンの中（差分の計画＋描画）{}／うち差分の計画 {}／セッションの手 {}（差分の適用 {appended}/{SAMPLES}）",
        summary(engines),
        summary(compiles),
        summary(commits)
    );
}

/// NFR-12：4096² に 0・999・5 万本を描いたあと、500 点の筆 1 本（大きさ 1・8・32・64）を足す。
#[test]
#[ignore]
fn bench_append_stroke() {
    let canvas = flat(SIDE);
    for n in [0usize, 999, 50_000] {
        let before = prior(n, SIDE);
        let (g, paint) = paint_doc(&before, None);
        eprintln!(
            "筆 {n} 本の文書（{} KB）",
            g.to_canonical_json().unwrap().len() / 1024
        );
        for threads in [3usize, 16] {
            let pool = pool(threads);
            let mut ed = pool.install(|| Editor::open(&g, canvas.clone(), paint));
            for size in [1u8, 8, 32, 64] {
                run(
                    &format!("{threads} スレッド・筆 {size}px"),
                    &mut ed,
                    &pool,
                    &|i| vec![stroke(7_000 + i as u64 * 31 + u64::from(size), size, SIDE)],
                );
            }
            if n <= 999 {
                pool.install(|| ed.check());
            }
        }
    }
}

/// 筆のあとに最近傍の 4 倍の拡大が続く文書（下流は全体を描き直す。範囲の描画は 10b）。描画は 1024²（出力 4096²）と 2048²。
#[test]
#[ignore]
fn bench_append_stroke_then_nearest4() {
    for side in [1024u32, 2048] {
        let canvas = flat(side);
        let before = prior(999, side);
        let tail = (
            "image.resize",
            json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        );
        let (g, paint) = paint_doc(&before, Some(tail));
        eprintln!("{side}² に筆 999 本 → 最近傍 ×4（出力 {}²）", side * 4);
        for threads in [3usize, 16] {
            let pool = pool(threads);
            let mut ed = pool.install(|| Editor::open(&g, canvas.clone(), paint));
            for size in [8u8, 32] {
                run(
                    &format!("{threads} スレッド・筆 {size}px"),
                    &mut ed,
                    &pool,
                    &|i| vec![stroke(9_000 + i as u64 * 17 + u64::from(size), size, side)],
                );
            }
            pool.install(|| ed.check());
        }
    }
}

/// 塗りつぶし（全面、4096²）を差分で足す：単色の画像（筆 0 本）に許容差 0 で、筆 999 本の上に許容差 255 で、4 連結の
/// つながった全面と、画像全体の同じ色。1 手ごとに取り消して同じ文書に足す（手間の上限に届かないように）。
#[test]
#[ignore]
fn bench_append_full_fill() {
    let canvas = flat(SIDE);
    for (n, tolerance) in [(0usize, 0u8), (999, 255)] {
        let (g, paint) = paint_doc(&prior(n, SIDE), None);
        eprintln!("4096² に筆 {n} 本・許容差 {tolerance} の全面の塗りつぶし");
        for threads in [3usize, 16] {
            let pool = pool(threads);
            let mut ed = pool.install(|| Editor::open(&g, canvas.clone(), paint));
            for contiguous in [true, false] {
                run_with(
                    &format!(
                        "{threads} スレッド・{}",
                        if contiguous { "連結" } else { "全体" }
                    ),
                    &mut ed,
                    &pool,
                    &|i| {
                        let fill = PaintAction::Fill {
                            at: kura_engine::geom::IPoint { x: 0, y: 0 },
                            color: kura_engine::paint::PaintColor::Rgba([
                                (i % 256) as u8,
                                (i / 256 % 256) as u8,
                                3,
                                255,
                            ]),
                            tolerance,
                            connectivity: kura_engine::paint::Connectivity::Four,
                            contiguous,
                            frame: 0,
                        };
                        vec![serde_json::to_value(fill).unwrap()]
                    },
                    true,
                );
            }
            pool.install(|| ed.check());
        }
    }
}

/// 取り消し（5 万本の文書で筆 1 本を足してから取り消す）の差分の計画：前の計画が最新の 1 つだけのとき（描画のノードは中身が
/// 変わったので全体を読み直す）と、取り消した先の版の計画を渡したとき（すべてのノードを共有する）。
#[test]
#[ignore]
fn bench_undo_plan() {
    let canvas = flat(SIDE);
    let (g, paint) = paint_doc(&prior(50_000, SIDE), None);
    let pool = pool(3);
    let mut ed = pool.install(|| Editor::open(&g, canvas.clone(), paint));
    let (mut latest, mut kept) = (Vec::new(), Vec::new());
    for i in 0..10u64 {
        let before = ed.plan.clone();
        pool.install(|| ed.step(vec![stroke(11_000 + i, 8, SIDE)]));
        ed.session.undo().unwrap();
        let t = Instant::now();
        let a = ed
            .engine
            .compile_snapshot(ed.session.current(), &ed.shapes, Some(&ed.plan))
            .unwrap();
        latest.push(t.elapsed().as_nanos());
        let t = Instant::now();
        let b = ed
            .engine
            .compile_snapshot(ed.session.current(), &ed.shapes, Some(&before))
            .unwrap();
        kept.push(t.elapsed().as_nanos());
        assert!(b.shares_node(&before, paint) && !a.shares_node(&before, paint));
        ed.plan = b;
    }
    eprintln!(
        "5 万本の取り消しの差分の計画：最新の計画だけ {}／取り消した先の版の計画 {}",
        summary(latest),
        summary(kept)
    );
}
