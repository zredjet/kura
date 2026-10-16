//! 版と作り直しの契約（docs/editing.md §8.4・§17 の E3・E5・E6・E7）。試験用の操作を登録したエンジンで確かめる。
//!
//! 試験用の操作 `test.shift`（音声の標本に値を足す）：
//! - v1：`delta` を足す（結果を変える修正の前。旧版として残す）
//! - v2：`delta` の 2 倍を足す（修正後。のちに引数の名前だけ変えて v3 に移し、実装は消した）
//! - v3：`amount` の 2 倍を足す（今の版。v2 の引数を移せる）
//!
//! 試験用の操作 `test.noise`：位置で決まる乱数（ChaCha8）を足す（E5 の、並び・件数に依存しないことの確かめ）。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::doc::{Graph, InputId, compose};
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Registry, Resolved, Signature, Unit, spec,
};
use kura_engine::render::{Engine, InputValue};
use kura_engine::shape::{Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::value::{Audio, Value};
use kura_engine::{Digest, EngineError, Issues, Limits, NoMonitor};
use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;

const SIG: Signature = Signature {
    inputs: &[PortSpec {
        name: "audio",
        accepts: &[ValueKind::Audio],
    }],
    lift: Lift::None,
    numeric: Numeric::Float,
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

fn map_audio(
    input: &Value,
    cx: &ApplyCx<'_>,
    f: impl Fn(usize, usize, f32) -> f32,
) -> Result<Applied, EngineError> {
    cx.check()?;
    let Value::Audio(a) = input else {
        return Err(EngineError::Internal("音声ではありません".into()));
    };
    let planes = a
        .planes()
        .iter()
        .enumerate()
        .map(|(c, p)| {
            Arc::new(
                p.iter()
                    .enumerate()
                    .map(|(i, s)| f(c, i, *s))
                    .collect::<Vec<f32>>(),
            )
        })
        .collect();
    Ok(Applied::value(Value::Audio(Arc::new(Audio::new(
        a.rate(),
        planes,
        a.meta.clone(),
    )?))))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeltaParams {
    delta: f64,
}
impl OpParams for DeltaParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "delta",
                ParamKind::Float {
                    min: -1.0,
                    max: 1.0,
                    step: 0.01,
                },
                Unit::None,
                json!(0.0),
            )],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AmountParams {
    amount: f64,
}
impl OpParams for AmountParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "amount",
                ParamKind::Float {
                    min: -1.0,
                    max: 1.0,
                    step: 0.01,
                },
                Unit::None,
                json!(0.0),
            )],
        }
    }
}

fn example<P>(params: P) -> Vec<Example<P>> {
    vec![Example {
        name: "noise",
        params,
        inputs: vec![InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: 100,
            seed: 1,
        }],
    }]
}

/// v1：delta を足す（旧版）。
struct ShiftV1;
impl Op for ShiftV1 {
    const ID: &'static str = "test.shift";
    const VERSION: u16 = 1;
    type Params = DeltaParams;
    fn signature() -> Signature {
        SIG
    }
    fn infer(_: &DeltaParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &DeltaParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        map_audio(inputs[0], cx, |_, _, s| (f64::from(s) + p.delta) as f32)
    }
    fn examples() -> Vec<Example<DeltaParams>> {
        example(DeltaParams { delta: 0.25 })
    }
}

/// v2：delta の 2 倍を足す（実装は消して、v3 の migrate で移す。ここでは期待値を作るためだけに使う）。
struct ShiftV2;
impl Op for ShiftV2 {
    const ID: &'static str = "test.shift";
    const VERSION: u16 = 2;
    type Params = DeltaParams;
    fn signature() -> Signature {
        SIG
    }
    fn infer(_: &DeltaParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &DeltaParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        map_audio(inputs[0], cx, |_, _, s| {
            (f64::from(s) + 2.0 * p.delta) as f32
        })
    }
    fn examples() -> Vec<Example<DeltaParams>> {
        example(DeltaParams { delta: 0.25 })
    }
}

/// v3：amount の 2 倍を足す（今の版。v2 の引数を名前だけ変えて移す）。
struct ShiftV3;
impl Op for ShiftV3 {
    const ID: &'static str = "test.shift";
    const VERSION: u16 = 3;
    type Params = AmountParams;
    fn signature() -> Signature {
        SIG
    }
    fn infer(_: &AmountParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &AmountParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        map_audio(inputs[0], cx, |_, _, s| {
            (f64::from(s) + 2.0 * p.amount) as f32
        })
    }
    fn migrate(from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String> {
        match from {
            2 => Ok(json!({ "amount": raw.get("delta").cloned().unwrap_or(json!(0.0)) })),
            _ => Err(format!("版 {from} からは移せません")),
        }
    }
    fn examples() -> Vec<Example<AmountParams>> {
        example(AmountParams { amount: 0.25 })
    }
}

/// 位置で決まる乱数を足す。
struct Noise;
impl Op for Noise {
    const ID: &'static str = "test.noise";
    const VERSION: u16 = 1;
    type Params = AmountParams;
    fn signature() -> Signature {
        SIG
    }
    fn infer(_: &AmountParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &AmountParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        let seed_cx = cx.seed;
        map_audio(inputs[0], cx, |c, i, s| {
            let mut r = kura_engine::determinism::rng_at(seed_cx, c as u64, i as u128);
            let n = f64::from(r.next_u32()) / 4_294_967_296.0 - 0.5;
            (f64::from(s) + p.amount * n) as f32
        })
    }
    fn examples() -> Vec<Example<AmountParams>> {
        example(AmountParams { amount: 0.1 })
    }
}

/// 登録表に操作を足す関数。
type Add = fn(&mut Registry) -> Result<(), String>;

fn engine_with(current: &[Add]) -> Engine {
    let mut r = Registry::empty();
    for add in current {
        add(&mut r).unwrap();
    }
    Engine::with_registry(r, Limits::default())
}

fn render(engine: &Engine, g: &Graph, input: &Value, source: Digest) -> Value {
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .unwrap();
    let mut out = engine
        .render(
            &plan,
            &BTreeMap::from([(
                InputId(0),
                InputValue {
                    value: input.clone(),
                    source,
                },
            )]),
            &NoMonitor,
        )
        .unwrap();
    out.outputs.swap_remove(0).value
}

fn doc(op: &str, v: u16, params: serde_json::Value) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear(op, v, params, 0).unwrap();
    g
}

fn input() -> Value {
    InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 500,
        seed: 9,
    }
    .generate()
    .unwrap()
}

/// 今の登録表：v1 は旧版、v3 が今の版（v2 の実装は消した）、test.noise。
fn today() -> Engine {
    engine_with(&[
        |r| r.add_current::<ShiftV3>(),
        |r| r.add_legacy::<ShiftV1>(),
        |r| r.add_current::<Noise>(),
    ])
}

/// E6：結果を変えた操作の旧版のノードは、旧版の実装で描かれる。
#[test]
fn legacy_nodes_render_with_the_legacy_implementation() {
    let then = engine_with(&[|r| r.add_current::<ShiftV1>()]);
    let g = doc("test.shift", 1, json!({ "delta": 0.25 }));
    assert_eq!(
        render(&today(), &g, &input(), Digest::default()),
        render(&then, &g, &input(), Digest::default())
    );
    assert!(
        matches!(today().registry().resolve("test.shift", 1, &json!({ "delta": 0.25 })), Ok(Resolved::Direct(i, _)) if i.version() == 1)
    );
}

/// E7：引数の形だけを変えて実装を消した版（v2）は、今の版へ移して描いても、当時の v2 の結果と同じ。
#[test]
fn migrated_versions_render_like_the_removed_implementation() {
    let then = engine_with(&[
        |r| r.add_current::<ShiftV2>(),
        |r| r.add_legacy::<ShiftV1>(),
    ]);
    let g = doc("test.shift", 2, json!({ "delta": 0.125 }));
    let now = today();
    assert!(
        matches!(now.registry().resolve("test.shift", 2, &json!({ "delta": 0.125 })), Ok(Resolved::Migrated(i, _)) if i.version() == 3)
    );
    assert_eq!(
        render(&now, &g, &input(), Digest::default()),
        render(&then, &g, &input(), Digest::default())
    );
    // 保存して読み直した文書でも同じ
    let reloaded = now.load(&g.to_canonical_json().unwrap()).unwrap();
    assert_eq!(
        render(&now, &reloaded, &input(), Digest::default()),
        render(&then, &g, &input(), Digest::default())
    );
    // 移せない古い版・新しすぎる版は読み込みで断る（E8）
    assert!(
        now.load(&doc("test.shift", 0, json!({})).to_canonical_json().unwrap())
            .is_err()
    );
    assert!(
        now.load(&doc("test.shift", 4, json!({})).to_canonical_json().unwrap())
            .is_err()
    );
}

/// E3：一括加工の継ぎ足しは「元＋編集＋レシピ」の累積の文書で、編集の結果にレシピを当てたものと同じ。
#[test]
fn composed_documents_equal_applying_the_recipe_to_the_edit() {
    let engine = today();
    let source = Digest(*blake3::hash(b"source").as_bytes());
    let mut edit = doc("test.noise", 1, json!({ "amount": 0.2 }));
    edit.nodes[0].seed = 11;
    let mut recipe = Graph::new_single();
    recipe
        .push_linear("test.shift", 3, json!({ "amount": 0.1 }), 0)
        .unwrap();
    recipe
        .push_linear("test.noise", 1, json!({ "amount": 0.05 }), 12)
        .unwrap();
    let composed = compose(&edit, &recipe).unwrap();
    let step = render(&engine, &edit, &input(), source);
    let two_steps = render(&engine, &recipe, &step, source);
    assert_eq!(render(&engine, &composed, &input(), source), two_steps);
}

/// E5：一括の対象の並び・件数を変えても、各アイテムの出力は変わらない（種は元のハッシュとノードの種だけで決まる）。
#[test]
fn batch_order_and_size_do_not_change_results() {
    let engine = today();
    let mut g = doc("test.noise", 1, json!({ "amount": 0.3 }));
    g.nodes[0].seed = 5;
    let items: Vec<(Digest, Value)> = (0..6u64)
        .map(|i| {
            let v = InputGen::AudioNoise {
                rate: 8_000,
                channels: 1,
                frames: 200,
                seed: i,
            }
            .generate()
            .unwrap();
            (Digest(*blake3::hash(&i.to_le_bytes()).as_bytes()), v)
        })
        .collect();
    let forward: Vec<Value> = items
        .iter()
        .map(|(s, v)| render(&engine, &g, v, *s))
        .collect();
    let backward: Vec<Value> = items
        .iter()
        .rev()
        .map(|(s, v)| render(&engine, &g, v, *s))
        .collect();
    let alone: Vec<Value> = items
        .iter()
        .map(|(s, v)| render(&today(), &g, v, *s))
        .collect();
    for i in 0..items.len() {
        assert_eq!(forward[i], backward[items.len() - 1 - i]);
        assert_eq!(forward[i], alone[i]);
    }
    // 元が違えば乱数も違う（種が元のハッシュに結び付いている）
    assert_ne!(forward[0], render(&engine, &g, &items[0].1, items[1].0));
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CountParams {
    count: u32,
}
impl OpParams for CountParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "count",
                ParamKind::Int {
                    min: 0,
                    max: 100,
                    step: 1,
                },
                Unit::None,
                json!(0),
            )],
        }
    }
}

/// `test.step` v2：count × 0.001 を足す（今の版）。v1 の引数 `n` を整数として読んで移す（v1 の実装は消した）。
struct StepV2;
impl Op for StepV2 {
    const ID: &'static str = "test.step";
    const VERSION: u16 = 2;
    type Params = CountParams;
    fn signature() -> Signature {
        SIG
    }
    fn infer(_: &CountParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &CountParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        map_audio(inputs[0], cx, |_, _, s| {
            (f64::from(s) + f64::from(p.count) * 0.001) as f32
        })
    }
    fn migrate(from: u16, raw: serde_json::Value) -> Result<serde_json::Value, String> {
        match (from, raw.get("n").and_then(serde_json::Value::as_u64)) {
            (1, Some(n)) => Ok(json!({ "count": n })),
            _ => Err(format!("版 {from} の引数 {raw} は移せません")),
        }
    }
    fn examples() -> Vec<Example<CountParams>> {
        example(CountParams { count: 3 })
    }
}

/// 移行は正規の JSON に直した引数に当てる：メモリ上の `5.0`・読み直した文書・正規でない書き方（`5.0`）が同じ結果になる
/// （migrate が整数として読んでも、書き方で結果が分かれない。ADR-145）。
#[test]
fn migration_sees_canonical_params() {
    let engine = engine_with(&[|r| r.add_current::<StepV2>()]);
    let in_memory = doc("test.step", 1, json!({ "n": 5.0 }));
    let canonical = in_memory.to_canonical_json().unwrap();
    let text = String::from_utf8(canonical.clone()).unwrap();
    assert!(text.contains(r#""n":5}"#), "{text}");
    let reloaded = engine.load(&canonical).unwrap();
    let loose = engine
        .load(text.replace(r#""n":5}"#, r#""n":5.0}"#).as_bytes())
        .unwrap();
    let want = render(
        &engine,
        &doc("test.step", 2, json!({ "count": 5 })),
        &input(),
        Digest::default(),
    );
    for g in [&in_memory, &reloaded, &loose] {
        assert_eq!(render(&engine, g, &input(), Digest::default()), want);
    }
}
