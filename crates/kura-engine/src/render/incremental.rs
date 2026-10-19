//! 差分の計画と差分の適用（段 10。docs/editing.md §12。セッションと実行器をつなぐ）。
//!
//! - `Engine::compile_snapshot`：セッションのスナップショットから、前の計画との差分で計画を作る。鍵（`op::node_key`）・入力・
//!   入力の形が前と同じノードは前の計画のノードをそのまま共有し（形が前と同じになったところで、後ろの推論は打ち切られる）、
//!   鍵だけが同じで入力の形が変わったノードは読んだ引数を使い回して推論だけし直す。追記だけの配列に足したノード
//!   （`NodeChange::Appended`）は、足した要素だけを `Op::parse_appended` で読み、`Op::infer_appended` で推論を足す。ほかは
//!   `compile` と同じ道（resolve・parse・infer）で作る。どの道でも、計画（各ノードの形・写し方・手間・申告・鍵、出力の形、
//!   最後の使い手、指摘）は `compile(&snap.to_graph())` と同じ。
//! - 差分の適用（`render_cached` の外れの道）：ノードの前回の描画（`NodeCache::last`）と入力の指紋・実装・規則・種・段が同じで、
//!   前の出力（`Exact`）がキャッシュにあり、メタデータの写しが恒等の操作（`MetaAccess::Map` で写し方が `Identity`。写す前と
//!   写したあとの値が同じ）なら、前の出力に `Op::apply_appended` を当てる。警告は [前の操作の警告 ＋ 足した分の操作の警告 ＋
//!   写しの警告] で組み直す（全体を当て直したときの順と同じ。docs/editing.md §12.4）。値は全体を当て直したものとビット単位で
//!   同じ（E21）。

use std::collections::BTreeMap;
use std::sync::Arc;

use super::cache::{Fingerprint, LastRun, NodeCache, RenderOptions};
use super::{
    Engine, NodeParts, NodeRun, Plan, PlannedNode, lookup, node_in_shapes, node_seed, plan_outputs,
    primary_input, release_schedule, settle, with_mapped_meta,
};
use crate::digest::Digest;
use crate::doc::{self, Graph, InputId, NodeId};
use crate::error::{EngineError, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::monitor::Monitor;
use crate::op::{AnyParams, ApplyCx, DynOp, InferCx, Inferred, MetaAccess};
use crate::session::snapshot::{SNode, SParams};
use crate::session::{NodeChange, Snapshot};
use crate::shape::Shape;
use crate::value::Value;

/// 差分の計画の土台（前の計画と、その元のスナップショットとの違い）。
struct Base<'p> {
    plan: &'p Plan,
    /// 前の計画のノード（ID ごと）。
    nodes: BTreeMap<NodeId, &'p Arc<PlannedNode>>,
    /// 前の計画のノードの出力の形。
    shapes: BTreeMap<NodeId, Shape>,
    /// 前の計画の元のスナップショットからの変わり方（元がなければ空。追記を見分けるのに使う）。
    changes: BTreeMap<NodeId, NodeChange>,
}

impl<'p> Base<'p> {
    fn new(plan: &'p Plan, snap: &Snapshot) -> Self {
        let changes = plan
            .source
            .as_ref()
            .map(|src| snap.diff(src).nodes.into_iter().collect())
            .unwrap_or_default();
        Self {
            plan,
            nodes: plan.nodes.iter().map(|n| (n.id, n)).collect(),
            shapes: plan
                .nodes
                .iter()
                .map(|n| (n.id, n.inferred.shape))
                .collect(),
            changes,
        }
    }

    /// 前の計画でのノードの入力の形。
    fn in_shapes(&self, node: &PlannedNode) -> Option<Vec<Shape>> {
        node.inputs
            .iter()
            .map(|r| lookup(&self.shapes, &self.plan.inputs, r))
            .collect()
    }
}

/// 形の確かめに使う骨組み（引数を null にした文書。`Graph::check_structure` は引数を見ないので、同じ指摘になる）。
fn skeleton(snap: &Snapshot) -> Graph {
    Graph {
        schema: snap.head.schema,
        rules: snap.head.rules,
        inputs: snap.head.inputs.clone(),
        nodes: snap
            .nodes
            .iter()
            .map(|n| doc::Node {
                id: n.id,
                op: n.op.to_string(),
                v: n.v,
                params: serde_json::Value::Null,
                inputs: n.inputs.to_vec(),
                seed: n.seed,
                enabled: n.enabled,
            })
            .collect(),
        outputs: snap.head.outputs.clone(),
        next_id: snap.head.next_id,
    }
}

/// スナップショットのノードの、計画のノードの材料。
fn parts_of(n: &SNode, op: Arc<dyn DynOp>, params: Arc<AnyParams>, key: Digest) -> NodeParts {
    NodeParts {
        id: n.id,
        op,
        params,
        key,
        inputs: n.inputs.to_vec(),
        seed: n.seed,
        enabled: n.enabled,
    }
}

impl Engine {
    /// セッションのスナップショットから計画を作る（描かない）。`prev` は前に作った計画（同じエンジンで等倍のもの。ほかは
    /// 使わない）で、変わらないノードを共有し、追記は足した分だけ読んで推論する。計画は `compile(&snap.to_graph(), …)` と同じ。
    pub fn compile_snapshot(
        &self,
        snap: &Snapshot,
        input_shapes: &BTreeMap<InputId, Shape>,
        prev: Option<&Plan>,
    ) -> Result<Plan, Issues> {
        let structure = skeleton(snap).check_structure(self.limits.max_nodes);
        if structure.has_errors() {
            return Err(structure);
        }
        let inputs = self.plan_inputs(&snap.head.inputs, input_shapes)?;
        let base = prev
            .filter(|p| p.engine == self.token && p.level == 0)
            .map(|p| Base::new(p, snap));
        let cx = InferCx {
            limits: &self.limits,
        };
        let mut shapes: BTreeMap<NodeId, Shape> = BTreeMap::new();
        let mut nodes = Vec::with_capacity(snap.nodes.len());
        for n in snap.nodes.iter() {
            let planned = self.plan_snapshot_node(n, &shapes, &inputs, base.as_ref(), &cx)?;
            shapes.insert(n.id, planned.inferred.shape);
            nodes.push(planned);
        }
        let outputs = plan_outputs(&snap.head.outputs, &shapes, &inputs)?;
        let release = release_schedule(&nodes, &outputs);
        Ok(Plan {
            rules: snap.head.rules,
            inputs,
            nodes,
            outputs,
            primary: primary_input(&snap.head.inputs),
            release,
            level: 0,
            engine: self.token,
            source: Some(snap.clone()),
        })
    }

    /// スナップショットのノード 1 つを計画にする（前の計画のノードの共有・推論だけのやり直し・追記・全体の順に試す）。
    fn plan_snapshot_node(
        &self,
        n: &SNode,
        shapes: &BTreeMap<NodeId, Shape>,
        inputs: &BTreeMap<InputId, Shape>,
        base: Option<&Base<'_>>,
        cx: &InferCx<'_>,
    ) -> Result<Arc<PlannedNode>, Issues> {
        let key = self.snapshot_key(n);
        // 形の確かめを通った文書では、入力は前のノードか文書の入力を読むので、形はいつも分かる（分からなければ全体の道で
        // compile と同じ指摘にする）
        let in_shapes = node_in_shapes(n.id, &n.inputs, shapes, inputs).ok();
        if let (Some(base), Some(key), Some(in_shapes)) = (base, key, in_shapes.as_deref())
            && let Some(p) = base.nodes.get(&n.id)
            && p.inputs.as_slice() == &n.inputs[..]
        {
            let same_shapes = base.in_shapes(p).as_deref() == Some(in_shapes);
            if p.key == key {
                // 中身が同じ：入力の形も同じなら前のノードをそのまま、違えば読んだ引数で推論だけし直す
                if same_shapes {
                    return Ok(Arc::clone(p));
                }
                let (lifted, inferred) =
                    self.infer_node(n.id, p.op.as_ref(), p.params.as_ref(), in_shapes, cx)?;
                let parts = parts_of(n, p.op.clone(), p.params.clone(), key);
                return self
                    .finish_node(parts, lifted, inferred, in_shapes)
                    .map(Arc::new);
            }
            if let Some(params) = self.parse_appended(n, p, base) {
                let params = params.map_err(|i| i.at(n.id))?;
                // 前と同じ入力の形で、外していない・持ち上げていないノードは、推論も足した分だけ
                let quick = if same_shapes && n.enabled && p.enabled && !p.lifted {
                    p.op.infer_appended(
                        p.params.as_ref(),
                        &p.inferred,
                        params.as_ref(),
                        in_shapes,
                        cx,
                    )
                } else {
                    None
                };
                let (lifted, inferred) = match quick {
                    Some(r) => (false, r.map_err(|i| i.at(n.id))?),
                    None => self.infer_node(n.id, p.op.as_ref(), params.as_ref(), in_shapes, cx)?,
                };
                let parts = parts_of(n, p.op.clone(), params, key);
                return self
                    .finish_node(parts, lifted, inferred, in_shapes)
                    .map(Arc::new);
            }
        }
        // 全体：保存の形のノードから、compile と同じ道で
        let node = n.to_node();
        let (op, params, key) = self.resolve_node(&node, key)?;
        let in_shapes = node_in_shapes(node.id, &node.inputs, shapes, inputs)?;
        let (lifted, inferred) =
            self.infer_node(node.id, op.as_ref(), params.as_ref(), &in_shapes, cx)?;
        self.finish_node(
            NodeParts::of(&node, op, params, key),
            lifted,
            inferred,
            &in_shapes,
        )
        .map(Arc::new)
    }

    /// 追記だけの配列に足したノードなら、前の計画の読んだ引数に、足した要素だけを読んで足す（`Op::parse_appended`）。
    /// 使えなければ None（全体で読む）。
    fn parse_appended(
        &self,
        n: &SNode,
        p: &PlannedNode,
        base: &Base<'_>,
    ) -> Option<Result<Arc<AnyParams>, Issues>> {
        let Some(NodeChange::Appended { field, from }) = base.changes.get(&n.id) else {
            return None;
        };
        let SParams::Log { log, .. } = &n.params else {
            return None;
        };
        // 移行したノード（前の計画の実装の版が文書の版と違う）は、要素も移すことがあるので全体で読む
        if p.op.id() != &*n.op || p.op.version() != n.v || log.len() < *from {
            return None;
        }
        let items: Vec<&serde_json::Value> = log.iter_from(*from).collect();
        p.op.parse_appended(p.params.as_ref(), field, &items)
            .map(|r| r.map(Arc::from))
    }

    /// セッションが求めたノードの鍵を、この登録表の計画でも使えるか。追記だけの配列の扱い（`Registry::append_field`）が
    /// セッションと同じなら、`compile` が求める鍵と同じ値なので使う（引数の指紋を求め直さない）。違えば None（求め直す）。
    fn snapshot_key(&self, n: &SNode) -> Option<Digest> {
        let field = self.registry.append_field(&n.op, n.v);
        let agrees = match &n.params {
            SParams::Log { field: f, .. } => field == Some(*f),
            SParams::Plain { value, .. } => {
                field.is_none_or(|f| !value.get(f).is_some_and(serde_json::Value::is_array))
            }
        };
        agrees.then_some(n.key)
    }

    /// 外れたノードを、前回の描画の出力に差分の適用で描く（`render_cached` の外れの道）。使えなければ None（全体で描く）。
    /// 前回の出力はキャッシュの `Exact` だけを使う。`verify_hits` なら全体で描き直して照らし、違えば `Internal`。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_appended(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        in_fps: &[Fingerprint],
        source: &Digest,
        cache: &mut NodeCache,
        opts: &RenderOptions,
        mon: &dyn Monitor,
    ) -> Result<Option<NodeRun>, EngineError> {
        let Some(last) = appended_base(plan, node, in_fps, source, cache) else {
            return Ok(None);
        };
        let Some(prev_out) = cache.exact_value(&last.output) else {
            return Ok(None);
        };
        let Some(run) = self.run_appended(plan, node, ins, &last, &prev_out, source, mon)? else {
            return Ok(None);
        };
        cache.stats.appended += 1;
        if opts.verify_hits {
            let fresh = self.run_node(plan, node, ins, source, mon, &mut |need, v, a, acx| {
                a.run_any(&need.params, v, acx)
            })?;
            if fresh.value.digest()? != run.value.digest()?
                || fresh.apply_warnings != run.apply_warnings
                || fresh.map_warnings != run.map_warnings
            {
                return Err(EngineError::Internal(format!(
                    "{} の差分の適用が、全体を当て直したものと違います",
                    node.op.id()
                ))
                .at(node.id));
            }
            cache.stats.verified += 1;
        }
        Ok(Some(run))
    }

    /// 前回の描画の出力 `prev_out` に差分の適用で描く。追記でなければ None。警告は [前回の操作の警告 ＋ 足した分の操作の
    /// 警告 ＋ 写しの警告] に組み直す。
    #[allow(clippy::too_many_arguments)]
    fn run_appended(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        last: &LastRun,
        prev_out: &Value,
        source: &Digest,
        mon: &dyn Monitor,
    ) -> Result<Option<NodeRun>, EngineError> {
        let mut cx = ApplyCx::new(mon, &self.limits, node_seed(node, source), plan.rules);
        let Some(applied) = node.op.apply_appended(
            last.params.as_ref(),
            prev_out,
            node.params.as_ref(),
            &mut cx,
        ) else {
            return Ok(None);
        };
        let at = |e: EngineError| e.at(node.id);
        let applied = applied.map_err(at)?;
        if cx.analyses_solved_in_place() != 0 {
            return Err(EngineError::Internal(format!(
                "{} が差分の適用で先に解いていない解析を使いました",
                node.op.id()
            )));
        }
        let space = settle(
            node.op.id(),
            "space",
            &node.inferred.space,
            applied.space,
            &SpaceMap::Unknown,
        )
        .map_err(at)?;
        let time = settle(
            node.op.id(),
            "time",
            &node.inferred.time,
            applied.time,
            &TimeMap::Unknown,
        )
        .map_err(at)?;
        let input = ins
            .first()
            .ok_or_else(|| EngineError::Internal("差分の適用の入力がありません".into()))?;
        let (value, map_warnings) =
            with_mapped_meta(plan.rules, applied.value, input, &space, &time).map_err(at)?;
        let mut apply_warnings = last.apply_warnings.clone();
        apply_warnings.extend(cx.warnings);
        Ok(Some(NodeRun {
            value,
            apply_warnings,
            map_warnings,
        }))
    }
}

/// 差分の適用を試してよいノードなら、前回の描画を返す：前回と入力の指紋・実装・規則・種・段が同じで鍵が違い、メタデータの
/// 写しが恒等（`MetaAccess::Map` で写し方が `Identity`）、持ち上げず、先に解く解析がない。
fn appended_base(
    plan: &Plan,
    node: &PlannedNode,
    in_fps: &[Fingerprint],
    source: &Digest,
    cache: &NodeCache,
) -> Option<LastRun> {
    if node.lifted
        || !node.needs.is_empty()
        || node.op.signature().meta != MetaAccess::Map
        || node.inferred.space != SpaceMap::Identity
        || node.inferred.time != TimeMap::Identity
    {
        return None;
    }
    let last = cache.last(node.id)?;
    let same = last.op.id() == node.op.id()
        && last.op.version() == node.op.version()
        && last.rules == plan.rules
        && last.seed == node_seed(node, source)
        && last.level == node.level
        && last.inputs.as_slice() == in_fps
        && last.key != node.key;
    same.then(|| last.clone())
}

impl Plan {
    /// ノードの ID（計画の順）。
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.nodes.iter().map(|n| n.id).collect()
    }

    /// `other` と、ID のノードの計画を共有しているか（差分の計画が、変わらないノードを作り直していないことの確かめ）。
    pub fn shares_node(&self, other: &Plan, id: NodeId) -> bool {
        let find = |p: &Plan| p.nodes.iter().find(|n| n.id == id).cloned();
        match (find(self), find(other)) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a, &b),
            _ => false,
        }
    }

    /// ノードの推論の結果（形・写し方・手間。外したノードは入力 0 をそのまま通す形）。
    pub fn node_inferred(&self, id: NodeId) -> Option<&Inferred> {
        self.nodes.iter().find(|n| n.id == id).map(|n| &n.inferred)
    }
}
