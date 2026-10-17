//! 解析の適合テスト（docs/editing.md §17 の E1・E24・E25 の解析版と、正規の形・引数・受け付ける種類の約束）。
//! 登録したすべての解析（今の版・旧版）のすべての例に、何もしなくても当たる。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::sync::Arc;

use kura_engine::analysis::{AnalysisRequest, Analyzed, DynAnalysis, DynAnalysisExample};
use kura_engine::monitor::{CancelAfter, CancelFlag, Monitor};
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{EngineError, IssueCode, Limits, NoMonitor, Shape, Value, ValueKind};
use serde_json::json;

fn engine() -> Engine {
    Engine::new().expect("登録表が作れること（同じ解析・版が 2 つない、申告した解析がある）")
}

fn request(a: &Arc<dyn DynAnalysis>, params: &serde_json::Value) -> AnalysisRequest {
    AnalysisRequest {
        id: a.id().to_string(),
        v: Some(a.version()),
        params: params.clone(),
    }
}

fn each_example(mut f: impl FnMut(&Engine, &Arc<dyn DynAnalysis>, &DynAnalysisExample, &Value)) {
    let engine = engine();
    let all = engine.analyses().all();
    assert!(!all.is_empty());
    for a in all {
        let examples = a.examples().unwrap();
        assert!(!examples.is_empty(), "{} に例がありません", a.id());
        let mut names = BTreeSet::new();
        for ex in &examples {
            assert!(
                names.insert(ex.name),
                "{} の例の名前が重複しています（{}）",
                a.id(),
                ex.name
            );
            let v = ex.input.generate().unwrap();
            assert!(
                a.accepts().contains(&v.kind()),
                "{} {}：受け付けない種類の例です",
                a.id(),
                ex.name
            );
            f(&engine, &a, ex, &v);
        }
    }
}

fn run(
    engine: &Engine,
    a: &Arc<dyn DynAnalysis>,
    ex: &DynAnalysisExample,
    v: &Value,
    mon: &dyn Monitor,
) -> Result<Analyzed, EngineError> {
    engine.analyze_dyn(&request(a, &ex.params), v, mon)
}

/// E1（解析）：スレッド数 1・3・16 で正規の形のバイト列が同じ。結果は見積もりの大きさ以下で、正規の形が往復する。
#[test]
fn analyses_are_deterministic_bounded_and_round_trip() {
    each_example(|engine, a, ex, v| {
        let mut outs = Vec::new();
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let out = pool
                .install(|| run(engine, a, ex, v, &NoMonitor))
                .unwrap_or_else(|e| panic!("{} {}: {e}", a.id(), ex.name));
            outs.push(out);
        }
        assert!(
            outs.windows(2).all(|w| w[0] == w[1]),
            "{} {}：スレッド数で結果が変わった",
            a.id(),
            ex.name
        );
        let out = &outs[0];
        assert_eq!(out.encoding, a.encoding());
        assert_eq!((out.id, out.version), (a.id(), a.version()));
        assert_eq!(out.params, ex.params, "{} {}", a.id(), ex.name);
        let estimate = a.estimate(&ex.params, &v.shape()).unwrap();
        assert!(
            out.output.len() as u64 <= estimate.bytes,
            "{} {}：結果 {} バイトが見積もり {} を超えた",
            a.id(),
            ex.name,
            out.output.len(),
            estimate.bytes
        );
        assert!(
            a.round_trip(&out.output).unwrap(),
            "{} {}：正規の形が往復しない",
            a.id(),
            ex.name
        );
        // 型付きの口に戻しても同じ
        let back = a.decode_any(&out.output).unwrap();
        assert_eq!(a.encode_any(&back).unwrap(), out.output);
        // ダイジェストは版で変わる
        assert_ne!(out.digest().unwrap(), {
            let mut other = out.clone();
            other.version += 1;
            other.digest().unwrap()
        });
    });
}

/// E24（解析）：解析の途中でも中止できる。入口の確認だけでなく、解析の中で中止を確かめていること。
#[test]
fn analyses_can_be_canceled_inside() {
    each_example(|engine, a, ex, v| {
        let counter = CancelAfter::new(0);
        run(engine, a, ex, v, &counter).unwrap();
        let calls = counter.calls();
        // 手間が 0 と見積もる解析（アルファのない画像の不透明な範囲など）は、入口の確認だけでよい
        let cost = a.estimate(&ex.params, &v.shape()).unwrap().cost;
        let least = if cost == 0 { 1 } else { 2 };
        assert!(
            calls >= least,
            "{} {}：解析の中で中止を確かめていません（{calls} 回・手間 {cost}）",
            a.id(),
            ex.name
        );
        for k in 1..=calls.min(4) {
            let r = run(engine, a, ex, v, &CancelAfter::new(k));
            assert!(
                matches!(r, Err(EngineError::Canceled)),
                "{} {}：{k} 回目で中止できない（{r:?}）",
                a.id(),
                ex.name
            );
        }
        let flag = CancelFlag::default();
        flag.cancel();
        assert!(matches!(
            run(engine, a, ex, v, &flag),
            Err(EngineError::Canceled)
        ));
    });
}

/// 上限を `shape` が超えるように絞る。
fn tighter(shape: &Shape) -> Limits {
    let base = Limits::default();
    match *shape {
        Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => Limits {
            max_dim: w.max().max(h.max()) - 1,
            ..base
        },
        Shape::Audio {
            channels, frames, ..
        } => {
            if frames.max() > 0 {
                Limits {
                    max_audio_frames: frames.max() - 1,
                    ..base
                }
            } else {
                Limits {
                    max_channels: u64::from(channels) - 1,
                    ..base
                }
            }
        }
    }
}

/// E25（解析）：上限を超える値・結果の見積もりは、解く前に `Limit` になる（中止の確認も呼ばない＝描いていない）。
#[test]
fn limits_are_checked_before_analyzing() {
    each_example(|_, a, ex, v| {
        let shape = v.shape();
        let tight = Engine::with_limits(tighter(&shape)).unwrap();
        let counter = CancelAfter::new(0);
        let r = tight.analyze_dyn(&request(a, &ex.params), v, &counter);
        assert!(
            matches!(r, Err(EngineError::Limit(_))),
            "{} {}：上限を超える値を解いた（{r:?}）",
            a.id(),
            ex.name
        );
        assert_eq!(counter.calls(), 0, "{} {}", a.id(), ex.name);
        // 結果の見積もり（か値の大きさ）が上限を超えるとき
        let estimate = a.estimate(&ex.params, &shape).unwrap();
        let small = Engine::with_limits(Limits {
            max_value_bytes: estimate.bytes.saturating_sub(1),
            ..Limits::default()
        })
        .unwrap();
        let counter = CancelAfter::new(0);
        let r = small.analyze_dyn(&request(a, &ex.params), v, &counter);
        assert!(
            matches!(r, Err(EngineError::Limit(_))),
            "{} {}：結果の見積もりが上限を超えるのに解いた（{r:?}）",
            a.id(),
            ex.name
        );
        assert_eq!(counter.calls(), 0, "{} {}", a.id(), ex.name);
    });
}

/// 引数：記述の key と serde のフィールド名が同じ、足りない項目は既定値、知らない項目は拒む、例の引数は正規の形。
#[test]
fn params_take_defaults_and_reject_unknown_keys() {
    let engine = engine();
    for a in engine.analyses().all() {
        let defaults = a.check_params(&a.schema().defaults()).unwrap();
        assert_eq!(
            a.check_params(&json!({})).unwrap(),
            defaults,
            "{}：足りない項目が既定値にならない",
            a.id()
        );
        let keys: BTreeSet<String> = defaults
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let schema_keys: BTreeSet<String> = a
            .schema()
            .params
            .iter()
            .map(|p| p.key.to_string())
            .collect();
        assert_eq!(
            keys,
            schema_keys,
            "{}：引数の記述の key と serde のフィールド名が違います",
            a.id()
        );
        let mut unknown = defaults.clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("__unknown".into(), json!(1));
        let issues = a.check_params(&unknown).unwrap_err();
        assert!(issues.has_errors(), "{}", a.id());
        assert!(a.check_params(&json!([])).is_err(), "{}", a.id());
        for ex in a.examples().unwrap() {
            assert_eq!(
                a.check_params(&ex.params).unwrap(),
                ex.params,
                "{} {}",
                a.id(),
                ex.name
            );
        }
        // 範囲の外も拒む（記述にある範囲）
        for spec in a.schema().params {
            if let kura_engine::op::ParamKind::Int { max, .. } = spec.kind {
                let mut over = defaults.clone();
                over.as_object_mut()
                    .unwrap()
                    .insert(spec.key.into(), json!(max + 1));
                let issues = a.check_params(&over).unwrap_err();
                assert!(
                    issues.0.iter().any(|i| i.code == IssueCode::OutOfRange),
                    "{} {}",
                    a.id(),
                    spec.key
                );
            }
        }
    }
}

/// 受け付けない種類の値は `Invalid`（落ちない）。知らない解析・版も `Invalid`。
#[test]
fn unaccepted_kinds_and_unknown_analyses_are_rejected() {
    let engine = engine();
    let values = [
        InputGen::Noise {
            w: 5,
            h: 4,
            fmt: PixelFormat::Rgba8,
            seed: 1,
        },
        InputGen::NoiseFrames {
            w: 5,
            h: 4,
            fmt: PixelFormat::Rgba8,
            count: 2,
            seed: 2,
        },
        InputGen::Sine {
            rate: 8_000,
            channels: 1,
            frames: 100,
            freq: 440,
            loop_range: None,
        },
    ]
    .map(|g| g.generate().unwrap());
    for a in engine.analyses().all() {
        for v in &values {
            if a.accepts().contains(&v.kind()) {
                continue;
            }
            let r = engine.analyze_dyn(&request(&a, &json!({})), v, &NoMonitor);
            assert!(
                matches!(&r, Err(EngineError::Invalid(i)) if i.0[0].code == IssueCode::BadInput),
                "{} {:?}: {r:?}",
                a.id(),
                v.kind()
            );
        }
    }
    let v = &values[0];
    for (id, ver) in [("image.nothing", None), ("image.opaque_bounds", Some(999))] {
        let r = engine.analyze_dyn(
            &AnalysisRequest {
                id: id.into(),
                v: ver,
                params: json!({}),
            },
            v,
            &NoMonitor,
        );
        assert!(matches!(r, Err(EngineError::Invalid(_))), "{id}: {r:?}");
    }
    // まとめて解く：要求の順に、1 つの失敗はほかに効かない
    let reqs = [
        AnalysisRequest::new("image.opaque_bounds"),
        AnalysisRequest::new("image.nothing"),
        AnalysisRequest::new("audio.loudness_gating"),
    ];
    let outs = engine.analyze_many(v, &reqs, &NoMonitor);
    assert_eq!(outs.len(), 3);
    assert!(outs[0].is_ok() && outs[1].is_err() && outs[2].is_err());
    let flag = CancelFlag::default();
    flag.cancel();
    assert!(
        engine
            .analyze_many(v, &reqs, &flag)
            .iter()
            .all(|r| matches!(r, Err(EngineError::Canceled)))
    );
}

/// カタログ：今の版の内部用でない解析だけ。内部用はカタログに出ない。
#[test]
fn the_catalog_hides_internal_analyses() {
    let engine = engine();
    let catalog = engine.analysis_catalog();
    for a in engine.analyses().current() {
        let listed = catalog.iter().any(|d| d.id == a.id());
        assert_eq!(listed, !a.internal(), "{}", a.id());
    }
    assert!(catalog.iter().all(|d| !d.accepts.is_empty()));
    // 受け付ける種類はすべて値の種類
    for a in engine.analyses().all() {
        for k in a.accepts() {
            assert!(matches!(
                k,
                ValueKind::Image | ValueKind::Frames | ValueKind::Audio
            ));
        }
    }
}
