//! 実行器（docs/editing.md §12）。文書を検証・推論して計画にし（compile）、元の値に当てる（render）。
//!
//! - load：文書を読み、スキーマ・知らない操作・新しすぎる版を確かめる（E8）。
//! - compile：形の正しさ → 操作の版の解決（旧版・移行）→ 引数の読み込みと範囲 → 形の推論 → 上限。描かない。
//! - render：入力の値を計画の形と上限に照らしてから、ノードを順に実行する。フレームの列への持ち上げと、
//!   メタデータの写しはここで行う。実際の形・写し方が推論と合わなければ内部の誤り（操作の不具合）として止める。
//!   使い終えた中間の値は、最後の使い手のあとで落とす。進捗は推論の手間（`Inferred::cost`）の累積で出す。
//! - render_cached（cache.rs）：ノードのキャッシュを引きながら描く（段 10。値は render と同じ）。
//! - compile_snapshot と差分の適用（incremental.rs）：セッションのスナップショットから前の計画との差分で計画を作り、
//!   追記だけの引数は前の出力に足した分だけ当てる（段 10。計画と値は compile・render と同じ）。
//! - estimate（estimate.rs）：描く前のメモリの見積もりと、予算の確保（段 10。値には効かない）。
//! - 範囲（region.rs）・変わった範囲（delta.rs）・範囲の描画（area.rs の render_area）：段 10。
//! - エディタの表示の経路（view.rs の render_view）：変わった範囲を推論し、前の表示の値に変わったタイルだけを描き直して
//!   継ぎはぎする（段 10。値は表示にだけ使う）。
//! - プレビューの段（preview.rs の plan_level）：等倍の計画から、継ぎ目までを等倍のまま共有し、後ろを段の形で推論し直した
//!   計画を作る（段 10。段の値は近似で、`render`・`render_with_solver` は段の計画を断る）。

pub mod area;
pub mod cache;
pub mod delta;
pub mod estimate;
mod incremental;
pub mod preview;
pub mod region;
pub mod view;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::analysis::{
    Analysis, AnalysisDescriptor, AnalysisRegistry, AnalysisRequest, AnalyzeCx, Analyzed,
    AnyOutput, DynAnalysis, Need, Solve, Solver,
};
use crate::determinism;
use crate::digest::Digest;
use crate::doc::{
    self, Graph, InputId, InputRole, InputSlot, NodeId, OutputId, OutputSpec, PortRef,
    QUANTIZE_VERSION, Quantize,
};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::Size;
use crate::limits::Limits;
use crate::mapping::{SpaceMap, TimeMap, map_audio_meta_for, map_image_meta_for};
use crate::monitor::Monitor;
use crate::op::{
    AnyParams, ApplyCx, DynOp, InferCx, Inferred, Lift, MetaAccess, Registry, Resolved,
};
use crate::shape::{Dim, Fullness, Shape, ValueKind};
use crate::value::{Frames, Image, Raster, Value};

/// 操作エンジン。操作と解析の登録表と、上限を持つ。
pub struct Engine {
    registry: Registry,
    analyses: AnalysisRegistry,
    limits: Limits,
    /// エンジンの通し番号（プロセスの中で一意）。キャッシュと計画を作ったエンジンに縛る（登録表・上限の違う値を混ぜない）。
    token: u64,
}

/// エンジンの通し番号の次の値。
static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

fn next_engine_token() -> u64 {
    NEXT_ENGINE.fetch_add(1, Ordering::Relaxed)
}

/// 文書の入力に結び付ける値。`source` は元のファイルの中身のハッシュ（乱数の種に使う）。
#[derive(Debug, Clone)]
pub struct InputValue {
    pub value: Value,
    pub source: Digest,
}

/// 計画の中のノード 1 つ。計画どうし（版の違う計画・プレビューの段）で `Arc` のまま共有する。
struct PlannedNode {
    id: NodeId,
    op: Arc<dyn DynOp>,
    params: Arc<AnyParams>,
    /// ノードの鍵（`op::node_key`。操作 ID・文書の版・引数の指紋・種・有効か）。キャッシュの指紋の静的な部分。
    key: Digest,
    inputs: Vec<PortRef>,
    seed: u64,
    enabled: bool,
    lifted: bool,
    inferred: Inferred,
    /// 描く前に解く解析（操作の申告。§11.2）。外したノードと持ち上げたノードは空。
    needs: Vec<Need>,
    /// 出力の値の解像度の段（0 は等倍。プレビューの段の計画の、継ぎ目の縮小とその後ろのノードが 1〜を持つ）。キャッシュの
    /// 指紋に入る（継ぎ目までのノードは等倍の計画と同じ指紋になる）。
    level: u8,
}

/// 検証と推論を済ませた計画。
#[derive(Clone)]
pub struct Plan {
    /// 文書の全体の規則の版（メタデータの写し方を選ぶ。§8.4）。
    rules: u32,
    inputs: BTreeMap<InputId, Shape>,
    nodes: Vec<Arc<PlannedNode>>,
    outputs: Vec<(OutputId, PortRef, Shape, Option<Quantize>)>,
    primary: Option<InputId>,
    /// ノード i を描いたあとに落としてよい値（最後の使い手が i のもの。出力が使う値は落とさない）。`nodes` と同じ長さ。
    release: Vec<Vec<NodeId>>,
    /// 解像度の段（0 は等倍。プレビューの段の計画が 1〜を持つ）。キャッシュの指紋に入る。
    level: u8,
    /// 計画を作ったエンジンの通し番号（`render_cached` は別のエンジンの計画を断る）。
    engine: u64,
    /// 計画の元のスナップショット（`compile_snapshot` で作った計画だけ。次の差分の計画が追記を見分けるのに使う）。
    source: Option<crate::session::Snapshot>,
}

impl Plan {
    /// 出力の形（描かずに分かる）。
    pub fn output_shapes(&self) -> Vec<(OutputId, Shape)> {
        self.outputs.iter().map(|(id, _, s, _)| (*id, *s)).collect()
    }

    /// ノードの推論の結果（エディタの表示用）。
    pub fn node_shape(&self, id: NodeId) -> Option<Shape> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.inferred.shape)
    }

    /// ノードが描く前に解く解析（操作の申告）。
    pub fn node_needs(&self, id: NodeId) -> Option<&[Need]> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.needs.as_slice())
    }

    /// 見積もりの手間の合計。
    pub fn cost(&self) -> u64 {
        self.nodes
            .iter()
            .map(|n| n.inferred.cost)
            .fold(0, u64::saturating_add)
    }

    /// ノードの鍵（`op::node_key`。キャッシュの指紋の静的な部分）。
    pub fn node_key(&self, id: NodeId) -> Option<Digest> {
        self.nodes.iter().find(|n| n.id == id).map(|n| n.key)
    }

    /// 解像度の段（0 は等倍）。
    pub fn level(&self) -> u8 {
        self.level
    }

    /// ノード i を描いたあとに落とす値の ID（最後の使い手の表。テストと、計画を作り直す実行器が照らす）。
    pub fn released_after(&self, index: usize) -> &[NodeId] {
        self.release.get(index).map_or(&[], Vec::as_slice)
    }
}

/// 最後の使い手の表：ノード i を描いたあとに落としてよい値。出力が使う値は落とさない。どこからも使われない値は、
/// 描いた直後に落とす。入力（`PortRef::Input`）は呼び手が持つので数えない。
fn release_schedule(
    nodes: &[Arc<PlannedNode>],
    outputs: &[(OutputId, PortRef, Shape, Option<Quantize>)],
) -> Vec<Vec<NodeId>> {
    let mut last: BTreeMap<NodeId, usize> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        last.insert(node.id, i);
        for r in &node.inputs {
            if let PortRef::Node { id } = r {
                last.insert(*id, i);
            }
        }
    }
    for (_, from, _, _) in outputs {
        if let PortRef::Node { id } = from {
            last.remove(id);
        }
    }
    let mut release = vec![Vec::new(); nodes.len()];
    for (id, i) in last {
        if let Some(slot) = release.get_mut(i) {
            slot.push(id);
        }
    }
    release
}

/// 描いた出力 1 つ。
#[derive(Debug, Clone)]
pub struct RenderedOutput {
    pub id: OutputId,
    pub value: Value,
    pub digest: Digest,
}

/// 描いた結果。
#[derive(Debug, Clone)]
pub struct Rendered {
    pub outputs: Vec<RenderedOutput>,
    pub warnings: Vec<Issue>,
}

fn lookup<T: Clone>(
    refs: &BTreeMap<NodeId, T>,
    inputs: &BTreeMap<InputId, T>,
    r: &PortRef,
) -> Option<T> {
    match r {
        PortRef::Input { id } => inputs.get(id).cloned(),
        PortRef::Node { id } => refs.get(id).cloned(),
    }
}

fn bad_input(message: impl Into<String>) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadInput, message)))
}

impl Engine {
    /// すべての操作を登録したエンジン。
    pub fn new() -> Result<Self, EngineError> {
        Self::with_limits(Limits::default())
    }

    /// 上限を指定したエンジン。`with_registries` の確かめに加えて、今の版の操作が申告する解析が、どれもその解析の
    /// 今の版であることを確かめる（`check_current_needs`）。
    pub fn with_limits(limits: Limits) -> Result<Self, EngineError> {
        let registry = crate::ops::registry().map_err(EngineError::Internal)?;
        let analyses = crate::analysis::registry().map_err(EngineError::Internal)?;
        check_current_needs(&registry, &analyses).map_err(EngineError::Internal)?;
        Self::with_registries(registry, analyses, limits)
    }

    /// 登録表を指定したエンジン（版の契約のテストで、試験用の操作を入れるときに使う）。解析はすべてのもの。
    /// 操作の申告は確かめない（計画を作るときに、申告した解析が登録されているかを確かめる）。
    pub fn with_registry(registry: Registry, limits: Limits) -> Self {
        Self {
            registry,
            analyses: crate::analysis::registry().unwrap_or_default(),
            limits,
            token: next_engine_token(),
        }
    }

    /// 操作と解析の登録表を指定したエンジン。操作が申告しうる解析（`Op::NEEDS`）がすべて登録されていること
    /// （旧版の操作が使う旧版の解析を含む）を確かめる。今の版の操作が解析の今の版を申告しているか
    /// （`check_current_needs`）は確かめない（試験用の登録表は、申告の誤りを試すために旧版を申告する操作を入れる）。
    pub fn with_registries(
        registry: Registry,
        analyses: AnalysisRegistry,
        limits: Limits,
    ) -> Result<Self, EngineError> {
        check_needs(&registry, &analyses).map_err(EngineError::Internal)?;
        Ok(Self {
            registry,
            analyses,
            limits,
            token: next_engine_token(),
        })
    }

    /// 登録表。
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// 解析の登録表。
    pub fn analyses(&self) -> &AnalysisRegistry {
        &self.analyses
    }

    /// 解析のカタログ（今の版で、内部用でないもの）。
    pub fn analysis_catalog(&self) -> Vec<AnalysisDescriptor> {
        self.analyses.catalog()
    }

    /// 解析する（型付き）。`analyze_dyn` と同じ道で確かめる：引数を正規の形にして記述の範囲を確かめ（範囲の外・
    /// 読めない引数は `Invalid`）、受け付けない種類は `Invalid`、上限を超える値・結果は解く前に `Limit`。解くのは
    /// 正規の形から読み直した引数（`analyze_dyn`・操作の申告と同じ値）。
    pub fn analyze<A: Analysis>(
        &self,
        v: &Value,
        p: &A::Params,
        mon: &dyn Monitor,
    ) -> Result<A::Output, EngineError> {
        let raw = crate::analysis::params_json(p).map_err(EngineError::Invalid)?;
        let params = self.prepare(&crate::analysis::erased::<A>(), &raw, v)?;
        let p = crate::analysis::typed_params::<A>(&params).map_err(EngineError::Invalid)?;
        A::run(
            &p,
            v,
            &AnalyzeCx {
                mon,
                limits: &self.limits,
            },
        )
    }

    /// 解析する（ID・版・引数の JSON から。kura-core のジョブ・ゴールデン・devtools）。結果は正規の形。
    pub fn analyze_dyn(
        &self,
        req: &AnalysisRequest,
        v: &Value,
        mon: &dyn Monitor,
    ) -> Result<Analyzed, EngineError> {
        let a = self
            .analyses
            .resolve(&req.id, req.v)
            .map_err(|i| EngineError::Invalid(Issues::one(i)))?;
        let params = self.prepare(a.as_ref(), &req.params, v)?;
        let cx = AnalyzeCx {
            mon,
            limits: &self.limits,
        };
        let out = a.run_any(&params, v, &cx)?;
        let output = a.encode_any(&out)?;
        if output.len() as u64 > self.limits.max_value_bytes {
            return Err(EngineError::Limit(format!(
                "{} の結果が大きすぎます",
                a.id()
            )));
        }
        Ok(Analyzed {
            id: a.id(),
            version: a.version(),
            params,
            encoding: a.encoding(),
            output,
        })
    }

    /// 1 つの値に複数の解析（取り込みのあとのジョブ）。要求ごとの結果を要求の順に返す（1 つの失敗はほかに効かない）。
    /// 進捗は済んだ要求の数。中止したら、残りの要求は `Canceled`。
    pub fn analyze_many(
        &self,
        v: &Value,
        reqs: &[AnalysisRequest],
        mon: &dyn Monitor,
    ) -> Vec<Result<Analyzed, EngineError>> {
        let total = reqs.len() as u64;
        let mut out = Vec::with_capacity(reqs.len());
        for (i, req) in reqs.iter().enumerate() {
            let r = if mon.canceled() {
                Err(EngineError::Canceled)
            } else {
                self.analyze_dyn(req, v, mon)
            };
            out.push(r);
            mon.progress(i as u64 + 1, total);
        }
        out
    }

    /// 解く前の確かめ（`analyze`・`analyze_dyn` が同じ順に通る）：引数を読んで正規の形にし（既定値・範囲・知らない
    /// 項目）、受け付ける種類か、値が上限に収まるか、結果が上限に収まる見込みかを見る。返すのは正規の引数。
    fn prepare(
        &self,
        a: &dyn DynAnalysis,
        raw: &serde_json::Value,
        v: &Value,
    ) -> Result<serde_json::Value, EngineError> {
        let params = a.check_params(raw).map_err(EngineError::Invalid)?;
        if !a.accepts().contains(&v.kind()) {
            return Err(bad_input(format!(
                "{} は {:?} を解析できません",
                a.id(),
                v.kind()
            )));
        }
        let shape = v.shape();
        self.limits.check(&shape).map_err(EngineError::Limit)?;
        let bytes = a
            .estimate(&params, &shape)
            .map_err(EngineError::Invalid)?
            .bytes;
        if bytes > self.limits.max_value_bytes {
            return Err(EngineError::Limit(format!(
                "{} の結果が上限を超える見込みです（{bytes} バイト）",
                a.id()
            )));
        }
        Ok(params)
    }

    /// 操作の申告（`Op::needs`）を確かめる：`NEEDS` にあり、登録されていて、その入力の種類を受け付け、引数が正規の形で、
    /// 結果が上限に収まる見込みであること。
    fn plan_needs(
        &self,
        op: &dyn DynOp,
        params: &AnyParams,
        in_shapes: &[Shape],
    ) -> Result<Vec<Need>, Issues> {
        let needs = op.needs(params, in_shapes)?;
        for n in &needs {
            let bad = |code: IssueCode, why: &str| {
                Issues::one(Issue::error(
                    code,
                    format!(
                        "{} が申告した解析 {} v{} を解けません（{why}）",
                        op.id(),
                        n.id,
                        n.v
                    ),
                ))
            };
            if !op.static_needs().contains(&(n.id, n.v)) {
                return Err(bad(IssueCode::BadInput, "Op::NEEDS にありません"));
            }
            let Some(a) = self.analyses.get(n.id, n.v) else {
                return Err(bad(IssueCode::UnknownVersion, "登録されていません"));
            };
            let Some(shape) = in_shapes.get(n.input) else {
                return Err(bad(IssueCode::BadInput, "その番号の入力がありません"));
            };
            if !a.accepts().contains(&shape.kind()) {
                return Err(bad(IssueCode::BadInput, "その種類の値を受け付けません"));
            }
            if a.check_params(&n.params).ok().as_ref() != Some(&n.params) {
                return Err(bad(IssueCode::BadParams, "引数が正規の形ではありません"));
            }
            let estimate = a.estimate(&n.params, shape)?;
            if estimate.bytes > self.limits.max_value_bytes {
                return Err(bad(IssueCode::TooLarge, "結果が上限を超える見込みです"));
            }
        }
        Ok(needs)
    }

    /// 上限。
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// 文書を読む（スキーマの移行を含む）。知らない操作・新しすぎる版・移せない版はここでエラーにする（E8）。
    pub fn load(&self, raw: &[u8]) -> Result<Graph, EngineError> {
        let g = doc::load(raw)?;
        let mut issues = Issues::none();
        for node in &g.nodes {
            if let Err(issue) = self.registry.resolve(&node.op, node.v, &node.params) {
                issues.push(issue.at(node.id));
            }
        }
        if issues.has_errors() {
            Err(EngineError::Invalid(issues))
        } else {
            Ok(g)
        }
    }

    /// 検証と推論をして計画にする。描かない。
    pub fn compile(
        &self,
        g: &Graph,
        input_shapes: &BTreeMap<InputId, Shape>,
    ) -> Result<Plan, Issues> {
        let structure = g.check_structure(self.limits.max_nodes);
        if structure.has_errors() {
            return Err(structure);
        }
        let inputs = self.plan_inputs(&g.inputs, input_shapes)?;
        let cx = InferCx {
            limits: &self.limits,
        };
        let mut shapes: BTreeMap<NodeId, Shape> = BTreeMap::new();
        let mut nodes = Vec::with_capacity(g.nodes.len());
        for node in &g.nodes {
            let (op, params, key) = self.resolve_node(node, None)?;
            let in_shapes = node_in_shapes(node.id, &node.inputs, &shapes, &inputs)?;
            let (lifted, inferred) =
                self.infer_node(node.id, op.as_ref(), params.as_ref(), &in_shapes, &cx)?;
            let planned = self.finish_node(
                NodeParts::of(node, op, params, key),
                lifted,
                inferred,
                &in_shapes,
            )?;
            shapes.insert(node.id, planned.inferred.shape);
            nodes.push(Arc::new(planned));
        }
        let outputs = plan_outputs(&g.outputs, &shapes, &inputs)?;
        let release = release_schedule(&nodes, &outputs);
        Ok(Plan {
            rules: g.rules,
            inputs,
            nodes,
            outputs,
            primary: primary_input(&g.inputs),
            release,
            level: 0,
            engine: self.token,
            source: None,
        })
    }

    /// 文書の入力の形を確かめる（ある・上限に収まる。compile の最初の段）。
    fn plan_inputs(
        &self,
        slots: &[InputSlot],
        input_shapes: &BTreeMap<InputId, Shape>,
    ) -> Result<BTreeMap<InputId, Shape>, Issues> {
        let mut inputs = BTreeMap::new();
        for slot in slots {
            let Some(shape) = input_shapes.get(&slot.id) else {
                return Err(Issues::one(Issue::error(
                    IssueCode::BadInput,
                    format!("入力 {} がありません", slot.id.0),
                )));
            };
            if let Err(why) = self.limits.check(shape) {
                return Err(Issues::one(Issue::error(IssueCode::TooLarge, why)));
            }
            inputs.insert(slot.id, *shape);
        }
        Ok(inputs)
    }

    /// ノードの（操作, 版, 引数）を実装に結び付けて引数を読み、ノードの鍵を求める（compile のノードの 1 段目）。`key` が
    /// あればそれを鍵にする（セッションが同じ式で求めたもの。引数の指紋を求め直さない）。
    fn resolve_node(&self, node: &doc::Node, key: Option<Digest>) -> Result<ResolvedNode, Issues> {
        let at = |i: Issues| i.at(node.id);
        // 引数は resolve の中で正規の JSON に往復させてある（migrate にも往復させたものが渡る）
        let (op, raw, migrated) = match self.registry.resolve(&node.op, node.v, &node.params) {
            Ok(Resolved::Direct(op, params)) => (op, params, false),
            Ok(Resolved::Migrated(op, params)) => (op, params, true),
            Err(issue) => return Err(Issues::one(issue.at(node.id))),
        };
        let key = match key {
            Some(key) => key,
            None => {
                // ノードの鍵は、文書に書いた（操作, 版, 引数の正規の形）から作る（セッションが同じ式で求める）。移した版は
                // 移す前の引数で数える。追記だけの配列の名前は、解決した実装の引数の記述で決める
                let doc_params = if migrated {
                    crate::op::canonical_params(&node.params)
                        .map_err(|i| Issues::one(i.at(node.id)))?
                } else {
                    raw.clone()
                };
                let params_fp =
                    crate::op::params_digest(&op.schema(), &doc_params).map_err(|e| {
                        Issues::one(Issue::error(IssueCode::BadParams, e.to_string()).at(node.id))
                    })?;
                crate::op::node_key(&node.op, node.v, &params_fp, node.seed, node.enabled)
            }
        };
        let params: Arc<AnyParams> = Arc::from(op.parse(&raw).map_err(at)?);
        Ok((op, params, key))
    }

    /// 形を推論する（compile のノードの 2 段目。フレームの列への持ち上げを含む）。返すのは持ち上げたかと、操作の推論
    /// （外したノードの扱いの前）。
    fn infer_node(
        &self,
        id: NodeId,
        op: &dyn DynOp,
        params: &AnyParams,
        in_shapes: &[Shape],
        cx: &InferCx<'_>,
    ) -> Result<(bool, Inferred), Issues> {
        let at = |i: Issues| i.at(id);
        let one = |code: IssueCode, msg: &str| Issues::one(Issue::error(code, msg).at(id));
        let sig = op.signature();
        let lifted = sig.lift == Lift::PerFrame
            && in_shapes
                .first()
                .is_some_and(|s| s.kind() == ValueKind::Frames);
        if !lifted {
            return Ok((false, op.infer(params, in_shapes, cx).map_err(at)?));
        }
        // キャンバスいっぱいのフレームの列だけ、各フレームに当てる（トリムしたフレームはキャンバスの座標の扱いが決まっていない）
        let Some(frame) = in_shapes[0].frame_image() else {
            return Err(one(
                IssueCode::BadInput,
                "トリムしたフレームの列には、この操作を当てられません",
            ));
        };
        let mut frame_inputs = in_shapes.to_vec();
        frame_inputs[0] = frame;
        let inner = op.infer(params, &frame_inputs, cx).map_err(at)?;
        let (
            Shape::Image { w, h, fmt },
            Shape::Frames { count, .. },
            Shape::Image { w: fw, h: fh, .. },
        ) = (inner.shape, in_shapes[0], frame)
        else {
            return Err(one(
                IssueCode::BadInput,
                "フレームの列に当てられない操作です",
            ));
        };
        // 出力の大きさが中身で決まる操作は、フレームごとに大きさが変わりうるので持ち上げない。
        // 判定は「入力の大きさを決めた形（上限の値）で推論し直しても、出力に AtMost が残るか」で行う
        // （軸ごとに比べると、軸を入れ替える回転や、もう一方の軸で決まる自動の軸を誤って断るため）
        let content_dependent = if fw.exact().is_some() && fh.exact().is_some() {
            w.exact().is_none() || h.exact().is_none()
        } else {
            let mut fixed = frame_inputs.clone();
            if let Shape::Image { fmt: ffmt, .. } = frame {
                fixed[0] = Shape::Image {
                    w: Dim::Exact(fw.max()),
                    h: Dim::Exact(fh.max()),
                    fmt: ffmt,
                };
            }
            match op.infer(params, &fixed, cx) {
                Ok(again) => match again.shape {
                    Shape::Image { w, h, .. } => w.exact().is_none() || h.exact().is_none(),
                    _ => true,
                },
                // 上限の大きさでは断られる操作は、実行で各フレームの大きさがそろうかを確かめる
                Err(_) => false,
            }
        };
        if content_dependent {
            return Err(one(
                IssueCode::BadInput,
                "出力の大きさが中身で決まる操作は、フレームの列に当てられません",
            ));
        }
        Ok((
            true,
            Inferred {
                shape: Shape::Frames {
                    count,
                    w,
                    h,
                    fmt,
                    full: Fullness::Full,
                },
                cost: inner.cost.saturating_mul(count.max()),
                ..inner
            },
        ))
    }

    /// 外したノードの形・上限・申告した解析を済ませて、計画のノードにする（compile のノードの 3 段目）。
    fn finish_node(
        &self,
        parts: NodeParts,
        lifted: bool,
        inferred: Inferred,
        in_shapes: &[Shape],
    ) -> Result<PlannedNode, Issues> {
        let id = parts.id;
        let one = |code: IssueCode, msg: &str| Issues::one(Issue::error(code, msg).at(id));
        let inferred = if parts.enabled {
            inferred
        } else {
            // 外したノードは入力 0 をそのまま通す。種類が変わる操作は外せない
            let Some(first) = in_shapes.first() else {
                return Err(one(IssueCode::BadInput, "入力のない操作は外せません"));
            };
            if first.kind() != inferred.shape.kind() {
                return Err(one(IssueCode::BadInput, "値の種類が変わる操作は外せません"));
            }
            Inferred {
                shape: *first,
                space: SpaceMap::Identity,
                time: TimeMap::Identity,
                cost: 0,
            }
        };
        if let Err(why) = self.limits.check(&inferred.shape) {
            return Err(Issues::one(Issue::error(IssueCode::TooLarge, why).at(id)));
        }
        // 申告した解析（外したノードは描かないので解かない。持ち上げる操作は申告できない）
        let needs = if parts.enabled && !lifted {
            self.plan_needs(parts.op.as_ref(), parts.params.as_ref(), in_shapes)
                .map_err(|i| i.at(id))?
        } else {
            Vec::new()
        };
        Ok(PlannedNode {
            needs,
            level: 0,
            id,
            op: parts.op,
            params: parts.params,
            key: parts.key,
            inputs: parts.inputs,
            seed: parts.seed,
            enabled: parts.enabled,
            lifted,
            inferred,
        })
    }

    /// 計画を入力に当てて描く。入力の値は計画の形と上限に照らしてから使う。操作が申告した解析は、各ノードを描く前に解く。
    /// 保存・一括の道で、縮小の段の計画は断る。
    pub fn render(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, InputValue>,
        mon: &dyn Monitor,
    ) -> Result<Rendered, EngineError> {
        self.render_with_solver(plan, inputs, mon, &Solve)
    }

    /// 描く（申告された解析を `solver` で解く。テストの記録と差し替えに使う。キャッシュは `render_cached` の道で引く）。
    /// 縮小の段の計画（`plan_level`）は断る（`Unsupported`。段の値は保存・一括に使わない）。
    pub fn render_with_solver(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, InputValue>,
        mon: &dyn Monitor,
        solver: &dyn Solver,
    ) -> Result<Rendered, EngineError> {
        // 縮小の段の値は近似なので、保存・一括（この道）には使わない（render_cached・render_area で描く）
        if plan.level != 0 {
            return Err(EngineError::Unsupported(
                "縮小の段の計画は保存・一括に使えません（等倍の計画で描きます）".into(),
            ));
        }
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        self.check_inputs(plan, |id| inputs.get(&id).map(|v| &v.value))?;
        let source = plan
            .primary
            .and_then(|id| inputs.get(&id))
            .map(|v| v.source)
            .unwrap_or_default();
        let input_values: BTreeMap<InputId, Value> =
            inputs.iter().map(|(k, v)| (*k, v.value.clone())).collect();
        let mut values: BTreeMap<NodeId, Value> = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut progress = Progress::start(plan, mon);
        for (i, node) in plan.nodes.iter().enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let ins = node_inputs(node, &values, &input_values)?;
            let (out, node_warnings) = if node.enabled {
                let run =
                    self.run_node(plan, node, &ins, &source, mon, &mut |need, v, a, acx| {
                        solver.solve(node.id, need, v, a, acx)
                    })?;
                let mut all = run.apply_warnings;
                all.extend(run.map_warnings);
                (run.value, all)
            } else {
                (pass_through(&ins)?, Vec::new())
            };
            check_node_shape(node, &out)?;
            push_warnings(&mut warnings, node.id, node_warnings);
            values.insert(node.id, out);
            for id in plan.released_after(i) {
                values.remove(id);
            }
            progress.node_done(node);
        }
        let outputs = plan
            .outputs
            .iter()
            .map(|(id, from, _, quantize)| {
                let mut value = lookup(&values, &input_values, from)
                    .ok_or_else(|| EngineError::Internal("出力の値がありません".into()))?;
                if let (Some(q), Value::Audio(a)) = (quantize, &value) {
                    value = crate::audio::quantize(a, q, &source, mon)?;
                }
                let digest = value.digest()?;
                Ok(RenderedOutput {
                    id: *id,
                    value,
                    digest,
                })
            })
            .collect::<Result<Vec<_>, EngineError>>()?;
        Ok(Rendered { outputs, warnings })
    }

    /// 入力の値を計画に照らす：ある・形が計画の推論と合う・上限に収まる（数は呼び手が照らす）。
    fn check_inputs<'v>(
        &self,
        plan: &Plan,
        get: impl Fn(InputId) -> Option<&'v Value>,
    ) -> Result<(), EngineError> {
        for (id, shape) in &plan.inputs {
            let Some(v) = get(*id) else {
                return Err(bad_input(format!("入力 {} がありません", id.0)));
            };
            let actual = v.shape();
            if !shape.admits(&actual) {
                return Err(bad_input(format!("入力 {} の形が計画と違います", id.0)));
            }
            self.limits.check(&actual).map_err(EngineError::Limit)?;
        }
        Ok(())
    }

    /// 有効なノード 1 つを描く：申告した解析を `solve` で解いて文脈に入れ、操作を当ててメタデータを写す。
    /// 操作が申告していない解析をその場で解いたら内部の誤り。`render` と `render_cached` が同じ道を通る。
    fn run_node(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        source: &Digest,
        mon: &dyn Monitor,
        solve: &mut SolveFn<'_>,
    ) -> Result<NodeRun, EngineError> {
        // 段の値は解析に使わない（段の計画は、解析を申告するノードを継ぎ目の後ろに置かない）
        if node.level != 0 && !node.needs.is_empty() {
            return Err(EngineError::Internal(format!(
                "{} が縮小の段の値に解析を当てようとしました",
                node.op.id()
            )));
        }
        let mut cx = ApplyCx::new(mon, &self.limits, node_seed(node, source), plan.rules);
        // 申告した解析を先に解く（render_cached では、入力の指紋 × ID × 版 × 引数で引いたキャッシュから）
        for need in &node.needs {
            let a = self.solvable(need)?;
            let v = ins
                .get(need.input)
                .ok_or_else(|| EngineError::Internal("解析する入力の値がありません".into()))?;
            let acx = AnalyzeCx {
                mon,
                limits: &self.limits,
            };
            let out = solve(need, v, a.as_ref(), &acx).map_err(|e| e.at(node.id))?;
            cx.preload(need, v, out)?;
        }
        let (value, map_warnings) =
            apply_node(plan.rules, node, ins, &mut cx, &mut |refs, _, cx| {
                node.op.apply(node.params.as_ref(), refs, cx)
            })
            .map_err(|e| e.at(node.id))?;
        // 申告（needs）と、操作が実際に使った解析が食い違えば操作の不具合（キャッシュの鍵が作れない）
        if cx.analyses_solved_in_place() != 0 {
            return Err(EngineError::Internal(format!(
                "{} が先に解いていない解析を使いました（needs と apply の食い違い）",
                node.op.id()
            )));
        }
        Ok(NodeRun {
            value,
            apply_warnings: cx.warnings,
            map_warnings,
        })
    }

    /// 申告した解析の実装（計画を作ったときに確かめてある）。
    fn solvable(&self, need: &Need) -> Result<Arc<dyn DynAnalysis>, EngineError> {
        self.analyses.get(need.id, need.v).ok_or_else(|| {
            EngineError::Internal(format!("解析 {} v{} がありません", need.id, need.v))
        })
    }
}

/// 操作の呼び方（[`apply_node`] に渡す）：入力（持ち上げたときは入力 0 をそのフレームの画像にしたもの）と、持ち上げたときの
/// フレームの番号（持ち上げないなら None）を受けて操作を当てる。全体の描画は `Op::apply`、範囲の描画（area.rs）はフレームごとの
/// タイルと前の値で `Op::apply_region`。
type ApplyFn<'a> = dyn FnMut(&[&Value], Option<usize>, &mut ApplyCx<'_>) -> Result<crate::op::Applied, EngineError>
    + 'a;

/// ノード 1 つを実行し、メタデータを写す（全体の描画と範囲の描画が同じ道を通る：持ち上げ・写し方の確かめ・メタデータの
/// 写し。操作の呼び方だけを `call` で替える）。返すのは値と、写したときの警告。
fn apply_node(
    rules: u32,
    node: &PlannedNode,
    ins: &[Value],
    cx: &mut ApplyCx<'_>,
    call: &mut ApplyFn<'_>,
) -> Result<(Value, Vec<Issue>), EngineError> {
    let sig = node.op.signature();
    let refs: Vec<&Value> = ins.iter().collect();
    let (value, space, time) = if node.lifted {
        let Some(Value::Frames(frames)) = ins.first() else {
            return Err(EngineError::Internal(
                "持ち上げの入力がフレームの列ではありません".into(),
            ));
        };
        // 推論で「分からない」だった列は、実行のときにキャンバスいっぱいか確かめる
        if !frames.is_full() {
            return Err(bad_input(
                "トリムしたフレームの列には、この操作を当てられません",
            ));
        }
        let mut out: Vec<(Raster, u32)> = Vec::with_capacity(frames.frames().len());
        let mut space = None;
        for (i, f) in frames.frames().iter().enumerate() {
            cx.check()?;
            // 各フレームは、フレームの列のメタデータを持った 1 枚の画像として当てる（単体の画像と同じ見え方にする）
            let single = Value::Image(Arc::new(Image {
                px: f.px.clone(),
                meta: frames.meta.clone(),
            }));
            let mut frame_refs = refs.clone();
            if let Some(first) = frame_refs.first_mut() {
                *first = &single;
            }
            let applied = call(&frame_refs, Some(i), cx)?;
            let Value::Image(img) = applied.value else {
                return Err(EngineError::Internal(
                    "持ち上げた操作が画像を返しませんでした".into(),
                ));
            };
            let actual = settle(
                node.op.id(),
                "space",
                &node.inferred.space,
                applied.space,
                &SpaceMap::Unknown,
            )?;
            // 結果は 1 つのキャンバスにそろう必要がある。フレームごとに大きさや写し方が変わる操作は当てられない
            // （推論で大きさが決まらない入力では、描くまで分からない）
            match &space {
                None => space = Some(actual),
                Some(s) if *s == actual => {}
                Some(_) => {
                    return Err(bad_input(format!(
                        "{} の結果がフレームごとに違う形になるので、フレームの列には当てられません",
                        node.op.id()
                    )));
                }
            }
            if let Some((first, _)) = out.first()
                && (first.width() != img.px.width() || first.height() != img.px.height())
            {
                return Err(bad_input(format!(
                    "{} の結果がフレームごとに違う大きさになるので、フレームの列には当てられません",
                    node.op.id()
                )));
            }
            out.push((img.px.clone(), f.duration_ms));
        }
        let value = Value::Frames(Arc::new(Frames::full(
            out,
            frames.meta.clone(),
            frames.loop_count,
        )?));
        (
            value,
            space.unwrap_or(SpaceMap::Identity),
            node.inferred.time.clone(),
        )
    } else {
        let applied = call(&refs, None, cx)?;
        let space = settle(
            node.op.id(),
            "space",
            &node.inferred.space,
            applied.space,
            &SpaceMap::Unknown,
        )?;
        let time = settle(
            node.op.id(),
            "time",
            &node.inferred.time,
            applied.time,
            &TimeMap::Unknown,
        )?;
        (applied.value, space, time)
    };
    if sig.meta == MetaAccess::Write {
        return Ok((value, Vec::new()));
    }
    match ins.first() {
        Some(input) => with_mapped_meta(rules, value, input, &space, &time),
        None => Ok((value, Vec::new())),
    }
}

/// 持ち上げる操作に渡す形（入力 0 をフレームの画像の形にする。範囲の申告 `reach`・`param_change`・`effect` に渡す）。
fn op_shapes(node: &PlannedNode, in_shapes: &[Shape]) -> Vec<Shape> {
    let mut s = in_shapes.to_vec();
    if node.lifted
        && let Some(first) = s.first_mut()
        && let Some(frame) = first.frame_image()
    {
        *first = frame;
    }
    s
}

/// 実装に結び付けたノード（実装・読んだ引数・ノードの鍵）。
type ResolvedNode = (Arc<dyn DynOp>, Arc<AnyParams>, Digest);

/// 計画の出力 1 つ（ID・読む値・形・量子化）。
type PlanOutput = (OutputId, PortRef, Shape, Option<Quantize>);

/// 計画のノードを作る材料（`compile` と `compile_snapshot` が同じ道で作る）。
struct NodeParts {
    id: NodeId,
    op: Arc<dyn DynOp>,
    params: Arc<AnyParams>,
    key: Digest,
    inputs: Vec<PortRef>,
    seed: u64,
    enabled: bool,
}

impl NodeParts {
    fn of(node: &doc::Node, op: Arc<dyn DynOp>, params: Arc<AnyParams>, key: Digest) -> Self {
        Self {
            id: node.id,
            op,
            params,
            key,
            inputs: node.inputs.clone(),
            seed: node.seed,
            enabled: node.enabled,
        }
    }
}

/// ノードの入力の形（計画の順）。前にないノードを読めば `BadGraph`。
fn node_in_shapes(
    id: NodeId,
    refs: &[PortRef],
    shapes: &BTreeMap<NodeId, Shape>,
    inputs: &BTreeMap<InputId, Shape>,
) -> Result<Vec<Shape>, Issues> {
    refs.iter()
        .map(|r| lookup(shapes, inputs, r))
        .collect::<Option<_>>()
        .ok_or_else(|| {
            Issues::one(Issue::error(IssueCode::BadGraph, "入力の形が分かりません").at(id))
        })
}

/// 出力の形を引き、量子化を確かめる（compile の最後の段）。量子化は音声の出力だけ、知っている版・ビット数だけ。
fn plan_outputs(
    specs: &[OutputSpec],
    shapes: &BTreeMap<NodeId, Shape>,
    inputs: &BTreeMap<InputId, Shape>,
) -> Result<Vec<PlanOutput>, Issues> {
    let outputs = specs
        .iter()
        .map(|o| lookup(shapes, inputs, &o.from).map(|s| (o.id, o.from, s, o.quantize)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Issues::one(Issue::error(IssueCode::BadGraph, "出力の形が分かりません")))?;
    for (_, _, shape, q) in &outputs {
        let Some(q) = q else { continue };
        let bad = |code, m: &str| Issues::one(Issue::error(code, m));
        if q.v != QUANTIZE_VERSION {
            return Err(bad(
                IssueCode::UnknownVersion,
                "出力の量子化の版を知りません",
            ));
        }
        if !matches!(q.bits, 8 | 16 | 24) {
            return Err(bad(
                IssueCode::OutOfRange,
                "出力の量子化のビット数は 8・16・24 です",
            ));
        }
        if !matches!(shape, Shape::Audio { .. }) {
            return Err(bad(
                IssueCode::BadInput,
                "量子化できるのは音声の出力だけです",
            ));
        }
    }
    Ok(outputs)
}

/// 主の入力（乱数の種の元）。
fn primary_input(slots: &[InputSlot]) -> Option<InputId> {
    slots
        .iter()
        .find(|i| i.role == InputRole::Primary)
        .map(|i| i.id)
}

/// 操作が申告しうる解析（`Op::NEEDS`）が、すべて登録されていること（旧版・退役した操作が使う解析の旧版も含む）。
fn check_needs(ops: &Registry, analyses: &AnalysisRegistry) -> Result<(), String> {
    for op in ops.all() {
        for (id, v) in op.static_needs() {
            if analyses.get(id, *v).is_none() {
                return Err(format!(
                    "{} v{} が申告する解析 {id} v{v} が登録されていません",
                    op.id(),
                    op.version()
                ));
            }
        }
    }
    Ok(())
}

/// 今の版の操作が申告しうる解析（`Op::NEEDS`）が、どれもその解析の今の版であること（旧版・退役した操作は除く。
/// それらは申告した旧版の解析で描き続ける）。申告される解析の版を上げて旧版を `legacy` に残したのに、操作の版を
/// 上げずに旧版を申告し続けると、カタログ・`analyze_dyn` の既定（今の版）で表示する値と、操作が使う値が黙って
/// ずれる（§11.2・E26）。操作の版を上げて、新しい版で今の版の解析を申告する（旧い版の操作は `legacy` に）。
fn check_current_needs(ops: &Registry, analyses: &AnalysisRegistry) -> Result<(), String> {
    for op in ops.current() {
        for (id, v) in op.static_needs() {
            match analyses.current_version(id) {
                Some(cur) if cur == *v => {}
                cur => {
                    let cur = cur.map_or_else(|| "なし".to_string(), |c| format!("v{c}"));
                    return Err(format!(
                        "今の版の操作 {} v{} が申告する解析 {id} v{v} は、その解析の今の版（{cur}）ではありません\
                         （解析の版を上げたら、操作の版も上げて今の版を申告する）",
                        op.id(),
                        op.version()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 推論の写し方と、実行が返した写し方を合わせる（E9）。推論が Unknown なら実行の値が必要。
/// そうでなければ、実行の値は無いか推論と同じであること。
fn settle<T: Clone + PartialEq + std::fmt::Debug>(
    op: &str,
    what: &str,
    inferred: &T,
    applied: Option<T>,
    unknown: &T,
) -> Result<T, EngineError> {
    match applied {
        None if inferred == unknown => Err(EngineError::Internal(format!(
            "{op} が実際の {what} を返しませんでした"
        ))),
        None => Ok(inferred.clone()),
        Some(actual) if inferred == unknown || actual == *inferred => Ok(actual),
        Some(actual) => Err(EngineError::Internal(format!(
            "{op} の {what} が推論と違います（推論 {inferred:?}、実際 {actual:?}）"
        ))),
    }
}

fn image_meta_and_size(v: &Value) -> Option<(&crate::value::ImageMeta, Size)> {
    match v {
        Value::Image(img) => Some((
            &img.meta,
            Size {
                w: img.px.width(),
                h: img.px.height(),
            },
        )),
        Value::Frames(fr) => Some((&fr.meta, fr.canvas())),
        Value::Audio(_) => None,
    }
}

/// 入力 0 のメタデータを、文書の規則の版の写し方で写して出力に付ける（版の選び方は `mapping::map_*_for`）。
fn with_mapped_meta(
    rules: u32,
    out: Value,
    input: &Value,
    space: &SpaceMap,
    time: &TimeMap,
) -> Result<(Value, Vec<Issue>), EngineError> {
    Ok(match (out, image_meta_and_size(input), input) {
        (Value::Image(img), Some((meta, size)), _) => {
            let (meta, warnings) = map_image_meta_for(rules, meta, size, space)?;
            let mut img = Arc::unwrap_or_clone(img);
            img.meta = meta;
            (Value::Image(Arc::new(img)), warnings)
        }
        (Value::Frames(fr), Some((meta, size)), _) => {
            let (meta, warnings) = map_image_meta_for(rules, meta, size, space)?;
            let mut fr = Arc::unwrap_or_clone(fr);
            fr.meta = meta;
            (Value::Frames(Arc::new(fr)), warnings)
        }
        (Value::Audio(a), _, Value::Audio(src)) => {
            let (meta, warnings) = map_audio_meta_for(rules, &src.meta, time, a.frames() as u64)?;
            let mut a = Arc::unwrap_or_clone(a);
            a.meta = meta;
            (Value::Audio(Arc::new(a)), warnings)
        }
        (other, _, _) => (other, Vec::new()),
    })
}

/// 申告した解析を解く口（`render` は `Solver`、`render_cached` はキャッシュ）。
type SolveFn<'a> = dyn FnMut(&Need, &Value, &dyn DynAnalysis, &AnalyzeCx<'_>) -> Result<AnyOutput, EngineError>
    + 'a;

/// 有効なノード 1 つを描いた結果。警告は、操作の警告とメタデータを写したときの警告を分けて持つ（並びは操作 → 写し）。
struct NodeRun {
    value: Value,
    apply_warnings: Vec<Issue>,
    map_warnings: Vec<Issue>,
}

/// ノードの操作に渡す種（ノードの種・元のハッシュ・操作の ID から。§8.2）。
fn node_seed(node: &PlannedNode, source: &Digest) -> u64 {
    determinism::seed(node.seed, &source.0, node.op.id())
}

/// ノードの入力の値（計画の順）。
fn node_inputs(
    node: &PlannedNode,
    values: &BTreeMap<NodeId, Value>,
    inputs: &BTreeMap<InputId, Value>,
) -> Result<Vec<Value>, EngineError> {
    node.inputs
        .iter()
        .map(|r| lookup(values, inputs, r))
        .collect::<Option<_>>()
        .ok_or_else(|| EngineError::Internal("入力の値がありません".into()))
}

/// 外したノードは入力 0 をそのまま通す。
fn pass_through(ins: &[Value]) -> Result<Value, EngineError> {
    ins.first()
        .cloned()
        .ok_or_else(|| EngineError::Internal("外したノードに入力がありません".into()))
}

/// ノードの出力の形が推論と合うか（E9。合わなければ操作の不具合）。
fn check_node_shape(node: &PlannedNode, out: &Value) -> Result<(), EngineError> {
    if node.inferred.shape.admits(&out.shape()) {
        Ok(())
    } else {
        Err(EngineError::Internal(format!(
            "{} の出力の形が推論と違います（推論 {:?}、実際 {:?}）",
            node.op.id(),
            node.inferred.shape,
            out.shape()
        )))
    }
}

/// ノードの警告を、ノードを付けて足す。持ち上げた操作はフレームごとに同じ警告を出すので、ノードごとに同じものは
/// 1 つにする（出た順は保つ）。
fn push_warnings(warnings: &mut Vec<Issue>, node: NodeId, node_warnings: Vec<Issue>) {
    for w in node_warnings {
        let w = w.at(node);
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
}

/// 進捗：推論の手間（`Inferred::cost`）の累積を、計画の手間の合計に対して出す（単調に増え、最後は合計）。
struct Progress<'a> {
    mon: &'a dyn Monitor,
    done: u64,
    total: u64,
}

impl<'a> Progress<'a> {
    fn start(plan: &Plan, mon: &'a dyn Monitor) -> Self {
        let total = plan.cost();
        mon.progress(0, total);
        Self {
            mon,
            done: 0,
            total,
        }
    }

    fn node_done(&mut self, node: &PlannedNode) {
        self.done = self.done.saturating_add(node.inferred.cost).min(self.total);
        self.mon.progress(self.done, self.total);
    }
}

/// 形の推論の段階で `Dim` が決まっているかを確かめる補助。
pub fn exact_size(w: Dim, h: Dim) -> Option<Size> {
    Some(Size {
        w: u32::try_from(w.exact()?).ok()?,
        h: u32::try_from(h.exact()?).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::audio::loudness_gating::LoudnessGating;
    use crate::analysis::image::opaque_bounds::{
        OpaqueBounds, OpaqueBoundsOutput, OpaqueBoundsParams,
    };
    use crate::analysis::{AnalysisExample, Estimate};
    use crate::op::Numeric;

    /// 試験用：`image.opaque_bounds` の版を上げたもの（docs の手順で解析の版を上げた形。値は v1 と同じでよい。
    /// 版の検査だけに使う）。
    struct OpaqueBoundsV2;

    impl Analysis for OpaqueBoundsV2 {
        const ID: &'static str = OpaqueBounds::ID;
        const VERSION: u16 = 2;
        const ACCEPTS: &'static [ValueKind] = OpaqueBounds::ACCEPTS;
        const NUMERIC: Numeric = OpaqueBounds::NUMERIC;
        type Params = OpaqueBoundsParams;
        type Output = OpaqueBoundsOutput;

        fn run(
            p: &OpaqueBoundsParams,
            v: &Value,
            cx: &AnalyzeCx<'_>,
        ) -> Result<OpaqueBoundsOutput, EngineError> {
            OpaqueBounds::run(p, v, cx)
        }

        fn examples() -> Vec<AnalysisExample<OpaqueBoundsParams>> {
            OpaqueBounds::examples()
        }

        fn estimate(p: &OpaqueBoundsParams, input: &Shape) -> Estimate {
            OpaqueBounds::estimate(p, input)
        }
    }

    /// 今の版の操作は、申告する解析の今の版を指す（検証で、opaque_bounds の版を上げて v1 を legacy に残すと、
    /// 今の版の image.trim_alpha v1 が旧版で切り続け、表示（v2）とずれても、どのテストも落ちなかった）。
    #[test]
    fn current_ops_declare_the_current_analyses() {
        let ops = crate::ops::registry().unwrap();
        let analyses = crate::analysis::registry().unwrap();
        check_needs(&ops, &analyses).unwrap();
        check_current_needs(&ops, &analyses).unwrap();
        assert!(Engine::new().is_ok());
        // 申告している今の版の操作がある（検査が空回りしていない）
        assert!(
            ops.current()
                .iter()
                .any(|o| o.static_needs().contains(&("image.opaque_bounds", 1)))
        );
        // 解析の版を上げて旧版を legacy に残す：登録はそろっている（check_needs は通る）が、今の版の操作が
        // 旧版を申告し続けるので断る
        let mut bumped = AnalysisRegistry::empty();
        bumped.add_current::<OpaqueBoundsV2>().unwrap();
        bumped.add_legacy::<OpaqueBounds>().unwrap();
        bumped.add_current::<LoudnessGating>().unwrap();
        check_needs(&ops, &bumped).unwrap();
        let err = check_current_needs(&ops, &bumped).unwrap_err();
        assert!(
            err.contains("image.trim_alpha v1") && err.contains("image.opaque_bounds v1"),
            "{err}"
        );
        assert!(err.contains("（v2）"), "{err}");
        // 申告される解析に今の版がない（旧版だけ）ときも断る
        let mut only_legacy = AnalysisRegistry::empty();
        only_legacy.add_legacy::<OpaqueBounds>().unwrap();
        only_legacy.add_current::<LoudnessGating>().unwrap();
        check_needs(&ops, &only_legacy).unwrap();
        let err = check_current_needs(&ops, &only_legacy).unwrap_err();
        assert!(err.contains("（なし）"), "{err}");
        // 旧版・退役した操作は旧版の解析を申告してよい（同じ操作を旧版・退役として入れると通る）
        let mut old_ops = Registry::empty();
        old_ops
            .add_legacy::<crate::ops::image::trim_alpha::TrimAlpha>()
            .unwrap();
        old_ops
            .add_retired::<crate::ops::audio::normalize::Normalize>()
            .unwrap();
        check_needs(&old_ops, &only_legacy).unwrap();
        check_current_needs(&old_ops, &bumped).unwrap();
        check_current_needs(&old_ops, &only_legacy).unwrap();
        // 確かめは Engine::new の道（with_limits）でだけ行う。試験用の登録表を渡す with_registries は通す
        assert!(Engine::with_registries(ops, bumped, Limits::default()).is_ok());
    }

    fn node(id: u32, from: PortRef) -> crate::doc::Node {
        crate::doc::Node {
            id: NodeId(id),
            op: "image.flip".into(),
            v: 1,
            params: serde_json::json!({"axis": "horizontal"}),
            inputs: vec![from],
            seed: 0,
            enabled: true,
        }
    }

    fn image_shape() -> BTreeMap<InputId, Shape> {
        BTreeMap::from([(
            InputId(0),
            Shape::Image {
                w: Dim::Exact(300),
                h: Dim::Exact(20),
                fmt: crate::value::PixelFormat::Rgba8,
            },
        )])
    }

    /// 最後の使い手の表：値はいちばん後ろの使い手のあとで落とす。出力が使う値は落とさず、どこからも使わない値は描いた直後に落とす。
    #[test]
    fn release_schedule_drops_after_the_last_use() {
        let engine = Engine::new().unwrap();
        let input = PortRef::Input { id: InputId(0) };
        let n = |id| PortRef::Node { id: NodeId(id) };
        // 線形：1 → 2 → 3（出力）
        let mut g = Graph::new_single();
        g.nodes = vec![node(1, input), node(2, n(1)), node(3, n(2))];
        g.outputs[0].from = n(3);
        g.next_id = 4;
        let plan = engine.compile(&g, &image_shape()).unwrap();
        assert_eq!(plan.released_after(0), &[] as &[NodeId]);
        assert_eq!(plan.released_after(1), &[NodeId(1)]);
        assert_eq!(plan.released_after(2), &[NodeId(2)]);
        // 枝分かれ：1 を 2 と 3 が使い、2 は使われない。出力は 3 と 1（1 は落とさない）
        g.nodes = vec![node(1, input), node(2, n(1)), node(3, n(1))];
        g.outputs[0].from = n(3);
        let mut out1 = g.outputs[0].clone();
        out1.id = OutputId(1);
        out1.from = n(1);
        let plan = engine.compile(&g, &image_shape()).unwrap();
        assert_eq!(plan.released_after(0), &[] as &[NodeId]);
        assert_eq!(plan.released_after(1), &[NodeId(2)]);
        assert_eq!(plan.released_after(2), &[NodeId(1)]);
        g.outputs.push(out1);
        let plan = engine.compile(&g, &image_shape()).unwrap();
        assert_eq!(plan.released_after(2), &[] as &[NodeId]);
        assert!(plan.released_after(9).is_empty());
    }

    static PROBE_OUT: std::sync::Mutex<Vec<std::sync::Weak<crate::value::Image>>> =
        std::sync::Mutex::new(Vec::new());
    static PROBE_ALIVE: std::sync::Mutex<Vec<Vec<bool>>> = std::sync::Mutex::new(Vec::new());

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ProbeParams {}

    impl crate::op::OpParams for ProbeParams {
        fn schema() -> crate::op::ParamSchema {
            crate::op::ParamSchema { params: Vec::new() }
        }
    }

    /// 試験用：入力の画像をそのまま新しい値にして返し、それまでに出した値が生きているかを描くたびに記録する
    /// （メタデータを自分で書く操作にして、出力の Arc を実行器がそのまま持つようにする）。
    struct Probe;

    impl crate::op::Op for Probe {
        const ID: &'static str = "test.probe";
        const VERSION: u16 = 1;
        type Params = ProbeParams;

        fn signature() -> crate::op::Signature {
            crate::op::Signature {
                inputs: &[crate::op::PortSpec {
                    name: "image",
                    accepts: &[ValueKind::Image],
                }],
                lift: Lift::None,
                numeric: Numeric::Exact,
                meta: MetaAccess::Write,
            }
        }

        fn infer(_: &ProbeParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
            Ok(Inferred {
                shape: inputs[0],
                space: SpaceMap::Identity,
                time: TimeMap::Identity,
                cost: 7,
            })
        }

        fn apply(
            _: &ProbeParams,
            inputs: &[&Value],
            _: &mut ApplyCx<'_>,
        ) -> Result<crate::op::Applied, EngineError> {
            let Value::Image(img) = inputs[0] else {
                return Err(EngineError::Internal("画像ではありません".into()));
            };
            let out = Arc::new((**img).clone());
            let mut weaks = PROBE_OUT.lock().unwrap();
            PROBE_ALIVE
                .lock()
                .unwrap()
                .push(weaks.iter().map(|w| w.strong_count() > 0).collect());
            weaks.push(Arc::downgrade(&out));
            Ok(crate::op::Applied::value(Value::Image(out)))
        }

        fn examples() -> Vec<crate::op::Example<ProbeParams>> {
            Vec::new()
        }
    }

    /// 3 段の鎖で、使い終えた中間の値は描き終える前に落ちる（3 段目を描くとき、1 段目の値はもうない）。
    /// 進捗は推論の手間の累積で、単調に増え、最後は合計になる。
    #[test]
    fn intermediate_values_drop_early_and_progress_adds_up() {
        let mut registry = Registry::empty();
        registry.add_current::<Probe>().unwrap();
        let engine = Engine::with_registry(registry, Limits::default());
        let mut g = Graph::new_single();
        for _ in 0..3 {
            g.push_linear("test.probe", 1, serde_json::json!({}), 0)
                .unwrap();
        }
        let plan = engine.compile(&g, &image_shape()).unwrap();
        let input = Value::Image(Arc::new(Image {
            px: Raster::filled(
                300,
                20,
                crate::value::PixelFormat::Rgba8,
                None,
                &[1, 2, 3, 4],
            )
            .unwrap(),
            meta: crate::value::ImageMeta::default(),
        }));
        struct Record(std::sync::Mutex<Vec<(u64, u64)>>);
        impl Monitor for Record {
            fn canceled(&self) -> bool {
                false
            }
            fn progress(&self, done: u64, total: u64) {
                self.0.lock().unwrap().push((done, total));
            }
        }
        let mon = Record(std::sync::Mutex::new(Vec::new()));
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input,
                source: Digest::default(),
            },
        )]);
        let out = engine.render(&plan, &inputs, &mon).unwrap();
        let alive = PROBE_ALIVE.lock().unwrap().clone();
        assert_eq!(alive, vec![vec![], vec![true], vec![false, true]]);
        // 出力（3 段目）は生きている
        let weaks = PROBE_OUT.lock().unwrap();
        assert!(weaks[2].strong_count() > 0 && weaks[1].strong_count() == 0);
        drop(out);
        let calls = mon.0.into_inner().unwrap();
        assert_eq!(calls, vec![(0, 21), (7, 21), (14, 21), (21, 21)]);
        assert_eq!(plan.cost(), 21);
    }
}
