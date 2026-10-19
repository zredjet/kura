//! 文書のスナップショット（段 10。セッションの履歴の 1 手）。
//!
//! - ノードは `Arc` で持ち、変えなかったノードは前の手と共有する。追記だけの引数は `AppendLog`（64 個ずつの切れ目）で持つ。
//! - 引数は正規の形（`op::canonical_params` に往復させたもの）で持つ。`to_graph` の正規の JSON は、保存の文書
//!   （`Graph::to_canonical_json`）とバイト単位で同じ。
//! - 正規の JSON の長さをノードごとに持ち、文書全体の長さ（`MAX_DOC_BYTES` の確かめ）を差分で求める。
//! - ノードの鍵は `op::node_key`（実行器と同じ式。`Registry::doc_node_key` と同じ値）。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::digest::{Digest, MAX_DEPTH, canonical_len_within};
use crate::doc::{Graph, InputSlot, MAX_DOC_BYTES, Node, NodeId, OutputSpec, PortRef};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::op::{self, ParamSchema};

use super::log::AppendLog;

/// 文書の中で、ノードの引数の値がいくつの入れ子の中にあるか（文書・`nodes`・ノード）。
pub(crate) const PARAMS_DEPTH: usize = 3;
/// 追記だけの配列の要素がいくつの入れ子の中にあるか（文書・`nodes`・ノード・引数・配列）。
pub(crate) const ITEM_DEPTH: usize = 5;
/// 引数の中の 1 つの項目の値がいくつの入れ子の中にあるか（文書・`nodes`・ノード・引数）。
pub(crate) const FIELD_DEPTH: usize = 4;
/// 出力の形式の設定（`FormatChoice::To.options`）がいくつの入れ子の中にあるか（文書・`outputs`・出力・形式）。
pub(crate) const OPTIONS_DEPTH: usize = 4;

/// 値を `depth` 段の配列で包んだもの（文書の中と同じ深さで、入れ子の上限を数えるため）。
struct Nest<'a, T: ?Sized>(usize, &'a T);

impl<T: Serialize + ?Sized> Serialize for Nest<'_, T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        if self.0 == 0 {
            return self.1.serialize(s);
        }
        let mut seq = s.serialize_seq(Some(1))?;
        seq.serialize_element(&Nest(self.0 - 1, self.1))?;
        seq.end()
    }
}

/// 保存の形にできない（大きすぎる・深すぎる）誤りに、編集のことだと分かる文言を付ける。
pub(crate) fn too_large(e: EngineError) -> EngineError {
    match e {
        EngineError::Limit(m) => {
            EngineError::Limit(format!("この編集をすると文書を保存の形にできません：{m}"))
        }
        other => other,
    }
}

/// 文書の中で `depth` 段の入れ子の中にある値の、正規の JSON の長さ。入れ子が `MAX_DEPTH` を超える・`MAX_DOC_BYTES` を
/// 超える値は `Limit`（保存・読み込みと同じ数え方。深い値でも、上限の段で数えるのをやめるので再帰があふれない）。
pub(crate) fn measure<T: Serialize + ?Sized>(
    value: &T,
    depth: usize,
) -> Result<usize, EngineError> {
    let pad = depth.saturating_mul(2);
    let n = canonical_len_within(
        &Nest(depth.min(MAX_DEPTH), value),
        MAX_DOC_BYTES.saturating_add(pad),
    )
    .map_err(too_large)?;
    Ok(n.saturating_sub(pad))
}

/// 文書の頭（ノードの外）。
#[derive(Debug, PartialEq)]
pub(crate) struct Head {
    pub(crate) schema: u32,
    pub(crate) rules: u32,
    pub(crate) inputs: Vec<InputSlot>,
    pub(crate) outputs: Vec<OutputSpec>,
    pub(crate) next_id: u32,
    /// `nodes` を `[]` にした文書の正規の JSON の長さ。
    pub(crate) len: usize,
}

impl Head {
    /// 頭を作り、正規の JSON の長さを数える（深さ・大きさの上限を超えれば `Limit`）。
    pub(crate) fn new(
        schema: u32,
        rules: u32,
        inputs: Vec<InputSlot>,
        outputs: Vec<OutputSpec>,
        next_id: u32,
    ) -> Result<Self, EngineError> {
        let shell = Graph {
            schema,
            rules,
            inputs,
            nodes: Vec::new(),
            outputs,
            next_id,
        };
        let len = canonical_len_within(&shell, MAX_DOC_BYTES).map_err(too_large)?;
        Ok(Self {
            schema: shell.schema,
            rules: shell.rules,
            inputs: shell.inputs,
            outputs: shell.outputs,
            next_id: shell.next_id,
            len,
        })
    }
}

/// ノードの引数。
#[derive(Debug, Clone)]
pub(crate) enum SParams {
    /// 正規の形の値そのまま。
    Plain { value: Arc<Value>, len: usize },
    /// 追記だけの配列（`field`）を `AppendLog` にしたもの。`shell` は `field` を `[]` にした正規の形のオブジェクト。
    Log {
        shell: Arc<Value>,
        shell_len: usize,
        field: &'static str,
        log: AppendLog,
    },
}

impl SParams {
    /// 正規の形の引数から作る。`field`（`Registry::append_field`）があり、その項目が配列なら `Log`、ほかは `Plain`。
    /// 同じ中身なら、いつも同じ形になる。深さ・大きさの上限を超えれば `Limit`。
    pub(crate) fn build(
        field: Option<&'static str>,
        canonical: Value,
    ) -> Result<Self, EngineError> {
        match (field, canonical) {
            (Some(f), Value::Object(mut map)) if map.get(f).is_some_and(Value::is_array) => {
                let items = match map.insert(f.to_string(), Value::Array(Vec::new())) {
                    Some(Value::Array(items)) => items,
                    _ => Vec::new(),
                };
                let shell = Value::Object(map);
                let shell_len = measure(&shell, PARAMS_DEPTH)?;
                let measured = items
                    .into_iter()
                    .map(|it| measure(&it, ITEM_DEPTH).map(|len| (it, len)))
                    .collect::<Result<Vec<_>, _>>()?;
                let log = AppendLog::new().extend(measured)?;
                Ok(Self::Log {
                    shell: Arc::new(shell),
                    shell_len,
                    field: f,
                    log,
                })
            }
            (_, value) => {
                let len = measure(&value, PARAMS_DEPTH)?;
                Ok(Self::Plain {
                    value: Arc::new(value),
                    len,
                })
            }
        }
    }

    /// 正規の JSON の長さ。
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Plain { len, .. } => *len,
            // `"field":[]` の `[]` の中に、要素と `,` が入る
            Self::Log { shell_len, log, .. } => shell_len
                .saturating_add(log.items_len())
                .saturating_add(log.len().saturating_sub(1)),
        }
    }

    /// 引数の指紋（`op::params_digest` と同じ値。追記の配列は持っている連鎖から）。
    pub(crate) fn digest(&self) -> Result<Digest, EngineError> {
        match self {
            Self::Plain { value, .. } => op::params_digest(&ParamSchema::default(), value),
            Self::Log {
                shell, field, log, ..
            } => op::params_digest_with_chain(shell, field, log.chain(), log.len() as u64),
        }
    }

    /// 保存の形の値（追記の配列は要素を写す）。
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Plain { value, .. } => (**value).clone(),
            Self::Log {
                shell, field, log, ..
            } => {
                let mut v = (**shell).clone();
                if let Value::Object(map) = &mut v {
                    map.insert((*field).to_string(), Value::Array(log.to_vec()));
                }
                v
            }
        }
    }

    /// `older` と共有していない分の見積もり（バイト）。
    fn unshared_bytes(&self, older: Option<&SParams>) -> u64 {
        match (self, older) {
            (Self::Plain { value: a, len }, Some(Self::Plain { value: b, .. })) => {
                if Arc::ptr_eq(a, b) { 0 } else { *len as u64 }
            }
            (
                Self::Log {
                    shell: sa,
                    shell_len,
                    log: la,
                    ..
                },
                Some(Self::Log {
                    shell: sb, log: lb, ..
                }),
            ) => {
                let shell = if Arc::ptr_eq(sa, sb) {
                    0
                } else {
                    *shell_len as u64
                };
                shell.saturating_add(la.unshared_bytes(lb))
            }
            (Self::Plain { len, .. }, _) => *len as u64,
            (Self::Log { shell_len, log, .. }, _) => {
                (*shell_len as u64).saturating_add(log.unshared_bytes(&AppendLog::new()))
            }
        }
    }
}

/// スナップショットの中のノード。
#[derive(Debug)]
pub(crate) struct SNode {
    pub(crate) id: NodeId,
    pub(crate) op: Arc<str>,
    pub(crate) v: u16,
    pub(crate) inputs: Arc<[PortRef]>,
    pub(crate) seed: u64,
    pub(crate) enabled: bool,
    pub(crate) params: SParams,
    /// 引数の指紋（`op::params_digest`）。
    pub(crate) params_digest: Digest,
    /// ノードの鍵（`op::node_key`）。
    pub(crate) key: Digest,
    /// 引数を `null` にしたノードの正規の JSON の長さ。
    pub(crate) frame_len: usize,
}

/// 引数を `null` にしたノードの正規の JSON の長さ。
fn frame_len(
    id: NodeId,
    op: &str,
    v: u16,
    inputs: &[PortRef],
    seed: u64,
    enabled: bool,
) -> Result<usize, EngineError> {
    let node = Node {
        id,
        op: op.to_string(),
        v,
        params: Value::Null,
        inputs: inputs.to_vec(),
        seed,
        enabled,
    };
    measure(&node, PARAMS_DEPTH - 1)
}

impl SNode {
    /// ノードを作り、引数の指紋・鍵・長さを求める。
    pub(crate) fn new(
        id: NodeId,
        op: Arc<str>,
        v: u16,
        inputs: Arc<[PortRef]>,
        seed: u64,
        enabled: bool,
        params: SParams,
    ) -> Result<Self, EngineError> {
        let params_digest = params.digest()?;
        let key = op::node_key(&op, v, &params_digest, seed, enabled);
        let frame_len = frame_len(id, &op, v, &inputs, seed, enabled)?;
        Ok(Self {
            id,
            op,
            v,
            inputs,
            seed,
            enabled,
            params,
            params_digest,
            key,
            frame_len,
        })
    }

    /// 引数だけを替えたノード。
    pub(crate) fn with_params(&self, params: SParams) -> Result<Self, EngineError> {
        let params_digest = params.digest()?;
        let key = op::node_key(&self.op, self.v, &params_digest, self.seed, self.enabled);
        Ok(Self {
            id: self.id,
            op: self.op.clone(),
            v: self.v,
            inputs: self.inputs.clone(),
            seed: self.seed,
            enabled: self.enabled,
            params,
            params_digest,
            key,
            frame_len: self.frame_len,
        })
    }

    /// 入力だけを替えたノード（鍵は変わらない）。
    pub(crate) fn with_inputs(&self, inputs: Arc<[PortRef]>) -> Result<Self, EngineError> {
        let frame_len = frame_len(self.id, &self.op, self.v, &inputs, self.seed, self.enabled)?;
        Ok(Self {
            id: self.id,
            op: self.op.clone(),
            v: self.v,
            inputs,
            seed: self.seed,
            enabled: self.enabled,
            params: self.params.clone(),
            params_digest: self.params_digest,
            key: self.key,
            frame_len,
        })
    }

    /// 有効か無効かだけを替えたノード。
    pub(crate) fn with_enabled(&self, enabled: bool) -> Result<Self, EngineError> {
        let frame_len = frame_len(self.id, &self.op, self.v, &self.inputs, self.seed, enabled)?;
        Ok(Self {
            id: self.id,
            op: self.op.clone(),
            v: self.v,
            inputs: self.inputs.clone(),
            seed: self.seed,
            enabled,
            params: self.params.clone(),
            params_digest: self.params_digest,
            key: op::node_key(&self.op, self.v, &self.params_digest, self.seed, enabled),
            frame_len,
        })
    }

    /// 正規の JSON の長さ（`"params":null` の `null` を引数に替えたもの）。
    pub(crate) fn len(&self) -> usize {
        self.frame_len
            .saturating_sub(4)
            .saturating_add(self.params.len())
    }

    /// 保存の形のノード。
    pub(crate) fn to_node(&self) -> Node {
        Node {
            id: self.id,
            op: self.op.to_string(),
            v: self.v,
            params: self.params.to_value(),
            inputs: self.inputs.to_vec(),
            seed: self.seed,
            enabled: self.enabled,
        }
    }

    /// 中身が同じか（ID・入力・鍵。鍵は操作・版・引数・種・有効かを含む）。
    pub(crate) fn same(&self, other: &SNode) -> bool {
        self.id == other.id && self.key == other.key && self.inputs == other.inputs
    }

    /// `older` と共有していない分の見積もり（バイト）。
    fn unshared_bytes(&self, older: Option<&Arc<SNode>>) -> u64 {
        (self.frame_len as u64).saturating_add(self.params.unshared_bytes(older.map(|o| &o.params)))
    }
}

/// 文書のスナップショット。写すのは軽い（`Arc` を 2 つ写すだけ）。描くスレッドへ渡してよい（Send＋Sync）。
#[derive(Clone)]
pub struct Snapshot {
    pub(crate) head: Arc<Head>,
    pub(crate) nodes: Arc<[Arc<SNode>]>,
    len: usize,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("nodes", &self.nodes.len())
            .field("encoded_len", &self.len)
            .finish()
    }
}

/// 引数の見え方。
#[derive(Debug, Clone, Copy)]
pub enum ParamsView<'a> {
    /// 正規の形の値。
    Json(&'a Value),
    /// 追記だけの配列（`field`）を `log` に分けたもの。`shell` は `field` を `[]` にした正規の形のオブジェクト。
    Log {
        shell: &'a Value,
        field: &'static str,
        log: &'a AppendLog,
    },
}

impl ParamsView<'_> {
    /// 保存の形の値（追記の配列は要素を写す）。
    pub fn to_value(&self) -> Value {
        match self {
            Self::Json(v) => (*v).clone(),
            Self::Log { shell, field, log } => {
                let mut v = (*shell).clone();
                if let Value::Object(map) = &mut v {
                    map.insert((*field).to_string(), Value::Array(log.to_vec()));
                }
                v
            }
        }
    }
}

/// ノードの見え方（実行器・エディタが読む）。
#[derive(Debug, Clone, Copy)]
pub struct NodeView<'a> {
    pub id: NodeId,
    pub op: &'a str,
    pub v: u16,
    pub inputs: &'a [PortRef],
    pub seed: u64,
    pub enabled: bool,
    /// ノードの鍵（`op::node_key`。`Registry::doc_node_key` と同じ値）。
    pub key: Digest,
    /// 引数の指紋（`op::params_digest`）。
    pub params_digest: Digest,
    pub params: ParamsView<'a>,
}

impl<'a> NodeView<'a> {
    fn of(n: &'a SNode) -> Self {
        Self {
            id: n.id,
            op: &n.op,
            v: n.v,
            inputs: &n.inputs,
            seed: n.seed,
            enabled: n.enabled,
            key: n.key,
            params_digest: n.params_digest,
            params: match &n.params {
                SParams::Plain { value, .. } => ParamsView::Json(value),
                SParams::Log {
                    shell, field, log, ..
                } => ParamsView::Log { shell, field, log },
            },
        }
    }
}

/// 前のスナップショットと比べたノードの変わり方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeChange {
    /// 中身が同じ（ID・入力・鍵が同じ）。
    Same,
    /// 追記だけの配列 `field` に、`from` 番目から要素を足しただけ（ほかは同じ。前の要素は共有している）。
    Appended { field: &'static str, from: usize },
    /// 中身が変わった。
    Changed,
    /// 新しく足した。
    Added,
    /// 消した。
    Removed,
}

/// 2 つのスナップショットの違い。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDiff {
    /// 新しい方のノードの順に、それぞれの変わり方。消したノードは最後に、前の順で。
    pub nodes: Vec<(NodeId, NodeChange)>,
    /// ノードの並び（ID の列）が違う。
    pub order_changed: bool,
    /// 出力が違う。
    pub outputs_changed: bool,
    /// 文書の入力・規則・スキーマが違う。
    pub head_changed: bool,
}

impl Snapshot {
    /// 頭とノードから作る（長さを足し合わせる）。
    pub(crate) fn from_parts(head: Arc<Head>, nodes: Vec<Arc<SNode>>) -> Self {
        let len = nodes
            .iter()
            .fold(head.len, |acc, n| acc.saturating_add(n.len()))
            .saturating_add(nodes.len().saturating_sub(1));
        Self {
            head,
            nodes: Arc::from(nodes),
            len,
        }
    }

    /// 保存の形の文書（追記の配列は要素を写す）。正規の JSON は保存の文書とバイト単位で同じ。
    pub fn to_graph(&self) -> Graph {
        Graph {
            schema: self.head.schema,
            rules: self.head.rules,
            inputs: self.head.inputs.clone(),
            nodes: self.nodes.iter().map(|n| n.to_node()).collect(),
            outputs: self.head.outputs.clone(),
            next_id: self.head.next_id,
        }
    }

    /// 正規の JSON（`to_graph().to_canonical_json()` と同じ）。
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, EngineError> {
        self.to_graph().to_canonical_json()
    }

    /// 文書のハッシュ（`to_graph().digest()` と同じ）。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        self.to_graph().digest()
    }

    /// 正規の JSON の長さ（`to_graph().encoded_len()` と同じ。写さずに、ノードごとに持っている長さから）。
    pub fn encoded_len(&self) -> usize {
        self.len
    }

    /// ノードの数。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// スキーマの版。
    pub fn schema(&self) -> u32 {
        self.head.schema
    }

    /// 全体の規則の版。
    pub fn rules(&self) -> u32 {
        self.head.rules
    }

    /// 文書の入力。
    pub fn inputs(&self) -> &[InputSlot] {
        &self.head.inputs
    }

    /// 文書の出力。
    pub fn outputs(&self) -> &[OutputSpec] {
        &self.head.outputs
    }

    /// 次に払い出すノード ID。
    pub fn next_id(&self) -> u32 {
        self.head.next_id
    }

    /// ノードを順に返す。
    pub fn nodes(&self) -> impl Iterator<Item = NodeView<'_>> + '_ {
        self.nodes.iter().map(|n| NodeView::of(n))
    }

    /// ID のノード。
    pub fn node(&self, id: NodeId) -> Option<NodeView<'_>> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| NodeView::of(n))
    }

    /// ID のノードの位置と中身。
    pub(crate) fn find(&self, id: NodeId) -> Option<(usize, &Arc<SNode>)> {
        self.nodes.iter().enumerate().find(|(_, n)| n.id == id)
    }

    /// `other` と、ID のノードを共有しているか（同じ `Arc`）。
    pub fn shares_node(&self, other: &Snapshot, id: NodeId) -> bool {
        match (self.find(id), other.find(id)) {
            (Some((_, a)), Some((_, b))) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// 中身が同じか（頭と、各ノードの ID・入力・鍵。保存の正規の JSON が同じことと同値）。
    pub(crate) fn same_content(&self, other: &Snapshot) -> bool {
        (Arc::ptr_eq(&self.head, &other.head) || self.head == other.head)
            && self.nodes.len() == other.nodes.len()
            && self
                .nodes
                .iter()
                .zip(other.nodes.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b) || a.same(b))
    }

    /// `older` と共有していない分の見積もり（バイト。履歴の上限に使う）：変えた頭・ノードの正規の JSON の長さ。追記なら
    /// 足した要素の長さと、写した切れ目・切れ目の列の番地の分。
    pub(crate) fn unshared_bytes(&self, older: &Snapshot) -> u64 {
        let mut bytes = if Arc::ptr_eq(&self.head, &older.head) {
            0
        } else {
            self.head.len as u64
        };
        let mut by_id: Option<BTreeMap<NodeId, &Arc<SNode>>> = None;
        for (i, n) in self.nodes.iter().enumerate() {
            let old = match older.nodes.get(i) {
                Some(o) if o.id == n.id => Some(o),
                _ => by_id
                    .get_or_insert_with(|| older.nodes.iter().map(|o| (o.id, o)).collect())
                    .get(&n.id)
                    .copied(),
            };
            if old.is_some_and(|o| Arc::ptr_eq(o, n)) {
                continue;
            }
            bytes = bytes.saturating_add(n.unshared_bytes(old));
        }
        bytes
    }

    /// `older`（前のスナップショット）からの違い。ノードは ID で対応させ、`Arc` の同一性で見る（O(ノードの数＋切れ目の数)）。
    pub fn diff(&self, older: &Snapshot) -> SnapshotDiff {
        let old_by_id: BTreeMap<NodeId, &Arc<SNode>> =
            older.nodes.iter().map(|o| (o.id, o)).collect();
        let mut nodes = Vec::with_capacity(self.nodes.len());
        for n in self.nodes.iter() {
            let change = match old_by_id.get(&n.id) {
                None => NodeChange::Added,
                Some(o) if Arc::ptr_eq(o, n) || o.same(n) => NodeChange::Same,
                Some(o) => appended(o, n).unwrap_or(NodeChange::Changed),
            };
            nodes.push((n.id, change));
        }
        let new_ids: Vec<NodeId> = self.nodes.iter().map(|n| n.id).collect();
        for o in older.nodes.iter() {
            if !new_ids.contains(&o.id) {
                nodes.push((o.id, NodeChange::Removed));
            }
        }
        let old_ids: Vec<NodeId> = older.nodes.iter().map(|n| n.id).collect();
        let same_head = Arc::ptr_eq(&self.head, &older.head);
        SnapshotDiff {
            nodes,
            order_changed: new_ids != old_ids,
            outputs_changed: !same_head && self.head.outputs != older.head.outputs,
            head_changed: !same_head
                && (self.head.schema != older.head.schema
                    || self.head.rules != older.head.rules
                    || self.head.inputs != older.head.inputs),
        }
    }
}

/// `new` が `old` の追記だけの配列に要素を足しただけなら、その変わり方。
fn appended(old: &SNode, new: &SNode) -> Option<NodeChange> {
    let (
        SParams::Log {
            shell: sa,
            field: fa,
            log: la,
            ..
        },
        SParams::Log {
            shell: sb,
            field: fb,
            log: lb,
            ..
        },
    ) = (&old.params, &new.params)
    else {
        return None;
    };
    let same_rest = old.op == new.op
        && old.v == new.v
        && old.seed == new.seed
        && old.enabled == new.enabled
        && old.inputs == new.inputs
        && fa == fb
        && (Arc::ptr_eq(sa, sb) || sa == sb);
    (same_rest && lb.len() > la.len() && lb.shared_prefix(la) == la.len()).then_some(
        NodeChange::Appended {
            field: fb,
            from: la.len(),
        },
    )
}

/// 文書の形の指摘を `Invalid` にする。
pub(crate) fn bad_graph(message: impl Into<String>) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadGraph, message)))
}
