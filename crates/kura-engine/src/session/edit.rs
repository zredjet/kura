//! 手を確定した頭に当てる（段 10）。検証は変えたところだけで行う。
//!
//! - 形（`Graph::check_structure` と同じ規則）：足すノードの ID は `next_id` から払い出し、構造の手は線形の列だけで受け、
//!   つなぎ直したノードの入力が前のノードか文書の入力を読むことを確かめる。
//! - 読み直せること（E8）：変えたノードの（操作, 版, 引数）が実装に結び付くこと。その版の実装があれば正規の形の引数で
//!   必ず通るので、移行するノードだけ `Registry::resolve` に通す。
//! - 大きさと深さ（E25）：変えた値を文書の中と同じ深さで数え、文書全体の長さを差分で求めて `MAX_DOC_BYTES` と照らす。
//!
//! 誤りの順は、参照の実装（文書を写して全体を確かめる）と同じ：形 → 値の深さ・大きさ → 文書の大きさ → 読み直せること。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::digest::canonical_len_within;
use crate::doc::{FormatChoice, Graph, MAX_DOC_BYTES, NodeId, OutputSpec, PortRef, SCHEMA};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::op::{self, Registry};
use crate::render::Engine;

use super::Edit;
use super::log::AppendLog;
use super::snapshot::{
    FIELD_DEPTH, Head, ITEM_DEPTH, OPTIONS_DEPTH, PARAMS_DEPTH, SNode, SParams, Snapshot,
    bad_graph, measure, too_large,
};

/// 手を当てた結果（検証を済ませたもの）。何も変えなければ `snap` は元のスナップショットの写し。
pub(crate) struct Outcome {
    pub(crate) snap: Snapshot,
    pub(crate) created: Option<NodeId>,
}

impl Outcome {
    fn of(snap: Snapshot) -> Self {
        Self {
            snap,
            created: None,
        }
    }
}

fn missing(id: NodeId) -> EngineError {
    EngineError::Invalid(Issues::one(
        Issue::error(IssueCode::BadGraph, format!("ノード {} がありません", id.0)).at(id),
    ))
}

fn bad_params(id: NodeId, key: &str, message: &str) -> EngineError {
    EngineError::Invalid(Issues::one(
        Issue::error(IssueCode::BadParams, message)
            .at(id)
            .param(key),
    ))
}

/// 値を、文書の中で `depth` 段の入れ子の中にあるものとして確かめ（深さ・大きさ）、正規の形にする。
/// 深さを先に数えるので、深すぎる値で正規化の再帰があふれない。
fn canonical(
    value: &Value,
    depth: usize,
    node: Option<NodeId>,
) -> Result<(Value, usize), EngineError> {
    let len = measure(value, depth)?;
    let canon = op::canonical_params(value).map_err(|i| {
        EngineError::Invalid(Issues::one(match node {
            Some(id) => i.at(id),
            None => i,
        }))
    })?;
    Ok((canon, len))
}

/// ノードが実装に結び付くか（E8）。その版の実装があれば、正規の形の引数は必ず通る。なければ移行を試す。
fn check_resolvable(reg: &Registry, node: &SNode) -> Result<(), EngineError> {
    if reg.get(&node.op, node.v).is_some() {
        return Ok(());
    }
    reg.resolve(&node.op, node.v, &node.params.to_value())
        .map(|_| ())
        .map_err(|i| EngineError::Invalid(Issues::one(i.at(node.id))))
}

/// 頭とノードからスナップショットを作り、文書の大きさを確かめる。
fn sized(head: Arc<Head>, nodes: Vec<Arc<SNode>>) -> Result<Snapshot, EngineError> {
    let snap = Snapshot::from_parts(head, nodes);
    if snap.encoded_len() > MAX_DOC_BYTES {
        return Err(EngineError::Limit(format!(
            "この編集をすると文書が大きすぎます（{} バイト。上限 {MAX_DOC_BYTES}）",
            snap.encoded_len()
        )));
    }
    Ok(snap)
}

/// `i` 番目のノードを替えたスナップショット（中身が同じなら元の写し）。`reg` があれば読み直せることも確かめる。
fn replace_node(
    cur: &Snapshot,
    i: usize,
    node: SNode,
    reg: Option<&Registry>,
) -> Result<Snapshot, EngineError> {
    if cur.nodes.get(i).is_some_and(|old| old.same(&node)) {
        return Ok(cur.clone());
    }
    let mut nodes = cur.nodes.to_vec();
    let node = Arc::new(node);
    if let Some(slot) = nodes.get_mut(i) {
        *slot = node.clone();
    }
    let snap = sized(cur.head.clone(), nodes)?;
    if let Some(reg) = reg {
        check_resolvable(reg, &node)?;
    }
    Ok(snap)
}

/// 線形の列の始まり（最初のノードの入力 0、ノードがなければ出力 0 の from。どちらも文書の入力）。
/// 線形の列：ノード i の入力 0 が 1 つ前のノード（最初のノードは文書の入力）で、出力 0 の from が最後のノード。
fn linear_source(cur: &Snapshot) -> Result<PortRef, EngineError> {
    let not_linear =
        || bad_graph("線形の列でない文書には、ノードを足す・消す・動かす手を当てられません");
    let out = cur.head.outputs.first().ok_or_else(not_linear)?;
    let Some(first) = cur.nodes.first() else {
        return match out.from {
            PortRef::Input { .. } => Ok(out.from),
            PortRef::Node { .. } => Err(not_linear()),
        };
    };
    let source = match first.inputs.first() {
        Some(r @ PortRef::Input { .. }) => *r,
        _ => return Err(not_linear()),
    };
    for w in cur.nodes.windows(2) {
        if w[1].inputs.first() != Some(&PortRef::Node { id: w[0].id }) {
            return Err(not_linear());
        }
    }
    match cur.nodes.last() {
        Some(last) if out.from == (PortRef::Node { id: last.id }) => Ok(source),
        _ => Err(not_linear()),
    }
}

/// 線形の列の入力 0 をつなぎ直す（変わるノードだけ作り直す）。最後のノード（出力 0 の from になるもの）を返す。
fn rewire(nodes: &mut [Arc<SNode>], source: PortRef) -> Result<PortRef, EngineError> {
    let mut prev = source;
    for n in nodes.iter_mut() {
        if n.inputs.first() != Some(&prev) {
            let mut inputs = Vec::with_capacity(n.inputs.len().max(1));
            inputs.push(prev);
            inputs.extend(n.inputs.iter().skip(1).copied());
            *n = Arc::new(n.with_inputs(Arc::from(inputs))?);
        }
        prev = PortRef::Node { id: n.id };
    }
    Ok(prev)
}

/// `next_id` と出力 0 の from を替えた頭（同じなら元の頭を共有する）。
fn head_with(cur: &Arc<Head>, next_id: u32, out0: PortRef) -> Result<Arc<Head>, EngineError> {
    if cur.next_id == next_id && cur.outputs.first().map(|o| o.from) == Some(out0) {
        return Ok(cur.clone());
    }
    let mut outputs = cur.outputs.clone();
    if let Some(o) = outputs.first_mut() {
        o.from = out0;
    }
    Ok(Arc::new(Head::new(
        cur.schema,
        cur.rules,
        cur.inputs.clone(),
        outputs,
        next_id,
    )?))
}

/// 出力の形式の設定を正規の形にする。
fn canonical_format(format: &FormatChoice) -> Result<FormatChoice, EngineError> {
    Ok(match format {
        FormatChoice::Keep => FormatChoice::Keep,
        FormatChoice::To { format, options } => FormatChoice::To {
            format: format.clone(),
            options: canonical(options, OPTIONS_DEPTH, None)?.0,
        },
    })
}

/// 文書からスナップショットを作る（開くとき・置き換え）。形・大きさ・深さ・読み直せること（E8）を全体で確かめる。
/// `reuse` があれば、中身が同じノード・頭はその `Arc` を共有する。
pub(crate) fn from_graph(
    engine: &Engine,
    g: &Graph,
    reuse: Option<&Snapshot>,
) -> Result<Snapshot, EngineError> {
    if g.schema != SCHEMA {
        return Err(bad_graph(format!(
            "今のスキーマ（{SCHEMA}）の文書ではありません（スキーマ {}。Engine::load で移してから）",
            g.schema
        )));
    }
    let structure = g.check_structure(engine.limits().max_nodes);
    if structure.has_errors() {
        return Err(EngineError::Invalid(structure));
    }
    canonical_len_within(g, MAX_DOC_BYTES).map_err(too_large)?;
    let reg = engine.registry();
    // 読み直せること（E8。`Engine::load` と同じくすべてのノードの指摘を集める）。引数は 1 度だけ正規の形にし、その版の実装が
    // あればそれで通る（`resolve` も正規の形にするだけ）。なければ移行を試す
    let mut issues = Issues::none();
    let mut canonical = Vec::with_capacity(g.nodes.len());
    for node in &g.nodes {
        let resolved = op::canonical_params(&node.params).and_then(|canon| {
            if reg.get(&node.op, node.v).is_none() {
                reg.resolve(&node.op, node.v, &canon)?;
            }
            Ok(canon)
        });
        match resolved {
            Ok(canon) => canonical.push(canon),
            Err(issue) => issues.push(issue.at(node.id)),
        }
    }
    if issues.has_errors() {
        return Err(EngineError::Invalid(issues));
    }
    let outputs = g
        .outputs
        .iter()
        .map(|o| {
            Ok(OutputSpec {
                format: canonical_format(&o.format)?,
                ..o.clone()
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    let head = Head::new(g.schema, g.rules, g.inputs.clone(), outputs, g.next_id)?;
    let head = match reuse {
        Some(old) if *old.head == head => old.head.clone(),
        _ => Arc::new(head),
    };
    let old_nodes: BTreeMap<NodeId, &Arc<SNode>> = reuse
        .map(|s| s.nodes.iter().map(|n| (n.id, n)).collect())
        .unwrap_or_default();
    let mut nodes = Vec::with_capacity(g.nodes.len());
    for (node, canon) in g.nodes.iter().zip(canonical) {
        let params = SParams::build(reg.append_field(&node.op, node.v), canon)?;
        let built = SNode::new(
            node.id,
            Arc::from(node.op.as_str()),
            node.v,
            Arc::from(node.inputs.as_slice()),
            node.seed,
            node.enabled,
            params,
        )?;
        let shared = old_nodes.get(&node.id).filter(|old| old.same(&built));
        nodes.push(match shared {
            Some(old) => Arc::clone(old),
            None => Arc::new(built),
        });
    }
    sized(head, nodes)
}

/// 手を確定した頭 `cur` に当てる。断ったら誤りを返す（`cur` は変えない）。
pub(crate) fn apply(engine: &Engine, cur: &Snapshot, edit: &Edit) -> Result<Outcome, EngineError> {
    let reg = engine.registry();
    match edit {
        Edit::SetParams { node, params } => {
            let (i, old) = cur.find(*node).ok_or_else(|| missing(*node))?;
            let (canon, _) = canonical(params, PARAMS_DEPTH, Some(*node))?;
            let p = SParams::build(reg.append_field(&old.op, old.v), canon)?;
            let new = old.with_params(p)?;
            replace_node(cur, i, new, Some(reg)).map(Outcome::of)
        }
        Edit::SetParam { node, key, value } => {
            let (i, old) = cur.find(*node).ok_or_else(|| missing(*node))?;
            let p = match &old.params {
                SParams::Log {
                    shell, field, log, ..
                } if key != field => {
                    let (canon, _) = canonical(value, FIELD_DEPTH, Some(*node))?;
                    let mut shell = (**shell).clone();
                    if let Value::Object(map) = &mut shell {
                        map.insert(key.clone(), canon);
                    }
                    let shell_len = measure(&shell, PARAMS_DEPTH)?;
                    SParams::Log {
                        shell: Arc::new(shell),
                        shell_len,
                        field,
                        log: log.clone(),
                    }
                }
                SParams::Log { shell, field, .. } => {
                    let (canon, _) = canonical(value, FIELD_DEPTH, Some(*node))?;
                    let mut whole = (**shell).clone();
                    if let Value::Object(map) = &mut whole {
                        map.insert(key.clone(), canon);
                    }
                    SParams::build(Some(field), whole)?
                }
                SParams::Plain { value: prev, .. } => {
                    let mut map = match prev.as_ref() {
                        Value::Null => Map::new(),
                        Value::Object(m) => m.clone(),
                        _ => {
                            return Err(bad_params(
                                *node,
                                key,
                                "引数がオブジェクトではないので、項目を替えられません",
                            ));
                        }
                    };
                    let (canon, _) = canonical(value, FIELD_DEPTH, Some(*node))?;
                    map.insert(key.clone(), canon);
                    SParams::build(reg.append_field(&old.op, old.v), Value::Object(map))?
                }
            };
            let new = old.with_params(p)?;
            replace_node(cur, i, new, Some(reg)).map(Outcome::of)
        }
        Edit::AppendItems { node, field, items } => {
            let (i, old) = cur.find(*node).ok_or_else(|| missing(*node))?;
            let Some(af) = reg
                .append_field(&old.op, old.v)
                .filter(|f| *f == field.as_str())
            else {
                return Err(bad_params(
                    *node,
                    field,
                    "この引数は追記だけの配列ではありません",
                ));
            };
            let (shell, shell_len, log) = match &old.params {
                SParams::Log {
                    shell,
                    shell_len,
                    log,
                    ..
                } => (shell.clone(), *shell_len, log.clone()),
                SParams::Plain { value, .. } => {
                    let mut map = match value.as_ref() {
                        Value::Null => Map::new(),
                        Value::Object(m) if !m.contains_key(af) => m.clone(),
                        Value::Object(_) => {
                            return Err(bad_params(
                                *node,
                                field,
                                "追記だけの引数が配列ではありません",
                            ));
                        }
                        _ => {
                            return Err(bad_params(
                                *node,
                                field,
                                "引数がオブジェクトではないので、追記できません",
                            ));
                        }
                    };
                    if items.is_empty() {
                        return Ok(Outcome::of(cur.clone()));
                    }
                    map.insert(af.to_string(), Value::Array(Vec::new()));
                    let shell = Value::Object(map);
                    let shell_len = measure(&shell, PARAMS_DEPTH)?;
                    (Arc::new(shell), shell_len, AppendLog::new())
                }
            };
            if items.is_empty() {
                return Ok(Outcome::of(cur.clone()));
            }
            let prepared = items
                .iter()
                .map(|it| canonical(it, ITEM_DEPTH, Some(*node)))
                .collect::<Result<Vec<_>, _>>()?;
            let log = log.extend(prepared)?;
            let new = old.with_params(SParams::Log {
                shell,
                shell_len,
                field: af,
                log,
            })?;
            replace_node(cur, i, new, Some(reg)).map(Outcome::of)
        }
        Edit::SetEnabled { node, enabled } => {
            let (i, old) = cur.find(*node).ok_or_else(|| missing(*node))?;
            if old.enabled == *enabled {
                return Ok(Outcome::of(cur.clone()));
            }
            let new = old.with_enabled(*enabled)?;
            replace_node(cur, i, new, None).map(Outcome::of)
        }
        Edit::AddNode {
            at,
            op,
            v,
            params,
            seed,
            extra_inputs,
        } => {
            let source = linear_source(cur)?;
            let n = cur.nodes.len();
            if *at > n {
                return Err(bad_graph(format!(
                    "足す位置（{at}）が列の外です（ノードは {n} 個）"
                )));
            }
            if (n as u64).saturating_add(1) > engine.limits().max_nodes {
                return Err(EngineError::Invalid(Issues::one(Issue::error(
                    IssueCode::TooLarge,
                    format!("ノードが多すぎます（上限 {}）", engine.limits().max_nodes),
                ))));
            }
            let id = NodeId(cur.head.next_id);
            let next_id = cur.head.next_id.checked_add(1).ok_or_else(|| {
                EngineError::Invalid(Issues::one(Issue::error(
                    IssueCode::TooLarge,
                    "ノードの ID を使い切りました",
                )))
            })?;
            let before = cur.nodes.get(..*at).unwrap_or_default();
            for r in extra_inputs {
                let known = match r {
                    PortRef::Input { id } => cur.head.inputs.iter().any(|s| s.id == *id),
                    PortRef::Node { id } => before.iter().any(|n| n.id == *id),
                };
                if !known {
                    return Err(bad_graph(
                        "2 つ目からの入力は、文書の入力か、足す位置より前のノードを読みます",
                    ));
                }
            }
            let (canon, _) = canonical(params, PARAMS_DEPTH, Some(id))?;
            let p = SParams::build(reg.append_field(op, *v), canon)?;
            let prev = match at.checked_sub(1).and_then(|k| cur.nodes.get(k)) {
                Some(n) => PortRef::Node { id: n.id },
                None => source,
            };
            let mut inputs = Vec::with_capacity(1 + extra_inputs.len());
            inputs.push(prev);
            inputs.extend(extra_inputs.iter().copied());
            let node = Arc::new(SNode::new(
                id,
                Arc::from(op.as_str()),
                *v,
                Arc::from(inputs),
                seed.unwrap_or(u64::from(id.0)),
                true,
                p,
            )?);
            let mut nodes = cur.nodes.to_vec();
            nodes.insert(*at, node.clone());
            let last = rewire(&mut nodes, source)?;
            let head = head_with(&cur.head, next_id, last)?;
            let snap = sized(head, nodes)?;
            check_resolvable(reg, &node)?;
            Ok(Outcome {
                snap,
                created: Some(id),
            })
        }
        Edit::RemoveNode { node } => {
            let source = linear_source(cur)?;
            let (i, _) = cur.find(*node).ok_or_else(|| missing(*node))?;
            let r = PortRef::Node { id: *node };
            let read_elsewhere = cur
                .nodes
                .iter()
                .any(|n| n.inputs.iter().skip(1).any(|x| *x == r))
                || cur.head.outputs.iter().skip(1).any(|o| o.from == r);
            if read_elsewhere {
                return Err(bad_graph(format!(
                    "ノード {} は、ほかのノードの 2 つ目からの入力か、ほかの出力が読んでいるので消せません",
                    node.0
                )));
            }
            let mut nodes = cur.nodes.to_vec();
            nodes.remove(i);
            let last = rewire(&mut nodes, source)?;
            let head = head_with(&cur.head, cur.head.next_id, last)?;
            sized(head, nodes).map(Outcome::of)
        }
        Edit::MoveNode { node, to } => {
            let source = linear_source(cur)?;
            let (i, _) = cur.find(*node).ok_or_else(|| missing(*node))?;
            if *to >= cur.nodes.len() {
                return Err(bad_graph(format!(
                    "動かす先（{to}）が列の外です（ノードは {} 個）",
                    cur.nodes.len()
                )));
            }
            if *to == i {
                return Ok(Outcome::of(cur.clone()));
            }
            let mut nodes = cur.nodes.to_vec();
            let moved = nodes.remove(i);
            nodes.insert(*to, moved);
            let last = rewire(&mut nodes, source)?;
            let pos: BTreeMap<NodeId, usize> =
                nodes.iter().enumerate().map(|(k, n)| (n.id, k)).collect();
            for (k, n) in nodes.iter().enumerate() {
                for r in n.inputs.iter().skip(1) {
                    if let PortRef::Node { id } = r
                        && pos.get(id).is_none_or(|p| *p >= k)
                    {
                        return Err(bad_graph(format!(
                            "動かすと、ノード {} の 2 つ目からの入力が後ろのノードを読みます",
                            n.id.0
                        )));
                    }
                }
            }
            let head = head_with(&cur.head, cur.head.next_id, last)?;
            sized(head, nodes).map(Outcome::of)
        }
        Edit::SetOutput {
            output,
            format,
            quantize,
            expand,
        } => {
            let k = cur
                .head
                .outputs
                .iter()
                .position(|o| o.id == *output)
                .ok_or_else(|| bad_graph(format!("出力 {} がありません", output.0)))?;
            let format = canonical_format(format)?;
            let mut outputs = cur.head.outputs.clone();
            if let Some(o) = outputs.get_mut(k) {
                o.format = format;
                o.quantize = *quantize;
                o.expand = expand.clone();
            }
            let head = Head::new(
                cur.head.schema,
                cur.head.rules,
                cur.head.inputs.clone(),
                outputs,
                cur.head.next_id,
            )?;
            if head == *cur.head {
                return Ok(Outcome::of(cur.clone()));
            }
            sized(Arc::new(head), cur.nodes.to_vec()).map(Outcome::of)
        }
        Edit::Replace { graph } => from_graph(engine, graph, Some(cur)).map(Outcome::of),
    }
}
