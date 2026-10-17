//! 操作が解析を申告する口（docs/editing.md §11.2）と、解析の版の契約。試験用の操作・解析を登録したエンジンで確かめる。
//!
//! 試験用の解析 `test.count`（音声のフレーム数を数える）：v1 はフレーム数、v2 はその 2 倍（結果を変えたので版を上げた。
//! v1 は旧版の操作が申告しているので legacy に残す）。
//! 試験用の操作 `test.uses`（解析の値を音声の先頭の標本に書く）：v1 は `test.count` v1、v2 は v2 を申告する。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use kura_engine::analysis::{
    Analysis, AnalysisExample, AnalysisRegistry, AnalysisRequest, AnalyzeCx, AnyOutput,
    DynAnalysis, Estimate, Need, NoParams, Solve, Solver,
};
use kura_engine::doc::{Graph, InputId, NodeId};
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, PortSpec,
    Registry, Signature,
};
use kura_engine::render::{Engine, InputValue};
use kura_engine::shape::{Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::value::{Audio, Value};
use kura_engine::{EngineError, IssueCode, Issues, Limits, NoMonitor};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Count {
    frames: u64,
}
kura_engine::json_output!(Count);

macro_rules! count {
    ($name:ident, $v:expr, $mul:expr) => {
        struct $name;
        impl Analysis for $name {
            const ID: &'static str = "test.count";
            const VERSION: u16 = $v;
            const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
            const NUMERIC: Numeric = Numeric::Exact;
            type Params = NoParams;
            type Output = Count;
            fn run(_: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<Count, EngineError> {
                cx.check()?;
                let Value::Audio(a) = v else {
                    return Err(EngineError::Internal("音声ではない".into()));
                };
                Ok(Count {
                    frames: a.frames() as u64 * $mul,
                })
            }
            fn examples() -> Vec<AnalysisExample<NoParams>> {
                vec![AnalysisExample {
                    name: "noise",
                    params: NoParams {},
                    input: InputGen::AudioNoise {
                        rate: 8_000,
                        channels: 1,
                        frames: 10,
                        seed: 1,
                    },
                }]
            }
            fn estimate(_: &NoParams, _: &Shape) -> Estimate {
                Estimate { bytes: 64, cost: 1 }
            }
        }
    };
}
count!(CountV1, 1, 1);
count!(CountV2, 2, 2);

const SIG: Signature = Signature {
    inputs: &[PortSpec {
        name: "audio",
        accepts: &[ValueKind::Audio],
    }],
    lift: Lift::None,
    numeric: Numeric::Exact,
    meta: MetaAccess::Map,
};

fn same_shape(inputs: &[Shape]) -> Result<Inferred, Issues> {
    Ok(Inferred {
        shape: inputs[0],
        space: SpaceMap::Identity,
        time: TimeMap::Identity,
        cost: 0,
    })
}

/// 音声の先頭の標本を `x` にする。
fn write_first(input: &Value, x: u64) -> Result<Applied, EngineError> {
    let Value::Audio(a) = input else {
        return Err(EngineError::Internal("音声ではない".into()));
    };
    let mut p = (*a.planes()[0]).clone();
    p[0] = x as f32;
    let mut planes = a.planes().to_vec();
    planes[0] = Arc::new(p);
    Ok(Applied::value(Value::Audio(Arc::new(Audio::new(
        a.rate(),
        planes,
        a.meta.clone(),
    )?))))
}

fn examples() -> Vec<Example<NoParams>> {
    vec![Example {
        name: "noise",
        params: NoParams {},
        inputs: vec![InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 10,
            seed: 1,
        }],
    }]
}

/// 解析 `A` を申告して、その値を書く操作。`declare`：NEEDS、`ask`：needs が返すか、`use_it`：apply で使うか。
macro_rules! user {
    ($name:ident, $id:expr, $v:expr, $a:ty, declare: $declare:expr, ask: $ask:expr, use_it: $use:expr) => {
        struct $name;
        impl Op for $name {
            const ID: &'static str = $id;
            const VERSION: u16 = $v;
            type Params = NoParams;
            const NEEDS: &'static [(&'static str, u16)] = $declare;
            fn needs(_: &NoParams, _: &[Shape]) -> Result<Vec<Need>, Issues> {
                Ok(if $ask {
                    vec![Need::of::<$a>(0, &NoParams {})?]
                } else {
                    Vec::new()
                })
            }
            fn signature() -> Signature {
                SIG
            }
            fn infer(_: &NoParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
                same_shape(inputs)
            }
            fn apply(
                _: &NoParams,
                inputs: &[&Value],
                cx: &mut ApplyCx<'_>,
            ) -> Result<Applied, EngineError> {
                cx.check()?;
                let n = if $use {
                    cx.analysis::<$a>(0, inputs[0], &NoParams {})?.frames
                } else {
                    7
                };
                write_first(inputs[0], n)
            }
            fn examples() -> Vec<Example<NoParams>> {
                examples()
            }
        }
    };
}
user!(UsesV1, "test.uses", 1, CountV1, declare: &[("test.count", 1)], ask: true, use_it: true);
user!(UsesV2, "test.uses", 2, CountV2, declare: &[("test.count", 2)], ask: true, use_it: true);
// NEEDS にない（ID, 版）を needs が返す
user!(Undeclared, "test.undeclared", 1, CountV2, declare: &[("test.count", 1)], ask: true, use_it: false);
// 申告せずに使う
user!(Sneaky, "test.sneaky", 1, CountV1, declare: &[], ask: false, use_it: true);
// NEEDS にはあるが needs が返さないのに使う（needs と apply の食い違い）
user!(Mismatch, "test.mismatch", 1, CountV1, declare: &[("test.count", 1)], ask: false, use_it: true);
// 登録されていない版を申告する
user!(Missing, "test.missing", 1, CountV1, declare: &[("test.count", 3)], ask: false, use_it: false);

/// フレームに持ち上げる操作は申告できない。
struct Lifted;
impl Op for Lifted {
    const ID: &'static str = "test.lifted";
    const VERSION: u16 = 1;
    type Params = NoParams;
    const NEEDS: &'static [(&'static str, u16)] = &[("test.count", 1)];
    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image, ValueKind::Frames],
            }],
            lift: Lift::PerFrame,
            ..SIG
        }
    }
    fn infer(_: &NoParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(_: &NoParams, inputs: &[&Value], _: &mut ApplyCx<'_>) -> Result<Applied, EngineError> {
        Ok(Applied::value(inputs[0].clone()))
    }
    fn examples() -> Vec<Example<NoParams>> {
        Vec::new()
    }
}

fn analyses() -> AnalysisRegistry {
    let mut a = AnalysisRegistry::empty();
    a.add_current::<CountV2>().unwrap();
    a.add_legacy::<CountV1>().unwrap();
    a
}

fn engine() -> Engine {
    let mut r = Registry::empty();
    r.add_current::<UsesV2>().unwrap();
    r.add_legacy::<UsesV1>().unwrap();
    r.add_current::<Undeclared>().unwrap();
    r.add_current::<Sneaky>().unwrap();
    r.add_current::<Mismatch>().unwrap();
    Engine::with_registries(r, analyses(), Limits::default()).unwrap()
}

fn doc(op: &str, v: u16) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear(op, v, json!({}), 0).unwrap();
    g
}

fn input() -> Value {
    InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 50,
        seed: 3,
    }
    .generate()
    .unwrap()
}

fn shapes(v: &Value) -> BTreeMap<InputId, Shape> {
    BTreeMap::from([(InputId(0), v.shape())])
}

fn render(
    engine: &Engine,
    g: &Graph,
    v: &Value,
    solver: &dyn Solver,
) -> Result<Value, EngineError> {
    let plan = engine.compile(g, &shapes(v))?;
    let mut out = engine.render_with_solver(
        &plan,
        &BTreeMap::from([(
            InputId(0),
            InputValue {
                value: v.clone(),
                source: Default::default(),
            },
        )]),
        &NoMonitor,
        solver,
    )?;
    Ok(out.outputs.swap_remove(0).value)
}

fn first(v: &Value) -> f32 {
    let Value::Audio(a) = v else { panic!() };
    a.planes()[0][0]
}

#[derive(Default)]
struct Recording {
    seen: Mutex<Vec<(NodeId, Need)>>,
}

impl Solver for Recording {
    fn solve(
        &self,
        node: NodeId,
        need: &Need,
        v: &Value,
        analysis: &dyn DynAnalysis,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError> {
        self.seen.lock().unwrap().push((node, need.clone()));
        Solve.solve(node, need, v, analysis, cx)
    }
}

/// 申告した解析は描く前に解かれ、操作はその値を使う。旧版の操作は旧版の解析で描かれる（解析の legacy）。
#[test]
fn declared_analyses_are_solved_first_and_versions_are_kept() {
    let engine = engine();
    let v = input();
    let rec = Recording::default();
    let out = render(&engine, &doc("test.uses", 2), &v, &rec).unwrap();
    assert_eq!(first(&out), 100.0);
    let seen = rec.seen.into_inner().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].1.id, seen[0].1.v), ("test.count", 2));
    // 旧版の操作は、申告した旧版の解析（v1）の値で描く
    let out = render(&engine, &doc("test.uses", 1), &v, &Solve).unwrap();
    assert_eq!(first(&out), 50.0);
    // 計画に申告が載る
    let g = doc("test.uses", 2);
    let plan = engine.compile(&g, &shapes(&v)).unwrap();
    assert_eq!(plan.node_needs(g.nodes[0].id).unwrap().len(), 1);
    // エンジンの解析の口でも同じ値
    let a = engine
        .analyze_dyn(&AnalysisRequest::new("test.count"), &v, &NoMonitor)
        .unwrap();
    assert_eq!(a.version, 2);
    assert_eq!(String::from_utf8(a.output).unwrap(), r#"{"frames":100}"#);
}

/// 外したノードは解析を解かない。
#[test]
fn disabled_nodes_do_not_solve() {
    let engine = engine();
    let v = input();
    let mut g = doc("test.uses", 2);
    g.nodes[0].enabled = false;
    let rec = Recording::default();
    let out = render(&engine, &g, &v, &rec).unwrap();
    assert_eq!(out, v);
    assert!(rec.seen.into_inner().unwrap().is_empty());
    let plan = engine.compile(&g, &shapes(&v)).unwrap();
    assert!(plan.node_needs(g.nodes[0].id).unwrap().is_empty());
}

/// 申告の誤り：NEEDS にない申告は計画で断る。申告せずに使う・needs と食い違うのは内部の誤り。
#[test]
fn wrong_declarations_are_caught() {
    let engine = engine();
    let v = input();
    let issues = engine
        .compile(&doc("test.undeclared", 1), &shapes(&v))
        .err()
        .unwrap();
    assert_eq!(issues.0[0].code, IssueCode::BadInput);
    assert!(
        issues.0[0].message.contains("NEEDS"),
        "{}",
        issues.0[0].message
    );
    for op in ["test.sneaky", "test.mismatch"] {
        let r = render(&engine, &doc(op, 1), &v, &Solve);
        assert!(matches!(r, Err(EngineError::Internal(_))), "{op}: {r:?}");
    }
}

/// 登録：持ち上げる操作は申告できない。申告した（ID, 版）が登録されていなければエンジンを作れない
/// （表示だけの解析として実装を消した版も、申告されていれば作れない）。
#[test]
fn registration_checks_declarations() {
    assert!(Registry::empty().add_current::<Lifted>().is_err());
    let mut r = Registry::empty();
    r.add_current::<Missing>().unwrap();
    assert!(Engine::with_registries(r, analyses(), Limits::default()).is_err());
    let mut r = Registry::empty();
    r.add_current::<UsesV2>().unwrap();
    r.add_legacy::<UsesV1>().unwrap();
    let mut dropped = AnalysisRegistry::empty();
    dropped.add_current::<CountV2>().unwrap();
    dropped.add_dropped("test.count", 1).unwrap();
    assert!(Engine::with_registries(r, dropped, Limits::default()).is_err());
    // 実装のある版を dropped にはできない
    let mut a = analyses();
    assert!(a.add_dropped("test.count", 1).is_err());
    assert!(a.add_legacy::<CountV1>().is_err());
}

/// 操作を直接呼ぶ（実行器を通さない）ときは、その場で同じ関数で解き、同じ値には覚えた結果を使う。別の値は解き直す。
#[test]
fn direct_calls_solve_in_place_and_reuse() {
    let engine = engine();
    let op = engine
        .registry()
        .all()
        .into_iter()
        .find(|o| o.id() == "test.uses" && o.version() == 2)
        .unwrap();
    let params = op.parse(&json!({})).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, 1);
    let v = input();
    let out = op.apply(params.as_ref(), &[&v], &mut cx).unwrap();
    assert_eq!(first(&out.value), 100.0);
    assert_eq!(cx.analyses_solved_in_place(), 1);
    op.apply(params.as_ref(), &[&v], &mut cx).unwrap();
    assert_eq!(cx.analyses_solved_in_place(), 1);
    let longer = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 70,
        seed: 3,
    }
    .generate()
    .unwrap();
    let out = op.apply(params.as_ref(), &[&longer], &mut cx).unwrap();
    assert_eq!(first(&out.value), 140.0);
    assert_eq!(cx.analyses_solved_in_place(), 2);
    // 申告していない操作から呼ぶと内部の誤り
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, 1);
    assert!(matches!(
        cx.analysis::<CountV1>(0, &v, &NoParams {}),
        Err(EngineError::Internal(_))
    ));
}
