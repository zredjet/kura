//! テスト・ゴールデン・devtools で使う補助：操作 1 つの文書を作って描く。

use std::collections::BTreeMap;

use crate::digest::{Digest, Hasher};
use crate::doc::{Graph, InputId};
use crate::error::EngineError;
use crate::monitor::{Monitor, NoMonitor};
use crate::op::DynExample;
use crate::render::{Engine, InputValue};
use crate::testgen::InputGen;
use crate::value::Value;

/// 例の入力の作り方から、元のハッシュの代わりのダイジェストを作る（種に使う）。
pub fn source_of(gens: &[InputGen]) -> Result<Digest, EngineError> {
    let bytes = crate::digest::canonical(&gens)?;
    Ok(Hasher::new("kura-testgen-v1")
        .bytes_with_len(&bytes)
        .finish())
}

/// 入力 1 つの文書に操作 1 つ（種 0）をつないだもの。
pub fn single_node_graph(
    op: &str,
    v: u16,
    params: serde_json::Value,
) -> Result<Graph, EngineError> {
    let mut g = Graph::new_single();
    g.push_linear(op, v, params, 0)?;
    Ok(g)
}

/// 文書を値に当てて、描いた結果をすべて（警告を含めて）返す。
pub fn render_full(
    engine: &Engine,
    graph: &Graph,
    input: Value,
    source: Digest,
    mon: &dyn Monitor,
) -> Result<crate::render::Rendered, EngineError> {
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    let plan = engine.compile(graph, &shapes)?;
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input,
            source,
        },
    )]);
    engine.render(&plan, &inputs, mon)
}

/// 文書を値に当てて、出力 0 の値を返す。
pub fn render_with(
    engine: &Engine,
    graph: &Graph,
    input: Value,
    source: Digest,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    let mut rendered = render_full(engine, graph, input, source, mon)?;
    if rendered.outputs.is_empty() {
        return Err(EngineError::Internal("出力がありません".into()));
    }
    Ok(rendered.outputs.swap_remove(0).value)
}

/// 操作 1 つを値に当てる（既定のエンジン、元のハッシュは 0）。
pub fn render_value(
    op: &str,
    v: u16,
    params: serde_json::Value,
    input: Value,
) -> Result<Value, EngineError> {
    let engine = Engine::new()?;
    render_with(
        &engine,
        &single_node_graph(op, v, params)?,
        input,
        Digest::default(),
        &NoMonitor,
    )
}

/// 入力の作り方から値を作って、操作 1 つを当てる。
pub fn render_single(
    op: &str,
    v: u16,
    params: serde_json::Value,
    inputs: &[InputGen],
) -> Result<Value, EngineError> {
    let first = inputs
        .first()
        .ok_or_else(|| EngineError::Internal("入力がありません".into()))?;
    let engine = Engine::new()?;
    render_with(
        &engine,
        &single_node_graph(op, v, params)?,
        first.generate()?,
        source_of(inputs)?,
        &NoMonitor,
    )
}

/// 例を描いて、出力のダイジェストを返す（ゴールデンに使う）。
pub fn example_digest(
    engine: &Engine,
    op: &str,
    v: u16,
    example: &DynExample,
) -> Result<Digest, EngineError> {
    let first = example
        .inputs
        .first()
        .ok_or_else(|| EngineError::Internal("例に入力がありません".into()))?;
    let out = render_with(
        engine,
        &single_node_graph(op, v, example.params.clone())?,
        first.generate()?,
        source_of(&example.inputs)?,
        &NoMonitor,
    )?;
    out.digest()
}
