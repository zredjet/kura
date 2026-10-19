//! E30（docs/editing.md §16・§17、requirements.md の NFR-11〜17）：段 10 の編集の性能のベンチ。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_edit -- --ignored --nocapture --test-threads=1`
//!
//! - 時間は表に出すだけで、断言するのは正しさだけ：エディタの経路と一括の経路のダイジェストが同じ（NFR-16）、予算を超える
//!   計画は Limit（NFR-17・E25）、見積もりが実際（描いた値で数えた生きている値）以上。
//! - 判定の条件はプレビューのプール 3 スレッド（4・16 も記録。ADR-154 (1)(c)）。「エンジンの中」は、編集を受けてから
//!   プレビューの値と変わったタイルの一覧ができるまで（セッションの手・差分の計画・段の計画・表示の経路
//!   （`Engine::render_view`：変わったタイルだけを描き直す）。ダイジェスト・エンコード・IPC・WebKit を含めない）。NFR-16 の
//!   照合は等倍の全体（`render_cached`）のダイジェストで。
//! - ワークロードは `kura_engine::testing::bench`（`kura-devtools bench edit` と同じもの）。ここは固定のコーデックだけで、
//!   Linux でも回る。macOS のコーデック・ファイルの読み書き・予算の確保の経路・子プロセスのピークのメモリは bench edit。
//! - 写真のプレビュー（NFR-11 の 24MP）は縮小の段の計画（`Engine::plan_level`）で、段 `PHOTO_LEVEL`（2）で判定し、段 1 も
//!   記録する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::Instant;

use kura_engine::budget::{Class, MemoryBudget};
use kura_engine::codec::CodecSet;
use kura_engine::doc::{DecodeOptions, Graph, InputId};
use kura_engine::monitor::CancelAfter;
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::estimate::{CacheLease, EstimateSpec, Source};
use kura_engine::render::{Engine, InputValue};
use kura_engine::session::CommitMode;
use kura_engine::testing::bench::cases::{self, Sweep};
use kura_engine::testing::bench::path::{
    EditorPath, Step, bulk_render, live_peak, plain_inputs, single_input,
};
use kura_engine::testing::bench::{
    PHOTO_FALLBACK, Stats, WARMUP, measure, measure_prepared, photo, sprite_batch, target,
};
use kura_engine::{Digest, EngineError, NoMonitor, Value};
use rayon::prelude::*;

/// 判定のスレッドの数と、記録するスレッドの数。
const JUDGED: usize = 3;
const THREADS: [usize; 3] = [3, 4, 16];
/// NFR-11・12 の回数（慣らしは `WARMUP`）。
const N: usize = 30;
/// エディタのキャッシュの上限。
const CACHE: u64 = 1 << 30;

fn clock() -> impl Fn() -> u64 + Send + Sync {
    let start = Instant::now();
    move || start.elapsed().as_nanos() as u64
}

fn pool(n: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .build()
        .unwrap()
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// 表の 1 行（時間は記録だけ。判定は `JUDGED` のスレッドの行だけに付ける）。
fn row(nfr: u8, case: &str, label: &str, threads: usize, s: &Stats, judged: bool) {
    let goal = match nfr {
        11 => target(11, "preview"),
        12 if case.starts_with("fill") => target(12, "fill"),
        12 => target(12, "stroke"),
        13 => target(13, "save"),
        14 => target(14, "bulk"),
        15 => target(15, "analysis"),
        _ => None,
    };
    let verdict = match (goal.and_then(|g| g.verdict(s)), threads == JUDGED && judged) {
        (Some(true), true) => "達成",
        (Some(false), true) => "未達",
        _ => "記録",
    };
    eprintln!(
        "NFR-{nfr} | {case:<28} | {label:<34} | {threads:>2} スレッド | n {:>3} | p50 {:>9.3} | p95 {:>9} | 最大 {:>9.3} ms | 目標 {:>6} | {verdict}",
        s.n,
        ms(s.p50),
        s.p95.map_or("-".to_string(), |v| format!("{:.3}", ms(v))),
        ms(s.max),
        goal.and_then(|g| g.limit_ns)
            .map_or("-".to_string(), |v| format!("{:.0}", ms(v))),
    );
}

/// エディタの経路の今の文書を、別のエンジン・キャッシュなしの一括の経路（1・16 スレッド）で描き、ダイジェストが同じこと
/// （NFR-16・E4）。
fn assert_paths_match(engine: &Engine, ed: &mut EditorPath, what: &str) {
    let got = pool(JUDGED).install(|| ed.digests(engine)).unwrap();
    let doc = ed.document().unwrap();
    let inputs = plain_inputs(ed.inputs());
    for t in [1, 16] {
        let fresh = Engine::new().unwrap();
        let want = pool(t)
            .install(|| bulk_render(&fresh, &doc, &inputs, &NoMonitor))
            .unwrap();
        let want: Vec<Digest> = want.outputs.iter().map(|o| o.digest).collect();
        assert_eq!(
            got, want,
            "{what}：エディタの経路と一括の経路（{t} スレッド）"
        );
    }
}

/// 引数を動かす（スライダー）：仮の値を出して、差分の計画と表示の経路（変わったタイルの一覧を含む）。写真は段 `level`
/// （`s.level` でなければ記録だけ）。
fn run_sweep(s: &Sweep, level: u8) {
    for threads in THREADS {
        let engine = Engine::new().unwrap();
        let pool = pool(threads);
        let mut ed = EditorPath::open(
            &engine,
            &s.graph,
            single_input(s.input.clone(), s.source).unwrap(),
            CACHE,
            Some(s.node),
        )
        .unwrap();
        pool.install(|| ed.render_preview(&engine, level)).unwrap();
        let c = clock();
        let mut levels = Vec::new();
        let stats = pool
            .install(|| {
                measure(&c, WARMUP, N, |i| {
                    ed.apply(&engine, Step::Preview((s.edit)(i)))?;
                    let out = ed.render_preview(&engine, level)?;
                    levels.push(out.level);
                    Ok(out)
                })
            })
            .unwrap();
        let fallback = level > 0 && levels.iter().any(|l| *l != level);
        let label = if fallback {
            PHOTO_FALLBACK.to_string()
        } else if level > 0 {
            format!("{}（段 {level}）", s.item)
        } else {
            s.item.to_string()
        };
        let judged = s.judged && !fallback && level == s.level;
        row(11, s.case, &label, threads, &stats, judged);
        if threads == JUDGED {
            assert_paths_match(&engine, &mut ed, s.case);
        }
    }
}

/// NFR-11：ドット絵 2048² の等倍・音声 5 分・写真 24MP（縮小の段 2。段 1 は記録）の引数の変更。
#[test]
#[ignore]
fn e30_preview() {
    for s in cases::pixel_sweeps(2048).unwrap() {
        run_sweep(&s, s.level);
    }
    for s in cases::audio_sweeps(44_100, 300).unwrap() {
        run_sweep(&s, s.level);
    }
    for s in cases::photo_sweeps(photo(7).generate().unwrap()).unwrap() {
        run_sweep(&s, s.level);
        run_sweep(&s, 1);
    }
}

/// 筆の文書を開いて、`make(i)` の要素を足す（`reset` なら 1 手ごとに測らずに取り消す）。
#[allow(clippy::too_many_arguments)]
fn run_paint(
    case: &str,
    label: &str,
    graph: &Graph,
    paint: kura_engine::doc::NodeId,
    canvas: &Value,
    judged: bool,
    reset: bool,
    make: &(dyn Fn(usize) -> serde_json::Value + Sync),
    check: bool,
) {
    for threads in THREADS {
        let engine = Engine::new().unwrap();
        let pool = pool(threads);
        let mut ed = EditorPath::open(
            &engine,
            graph,
            single_input(canvas.clone(), Digest([2; 32])).unwrap(),
            CACHE,
            Some(paint),
        )
        .unwrap();
        pool.install(|| ed.render_view(&engine, &NoMonitor))
            .unwrap();
        let c = clock();
        let commit = |i: usize| {
            Step::Commit(
                kura_engine::session::Edit::AppendItems {
                    node: paint,
                    field: "actions".into(),
                    items: vec![make(i)],
                },
                CommitMode::Step,
            )
        };
        let stats = pool
            .install(|| {
                if reset {
                    measure_prepared(
                        &c,
                        WARMUP,
                        N,
                        &mut ed,
                        |ed, i| {
                            if i > 0 {
                                ed.show(&engine, Step::Undo, 0)?;
                            }
                            Ok(())
                        },
                        |ed, i| ed.show(&engine, commit(i), 0),
                    )
                } else {
                    measure(&c, WARMUP, N, |i| ed.show(&engine, commit(i), 0))
                }
            })
            .unwrap();
        row(12, case, label, threads, &stats, judged);
        if check && threads == JUDGED {
            assert_paths_match(&engine, &mut ed, case);
        }
    }
}

/// NFR-12：4096² に 0・999・5 万本を描いたあとの 500 点の筆（1・8・32・64px）、筆のあとに最近傍 ×4 が続く文書、全面の
/// 塗りつぶし（4096²。連結・全体）。
#[test]
#[ignore]
fn e30_stroke() {
    let side = 4096;
    let canvas = cases::flat_canvas(side).unwrap();
    for prior in [0usize, 999, 50_000] {
        let (g, paint) =
            cases::paint_doc(cases::prior_strokes(prior, side).unwrap(), None).unwrap();
        for size in [1u8, 8, 32, 64] {
            run_paint(
                &format!("stroke-{prior}-{size}px"),
                &format!("4096²・前の筆 {prior} 本・{size}px"),
                &g,
                paint,
                &canvas,
                true,
                false,
                &|i| cases::stroke_item(7_000 + i as u64 * 31, size, side).unwrap(),
                prior <= 999,
            );
        }
    }
    for small in [1024u32, 2048] {
        let canvas = cases::flat_canvas(small).unwrap();
        let (g, paint) = cases::paint_doc(
            cases::prior_strokes(999, small).unwrap(),
            Some(cases::nearest4()),
        )
        .unwrap();
        for size in [8u8, 32] {
            run_paint(
                &format!("stroke-nearest4-{small}-{size}px"),
                &format!("{small}² → 最近傍 ×4・{size}px"),
                &g,
                paint,
                &canvas,
                small * 4 <= 4096,
                false,
                &|i| cases::stroke_item(9_000 + i as u64 * 17, size, small).unwrap(),
                true,
            );
        }
    }
    let (g, paint) = cases::paint_doc(Vec::new(), None).unwrap();
    for contiguous in [true, false] {
        run_paint(
            if contiguous {
                "fill-contiguous"
            } else {
                "fill-global"
            },
            "4096² の全面の塗りつぶし",
            &g,
            paint,
            &canvas,
            true,
            true,
            &|i| cases::fill_item(i, 0, contiguous).unwrap(),
            true,
        );
    }
}

/// NFR-13（24MP の JPEG → 線形光で 50% → JPEG）・NFR-14（ドット絵の PNG 1,000 件のトリム・4 倍。メモリの中で読み書き）・
/// NFR-15（5 分の曲の解析。WAV と FLAC）。どれもスレッドの数によらず同じ値（NFR-16）。
#[test]
#[ignore]
fn e30_save_bulk_analysis() {
    let codecs = CodecSet::pinned();
    let engine = Engine::new().unwrap();
    // NFR-13
    let photo = photo(7).generate().unwrap();
    let (jpeg, _) = codecs
        .encode_bytes(&photo, "jpeg", &cases::save_jpeg(), &NoMonitor)
        .unwrap();
    let source = Digest(*blake3::hash(&jpeg).as_bytes());
    for (filter, judged) in [("lanczos3", true), ("bilinear", false)] {
        let g = cases::save_doc(filter).unwrap();
        let mut outs: Vec<Vec<u8>> = Vec::new();
        for threads in THREADS {
            let c = clock();
            let mut last = Vec::new();
            let stats = pool(threads)
                .install(|| {
                    measure(&c, 2, 10, |_| {
                        let (value, _) = codecs.decode(
                            &mut Cursor::new(jpeg.as_slice()),
                            "jpeg",
                            &DecodeOptions::default(),
                            engine.limits(),
                            &NoMonitor,
                        )?;
                        let inputs = BTreeMap::from([(InputId(0), InputValue { value, source })]);
                        let shapes =
                            BTreeMap::from([(InputId(0), inputs[&InputId(0)].value.shape())]);
                        let plan = engine.compile(&g, &shapes)?;
                        let r = engine.render(&plan, &inputs, &NoMonitor)?;
                        let (bytes, _) = codecs.encode_bytes(
                            &r.outputs[0].value,
                            "jpeg",
                            &cases::save_jpeg(),
                            &NoMonitor,
                        )?;
                        last = bytes;
                        Ok(())
                    })
                })
                .unwrap();
            row(
                13,
                &format!("save-{filter}"),
                "24MP の合成の写真",
                threads,
                &stats,
                judged,
            );
            outs.push(last);
        }
        assert!(
            outs.windows(2).all(|w| w[0] == w[1]),
            "保存のバイト列がスレッドの数で変わった"
        );
    }
    // NFR-14
    let items: Vec<Vec<u8>> = sprite_batch(1_000, 1)
        .iter()
        .map(|g| {
            codecs
                .encode_bytes(
                    &g.generate().unwrap(),
                    "png",
                    &cases::bulk_png(),
                    &NoMonitor,
                )
                .unwrap()
                .0
        })
        .collect();
    let g = cases::bulk_doc().unwrap();
    let mut outs: Vec<Vec<Digest>> = Vec::new();
    for threads in THREADS {
        let c = clock();
        let mut last = Vec::new();
        let stats = pool(threads)
            .install(|| {
                measure(&c, 1, 3, |_| {
                    last = items
                        .par_iter()
                        .map(|bytes| -> Result<Digest, EngineError> {
                            let (value, _) = codecs.decode(
                                &mut Cursor::new(bytes.as_slice()),
                                "png",
                                &DecodeOptions::default(),
                                engine.limits(),
                                &NoMonitor,
                            )?;
                            let shapes = BTreeMap::from([(InputId(0), value.shape())]);
                            let plan = engine.compile(&g, &shapes)?;
                            let source = Digest(*blake3::hash(bytes).as_bytes());
                            let inputs =
                                BTreeMap::from([(InputId(0), InputValue { value, source })]);
                            let r = engine.render(&plan, &inputs, &NoMonitor)?;
                            let (out, _) = codecs.encode_bytes(
                                &r.outputs[0].value,
                                "png",
                                &cases::bulk_png(),
                                &NoMonitor,
                            )?;
                            Ok(Digest(*blake3::hash(&out).as_bytes()))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(())
                })
            })
            .unwrap();
        row(
            14,
            "bulk",
            "PNG 1,000 件・トリム → 最近傍 ×4",
            threads,
            &stats,
            true,
        );
        outs.push(last);
    }
    assert!(
        outs.windows(2).all(|w| w[0] == w[1]),
        "一括の結果がスレッドの数で変わった"
    );
    // NFR-15
    let audio = kura_engine::testing::bench::program_audio(44_100, 300).unwrap();
    let reqs = cases::analysis_requests();
    for format in ["wav", "flac"] {
        let (bytes, _) = codecs
            .encode_bytes(
                &audio,
                format,
                &serde_json::json!({ "bits": 16 }),
                &NoMonitor,
            )
            .unwrap();
        let mut outs: Vec<Vec<Vec<u8>>> = Vec::new();
        for threads in THREADS {
            let c = clock();
            let mut last = Vec::new();
            let stats = pool(threads)
                .install(|| {
                    measure(&c, 1, 5, |_| {
                        let (v, _) = codecs.decode(
                            &mut Cursor::new(bytes.as_slice()),
                            format,
                            &DecodeOptions::default(),
                            engine.limits(),
                            &NoMonitor,
                        )?;
                        last = engine
                            .analyze_many(&v, &reqs, &NoMonitor)
                            .into_iter()
                            .map(|r| r.map(|a| a.output))
                            .collect::<Result<Vec<_>, _>>()?;
                        Ok(())
                    })
                })
                .unwrap();
            row(
                15,
                &format!("analysis-{format}"),
                "5 分 44.1 kHz ステレオ",
                threads,
                &stats,
                true,
            );
            outs.push(last);
        }
        assert!(
            outs.windows(2).all(|w| w[0] == w[1]),
            "解析の結果がスレッドの数で変わった"
        );
    }
}

/// NFR-17（エンジンの側）：ベンチのワークロードの見積もりが、描いた値で数えた生きている値以上（3・16 スレッドの並列の数）。
/// 予算が見積もり − 1 なら、一括もエディタも描く前に Limit（予算の使用量は 0 のまま、操作は 1 つも動かない）。
#[test]
#[ignore]
fn e30_memory() {
    let engine = Engine::new().unwrap();
    let photo = photo(7).generate().unwrap();
    let sprite = sprite_batch(1, 3)[0].generate().unwrap();
    let canvas = cases::flat_canvas(4096).unwrap();
    let (paint, _) = cases::paint_doc(cases::prior_strokes(999, 4096).unwrap(), None).unwrap();
    let mut work: Vec<(String, Graph, Value)> = vec![
        (
            "save-lanczos3".into(),
            cases::save_doc("lanczos3").unwrap(),
            photo.clone(),
        ),
        ("bulk".into(), cases::bulk_doc().unwrap(), sprite),
        ("paint-999".into(), paint, canvas),
    ];
    for s in cases::pixel_sweeps(2048)
        .unwrap()
        .into_iter()
        .chain(cases::audio_sweeps(44_100, 300).unwrap())
        .chain(cases::photo_sweeps(photo.clone()).unwrap())
    {
        work.push((s.case.to_string(), s.graph, s.input));
    }
    for (name, g, input) in &work {
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input.clone(),
                source: Digest([5; 32]),
            },
        )]);
        let plan = engine
            .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
            .unwrap();
        let live = live_peak(&engine, g, &inputs).unwrap();
        for tasks in [3u64, 16] {
            let spec = EstimateSpec::new(tasks).input(InputId(0), Source::Memory);
            let est = engine.estimate(&plan, &spec, None).unwrap();
            eprintln!(
                "NFR-17 | {name:<28} | {tasks:>2} 並列 | 見積もり {:>8.1} MiB | 生きている値 {:>8.1} MiB",
                est.peak as f64 / (1 << 20) as f64,
                live as f64 / (1 << 20) as f64
            );
            assert!(
                est.peak >= live,
                "{name}：見積もり {} < 実際 {live}",
                est.peak
            );
            // 一括：見積もり − 1 の予算は描く前に Limit
            let budget = MemoryBudget::new(est.peak - 1);
            let mon = CancelAfter::new(u64::MAX);
            let err = engine
                .admit(&plan, &spec, &budget, Class::Bulk, &mon)
                .unwrap_err();
            assert!(matches!(err, EngineError::Limit(_)), "{name}: {err:?}");
            assert_eq!((budget.used().unwrap(), mon.calls()), (0, 0), "{name}");
        }
        // エディタ：入力をキャッシュが数え、キャッシュの上限を 0 にしても足りない予算は Limit
        let fin = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input.clone(), Digest([5; 32])).unwrap(),
        )]);
        let mut cache = NodeCache::new(0);
        cache.register_external(input);
        let spec = EstimateSpec::new(3).all_inputs(&plan, Source::Held);
        let est = engine.estimate(&plan, &spec, None).unwrap();
        let budget = MemoryBudget::new(cache.bytes() + est.peak - 1);
        let mut held = CacheLease::open(&budget, &mut cache, 0).unwrap();
        let err = engine
            .admit_cached(&plan, &spec, &fin, &mut cache, &mut held)
            .unwrap_err();
        assert!(matches!(err, EngineError::Limit(_)), "{name}: {err:?}");
        drop(held);
        assert_eq!(budget.used().unwrap(), 0, "{name}");
    }
}
