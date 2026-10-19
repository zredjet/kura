//! EditSession（段 10。E33・E1・E8・E25）：素朴な参照の実装（文書を丸ごと写す `Vec<Graph>`）との照合と、まとめ方・仮の値・
//! Rev・履歴の上限・構造の手・検証・共有・ノードの鍵の性質。下書きの形式は作らない（M12）代わりに、頭の文書から作り直すと
//! 同じ文書・同じ鍵になることを確かめる。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};

use kura_engine::digest::{Digest, MAX_DEPTH};
use kura_engine::doc::{
    DecodeOptions, Expand, FormatChoice, Graph, InputId, InputRole, InputSlot, MAX_DOC_BYTES,
    NodeId, OutputId, PortRef, SCHEMA,
};
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, spec,
};
use kura_engine::render::{Engine, InputValue};
use kura_engine::session::{
    CommitMode, Committed, Edit, EditSession, HistoryLimits, NodeChange, ParamsView, Rev, Snapshot,
};
use kura_engine::shape::{Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::value::{PixelFormat, Value};
use kura_engine::{EngineError, IssueCode, Issues, Limits, NoMonitor, determinism};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value as J, json};

// ---------------------------------------------------------------------------------------------
// 補助
// ---------------------------------------------------------------------------------------------

fn below(r: &mut ChaCha8Rng, n: u64) -> u64 {
    ((u128::from(r.next_u64()) * u128::from(n.max(1))) >> 64) as u64
}

fn pick<T: Copy>(r: &mut ChaCha8Rng, xs: &[T]) -> T {
    xs[below(r, xs.len() as u64) as usize]
}

fn chance(r: &mut ChaCha8Rng, percent: u64) -> bool {
    below(r, 100) < percent
}

fn json_of(g: &Graph) -> Vec<u8> {
    g.to_canonical_json().unwrap()
}

fn snap_json(s: &Snapshot) -> Vec<u8> {
    s.to_canonical_json().unwrap()
}

/// 入れ子が `levels` 段の値（配列の中に配列…）。
fn deep(levels: usize) -> J {
    let mut v = json!(1);
    for _ in 0..levels {
        v = json!([v]);
    }
    v
}

/// 入力 0（Primary）と入力 1（Fixed）を持ち、`nodes` を線形につないだ文書。
fn chain(nodes: &[(&str, J)]) -> Graph {
    let mut g = Graph::new_single();
    g.inputs.push(InputSlot {
        id: InputId(1),
        role: InputRole::Fixed {
            hash: Digest([7; 32]),
        },
        decode: DecodeOptions::default(),
    });
    for (i, (op, params)) in nodes.iter().enumerate() {
        g.push_linear(op, 1, params.clone(), i as u64 * 3).unwrap();
    }
    g
}

fn sample() -> Graph {
    chain(&[
        (
            "image.crop",
            json!({ "rect": { "x": 1, "y": 1, "w": 6, "h": 6 } }),
        ),
        ("image.flip", json!({ "axis": "horizontal" })),
        (
            "paint.actions",
            json!({ "actions": [{ "k": 0 }, { "k": 1 }] }),
        ),
    ])
}

fn open(g: &Graph) -> EditSession {
    EditSession::open(
        &Engine::new().unwrap(),
        g,
        Digest([1; 32]),
        HistoryLimits::default(),
    )
    .unwrap()
}

fn set_param(node: u32, key: &str, value: J) -> Edit {
    Edit::SetParam {
        node: NodeId(node),
        key: key.into(),
        value,
    }
}

fn append(node: u32, items: Vec<J>) -> Edit {
    Edit::AppendItems {
        node: NodeId(node),
        field: "actions".into(),
        items,
    }
}

/// 誤りの種類（照合用）。
fn kind(e: &EngineError) -> String {
    match e {
        EngineError::Invalid(_) => "invalid".into(),
        EngineError::Limit(_) => "limit".into(),
        other => format!("other: {other}"),
    }
}

fn invalid_code(e: &EngineError) -> Option<IssueCode> {
    match e {
        EngineError::Invalid(Issues(issues)) => issues.first().map(|i| i.code),
        _ => None,
    }
}

/// 今の文書・確定した頭・番号・取り消しの可否・長さ・鍵の、セッションの中での約束。
fn check_invariants(engine: &Engine, s: &EditSession) {
    let snaps = if s.is_provisional() {
        vec![s.current(), s.head()]
    } else {
        vec![s.head()]
    };
    for snap in snaps {
        let g = snap.to_graph();
        assert_eq!(snap.encoded_len(), g.encoded_len().unwrap());
        assert_eq!(snap_json(snap), json_of(&g));
        assert!(g.check_structure(engine.limits().max_nodes).is_empty());
        for (view, node) in snap.nodes().zip(&g.nodes) {
            assert_eq!(view.id, node.id);
            assert_eq!(view.key, engine.registry().doc_node_key(node).unwrap());
        }
    }
    assert_eq!(
        s.snapshot(s.rev()).map(snap_json),
        Some(snap_json(s.current()))
    );
    assert_eq!(
        s.snapshot(s.head_rev()).map(snap_json),
        Some(snap_json(s.head()))
    );
}

// ---------------------------------------------------------------------------------------------
// 素朴な参照の実装（文書を丸ごと写す。まとめ方の規則は同じ）
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Key {
    Param(NodeId, String),
    Params(NodeId),
}

fn merge_key(e: &Edit) -> Option<Key> {
    match e {
        Edit::SetParam { node, key, .. } => Some(Key::Param(*node, key.clone())),
        Edit::SetParams { node, .. } => Some(Key::Params(*node)),
        _ => None,
    }
}

fn model_bad(msg: &str) -> EngineError {
    EngineError::Invalid(Issues::one(kura_engine::Issue::error(
        IssueCode::BadGraph,
        msg,
    )))
}

/// 線形の列の始まり（列でなければ誤り）。
fn model_linear(g: &Graph) -> Result<PortRef, EngineError> {
    let out = g.outputs.first().ok_or_else(|| model_bad("出力がない"))?;
    let Some(first) = g.nodes.first() else {
        return match out.from {
            PortRef::Input { .. } => Ok(out.from),
            PortRef::Node { .. } => Err(model_bad("列でない")),
        };
    };
    let source = match first.inputs.first() {
        Some(r @ PortRef::Input { .. }) => *r,
        _ => return Err(model_bad("列でない")),
    };
    for w in g.nodes.windows(2) {
        if w[1].inputs.first() != Some(&PortRef::Node { id: w[0].id }) {
            return Err(model_bad("列でない"));
        }
    }
    let last = g.nodes.last().map(|n| n.id).unwrap();
    if out.from != (PortRef::Node { id: last }) {
        return Err(model_bad("列でない"));
    }
    Ok(source)
}

/// 線形の列の入力 0 と出力 0 をつなぎ直す。
fn model_rewire(g: &mut Graph, source: PortRef) {
    let mut prev = source;
    for n in &mut g.nodes {
        n.inputs[0] = prev;
        prev = PortRef::Node { id: n.id };
    }
    g.outputs[0].from = prev;
}

fn model_node(g: &mut Graph, id: NodeId) -> Result<&mut kura_engine::Node, EngineError> {
    g.nodes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| model_bad("ノードがない"))
}

/// 手を素朴に当てる（文書を写して書き換え、全体を確かめる）。
fn model_apply(
    engine: &Engine,
    head: &Graph,
    edit: &Edit,
) -> Result<(Graph, Option<NodeId>), EngineError> {
    let mut g = head.clone();
    let mut created = None;
    match edit {
        Edit::AddNode {
            at,
            op,
            v,
            params,
            seed,
            extra_inputs,
        } => {
            let source = model_linear(&g)?;
            if *at > g.nodes.len() {
                return Err(model_bad("位置"));
            }
            for r in extra_inputs {
                let ok = match r {
                    PortRef::Input { id } => g.inputs.iter().any(|i| i.id == *id),
                    PortRef::Node { id } => g.nodes[..*at].iter().any(|n| n.id == *id),
                };
                if !ok {
                    return Err(model_bad("入力"));
                }
            }
            let id = NodeId(g.next_id);
            g.next_id = g.next_id.checked_add(1).ok_or_else(|| model_bad("ID"))?;
            let mut inputs = vec![source];
            inputs.extend(extra_inputs.iter().copied());
            g.nodes.insert(
                *at,
                kura_engine::Node {
                    id,
                    op: op.clone(),
                    v: *v,
                    params: params.clone(),
                    inputs,
                    seed: seed.unwrap_or(u64::from(id.0)),
                    enabled: true,
                },
            );
            model_rewire(&mut g, source);
            created = Some(id);
        }
        Edit::RemoveNode { node } => {
            let source = model_linear(&g)?;
            let i = g
                .nodes
                .iter()
                .position(|n| n.id == *node)
                .ok_or_else(|| model_bad("ノードがない"))?;
            let r = PortRef::Node { id: *node };
            if g.nodes.iter().any(|n| n.inputs[1..].contains(&r))
                || g.outputs[1..].iter().any(|o| o.from == r)
            {
                return Err(model_bad("参照されている"));
            }
            g.nodes.remove(i);
            model_rewire(&mut g, source);
        }
        Edit::MoveNode { node, to } => {
            let source = model_linear(&g)?;
            let i = g
                .nodes
                .iter()
                .position(|n| n.id == *node)
                .ok_or_else(|| model_bad("ノードがない"))?;
            if *to >= g.nodes.len() {
                return Err(model_bad("位置"));
            }
            let n = g.nodes.remove(i);
            g.nodes.insert(*to, n);
            model_rewire(&mut g, source);
        }
        Edit::SetEnabled { node, enabled } => {
            model_node(&mut g, *node)?.enabled = *enabled;
        }
        Edit::SetParams { node, params } => {
            model_node(&mut g, *node)?.params = params.clone();
        }
        Edit::SetParam { node, key, value } => {
            let n = model_node(&mut g, *node)?;
            if n.params.is_null() {
                n.params = json!({});
            }
            let Some(map) = n.params.as_object_mut() else {
                return Err(model_bad("オブジェクトでない"));
            };
            map.insert(key.clone(), value.clone());
        }
        Edit::AppendItems { node, field, items } => {
            let n = model_node(&mut g, *node)?;
            if engine.registry().append_field(&n.op, n.v) != Some(field.as_str()) {
                return Err(model_bad("追記できない"));
            }
            if n.params.is_null() {
                n.params = json!({});
            }
            let Some(map) = n.params.as_object_mut() else {
                return Err(model_bad("オブジェクトでない"));
            };
            match map.get(field) {
                None | Some(J::Array(_)) => {}
                Some(_) => return Err(model_bad("配列でない")),
            }
            if items.is_empty() {
                return Ok((head.clone(), None));
            }
            let arr = map.entry(field.clone()).or_insert_with(|| json!([]));
            if let J::Array(a) = arr {
                a.extend(items.iter().cloned());
            }
        }
        Edit::SetOutput {
            output,
            format,
            quantize,
            expand,
        } => {
            let o = g
                .outputs
                .iter_mut()
                .find(|o| o.id == *output)
                .ok_or_else(|| model_bad("出力がない"))?;
            o.format = format.clone();
            o.quantize = *quantize;
            o.expand = expand.clone();
        }
        Edit::Replace { graph } => {
            if graph.schema != SCHEMA {
                return Err(model_bad("スキーマ"));
            }
            g = graph.clone();
        }
    }
    let structure = g.check_structure(engine.limits().max_nodes);
    if structure.has_errors() {
        return Err(EngineError::Invalid(structure));
    }
    let bytes = g.to_canonical_json()?;
    engine.load(&bytes)?;
    Ok((g, created))
}

struct Prov {
    rev: Rev,
    g: Graph,
    key: Option<Key>,
    created: Option<NodeId>,
}

struct Model {
    entries: Vec<(Rev, Graph)>,
    cursor: usize,
    prov: Option<Prov>,
    recent: VecDeque<(Rev, Graph)>,
    open: Option<Key>,
    next_rev: u64,
    max_steps: usize,
    saved: Rev,
}

impl Model {
    fn new(g: &Graph, max_steps: usize) -> Self {
        Self {
            entries: vec![(Rev(0), g.clone())],
            cursor: 0,
            prov: None,
            recent: VecDeque::new(),
            open: None,
            next_rev: 1,
            max_steps,
            saved: Rev(0),
        }
    }
    fn head(&self) -> &Graph {
        &self.entries[self.cursor].1
    }
    fn head_rev(&self) -> Rev {
        self.entries[self.cursor].0
    }
    fn rev(&self) -> Rev {
        self.prov.as_ref().map_or(self.head_rev(), |p| p.rev)
    }
    fn current(&self) -> &Graph {
        self.prov.as_ref().map_or(self.head(), |p| &p.g)
    }
    fn fresh(&mut self) -> Rev {
        let r = Rev(self.next_rev);
        self.next_rev += 1;
        r
    }
    fn discard(&mut self) {
        if let Some(p) = self.prov.take() {
            self.recent.push_back((p.rev, p.g));
            if self.recent.len() > 4 {
                self.recent.pop_front();
            }
        }
    }
    fn snapshot(&self, rev: Rev) -> Option<&Graph> {
        self.entries
            .iter()
            .find(|(r, _)| *r == rev)
            .map(|(_, g)| g)
            .or_else(|| self.prov.as_ref().filter(|p| p.rev == rev).map(|p| &p.g))
            .or_else(|| self.recent.iter().find(|(r, _)| *r == rev).map(|(_, g)| g))
    }
    fn push(
        &mut self,
        g: Graph,
        rev: Option<Rev>,
        key: Option<Key>,
        created: Option<NodeId>,
    ) -> Committed {
        if json_of(&g) == json_of(self.head()) {
            return Committed {
                rev: self.head_rev(),
                created: None,
                merged: false,
                noop: true,
            };
        }
        self.entries.truncate(self.cursor + 1);
        if let Some(k) = &key
            && self.open.as_ref() == Some(k)
            && self.cursor >= 1
        {
            let below = self.entries[self.cursor - 1].clone();
            if json_of(&g) == json_of(&below.1) {
                self.entries.pop();
                self.cursor -= 1;
                self.open = None;
                return Committed {
                    rev: below.0,
                    created: None,
                    merged: true,
                    noop: false,
                };
            }
            let rev = rev.unwrap_or_else(|| self.fresh());
            self.entries[self.cursor] = (rev, g);
            return Committed {
                rev,
                created,
                merged: true,
                noop: false,
            };
        }
        let rev = rev.unwrap_or_else(|| self.fresh());
        self.entries.push((rev, g));
        self.cursor += 1;
        self.open = key;
        while self.entries.len() > 1 && self.entries.len() - 1 > self.max_steps {
            self.entries.remove(0);
            self.cursor -= 1;
        }
        if self.cursor == 0 {
            self.open = None;
        }
        Committed {
            rev,
            created,
            merged: false,
            noop: false,
        }
    }
    fn commit(
        &mut self,
        engine: &Engine,
        edit: &Edit,
        mode: CommitMode,
    ) -> Result<Committed, EngineError> {
        let (g, created) = model_apply(engine, self.head(), edit)?;
        self.discard();
        let key = match mode {
            CommitMode::Merge => merge_key(edit),
            CommitMode::Step => None,
        };
        Ok(self.push(g, None, key, created))
    }
    fn preview(&mut self, engine: &Engine, edit: &Edit) -> Result<Rev, EngineError> {
        let (g, created) = model_apply(engine, self.head(), edit)?;
        self.discard();
        if json_of(&g) == json_of(self.head()) {
            return Ok(self.head_rev());
        }
        let rev = self.fresh();
        self.prov = Some(Prov {
            rev,
            g,
            key: merge_key(edit),
            created,
        });
        Ok(rev)
    }
    fn cancel(&mut self) -> Option<Rev> {
        self.prov.as_ref()?;
        self.discard();
        Some(self.head_rev())
    }
    fn commit_preview(&mut self, mode: CommitMode) -> Committed {
        let Some(p) = self.prov.take() else {
            return Committed {
                rev: self.head_rev(),
                created: None,
                merged: false,
                noop: true,
            };
        };
        let key = match mode {
            CommitMode::Merge => p.key,
            CommitMode::Step => None,
        };
        self.push(p.g, Some(p.rev), key, p.created)
    }
    fn undo(&mut self) -> Option<Rev> {
        self.discard();
        self.open = None;
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(self.head_rev())
    }
    fn redo(&mut self) -> Option<Rev> {
        self.discard();
        self.open = None;
        if self.cursor + 1 >= self.entries.len() {
            return None;
        }
        self.cursor += 1;
        Some(self.head_rev())
    }
}

// ---------------------------------------------------------------------------------------------
// ランダムな手
// ---------------------------------------------------------------------------------------------

fn small_value(r: &mut ChaCha8Rng, depth: u32) -> J {
    match below(r, if depth > 2 { 7 } else { 9 }) {
        0 => J::Null,
        1 => json!(below(r, 2) == 1),
        2 => json!(pick(r, &[1, 16, -3, 2])),
        // 16.0 は正規の形で 16 になる（同じ鍵・同じ文書）
        3 => json!(pick(r, &[16.0, 0.5, -0.0, 1e300])),
        4 => json!(pick(r, &["a", "horizontal", "vertical", "\"nodes\":[]"])),
        5 | 6 => json!(below(r, 3)),
        7 => J::Array(
            (0..below(r, 3))
                .map(|_| small_value(r, depth + 1))
                .collect(),
        ),
        _ => {
            let mut m = serde_json::Map::new();
            for _ in 0..below(r, 3) {
                let k = ["a", "b", "x"][below(r, 3) as usize];
                m.insert(k.into(), small_value(r, depth + 1));
            }
            J::Object(m)
        }
    }
}

/// 引数の値（ときどき、入れ子の上限の前後の深いもの）。`room` は文書の中で、その値の外にある入れ子の段の数。
fn param_value(r: &mut ChaCha8Rng, room: usize) -> J {
    if chance(r, 20) {
        // 入れ子の上限ちょうどと、1 段・2 段こえるもの
        let levels = MAX_DEPTH - room - 1 + below(r, 3) as usize;
        return deep(levels);
    }
    small_value(r, 0)
}

fn node_ids(g: &Graph) -> Vec<NodeId> {
    g.nodes.iter().map(|n| n.id).collect()
}

fn pick_node(r: &mut ChaCha8Rng, g: &Graph, sticky: &Option<(NodeId, String)>) -> NodeId {
    if let Some((n, _)) = sticky
        && chance(r, 40)
    {
        return *n;
    }
    let ids = node_ids(g);
    if ids.is_empty() || chance(r, 6) {
        return NodeId(900 + below(r, 3) as u32);
    }
    ids[below(r, ids.len() as u64) as usize]
}

fn random_graph(r: &mut ChaCha8Rng) -> Graph {
    let ops: [(&str, J); 4] = [
        (
            "image.crop",
            json!({ "rect": { "x": 0, "y": 0, "w": 4, "h": 4 } }),
        ),
        ("image.flip", json!({ "axis": "vertical" })),
        ("paint.actions", json!({ "actions": [{ "k": 9 }] })),
        ("image.flip", json!({ "axis": 16.0 })),
    ];
    let n = below(r, 4) as usize;
    let spec: Vec<(&str, J)> = (0..n).map(|_| ops[below(r, 4) as usize].clone()).collect();
    let mut g = chain(&spec);
    match below(r, 8) {
        // 線形の列でない文書（2 つ目のノードが入力 0 を直接読む）
        0 if g.nodes.len() >= 2 => g.nodes[1].inputs[0] = PortRef::Input { id: InputId(0) },
        // 古いスキーマ（置き換えでは断る）
        1 => g.schema = SCHEMA - 1,
        // 知らない操作（E8）
        2 if !g.nodes.is_empty() => g.nodes[0].op = "test.unknown".into(),
        _ => {}
    }
    g
}

fn random_edit(r: &mut ChaCha8Rng, g: &Graph, sticky: &mut Option<(NodeId, String)>) -> Edit {
    let n = g.nodes.len() as u64;
    match below(r, 22) {
        0..=5 => {
            let node = pick_node(r, g, sticky);
            let key = match sticky {
                Some((sn, k)) if *sn == node && chance(r, 70) => k.clone(),
                _ => ["axis", "rect", "x", "actions"][below(r, 4) as usize].to_string(),
            };
            *sticky = Some((node, key.clone()));
            let value = if key == "x" {
                json!(pick(r, &[1, 2, 3]))
            } else if key == "actions" && chance(r, 50) {
                J::Array((0..below(r, 3)).map(|i| json!({ "k": i })).collect())
            } else {
                param_value(r, 4)
            };
            Edit::SetParam { node, key, value }
        }
        6 => Edit::SetParams {
            node: pick_node(r, g, sticky),
            params: match below(r, 10) {
                0..=4 => json!({ "axis": pick(r, &["horizontal", "vertical"]) }),
                // 入れ子の上限ちょうどと、1 段こえるもの
                5 | 6 => deep(MAX_DEPTH - 3 + below(r, 2) as usize),
                _ => small_value(r, 0),
            },
        },
        7..=9 => Edit::AppendItems {
            node: pick_node(r, g, sticky),
            field: if chance(r, 90) { "actions" } else { "points" }.into(),
            items: (0..below(r, 4))
                .map(|i| {
                    if chance(r, 5) {
                        deep(MAX_DEPTH - 5 + below(r, 2) as usize)
                    } else {
                        json!({ "k": i, "v": small_value(r, 1) })
                    }
                })
                .collect(),
        },
        10 | 11 => {
            let at = below(r, n + 2) as usize;
            let (op, params) = match below(r, 6) {
                0 => ("image.flip", json!({ "axis": "horizontal" })),
                1 => ("paint.actions", json!({ "actions": [] })),
                2 => ("paint.actions", J::Null),
                3 => ("test.unknown", json!({})),
                // 入れ子の上限ちょうどと、1 段こえるもの
                4 => ("image.crop", deep(MAX_DEPTH - 3 + below(r, 2) as usize)),
                _ => ("image.flip", json!({})),
            };
            let v = if chance(r, 8) { 99 } else { 1 };
            let extra_inputs = match below(r, 6) {
                0 => vec![PortRef::Input { id: InputId(1) }],
                1 => vec![PortRef::Input { id: InputId(7) }],
                2 if n > 0 => vec![PortRef::Node {
                    id: g.nodes[below(r, n) as usize].id,
                }],
                _ => Vec::new(),
            };
            Edit::AddNode {
                at,
                op: op.into(),
                v,
                params,
                seed: if chance(r, 50) {
                    Some(r.next_u64())
                } else {
                    None
                },
                extra_inputs,
            }
        }
        12 => Edit::RemoveNode {
            node: pick_node(r, g, sticky),
        },
        13 | 20 | 21 => Edit::MoveNode {
            node: pick_node(r, g, sticky),
            to: {
                let wide = chance(r, 15);
                below(r, if wide { n + 1 } else { n }) as usize
            },
        },
        14 | 15 => Edit::SetEnabled {
            node: pick_node(r, g, sticky),
            enabled: chance(r, 50),
        },
        16 => Edit::SetOutput {
            output: OutputId(if chance(r, 90) { 0 } else { 5 }),
            format: if chance(r, 50) {
                FormatChoice::Keep
            } else {
                FormatChoice::To {
                    format: "png".into(),
                    options: param_value(r, 4),
                }
            },
            quantize: None,
            expand: Expand::One,
        },
        17 => Edit::Replace {
            graph: random_graph(r),
        },
        // 何も変えない手
        _ => match g.nodes.first() {
            Some(first) => Edit::SetEnabled {
                node: first.id,
                enabled: first.enabled,
            },
            None => Edit::AppendItems {
                node: NodeId(900),
                field: "actions".into(),
                items: Vec::new(),
            },
        },
    }
}

/// 1 手ぶんの結果（照合と、同じ列を 2 回当てたときの照合に使う）。
#[derive(Debug, PartialEq)]
struct StepTrace {
    /// 何をしたか（`commit:SetParam` など）。
    label: String,
    result: String,
    rev: Rev,
    head_rev: Rev,
    doc: Vec<u8>,
    keys: Vec<Digest>,
}

fn edit_name(e: &Edit) -> &'static str {
    match e {
        Edit::AddNode { .. } => "AddNode",
        Edit::RemoveNode { .. } => "RemoveNode",
        Edit::MoveNode { .. } => "MoveNode",
        Edit::SetEnabled { .. } => "SetEnabled",
        Edit::SetParams { .. } => "SetParams",
        Edit::SetParam { .. } => "SetParam",
        Edit::AppendItems { .. } => "AppendItems",
        Edit::SetOutput { .. } => "SetOutput",
        Edit::Replace { .. } => "Replace",
    }
}

/// ランダムな列をセッションと参照の実装の両方に当て、各手のあとに照らす。各手の結果を返す。
fn run_sequence(seed: u64, steps: usize, max_steps: usize) -> Vec<StepTrace> {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(seed);
    let start = loop {
        let g = random_graph(&mut r);
        if model_apply(
            &engine,
            &Graph::new_single(),
            &Edit::Replace { graph: g.clone() },
        )
        .is_ok()
        {
            break g;
        }
    };
    let limits = HistoryLimits {
        max_steps,
        max_bytes: u64::MAX,
    };
    let mut s = EditSession::open(&engine, &start, Digest([3; 32]), limits).unwrap();
    let mut m = Model::new(&start, max_steps);
    let mut sticky = None;
    let mut burst_left = 0usize;
    let mut trace = Vec::new();
    for step in 0..steps {
        let mode = if chance(&mut r, 50) {
            CommitMode::Merge
        } else {
            CommitMode::Step
        };
        // 矢印のキーの連打は数手つづける
        let in_burst = burst_left > 0;
        let what = if in_burst {
            burst_left -= 1;
            40
        } else {
            below(&mut r, 100)
        };
        let ctx = format!("seed {seed} step {step}");
        let mut label = String::new();
        let result = match what {
            // 同じ引数を続けて変える（矢印のキー）。値は 1〜3 なので、前の値に戻ってまとめた手が消えることもある
            40..=49 => {
                if !in_burst {
                    burst_left = 1 + below(&mut r, 5) as usize;
                }
                let node = match &sticky {
                    Some((n, _)) => *n,
                    None => pick_node(&mut r, m.head(), &sticky),
                };
                sticky = Some((node, "x".into()));
                let e = set_param(node.0, "x", json!(pick(&mut r, &[1, 2, 3])));
                if chance(&mut r, 50) {
                    label = "burst:commit".into();
                    let a = s
                        .commit(&engine, e.clone(), CommitMode::Merge)
                        .map_err(|e| kind(&e));
                    let b = m
                        .commit(&engine, &e, CommitMode::Merge)
                        .map_err(|e| kind(&e));
                    assert_eq!(a, b, "{ctx}: burst {e:?}");
                    format!("{a:?}")
                } else {
                    label = "burst:preview".into();
                    let a = s.preview(&engine, e.clone()).map_err(|e| kind(&e));
                    let b = m.preview(&engine, &e).map_err(|e| kind(&e));
                    assert_eq!(a, b, "{ctx}: burst preview {e:?}");
                    let a = s.commit_preview(CommitMode::Merge).unwrap();
                    let b = m.commit_preview(CommitMode::Merge);
                    assert_eq!(a, b, "{ctx}: burst commit_preview");
                    format!("Ok({a:?})")
                }
            }
            0..=39 => {
                let e = random_edit(&mut r, m.head(), &mut sticky);
                label = format!("commit:{}", edit_name(&e));
                let a = s.commit(&engine, e.clone(), mode).map_err(|e| kind(&e));
                let b = m.commit(&engine, &e, mode).map_err(|e| kind(&e));
                assert_eq!(a, b, "{ctx}: commit {e:?} {mode:?}");
                format!("{a:?}")
            }
            50..=64 => {
                let e = random_edit(&mut r, m.head(), &mut sticky);
                label = format!("preview:{}", edit_name(&e));
                let a = s.preview(&engine, e.clone()).map_err(|e| kind(&e));
                let b = m.preview(&engine, &e).map_err(|e| kind(&e));
                assert_eq!(a, b, "{ctx}: preview {e:?}");
                format!("{a:?}")
            }
            65..=69 => {
                let (a, b) = (s.cancel_preview(), m.cancel());
                assert_eq!(a, b, "{ctx}: cancel");
                format!("{a:?}")
            }
            70..=76 => {
                label = "commit_preview".into();
                let a = s.commit_preview(mode).unwrap();
                let b = m.commit_preview(mode);
                assert_eq!(a, b, "{ctx}: commit_preview {mode:?}");
                format!("{a:?}")
            }
            77..=80 => {
                s.seal();
                m.open = None;
                "seal".into()
            }
            81..=89 => {
                label = "undo".into();
                let (a, b) = (s.undo(), m.undo());
                assert_eq!(a, b, "{ctx}: undo");
                format!("{a:?}")
            }
            90..=96 => {
                label = "redo".into();
                let (a, b) = (s.redo(), m.redo());
                assert_eq!(a, b, "{ctx}: redo");
                format!("{a:?}")
            }
            _ => {
                s.mark_saved();
                m.saved = m.head_rev();
                "saved".into()
            }
        };
        assert_eq!(snap_json(s.current()), json_of(m.current()), "{ctx}");
        assert_eq!(snap_json(s.head()), json_of(m.head()), "{ctx}");
        assert_eq!((s.rev(), s.head_rev()), (m.rev(), m.head_rev()), "{ctx}");
        assert_eq!(s.is_provisional(), m.prov.is_some(), "{ctx}");
        assert_eq!(s.can_undo(), m.cursor > 0, "{ctx}");
        assert_eq!(s.can_redo(), m.cursor + 1 < m.entries.len(), "{ctx}");
        assert_eq!(s.history_len(), m.entries.len(), "{ctx}");
        assert_eq!(s.is_dirty(), m.head_rev() != m.saved, "{ctx}");
        assert_eq!(s.encoded_len(), m.current().encoded_len().unwrap(), "{ctx}");
        check_invariants(&engine, &s);
        // 引ける番号は同じで、同じ文書（履歴の手は 10 手ごとに全部、ほかは前後 2 手）
        let mut revs: Vec<Rev> = m
            .entries
            .iter()
            .enumerate()
            .filter(|(i, _)| step % 10 == 0 || i.abs_diff(m.cursor) <= 2)
            .map(|(_, (r, _))| *r)
            .collect();
        revs.extend(m.recent.iter().map(|(r, _)| *r));
        revs.extend(m.prov.as_ref().map(|p| p.rev));
        for rev in revs {
            assert_eq!(
                s.snapshot(rev).map(snap_json),
                m.snapshot(rev).map(json_of),
                "{ctx}: {rev:?}"
            );
        }
        assert_eq!(s.snapshot(Rev(m.next_rev)).map(snap_json), None, "{ctx}");
        trace.push(StepTrace {
            label,
            result,
            rev: s.rev(),
            head_rev: s.head_rev(),
            doc: snap_json(s.current()),
            keys: s.current().nodes().map(|n| n.key).collect(),
        });
    }
    trace
}

// ---------------------------------------------------------------------------------------------
// E33・E1：参照の実装との照合
// ---------------------------------------------------------------------------------------------

/// ランダムな列を種ごとに当て、手の種類と結果の組を数える。
fn model_coverage(seeds: std::ops::Range<u64>, steps: usize) -> BTreeMap<String, usize> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for seed in seeds {
        let max_steps = [1000, 6, 2, 0][(seed % 4) as usize];
        let trace = run_sequence(0x5e55_0000 + seed, steps, max_steps);
        let mut prev_head = Rev(0);
        for t in &trace {
            let ok = ["Ok", "Some", "Committed"]
                .iter()
                .any(|p| t.result.starts_with(p));
            let outcome = if t.result.starts_with("Err") {
                if t.result.contains("limit") {
                    "limit"
                } else {
                    "invalid"
                }
            } else if t.result.contains("noop: true") {
                "noop"
            } else if t.result.contains("merged: true") && t.head_rev < prev_head {
                "collapsed"
            } else if t.result.contains("merged: true") {
                "merged"
            } else if ok {
                "ok"
            } else {
                "none"
            };
            *seen.entry(format!("{}={outcome}", t.label)).or_default() += 1;
            if let Some((_, edit)) = t.label.split_once(':') {
                *seen.entry(format!("*:{edit}={outcome}")).or_default() += 1;
            }
            *seen.entry(format!("*={outcome}")).or_default() += 1;
            prev_head = t.head_rev;
        }
    }
    seen
}

/// ランダムな編集の列（足す・消す・動かす・引数・追記・有効と無効・出力・置き換え・仮の値・まとめ・区切り・取り消し・
/// やり直し）で、セッションと素朴な参照の実装の文書・番号・取り消しの可否・結果が、どの手のあとでも同じ。
#[test]
fn matches_the_naive_model_on_random_sequences() {
    let seen = model_coverage(0..12, 150);
    // どの手も、通る場合と断る場合を十分に回している（列の作り方が偏って照合が空回りしないように）
    let at_least = |key: &str, n: usize| {
        assert!(seen.get(key).copied().unwrap_or(0) >= n, "{key}: {seen:#?}");
    };
    for e in [
        "AddNode",
        "RemoveNode",
        "MoveNode",
        "SetEnabled",
        "SetParams",
        "SetParam",
        "AppendItems",
        "SetOutput",
        "Replace",
    ] {
        at_least(&format!("commit:{e}=ok"), 3);
        at_least(&format!("*:{e}=invalid"), 1);
    }
    for e in ["SetParam", "SetParams", "AddNode", "SetOutput"] {
        at_least(&format!("*:{e}=limit"), 1);
    }
    at_least("burst:commit=merged", 10);
    at_least("burst:commit=collapsed", 3);
    at_least("burst:preview=merged", 5);
    at_least("burst:preview=collapsed", 2);
    at_least("*=noop", 20);
    at_least("preview:SetParam=ok", 10);
    at_least("commit_preview=ok", 5);
    at_least("undo=ok", 30);
    at_least("redo=ok", 10);
}

/// 同じ照合を長く回す（ふだんは回さない：`cargo test --release -p kura-engine --test session -- --ignored`）。
#[test]
#[ignore]
fn matches_the_naive_model_on_long_random_sequences() {
    model_coverage(1_000..1_400, 300);
}

/// E1：同じ列を 2 回当てると、どの手のあとでも同じ番号・同じ鍵・同じ文書。
#[test]
fn same_sequence_gives_same_revs_keys_and_documents() {
    for seed in [11u64, 12] {
        let a = run_sequence(seed, 80, 1000);
        let b = run_sequence(seed, 80, 1000);
        assert_eq!(a, b);
    }
}

// ---------------------------------------------------------------------------------------------
// まとめ方
// ---------------------------------------------------------------------------------------------

/// Merge：同じキー（ノードと引数の名前）の続く変更は 1 手。別のキー・Step・seal・undo・redo・Replace で区切る。
#[test]
fn merge_folds_one_key_and_breaks_on_others() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    let crop = |x: i64| set_param(1, "rect", json!({ "x": x, "y": 1, "w": 6, "h": 6 }));
    let c1 = s.commit(&engine, crop(2), CommitMode::Merge).unwrap();
    let c2 = s.commit(&engine, crop(3), CommitMode::Merge).unwrap();
    let c3 = s.commit(&engine, crop(4), CommitMode::Merge).unwrap();
    assert!(!c1.merged && c2.merged && c3.merged);
    assert!(c1.rev < c2.rev && c2.rev < c3.rev);
    assert_eq!(s.history_len(), 2);
    // まとめた手は新しい番号になり、古い番号は引けない
    assert!(s.snapshot(c1.rev).is_none());
    // 別のキー（同じノードの別の引数）は新しい手
    let other = s
        .commit(&engine, set_param(1, "x", json!(1)), CommitMode::Merge)
        .unwrap();
    assert!(!other.merged);
    assert_eq!(s.history_len(), 3);
    // seal のあとは新しい手
    s.seal();
    let after_seal = s
        .commit(&engine, set_param(1, "x", json!(2)), CommitMode::Merge)
        .unwrap();
    assert!(!after_seal.merged);
    assert_eq!(s.history_len(), 4);
    // Step のあとは新しい手
    s.commit(&engine, set_param(1, "x", json!(3)), CommitMode::Step)
        .unwrap();
    let after_step = s
        .commit(&engine, set_param(1, "x", json!(4)), CommitMode::Merge)
        .unwrap();
    assert!(!after_step.merged);
    // undo・redo のあとは新しい手
    let before = s.history_len();
    s.undo();
    s.redo();
    let after_redo = s
        .commit(&engine, set_param(1, "x", json!(5)), CommitMode::Merge)
        .unwrap();
    assert!(!after_redo.merged);
    assert_eq!(s.history_len(), before + 1);
    // Replace は区切る
    s.commit(
        &engine,
        Edit::Replace { graph: sample() },
        CommitMode::Merge,
    )
    .unwrap();
    let after_replace = s
        .commit(&engine, set_param(1, "x", json!(6)), CommitMode::Merge)
        .unwrap();
    assert!(!after_replace.merged);
    // SetParams のキーはノード
    let p1 = s
        .commit(
            &engine,
            Edit::SetParams {
                node: NodeId(2),
                params: json!({ "axis": "vertical" }),
            },
            CommitMode::Merge,
        )
        .unwrap();
    let p2 = s
        .commit(
            &engine,
            Edit::SetParams {
                node: NodeId(2),
                params: json!({ "axis": "horizontal", "x": 1 }),
            },
            CommitMode::Merge,
        )
        .unwrap();
    assert!(!p1.merged && p2.merged);
}

/// まとめている手で値を元に戻すと、その手が消える（取り消すものがなくなる）。
#[test]
fn returning_the_value_removes_the_step() {
    let engine = Engine::new().unwrap();
    let g = sample();
    let mut s = open(&g);
    let r0 = s.head_rev();
    s.commit(
        &engine,
        set_param(2, "axis", json!("vertical")),
        CommitMode::Merge,
    )
    .unwrap();
    assert!(s.can_undo());
    let back = s
        .commit(
            &engine,
            set_param(2, "axis", json!("horizontal")),
            CommitMode::Merge,
        )
        .unwrap();
    assert_eq!(
        back,
        Committed {
            rev: r0,
            created: None,
            merged: true,
            noop: false
        }
    );
    assert!(!s.can_undo());
    assert_eq!(s.history_len(), 1);
    assert_eq!(snap_json(s.head()), json_of(&g));
    // 消したあとの Merge は、土台の手にまとめず新しい手になる
    let again = s
        .commit(
            &engine,
            set_param(2, "axis", json!("vertical")),
            CommitMode::Merge,
        )
        .unwrap();
    assert!(!again.merged);
    assert_eq!(s.history_len(), 2);
}

/// まとめられない手（足す・追記・有効と無効など）は、Merge を渡しても Step と同じ。
#[test]
fn unmergeable_edits_are_steps_even_with_merge() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    for i in 0..3 {
        let c = s
            .commit(
                &engine,
                append(3, vec![json!({ "k": 10 + i })]),
                CommitMode::Merge,
            )
            .unwrap();
        assert!(!c.merged);
    }
    s.commit(
        &engine,
        Edit::SetEnabled {
            node: NodeId(2),
            enabled: false,
        },
        CommitMode::Merge,
    )
    .unwrap();
    assert_eq!(s.history_len(), 5);
}

/// 何も変えない commit は手を積まず、番号も変えない（16.0 と 16 のような書き方の違いも同じ中身）。
#[test]
fn noop_commits_do_not_add_steps() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    let rev = s.rev();
    for e in [
        set_param(2, "axis", json!("horizontal")),
        set_param(1, "rect", json!({ "x": 1.0, "y": 1, "w": 6.0, "h": 6 })),
        Edit::SetEnabled {
            node: NodeId(1),
            enabled: true,
        },
        append(3, Vec::new()),
        Edit::MoveNode {
            node: NodeId(2),
            to: 1,
        },
        Edit::Replace { graph: sample() },
    ] {
        let c = s.commit(&engine, e.clone(), CommitMode::Step).unwrap();
        assert!(c.noop, "{e:?}");
        assert_eq!(c.rev, rev);
    }
    assert_eq!(s.history_len(), 1);
    assert!(!s.can_undo());
}

// ---------------------------------------------------------------------------------------------
// 仮の値と番号
// ---------------------------------------------------------------------------------------------

/// preview は履歴に入らず、次の preview で置き換わる。cancel で頭に戻る。commit は仮の値ではなく頭に当たる。
/// undo で仮の値は捨てられる。commit_preview は仮の値の番号で確定する。
#[test]
fn previews_stay_out_of_history() {
    let engine = Engine::new().unwrap();
    let g = sample();
    let mut s = open(&g);
    let head = s.head_rev();
    let p1 = s
        .preview(&engine, set_param(2, "axis", json!("vertical")))
        .unwrap();
    assert!(s.is_provisional() && p1 > head);
    assert_eq!(s.rev(), p1);
    assert_eq!(s.head_rev(), head);
    assert_eq!(s.history_len(), 1);
    assert!(!s.can_undo());
    // 次の preview は頭に当たり、前の仮の値を置き換える
    let p2 = s
        .preview(
            &engine,
            set_param(1, "rect", json!({ "x": 2, "y": 1, "w": 6, "h": 6 })),
        )
        .unwrap();
    assert!(p2 > p1);
    let doc = s.current().to_graph();
    assert_eq!(doc.nodes[1].params, json!({ "axis": "horizontal" }));
    // cancel で頭に戻る
    assert_eq!(s.cancel_preview(), Some(head));
    assert_eq!(s.cancel_preview(), None);
    assert!(!s.is_provisional());
    assert_eq!(snap_json(s.current()), json_of(&g));
    // commit は頭に当たり、仮の値を捨てる
    s.preview(&engine, set_param(2, "axis", json!("vertical")))
        .unwrap();
    let c = s
        .commit(
            &engine,
            set_param(1, "rect", json!({ "x": 3, "y": 1, "w": 6, "h": 6 })),
            CommitMode::Step,
        )
        .unwrap();
    assert!(!s.is_provisional());
    let doc = s.head().to_graph();
    assert_eq!(doc.nodes[1].params, json!({ "axis": "horizontal" }));
    assert_eq!(doc.nodes[0].params["rect"]["x"], json!(3));
    // commit_preview は仮の値の番号で確定する
    let p = s
        .preview(&engine, set_param(2, "axis", json!("vertical")))
        .unwrap();
    let cp = s.commit_preview(CommitMode::Step).unwrap();
    assert_eq!(cp.rev, p);
    assert!(!cp.noop);
    assert_eq!(s.head_rev(), p);
    // 仮の値がなければ何もしない
    assert!(s.commit_preview(CommitMode::Step).unwrap().noop);
    // undo で仮の値は捨てられる
    s.preview(&engine, set_param(2, "axis", json!("horizontal")))
        .unwrap();
    assert_eq!(s.undo(), Some(c.rev));
    assert!(!s.is_provisional());
    // 何も変えない preview は仮の値を出さない
    assert_eq!(
        s.preview(&engine, set_param(2, "axis", json!("horizontal")))
            .unwrap(),
        s.head_rev()
    );
    assert!(!s.is_provisional());
    // 足すノードの preview は、確定したときに ID を返す
    s.preview(
        &engine,
        Edit::AddNode {
            at: 0,
            op: "image.flip".into(),
            v: 1,
            params: json!({ "axis": "vertical" }),
            seed: None,
            extra_inputs: Vec::new(),
        },
    )
    .unwrap();
    let added = s.commit_preview(CommitMode::Step).unwrap();
    assert_eq!(added.created, Some(NodeId(4)));
}

/// 番号は commit・preview で単調に増え、undo・redo は戻った先の手の番号を返す。捨てた仮の値は 4 つまで引ける。
#[test]
fn revs_are_monotonic_and_undo_returns_the_target_rev() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    let r0 = s.rev();
    let r1 = s
        .commit(&engine, set_param(1, "x", json!(1)), CommitMode::Step)
        .unwrap()
        .rev;
    let r2 = s
        .commit(&engine, set_param(1, "x", json!(2)), CommitMode::Step)
        .unwrap()
        .rev;
    assert!(r0 < r1 && r1 < r2);
    assert_eq!(s.undo(), Some(r1));
    assert_eq!(s.undo(), Some(r0));
    assert_eq!(s.undo(), None);
    assert_eq!(s.redo(), Some(r1));
    assert_eq!(s.redo(), Some(r2));
    assert_eq!(s.redo(), None);
    // 捨てた仮の値は直前の 4 つまで引ける
    let previews: Vec<Rev> = (0..6)
        .map(|i| {
            s.preview(&engine, set_param(1, "x", json!(10 + i)))
                .unwrap()
        })
        .collect();
    assert!(previews.windows(2).all(|w| w[0] < w[1] && w[0] > r2));
    s.cancel_preview();
    for (i, rev) in previews.iter().enumerate() {
        assert_eq!(s.snapshot(*rev).is_some(), i >= 2, "{i}");
    }
    let doc = s.snapshot(previews[5]).unwrap().to_graph();
    assert_eq!(doc.nodes[0].params["x"], json!(15));
    // 新しい commit で redo の枝を切ると、切った手の番号は引けない
    s.undo();
    s.commit(&engine, set_param(1, "x", json!(99)), CommitMode::Step)
        .unwrap();
    assert!(s.snapshot(r2).is_none());
    assert!(s.snapshot(r1).is_some());
}

/// 保存の印：戻って進めば、保存した頭と同じなので汚れていない。
#[test]
fn saved_mark_follows_the_head_rev() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    assert!(!s.is_dirty());
    s.commit(&engine, set_param(1, "x", json!(1)), CommitMode::Step)
        .unwrap();
    assert!(s.is_dirty());
    s.mark_saved();
    assert!(!s.is_dirty());
    s.undo();
    assert!(s.is_dirty());
    s.redo();
    assert!(!s.is_dirty());
    assert_eq!(s.base(), Digest([1; 32]));
}

// ---------------------------------------------------------------------------------------------
// 履歴の上限
// ---------------------------------------------------------------------------------------------

/// 手の数の上限：古い手から捨て、カーソルは土台より前にならない。
#[test]
fn step_limit_drops_the_oldest_steps() {
    let engine = Engine::new().unwrap();
    let limits = HistoryLimits {
        max_steps: 3,
        max_bytes: u64::MAX,
    };
    let mut s = EditSession::open(&engine, &sample(), Digest([0; 32]), limits).unwrap();
    let revs: Vec<Rev> = (0..6)
        .map(|i| {
            s.commit(&engine, set_param(1, "x", json!(i)), CommitMode::Step)
                .unwrap()
                .rev
        })
        .collect();
    assert_eq!(s.history_len(), 4);
    let mut back = Vec::new();
    while let Some(r) = s.undo() {
        back.push(r);
    }
    assert_eq!(back, vec![revs[4], revs[3], revs[2]]);
    assert_eq!(s.head().to_graph().nodes[0].params["x"], json!(2));
    assert!(s.snapshot(revs[1]).is_none());
}

/// バイトの上限：大きな手（Replace）で効き、古い手から捨てる。数え方は手を積むたびに増え、捨てると減る。
#[test]
fn byte_limit_drops_the_oldest_steps() {
    let engine = Engine::new().unwrap();
    let big = |c: char| {
        chain(&[(
            "image.flip",
            json!({ "axis": "horizontal", "pad": c.to_string().repeat(1 << 20) }),
        )])
    };
    let limits = HistoryLimits {
        max_steps: 1000,
        max_bytes: 3 << 20,
    };
    let mut s = EditSession::open(&engine, &sample(), Digest([0; 32]), limits).unwrap();
    let start = s.history_bytes();
    assert_eq!(start, s.head().encoded_len() as u64);
    let mut last = start;
    // 小さな手は少しずつ増える
    for i in 0..3 {
        s.commit(&engine, set_param(1, "x", json!(i)), CommitMode::Step)
            .unwrap();
        assert!(s.history_bytes() > last);
        last = s.history_bytes();
    }
    for (i, c) in ['a', 'b', 'c', 'd', 'e'].into_iter().enumerate() {
        s.commit(&engine, Edit::Replace { graph: big(c) }, CommitMode::Step)
            .unwrap();
        assert!(s.history_bytes() <= limits.max_bytes, "{i}");
    }
    // 1 MiB の手は 2 つまでしか残らない（3 つ目を足すと 3 MiB を超える）
    assert_eq!(s.history_len(), 2);
    assert!(s.can_undo());
    s.undo();
    assert!(!s.can_undo());
    assert!(
        s.head().to_graph().nodes[0].params["pad"]
            .as_str()
            .unwrap()
            .starts_with('d')
    );
    // 1 手で上限を超えるなら、今の手だけが残る
    let tiny = HistoryLimits {
        max_steps: 1000,
        max_bytes: 1 << 20,
    };
    let mut t = EditSession::open(&engine, &sample(), Digest([0; 32]), tiny).unwrap();
    t.commit(&engine, Edit::Replace { graph: big('z') }, CommitMode::Step)
        .unwrap();
    assert_eq!(t.history_len(), 1);
    assert!(!t.can_undo());
}

/// 追記の手のバイトは、足した要素と写した切れ目の分だけ（文書全体ではない）。
#[test]
fn append_steps_count_only_what_they_add() {
    let engine = Engine::new().unwrap();
    let items: Vec<J> = (0..1000)
        .map(|i| json!({ "k": i, "pad": "p".repeat(100) }))
        .collect();
    let g = chain(&[("paint.actions", json!({ "actions": items }))]);
    let mut s = open(&g);
    let before = s.history_bytes();
    let item = json!({ "k": 5000, "pad": "q".repeat(100) });
    let item_len = serde_json::to_vec(&item).unwrap().len() as u64;
    s.commit(&engine, append(1, vec![item]), CommitMode::Step)
        .unwrap();
    let added = s.history_bytes() - before;
    assert!(added >= item_len, "{added}");
    // 切れ目の列（16 個）・写した最後の切れ目（41 個）・ノードの枠の分を足しても、文書（約 120 KB）よりずっと小さい
    assert!(added < 2_000, "{added}");
}

// ---------------------------------------------------------------------------------------------
// 構造の手
// ---------------------------------------------------------------------------------------------

/// 線形の列での足す・消す・動かす：つなぎ直しと出力の from が正しく、next_id を使い回さない。
#[test]
fn structure_edits_rewire_linear_chains() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    let add = |at: usize, seed: Option<u64>| Edit::AddNode {
        at,
        op: "image.flip".into(),
        v: 1,
        params: json!({ "axis": "vertical" }),
        seed,
        extra_inputs: vec![PortRef::Input { id: InputId(1) }],
    };
    let a = s.commit(&engine, add(1, None), CommitMode::Step).unwrap();
    assert_eq!(a.created, Some(NodeId(4)));
    let g = s.head().to_graph();
    assert_eq!(
        node_ids(&g),
        vec![NodeId(1), NodeId(4), NodeId(2), NodeId(3)]
    );
    assert_eq!(
        g.nodes[1].inputs,
        vec![
            PortRef::Node { id: NodeId(1) },
            PortRef::Input { id: InputId(1) }
        ]
    );
    assert_eq!(g.nodes[2].inputs, vec![PortRef::Node { id: NodeId(4) }]);
    // 種を渡さなければ ID の値
    assert_eq!(g.nodes[1].seed, 4);
    assert_eq!(g.next_id, 5);
    // 末尾に足すと出力 0 がつながる
    let b = s
        .commit(&engine, add(4, Some(77)), CommitMode::Step)
        .unwrap();
    let g = s.head().to_graph();
    assert_eq!(
        g.outputs[0].from,
        PortRef::Node {
            id: b.created.unwrap()
        }
    );
    assert_eq!(g.nodes[4].seed, 77);
    // 先頭のノードを消すと、次のノードが入力 0 を読む
    s.commit(
        &engine,
        Edit::RemoveNode { node: NodeId(1) },
        CommitMode::Step,
    )
    .unwrap();
    let g = s.head().to_graph();
    assert_eq!(g.nodes[0].id, NodeId(4));
    assert_eq!(g.nodes[0].inputs[0], PortRef::Input { id: InputId(0) });
    // 最後のノードを消すと、出力 0 が前のノードにつながる
    s.commit(
        &engine,
        Edit::RemoveNode { node: NodeId(5) },
        CommitMode::Step,
    )
    .unwrap();
    let g = s.head().to_graph();
    assert_eq!(g.outputs[0].from, PortRef::Node { id: NodeId(3) });
    // 消した ID は使い回さない
    let c = s.commit(&engine, add(0, None), CommitMode::Step).unwrap();
    assert_eq!(c.created, Some(NodeId(6)));
    // 動かす：先頭のノードを末尾へ
    s.commit(
        &engine,
        Edit::MoveNode {
            node: NodeId(6),
            to: 3,
        },
        CommitMode::Step,
    )
    .unwrap();
    let g = s.head().to_graph();
    assert_eq!(
        node_ids(&g),
        vec![NodeId(4), NodeId(2), NodeId(3), NodeId(6)]
    );
    assert_eq!(g.nodes[0].inputs[0], PortRef::Input { id: InputId(0) });
    assert_eq!(g.nodes[3].inputs[0], PortRef::Node { id: NodeId(3) });
    assert_eq!(g.outputs[0].from, PortRef::Node { id: NodeId(6) });
    assert!(g.check_structure(100).is_empty());
    // 全部消すと、出力 0 は入力 0
    for id in [4, 2, 3, 6] {
        s.commit(
            &engine,
            Edit::RemoveNode { node: NodeId(id) },
            CommitMode::Step,
        )
        .unwrap();
    }
    let g = s.head().to_graph();
    assert!(g.nodes.is_empty());
    assert_eq!(g.outputs[0].from, PortRef::Input { id: InputId(0) });
    assert_eq!(g.next_id, 7);
}

/// 列でない文書の構造の手、2 つ目からの入力で参照されているノードを消す手、後ろのノードを読む入力は断り、状態を変えない。
/// 引数と有効・無効の手は、列でない文書でも受け付ける。
#[test]
fn structure_edits_refuse_nonlinear_documents_and_referenced_nodes() {
    let engine = Engine::new().unwrap();
    let mut g = sample();
    g.nodes[1].inputs[0] = PortRef::Input { id: InputId(0) };
    let mut s = open(&g);
    let before = snap_json(s.head());
    for e in [
        Edit::RemoveNode { node: NodeId(2) },
        Edit::MoveNode {
            node: NodeId(2),
            to: 0,
        },
        Edit::AddNode {
            at: 0,
            op: "image.flip".into(),
            v: 1,
            params: json!({}),
            seed: None,
            extra_inputs: Vec::new(),
        },
    ] {
        let err = s.commit(&engine, e.clone(), CommitMode::Step).unwrap_err();
        assert_eq!(invalid_code(&err), Some(IssueCode::BadGraph), "{e:?}");
    }
    s.commit(
        &engine,
        set_param(2, "axis", json!("vertical")),
        CommitMode::Step,
    )
    .unwrap();
    s.commit(
        &engine,
        Edit::SetEnabled {
            node: NodeId(1),
            enabled: false,
        },
        CommitMode::Step,
    )
    .unwrap();
    assert_ne!(snap_json(s.head()), before);
    // 2 つ目からの入力で参照されているノード
    let mut s = open(&sample());
    s.commit(
        &engine,
        Edit::AddNode {
            at: 3,
            op: "image.flip".into(),
            v: 1,
            params: json!({}),
            seed: None,
            extra_inputs: vec![PortRef::Node { id: NodeId(1) }],
        },
        CommitMode::Step,
    )
    .unwrap();
    let before = (snap_json(s.head()), s.rev(), s.history_len());
    for e in [
        Edit::RemoveNode { node: NodeId(1) },
        // 参照されているノードを、参照するノードより後ろへ動かす
        Edit::MoveNode {
            node: NodeId(1),
            to: 3,
        },
        Edit::MoveNode {
            node: NodeId(4),
            to: 0,
        },
        // 後ろのノード・ない入力を読む
        Edit::AddNode {
            at: 0,
            op: "image.flip".into(),
            v: 1,
            params: json!({}),
            seed: None,
            extra_inputs: vec![PortRef::Node { id: NodeId(2) }],
        },
        Edit::AddNode {
            at: 0,
            op: "image.flip".into(),
            v: 1,
            params: json!({}),
            seed: None,
            extra_inputs: vec![PortRef::Input { id: InputId(9) }],
        },
        Edit::AddNode {
            at: 9,
            op: "image.flip".into(),
            v: 1,
            params: json!({}),
            seed: None,
            extra_inputs: Vec::new(),
        },
        Edit::MoveNode {
            node: NodeId(2),
            to: 4,
        },
        Edit::RemoveNode { node: NodeId(42) },
    ] {
        let err = s.commit(&engine, e.clone(), CommitMode::Step).unwrap_err();
        assert_eq!(invalid_code(&err), Some(IssueCode::BadGraph), "{e:?}");
        assert!(s.preview(&engine, e.clone()).is_err(), "{e:?}");
        assert_eq!((snap_json(s.head()), s.rev(), s.history_len()), before);
    }
    // ノードの数の上限
    let small = Engine::with_limits(Limits {
        max_nodes: 3,
        ..Limits::default()
    })
    .unwrap();
    let mut t =
        EditSession::open(&small, &sample(), Digest([0; 32]), HistoryLimits::default()).unwrap();
    let err = t
        .commit(
            &small,
            Edit::AddNode {
                at: 0,
                op: "image.flip".into(),
                v: 1,
                params: json!({}),
                seed: None,
                extra_inputs: Vec::new(),
            },
            CommitMode::Step,
        )
        .unwrap_err();
    assert_eq!(invalid_code(&err), Some(IssueCode::TooLarge));
}

// ---------------------------------------------------------------------------------------------
// 検証（E8・E25）
// ---------------------------------------------------------------------------------------------

/// 知らない操作・新しすぎる版（E8）、深すぎる入れ子・大きすぎる文書（E25）は断り、状態を変えない。
/// 入れ子は保存・読み込みと同じ数え方（上限ちょうどは受け付ける）。
#[test]
fn validation_refuses_and_keeps_state() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    s.preview(&engine, set_param(1, "x", json!(5))).unwrap();
    let before = (
        snap_json(s.head()),
        snap_json(s.current()),
        s.rev(),
        s.history_len(),
    );
    let add = |op: &str, v: u16, params: J| Edit::AddNode {
        at: 0,
        op: op.into(),
        v,
        params,
        seed: None,
        extra_inputs: Vec::new(),
    };
    let cases: Vec<(Edit, &str, Option<IssueCode>)> = vec![
        (
            add("test.unknown", 1, json!({})),
            "invalid",
            Some(IssueCode::UnknownOp),
        ),
        (
            add("image.flip", 99, json!({})),
            "invalid",
            Some(IssueCode::UnknownVersion),
        ),
        (add("image.flip", 1, deep(MAX_DEPTH - 2)), "limit", None),
        (set_param(2, "axis", deep(MAX_DEPTH - 3)), "limit", None),
        (
            Edit::SetParams {
                node: NodeId(2),
                params: deep(MAX_DEPTH - 2),
            },
            "limit",
            None,
        ),
        (append(3, vec![deep(MAX_DEPTH - 4)]), "limit", None),
        (
            set_param(2, "pad", json!("a".repeat(MAX_DOC_BYTES))),
            "limit",
            None,
        ),
        (
            Edit::SetOutput {
                output: OutputId(0),
                format: FormatChoice::To {
                    format: "png".into(),
                    options: deep(MAX_DEPTH - 3),
                },
                quantize: None,
                expand: Expand::One,
            },
            "limit",
            None,
        ),
        (
            set_param(9, "axis", json!(1)),
            "invalid",
            Some(IssueCode::BadGraph),
        ),
        (
            Edit::SetOutput {
                output: OutputId(3),
                format: FormatChoice::Keep,
                quantize: None,
                expand: Expand::One,
            },
            "invalid",
            Some(IssueCode::BadGraph),
        ),
        // 追記できない引数・形
        (
            Edit::AppendItems {
                node: NodeId(2),
                field: "actions".into(),
                items: vec![json!(1)],
            },
            "invalid",
            None,
        ),
        (
            Edit::AppendItems {
                node: NodeId(3),
                field: "points".into(),
                items: vec![json!(1)],
            },
            "invalid",
            None,
        ),
        (
            Edit::Replace {
                graph: {
                    let mut g = sample();
                    g.schema = SCHEMA - 1;
                    g
                },
            },
            "invalid",
            None,
        ),
        (
            Edit::Replace {
                graph: {
                    let mut g = sample();
                    g.nodes[2].op = "test.unknown".into();
                    g
                },
            },
            "invalid",
            Some(IssueCode::UnknownOp),
        ),
        (
            Edit::Replace {
                graph: {
                    let mut g = sample();
                    g.nodes[0].inputs[0] = PortRef::Node { id: NodeId(3) };
                    g
                },
            },
            "invalid",
            Some(IssueCode::BadGraph),
        ),
    ];
    for (e, want, code) in cases {
        let err = s.commit(&engine, e.clone(), CommitMode::Step).unwrap_err();
        assert_eq!(kind(&err), want, "{e:?}");
        if code.is_some() {
            assert_eq!(invalid_code(&err), code, "{e:?}");
        }
        assert!(s.preview(&engine, e.clone()).is_err(), "{e:?}");
        assert_eq!(
            (
                snap_json(s.head()),
                snap_json(s.current()),
                s.rev(),
                s.history_len()
            ),
            before,
            "{e:?}"
        );
    }
    // 上限ちょうどの深さは受け付け、保存して読み直せる
    for e in [
        add("image.flip", 1, deep(MAX_DEPTH - 3)),
        set_param(2, "axis", deep(MAX_DEPTH - 4)),
        append(3, vec![deep(MAX_DEPTH - 5)]),
    ] {
        let c = s.commit(&engine, e.clone(), CommitMode::Step).unwrap();
        assert!(!c.noop);
        let bytes = s.head().to_canonical_json().unwrap();
        assert_eq!(
            engine.load(&bytes).unwrap().to_canonical_json().unwrap(),
            bytes
        );
    }
}

/// encoded_len は to_graph().encoded_len() と同じ（ランダムな追記・引数の変更・足す・消すのあとで）。
#[test]
fn encoded_len_matches_the_document() {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(0xe1e1);
    let mut s = open(&sample());
    let mut sticky = None;
    for _ in 0..200 {
        let e = random_edit(&mut r, &s.head().to_graph(), &mut sticky);
        let _ = s.commit(&engine, e, CommitMode::Step);
        let g = s.head().to_graph();
        assert_eq!(s.head().encoded_len(), g.encoded_len().unwrap());
        assert_eq!(snap_json(s.head()), json_of(&g));
    }
}

// ---------------------------------------------------------------------------------------------
// 共有と違い
// ---------------------------------------------------------------------------------------------

/// 追記のあとも、ほかのノードと前の切れ目は共有したまま。diff は Appended{from} を返し、前の引数に足した要素を足すと
/// 新しい引数になる。引数を変えたノードは Changed、足した・消したノードは Added・Removed。
#[test]
fn appends_share_nodes_and_diff_reports_them() {
    let engine = Engine::new().unwrap();
    let items: Vec<J> = (0..130).map(|i| json!({ "k": i })).collect();
    let g = chain(&[
        ("image.flip", json!({ "axis": "vertical" })),
        ("paint.actions", json!({ "actions": items })),
        ("image.flip", json!({ "axis": "horizontal" })),
    ]);
    let mut s = open(&g);
    let mut r = determinism::rng(0xd1ff);
    for step in 0..30 {
        let old = s.head().clone();
        let n = 1 + below(&mut r, 70) as usize;
        let add: Vec<J> = (0..n).map(|i| json!({ "k": 1000 * step + i })).collect();
        s.commit(&engine, append(2, add.clone()), CommitMode::Step)
            .unwrap();
        let new = s.head().clone();
        assert!(new.shares_node(&old, NodeId(1)));
        assert!(new.shares_node(&old, NodeId(3)));
        assert!(!new.shares_node(&old, NodeId(2)));
        let d = new.diff(&old);
        let from = old.node(NodeId(2)).map(|v| match v.params {
            ParamsView::Log { log, .. } => log.len(),
            ParamsView::Json(_) => panic!("追記の配列のはず"),
        });
        assert_eq!(
            d.nodes,
            vec![
                (NodeId(1), NodeChange::Same),
                (
                    NodeId(2),
                    NodeChange::Appended {
                        field: "actions",
                        from: from.unwrap()
                    }
                ),
                (NodeId(3), NodeChange::Same),
            ]
        );
        assert!(!d.order_changed && !d.outputs_changed && !d.head_changed);
        // 前の切れ目は共有している
        let (ParamsView::Log { log: a, .. }, ParamsView::Log { log: b, .. }) = (
            old.node(NodeId(2)).unwrap().params,
            new.node(NodeId(2)).unwrap().params,
        ) else {
            panic!()
        };
        assert_eq!(b.shared_prefix(a), a.len());
        // 前の引数に足した要素を足すと、新しい引数
        let mut prev = old.node(NodeId(2)).unwrap().params.to_value();
        prev["actions"]
            .as_array_mut()
            .unwrap()
            .extend(b.iter_from(from.unwrap()).cloned());
        assert_eq!(prev, new.node(NodeId(2)).unwrap().params.to_value());
        assert_eq!(b.iter_from(from.unwrap()).cloned().collect::<Vec<_>>(), add);
    }
    // 引数の変更・足す・消す・動かす・出力
    let old = s.head().clone();
    s.commit(
        &engine,
        set_param(1, "axis", json!("horizontal")),
        CommitMode::Step,
    )
    .unwrap();
    let d = s.head().diff(&old);
    assert_eq!(d.nodes[0], (NodeId(1), NodeChange::Changed));
    assert_eq!(d.nodes[1], (NodeId(2), NodeChange::Same));
    let old = s.head().clone();
    s.commit(
        &engine,
        Edit::RemoveNode { node: NodeId(1) },
        CommitMode::Step,
    )
    .unwrap();
    s.commit(
        &engine,
        Edit::AddNode {
            at: 2,
            op: "image.flip".into(),
            v: 1,
            params: json!({}),
            seed: None,
            extra_inputs: Vec::new(),
        },
        CommitMode::Step,
    )
    .unwrap();
    let d = s.head().diff(&old);
    assert_eq!(
        d.nodes,
        vec![
            // 入力が変わった
            (NodeId(2), NodeChange::Changed),
            (NodeId(3), NodeChange::Same),
            (NodeId(4), NodeChange::Added),
            (NodeId(1), NodeChange::Removed),
        ]
    );
    assert!(d.order_changed && d.outputs_changed && !d.head_changed);
    // 中身が同じなら、作り直したスナップショットでも Same
    let again = open(&s.head().to_graph());
    let d = again.head().diff(s.head());
    assert!(d.nodes.iter().all(|(_, c)| *c == NodeChange::Same));
    assert!(!d.order_changed && !d.outputs_changed && !d.head_changed);
}

/// undo・redo は同じスナップショットに戻る（ノードを共有したまま）。
#[test]
fn undo_returns_the_same_shared_snapshot() {
    let engine = Engine::new().unwrap();
    let mut s = open(&sample());
    let first = s.head().clone();
    s.commit(
        &engine,
        append(3, vec![json!({ "k": 5 })]),
        CommitMode::Step,
    )
    .unwrap();
    let second = s.head().clone();
    s.undo();
    assert!(s.head().shares_node(&first, NodeId(3)));
    s.redo();
    assert!(s.head().shares_node(&second, NodeId(3)));
}

// ---------------------------------------------------------------------------------------------
// ノードの鍵
// ---------------------------------------------------------------------------------------------

/// 鍵が等しいことと、（操作・版・正規の引数・種・有効か）の正規の JSON が等しいことは同値。16.0 と 16 は同じ鍵。
#[test]
fn node_keys_are_equal_iff_the_canonical_nodes_are() {
    let engine = Engine::new().unwrap();
    let params = [
        json!({ "axis": 16 }),
        json!({ "axis": 16.0 }),
        json!({ "axis": "vertical" }),
        json!({ "actions": [] }),
        json!({ "actions": [{ "k": 1 }] }),
        json!({ "actions": [{ "k": 1.0 }] }),
        json!({ "actions": [{ "k": 1 }, { "k": 2 }] }),
        json!({ "actions": [{ "k": 1 }], "x": 1 }),
        json!({}),
        J::Null,
    ];
    let mut nodes = Vec::new();
    for op in ["image.flip", "paint.actions"] {
        for p in &params {
            for seed in [0u64, 1] {
                for enabled in [true, false] {
                    nodes.push((op, p.clone(), seed, enabled));
                }
            }
        }
    }
    let mut g = Graph::new_single();
    for (op, p, seed, _) in &nodes {
        g.push_linear(op, 1, p.clone(), *seed).unwrap();
    }
    for (n, (_, _, _, enabled)) in g.nodes.iter_mut().zip(&nodes) {
        n.enabled = *enabled;
    }
    let s = open(&g);
    let views: Vec<_> = s.head().nodes().collect();
    let canon = |n: &kura_engine::Node| {
        kura_engine::digest::canonical_json(&json!({
            "op": n.op, "v": n.v, "params": n.params, "seed": n.seed, "enabled": n.enabled
        }))
        .unwrap()
    };
    for (a, na) in views.iter().zip(&g.nodes) {
        assert_eq!(a.key, engine.registry().doc_node_key(na).unwrap());
        for (b, nb) in views.iter().zip(&g.nodes) {
            assert_eq!(a.key == b.key, canon(na) == canon(nb), "{na:?} {nb:?}");
        }
    }
    // 16.0 と 16、{"k":1.0} と {"k":1} は同じ鍵（操作ごとに 40 個、引数ごとに 4 個）
    assert_eq!(views[0].key, views[4].key);
    assert_eq!(views[40 + 16].key, views[40 + 20].key);
    assert_ne!(views[40 + 16].key, views[40 + 24].key);
}

/// 追記で作った連鎖・鍵は、保存して開き直した文書から全体で作ったものと同じ。
#[test]
fn appended_chains_equal_chains_from_reopened_documents() {
    let engine = Engine::new().unwrap();
    let mut s = open(&chain(&[("paint.actions", json!({ "actions": [] }))]));
    let mut r = determinism::rng(0xc4a1);
    for step in 0..40 {
        let n = below(&mut r, 9) as usize;
        let items: Vec<J> = (0..n)
            .map(|i| json!({ "k": step * 10 + i, "f": 0.5 * i as f64, "w": 2.0 }))
            .collect();
        s.commit(&engine, append(1, items), CommitMode::Step)
            .unwrap();
        let bytes = s.head().to_canonical_json().unwrap();
        let again =
            EditSession::open_json(&engine, &bytes, Digest([0; 32]), HistoryLimits::default())
                .unwrap();
        let (a, b) = (
            s.head().node(NodeId(1)).unwrap(),
            again.head().node(NodeId(1)).unwrap(),
        );
        assert_eq!(a.key, b.key);
        assert_eq!(a.params_digest, b.params_digest);
        let (ParamsView::Log { log: la, .. }, ParamsView::Log { log: lb, .. }) =
            (a.params, b.params)
        else {
            panic!("追記の配列のはず")
        };
        assert_eq!(la.chain(), lb.chain());
        assert_eq!(la.len(), lb.len());
    }
}

/// 下書きの形式の代わり：頭の文書（Graph・正規の JSON）と土台の版から作り直すと、同じ文書・同じ鍵。仮の値は入らない。
/// 古いスキーマの文書は Engine::load で移す。新しすぎるスキーマ・壊れたバイト列は誤り（panic しない）。
#[test]
fn reopening_from_the_head_gives_the_same_document_and_keys() {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(0x4e0b);
    let mut s = open(&sample());
    let mut sticky = None;
    for _ in 0..60 {
        let e = random_edit(&mut r, &s.head().to_graph(), &mut sticky);
        let _ = s.commit(&engine, e, CommitMode::Step);
    }
    assert!(!s.head().to_graph().nodes.is_empty());
    s.preview(
        &engine,
        Edit::SetOutput {
            output: OutputId(0),
            format: FormatChoice::To {
                format: "png".into(),
                options: json!({ "x": 12345 }),
            },
            quantize: None,
            expand: Expand::One,
        },
    )
    .unwrap();
    assert!(s.is_provisional());
    let head = s.head().to_graph();
    let bytes = s.head().to_canonical_json().unwrap();
    let base = Digest([9; 32]);
    let a = EditSession::open(&engine, &head, base, HistoryLimits::default()).unwrap();
    let b = EditSession::open_json(&engine, &bytes, base, HistoryLimits::default()).unwrap();
    for t in [&a, &b] {
        assert_eq!(snap_json(t.head()), bytes);
        assert_eq!(t.base(), base);
        assert!(!t.can_undo() && !t.is_provisional());
        let keys: Vec<Digest> = t.head().nodes().map(|n| n.key).collect();
        assert_eq!(keys, s.head().nodes().map(|n| n.key).collect::<Vec<_>>());
        assert_eq!(t.head().digest().unwrap(), s.head().digest().unwrap());
    }
    // 古いスキーマ（rules のない 1）は移す
    let mut v: J = serde_json::from_slice(&bytes).unwrap();
    v.as_object_mut().unwrap().remove("rules");
    v["schema"] = json!(1);
    let old = EditSession::open_json(
        &engine,
        &serde_json::to_vec(&v).unwrap(),
        base,
        HistoryLimits::default(),
    )
    .unwrap();
    assert_eq!(snap_json(old.head()), bytes);
    // 今のスキーマでない Graph は open で断る（Engine::load で移してから）
    let mut g = head.clone();
    g.schema = SCHEMA - 1;
    assert!(EditSession::open(&engine, &g, base, HistoryLimits::default()).is_err());
    v["schema"] = json!(SCHEMA + 1);
    let err = EditSession::open_json(
        &engine,
        &serde_json::to_vec(&v).unwrap(),
        base,
        HistoryLimits::default(),
    )
    .err()
    .unwrap();
    assert_eq!(invalid_code(&err), Some(IssueCode::SchemaTooNew));
    // 壊れたバイト列
    for i in 0..200 {
        let mut broken = bytes.clone();
        let at = below(&mut r, broken.len() as u64) as usize;
        match i % 3 {
            0 => broken.truncate(at),
            1 => broken[at] = r.next_u32() as u8,
            _ => broken.insert(at, b'{'),
        }
        let _ = EditSession::open_json(&engine, &broken, base, HistoryLimits::default());
    }
}

// ---------------------------------------------------------------------------------------------
// 実行器とのつなぎ（段 10 では to_graph → compile）
// ---------------------------------------------------------------------------------------------

/// 筆の追記・引数の変更・足すのあとの頭を to_graph して compile・render すると、同じ文書を素直に組んで描いたものと同じ。
#[test]
fn to_graph_compiles_and_renders_like_the_plain_document() {
    let engine = Engine::new().unwrap();
    let stroke = |x: i64, c: u8| {
        json!({
            "type": "stroke", "tool": "pen", "color": { "rgba": [c, 10, 200, 255] }, "blend": "over",
            "brush": { "shape": "circle", "size": 2 }, "pixelPerfect": false, "frame": 0,
            "points": kura_engine::paint::PointList(vec![
                kura_engine::geom::IPoint { x, y: 1 },
                kura_engine::geom::IPoint { x: x + 5, y: 9 },
            ])
        })
    };
    let g = chain(&[
        ("image.flip", json!({ "axis": "horizontal" })),
        ("paint.actions", json!({ "actions": [stroke(1, 50)] })),
    ]);
    let mut s = open(&g);
    let mut plain = g.clone();
    for i in 0..5 {
        s.commit(
            &engine,
            append(2, vec![stroke(i * 3, 90 + i as u8)]),
            CommitMode::Step,
        )
        .unwrap();
        plain.nodes[1].params["actions"]
            .as_array_mut()
            .unwrap()
            .push(stroke(i * 3, 90 + i as u8));
    }
    s.commit(
        &engine,
        set_param(1, "axis", json!("vertical")),
        CommitMode::Step,
    )
    .unwrap();
    plain.nodes[0].params = json!({ "axis": "vertical" });
    let input = InputGen::Checker {
        w: 32,
        h: 24,
        fmt: PixelFormat::Rgba8,
        cell: 4,
    }
    .generate()
    .unwrap();
    let shapes = BTreeMap::from([(InputId(0), input.shape()), (InputId(1), input.shape())]);
    let values = BTreeMap::from([
        (
            InputId(0),
            InputValue {
                value: input.clone(),
                source: Digest([5; 32]),
            },
        ),
        (
            InputId(1),
            InputValue {
                value: input.clone(),
                source: Digest([6; 32]),
            },
        ),
    ]);
    let render = |g: &Graph| {
        let plan = engine.compile(g, &shapes).unwrap();
        engine.render(&plan, &values, &NoMonitor).unwrap().outputs[0].digest
    };
    assert_eq!(render(&s.head().to_graph()), render(&plain));
    assert_eq!(snap_json(s.head()), json_of(&plain));
}

// ---------------------------------------------------------------------------------------------
// 移行する版（実装のない古い版）のノード
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemsParams {
    #[serde(default)]
    items: Vec<J>,
}

impl OpParams for ItemsParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec("items", ParamKind::Strokes, Unit::None, json!([]))],
        }
    }
}

/// 試験用の操作（v2 が今の版。v1 は実装がなく、引数をそのまま v2 へ移す。`bad` を含む要素は移せない）。
struct ItemsOp;

impl Op for ItemsOp {
    const ID: &'static str = "test.items";
    const VERSION: u16 = 2;
    type Params = ItemsParams;
    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }
    fn infer(_: &ItemsParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: 0,
        })
    }
    fn apply(
        _: &ItemsParams,
        inputs: &[&Value],
        _: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        Ok(Applied::value(inputs[0].clone()))
    }
    fn migrate(from: u16, raw: J) -> Result<J, String> {
        if from == 1 && !raw.to_string().contains("bad") {
            Ok(raw)
        } else {
            Err("移せない".into())
        }
    }
    fn examples() -> Vec<Example<ItemsParams>> {
        Vec::new()
    }
}

/// 実装のない古い版のノードも、`resolve` が使う実装の記述で追記の配列になり、鍵は Registry::doc_node_key と同じ。
/// 追記で移せなくなる要素は断る（E8）。
#[test]
fn migrated_nodes_use_the_resolved_schema() {
    let mut registry = kura_engine::ops::registry().unwrap();
    registry.add_current::<ItemsOp>().unwrap();
    let engine = Engine::with_registry(registry, Limits::default());
    assert_eq!(
        engine.registry().append_field("test.items", 1),
        Some("items")
    );
    assert_eq!(
        engine.registry().append_field("test.items", 2),
        Some("items")
    );
    assert_eq!(engine.registry().append_field("test.items", 3), None);
    let mut g = Graph::new_single();
    g.push_linear("test.items", 1, json!({ "items": [1, 2] }), 0)
        .unwrap();
    let mut s = EditSession::open(&engine, &g, Digest([0; 32]), HistoryLimits::default()).unwrap();
    let old = s.head().clone();
    s.commit(
        &engine,
        Edit::AppendItems {
            node: NodeId(1),
            field: "items".into(),
            items: vec![json!(3)],
        },
        CommitMode::Step,
    )
    .unwrap();
    assert_eq!(
        s.head().diff(&old).nodes,
        vec![(
            NodeId(1),
            NodeChange::Appended {
                field: "items",
                from: 2
            }
        )]
    );
    let doc = s.head().to_graph();
    assert_eq!(
        s.head().node(NodeId(1)).unwrap().key,
        engine.registry().doc_node_key(&doc.nodes[0]).unwrap()
    );
    let err = s
        .commit(
            &engine,
            Edit::AppendItems {
                node: NodeId(1),
                field: "items".into(),
                items: vec![json!("bad")],
            },
            CommitMode::Step,
        )
        .unwrap_err();
    assert_eq!(invalid_code(&err), Some(IssueCode::UnknownVersion));
    assert_eq!(snap_json(s.head()), json_of(&doc));
}

/// セッションとスナップショットは、描くスレッドへ渡せる。
#[test]
fn sessions_and_snapshots_are_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<EditSession>();
    check::<Snapshot>();
}
