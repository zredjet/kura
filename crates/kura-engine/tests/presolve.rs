//! M17 段 3：流し解きを区切りで進める仕事（`render::presolve`。docs/editing.md §18.3）。区切り方・中止によらず、解けた結果が
//! 描画の中の流し解き・`Analysis::run` とビット一致し、解けた結果で描くと流し解きをせずに全体の描画と同じ値になること、
//! 外の解析が追い出されないことを確かめる（試験の操作 `test.iir1` と、登録した `audio.eq`・`audio.normalize`）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::monitor::CancelAfter;
use kura_engine::ops::audio::eq::{BandChannel, BandType, EqParams, Phase, band};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::presolve::PresolveJob;
use kura_engine::render::region::Area;
use kura_engine::render::view::ViewRequest;
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::check_region;
use kura_engine::testing::stateful::{Iir1Params, OP_ID, engine as stateful_engine, solves};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{Digest, EngineError, NoMonitor, Value};
use serde_json::json;

const C: u64 = AUDIO_CHUNK as u64;

fn noise(channels: u16, frames: u64, seed: u64) -> Value {
    InputGen::AudioNoise {
        rate: 8_000,
        channels,
        frames,
        seed,
    }
    .generate()
    .unwrap()
}

fn iir(b0: f64, a1: f64) -> Iir1Params {
    Iir1Params {
        b0,
        b1: 0.4321,
        a1,
        lookahead: 0,
        link: false,
        grid: C,
        wet: 0.8,
        gain_db: -1.0,
    }
}

fn eq_ms() -> serde_json::Value {
    let mut mid = band(BandType::Bell, 300.0, 6.0, 2.0);
    mid.channel = BandChannel::Mid;
    let mut side = band(BandType::HighShelf, 1_500.0, -4.0, 0.7);
    side.channel = BandChannel::Side;
    serde_json::to_value(EqParams {
        bands: vec![mid, band(BandType::LowCut, 60.0, 0.0, 0.7), side],
        phase: Phase::Minimum,
        gain_db: -1.0,
    })
    .unwrap()
}

/// 鎖（操作の ID・引数）。
fn chain(ops: &[(&str, serde_json::Value)]) -> Graph {
    let mut g = Graph::new_single();
    for (i, (op, p)) in ops.iter().enumerate() {
        g.push_linear(op, 1, p.clone(), i as u64 + 1).unwrap();
    }
    g
}

/// 確かめる文書（名前・鎖・仕事の数）。
fn docs() -> Vec<(&'static str, Graph, usize)> {
    let p = |x: &Iir1Params| serde_json::to_value(x).unwrap();
    vec![
        // 入力に直結した申告（描画の中では値に run を当てる）
        ("iir-direct", chain(&[(OP_ID, p(&iir(0.4, -0.7)))]), 1),
        (
            "gain-iir",
            chain(&[
                ("audio.gain", json!({ "db": -3.0 })),
                (OP_ID, p(&iir(0.5, -0.6))),
            ]),
            1,
        ),
        // 入れ子：後ろの仕事の窓は前の結果で描く
        (
            "iir-iir",
            chain(&[(OP_ID, p(&iir(0.3, -0.8))), (OP_ID, p(&iir(0.6, -0.5)))]),
            2,
        ),
        (
            "gain-eq",
            chain(&[("audio.gain", json!({ "db": 2.0 })), ("audio.eq", eq_ms())]),
            1,
        ),
        (
            "gain-normalize-eq",
            chain(&[
                ("audio.gain", json!({ "db": -6.0 })),
                (
                    "audio.normalize",
                    json!({ "target": -20.0, "peakLimit": null }),
                ),
                ("audio.eq", eq_ms()),
            ]),
            2,
        ),
    ]
}

fn compile(engine: &Engine, g: &Graph, input: &Value) -> Plan {
    engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .unwrap()
}

fn fin(input: &Value) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest([3; 32])).unwrap(),
    )])
}

fn render(engine: &Engine, plan: &Plan, input: &Value) -> Value {
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source: Digest([3; 32]),
        },
    )]);
    engine
        .render(plan, &plain, &NoMonitor)
        .unwrap()
        .outputs
        .swap_remove(0)
        .value
}

/// 全体の表示を描く（流し解きの数を返す）。
fn view_all(
    engine: &Engine,
    plan: &Plan,
    input: &Value,
    cache: &mut NodeCache,
    want: &Value,
) -> u64 {
    let req = ViewRequest {
        output: OutputId(0),
        area: Area::All,
        focus: None,
    };
    let got = engine
        .render_view(plan, &fin(input), cache, &req, None, &NoMonitor)
        .unwrap();
    check_region(got.view.value(), want, &Area::All).unwrap();
    got.area.presolved
}

/// 解析の結果の正規の形。
fn encoded(
    engine: &Engine,
    key_id: &str,
    v: u16,
    out: &kura_engine::analysis::AnyOutput,
) -> Vec<u8> {
    engine
        .analyses()
        .get(key_id, v)
        .unwrap()
        .encode_any(out)
        .unwrap()
}

/// 区切り（チャンクの数）によらず、解けた結果が描画の中の流し解き（新しいキャッシュで全体を描いたもの）とビット一致し、
/// 後ろの仕事の窓で入れ子に解かず、解けた結果で描くと流し解きをせずに全体の描画と同じ。
#[test]
fn any_slicing_matches_the_render() {
    let engine = stateful_engine().unwrap();
    let input = noise(2, 5 * C + 123, 41);
    for (name, g, tasks) in docs() {
        let plan = compile(&engine, &g, &input);
        let want = render(&engine, &plan, &input);
        // 参照：新しいキャッシュで全体を描いたときに入る結果
        let mut fresh = NodeCache::new(512 << 20);
        let missing = engine
            .missing_solves(&plan, &fin(&input), &fresh, OutputId(0))
            .unwrap();
        assert_eq!(missing.tasks.len(), tasks, "{name}");
        assert_eq!(missing.needed.len(), tasks, "{name}");
        assert!(!missing.unstreamable, "{name}");
        assert!(missing.estimated_ns() > 0.0, "{name}");
        view_all(&engine, &plan, &input, &mut fresh, &want);
        for chunks in [1u64, 2, 5, 6, 16, 17, 1_000] {
            let mut cache = NodeCache::new(512 << 20);
            let missing = engine
                .missing_solves(&plan, &fin(&input), &cache, OutputId(0))
                .unwrap();
            let mut job = PresolveJob::new(missing);
            let mut steps = 0;
            let mut nested = 0;
            loop {
                let p = job
                    .step(&engine, &plan, &fin(&input), &mut cache, chunks, &NoMonitor)
                    .unwrap();
                assert!(p.chunks <= chunks, "{name} {chunks}");
                nested += p.stream.area.presolved;
                steps += 1;
                if p.done {
                    assert_eq!(p.frames_done, p.frames_total, "{name} {chunks}");
                    break;
                }
                assert!(steps < 10_000, "{name} {chunks}：終わらない");
            }
            assert_eq!(nested, 0, "{name} {chunks}：窓の中で入れ子に解いた");
            let per_task = (5 * C + 123).div_ceil(C);
            assert!(
                steps as u64 <= (tasks as u64 * per_task).div_ceil(chunks) + tasks as u64,
                "{name} {chunks}：区切り {steps}"
            );
            for s in job.finished() {
                let t = job.tasks().iter().find(|t| t.key == s.key).unwrap();
                let reference = fresh.peek_analysis(&s.key).unwrap();
                assert_eq!(
                    encoded(&engine, t.need.id, t.need.v, &s.out),
                    encoded(&engine, t.need.id, t.need.v, &reference),
                    "{name} {chunks}：{}",
                    t.need.id
                );
            }
            assert!(
                engine
                    .missing_solves(&plan, &fin(&input), &cache, OutputId(0))
                    .unwrap()
                    .tasks
                    .is_empty(),
                "{name} {chunks}"
            );
            assert_eq!(
                view_all(&engine, &plan, &input, &mut cache, &want),
                0,
                "{name} {chunks}"
            );
        }
    }
}

/// 窓を描く途中（・2 つ目の窓）で中止しても、続きから流して同じ結果になる（受け手の状態は窓の境でしか止まらない）。
#[test]
fn cancel_midway_then_resume_is_bit_exact() {
    let engine = stateful_engine().unwrap();
    let input = noise(2, 4 * C + 77, 9);
    let a = iir(0.45, -0.75);
    let b = iir(0.55, -0.4);
    let g = chain(&[
        ("audio.gain", json!({ "db": -2.0 })),
        (OP_ID, serde_json::to_value(&a).unwrap()),
        (OP_ID, serde_json::to_value(&b).unwrap()),
    ]);
    let plan = compile(&engine, &g, &input);
    let want = render(&engine, &plan, &input);
    let mut fresh = NodeCache::new(512 << 20);
    view_all(&engine, &plan, &input, &mut fresh, &want);
    for after in [1u64, 2, 3, 5, 8, 13, 21, 34, 55] {
        let mut cache = NodeCache::new(512 << 20);
        let missing = engine
            .missing_solves(&plan, &fin(&input), &cache, OutputId(0))
            .unwrap();
        let mut job = PresolveJob::new(missing);
        // 中止する区切りを何度か挟む
        let mut canceled = 0;
        for _ in 0..4 {
            match job.step(
                &engine,
                &plan,
                &fin(&input),
                &mut cache,
                3,
                &CancelAfter::new(after),
            ) {
                Ok(_) => {}
                Err(EngineError::Canceled) => canceled += 1,
                Err(e) => panic!("{after}：{e}"),
            }
        }
        assert!(canceled > 0 || after > 8, "{after}：中止が起きていない");
        while !job.is_done() {
            job.step(&engine, &plan, &fin(&input), &mut cache, 2, &NoMonitor)
                .unwrap();
        }
        for s in job.finished() {
            let t = job.tasks().iter().find(|t| t.key == s.key).unwrap();
            assert_eq!(
                encoded(&engine, t.need.id, t.need.v, &s.out),
                encoded(
                    &engine,
                    t.need.id,
                    t.need.v,
                    &fresh.peek_analysis(&s.key).unwrap()
                ),
                "{after}"
            );
        }
        assert_eq!(
            view_all(&engine, &plan, &input, &mut cache, &want),
            0,
            "{after}"
        );
    }
}

/// 解けた結果を外の解析として登録すると、上限 0 のキャッシュでも追い出されず、描画は流し解きをしない（エディタが握る計画の表）。
/// `clear` で外れる。
#[test]
fn external_analyses_are_kept_and_used() {
    let engine = stateful_engine().unwrap();
    let input = noise(1, 3 * C + 5, 77);
    let p = iir(0.35, -0.85);
    let g = chain(&[
        ("audio.gain", json!({ "db": 1.0 })),
        (OP_ID, serde_json::to_value(&p).unwrap()),
    ]);
    let plan = compile(&engine, &g, &input);
    let want = render(&engine, &plan, &input);
    let mut cache = NodeCache::new(512 << 20);
    let mut job = PresolveJob::new(
        engine
            .missing_solves(&plan, &fin(&input), &cache, OutputId(0))
            .unwrap(),
    );
    while !job
        .step(&engine, &plan, &fin(&input), &mut cache, 100, &NoMonitor)
        .unwrap()
        .done
    {}
    let mut small = NodeCache::new(0);
    for s in job.finished() {
        assert_eq!(
            small.register_external_analysis(s.key, s.out.clone(), s.bytes),
            s.bytes
        );
        assert!(small.contains_analysis(&s.key));
    }
    assert!(small.bytes() >= job.finished().iter().map(|s| s.bytes).sum::<u64>());
    let before = solves(&p.state());
    assert_eq!(view_all(&engine, &plan, &input, &mut small, &want), 0);
    assert_eq!(solves(&p.state()), before, "表を解き直した");
    for s in job.finished() {
        assert!(small.contains_analysis(&s.key), "追い出された");
    }
    // ふつうの項目に戻すと、上限 0 では残らない（上限のあるキャッシュでは残る）
    let key = job.finished()[0].key;
    assert!(small.demote_external_analysis(&key));
    assert!(!small.contains_analysis(&key));
    let mut roomy = NodeCache::new(64 << 20);
    roomy.register_external_analysis(key, job.finished()[0].out.clone(), job.finished()[0].bytes);
    assert!(roomy.demote_external_analysis(&key));
    assert!(roomy.contains_analysis(&key));
    assert!(!roomy.demote_external_analysis(&key));
    small.register_external_analysis(key, job.finished()[0].out.clone(), job.finished()[0].bytes);
    // 外すと数えなくなる
    assert_eq!(
        small.unregister_external_analysis(&key),
        Some(job.finished()[0].bytes)
    );
    assert!(!small.contains_analysis(&key));
    small.register_external_analysis(key, job.finished()[0].out.clone(), 7);
    small.clear();
    assert!(!small.contains_analysis(&key));
    assert_eq!(small.bytes(), 0);
}

/// 長さ 0 の入力は区切り 1 回で解ける（チャンクの数 0 でも）。仕事を作った計画と違う計画で進めると内部の誤り。キャッシュに
/// あるものは仕事にならない。
#[test]
fn edges() {
    let engine = stateful_engine().unwrap();
    let empty = noise(2, 0, 1);
    let g = chain(&[
        ("audio.gain", json!({ "db": -1.0 })),
        (OP_ID, serde_json::to_value(iir(0.4, -0.7)).unwrap()),
    ]);
    let plan = compile(&engine, &g, &empty);
    let mut cache = NodeCache::new(64 << 20);
    let missing = engine
        .missing_solves(&plan, &fin(&empty), &cache, OutputId(0))
        .unwrap();
    assert_eq!(missing.tasks.len(), 1);
    let mut job = PresolveJob::new(missing);
    assert!(
        job.step(&engine, &plan, &fin(&empty), &mut cache, 0, &NoMonitor)
            .unwrap()
            .done
    );
    // 違う計画
    let input = noise(2, 2 * C, 3);
    let plan_a = compile(&engine, &g, &input);
    let g2 = chain(&[
        ("audio.gain", json!({ "db": -2.0 })),
        (OP_ID, serde_json::to_value(iir(0.4, -0.7)).unwrap()),
    ]);
    let plan_b = compile(&engine, &g2, &input);
    let mut cache = NodeCache::new(64 << 20);
    let mut job = PresolveJob::new(
        engine
            .missing_solves(&plan_a, &fin(&input), &cache, OutputId(0))
            .unwrap(),
    );
    assert!(matches!(
        job.step(&engine, &plan_b, &fin(&input), &mut cache, 1, &NoMonitor),
        Err(EngineError::Internal(_))
    ));
    // 解けたものは仕事にならない（下流の gain だけを変えた計画も同じ鍵）
    while !job
        .step(&engine, &plan_a, &fin(&input), &mut cache, 1, &NoMonitor)
        .unwrap()
        .done
    {}
    let g3 = {
        let mut g = g.clone();
        g.push_linear("audio.gain", 1, json!({ "db": 4.0 }), 9)
            .unwrap();
        g
    };
    let plan_c = compile(&engine, &g3, &input);
    let m = engine
        .missing_solves(&plan_c, &fin(&input), &cache, OutputId(0))
        .unwrap();
    assert!(m.tasks.is_empty());
    assert_eq!(m.needed.len(), 1);
    assert!(m.needed[0].1 > 0);
}

/// ノードの区間の見積もり（区切りごとの確保）：区間を広げると増え、全体の流し（`stream`）の見積もりのピーク以下。
#[test]
fn node_area_estimate_grows_with_the_span() {
    let engine = stateful_engine().unwrap();
    let input = noise(2, 40 * C, 5);
    let g = chain(&[
        ("audio.gain", json!({ "db": -3.0 })),
        (
            "audio.fade",
            json!({ "fadeIn": 100, "fadeOut": 0, "curve": "linear" }),
        ),
        (OP_ID, serde_json::to_value(iir(0.4, -0.7)).unwrap()),
    ]);
    let plan = compile(&engine, &g, &input);
    let port = {
        let missing = engine
            .missing_solves(&plan, &fin(&input), &NodeCache::new(0), OutputId(0))
            .unwrap();
        let job = PresolveJob::new(missing);
        let (port, span) = job.next_span(4).unwrap();
        assert_eq!(span, (0, 4 * C));
        port
    };
    let est = |chunks: u64| {
        engine
            .estimate(
                &plan,
                &EstimateSpec::new(3)
                    .all_inputs(&plan, Source::Held)
                    .port_area(port, Area::span(0, chunks * C)),
                None,
            )
            .unwrap()
            .peak
    };
    let (one, eight) = (est(1), est(8));
    assert!(one > 0 && eight > one, "{one} {eight}");
    assert!(eight >= 8 * C * 2 * 4, "{eight}");
}
