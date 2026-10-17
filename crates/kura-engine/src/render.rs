//! 実行器（docs/editing.md §12）。文書を検証・推論して計画にし（compile）、元の値に当てる（render）。
//!
//! - load：文書を読み、スキーマ・知らない操作・新しすぎる版を確かめる（E8）。
//! - compile：形の正しさ → 操作の版の解決（旧版・移行）→ 引数の読み込みと範囲 → 形の推論 → 上限。描かない。
//! - render：入力の値を計画の形と上限に照らしてから、ノードを順に実行する。フレームの列への持ち上げと、
//!   メタデータの写しはここで行う。実際の形・写し方が推論と合わなければ内部の誤り（操作の不具合）として止める。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::analysis::{
    Analysis, AnalysisDescriptor, AnalysisRegistry, AnalysisRequest, AnalyzeCx, Analyzed,
    DynAnalysis, Need, Solve, Solver,
};
use crate::determinism;
use crate::digest::Digest;
use crate::doc::{
    self, Graph, InputId, InputRole, NodeId, OutputId, PortRef, QUANTIZE_VERSION, Quantize,
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
}

/// 文書の入力に結び付ける値。`source` は元のファイルの中身のハッシュ（乱数の種に使う）。
#[derive(Debug, Clone)]
pub struct InputValue {
    pub value: Value,
    pub source: Digest,
}

struct PlannedNode {
    id: NodeId,
    op: Arc<dyn DynOp>,
    params: Box<AnyParams>,
    inputs: Vec<PortRef>,
    seed: u64,
    enabled: bool,
    lifted: bool,
    inferred: Inferred,
    /// 描く前に解く解析（操作の申告。§11.2）。外したノードと持ち上げたノードは空。
    needs: Vec<Need>,
}

/// 検証と推論を済ませた計画。
pub struct Plan {
    /// 文書の全体の規則の版（メタデータの写し方を選ぶ。§8.4）。
    rules: u32,
    inputs: BTreeMap<InputId, Shape>,
    nodes: Vec<PlannedNode>,
    outputs: Vec<(OutputId, PortRef, Shape, Option<Quantize>)>,
    primary: Option<InputId>,
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
        let mut inputs = BTreeMap::new();
        for slot in &g.inputs {
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
        let cx = InferCx {
            limits: &self.limits,
        };
        let mut shapes: BTreeMap<NodeId, Shape> = BTreeMap::new();
        let mut nodes = Vec::with_capacity(g.nodes.len());
        for node in &g.nodes {
            let at = |i: Issues| i.at(node.id);
            let one = |code: IssueCode, msg: &str| Issues::one(Issue::error(code, msg).at(node.id));
            // 引数は resolve の中で正規の JSON に往復させてある（migrate にも往復させたものが渡る）
            let (op, raw) = match self.registry.resolve(&node.op, node.v, &node.params) {
                Ok(Resolved::Direct(op, params) | Resolved::Migrated(op, params)) => (op, params),
                Err(issue) => return Err(Issues::one(issue.at(node.id))),
            };
            let params = op.parse(&raw).map_err(at)?;
            let in_shapes: Vec<Shape> = node
                .inputs
                .iter()
                .map(|r| lookup(&shapes, &inputs, r))
                .collect::<Option<_>>()
                .ok_or_else(|| one(IssueCode::BadGraph, "入力の形が分かりません"))?;
            let sig = op.signature();
            let lifted = sig.lift == Lift::PerFrame
                && in_shapes
                    .first()
                    .is_some_and(|s| s.kind() == ValueKind::Frames);
            let inferred = if lifted {
                // キャンバスいっぱいのフレームの列だけ、各フレームに当てる（トリムしたフレームはキャンバスの座標の扱いが決まっていない）
                let Some(frame) = in_shapes[0].frame_image() else {
                    return Err(one(
                        IssueCode::BadInput,
                        "トリムしたフレームの列には、この操作を当てられません",
                    ));
                };
                let mut frame_inputs = in_shapes.clone();
                frame_inputs[0] = frame;
                let inner = op.infer(params.as_ref(), &frame_inputs, &cx).map_err(at)?;
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
                    match op.infer(params.as_ref(), &fixed, &cx) {
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
                }
            } else {
                op.infer(params.as_ref(), &in_shapes, &cx).map_err(at)?
            };
            let inferred = if node.enabled {
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
                return Err(Issues::one(
                    Issue::error(IssueCode::TooLarge, why).at(node.id),
                ));
            }
            // 申告した解析（外したノードは描かないので解かない。持ち上げる操作は申告できない）
            let needs = if node.enabled && !lifted {
                self.plan_needs(op.as_ref(), params.as_ref(), &in_shapes)
                    .map_err(at)?
            } else {
                Vec::new()
            };
            shapes.insert(node.id, inferred.shape);
            nodes.push(PlannedNode {
                needs,
                id: node.id,
                op,
                params,
                inputs: node.inputs.clone(),
                seed: node.seed,
                enabled: node.enabled,
                lifted,
                inferred,
            });
        }
        let outputs = g
            .outputs
            .iter()
            .map(|o| lookup(&shapes, &inputs, &o.from).map(|s| (o.id, o.from, s, o.quantize)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                Issues::one(Issue::error(IssueCode::BadGraph, "出力の形が分かりません"))
            })?;
        // 量子化は音声の出力だけ、知っている版・ビット数だけ
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
        let primary = g
            .inputs
            .iter()
            .find(|i| i.role == InputRole::Primary)
            .map(|i| i.id);
        Ok(Plan {
            rules: g.rules,
            inputs,
            nodes,
            outputs,
            primary,
        })
    }

    /// 計画を入力に当てて描く。入力の値は計画の形と上限に照らしてから使う。操作が申告した解析は、各ノードを描く前に解く。
    pub fn render(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, InputValue>,
        mon: &dyn Monitor,
    ) -> Result<Rendered, EngineError> {
        self.render_with_solver(plan, inputs, mon, &Solve)
    }

    /// 描く（申告された解析を `solver` で解く。段 10 のキャッシュ・テストの記録と差し替えに使う）。
    pub fn render_with_solver(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, InputValue>,
        mon: &dyn Monitor,
        solver: &dyn Solver,
    ) -> Result<Rendered, EngineError> {
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        for (id, shape) in &plan.inputs {
            let Some(v) = inputs.get(id) else {
                return Err(bad_input(format!("入力 {} がありません", id.0)));
            };
            let actual = v.value.shape();
            if !shape.admits(&actual) {
                return Err(bad_input(format!("入力 {} の形が計画と違います", id.0)));
            }
            self.limits.check(&actual).map_err(EngineError::Limit)?;
        }
        let source = plan
            .primary
            .and_then(|id| inputs.get(&id))
            .map(|v| v.source)
            .unwrap_or_default();
        let input_values: BTreeMap<InputId, Value> =
            inputs.iter().map(|(k, v)| (*k, v.value.clone())).collect();
        let mut values: BTreeMap<NodeId, Value> = BTreeMap::new();
        let mut warnings = Vec::new();
        for node in &plan.nodes {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let ins: Vec<Value> = node
                .inputs
                .iter()
                .map(|r| lookup(&values, &input_values, r))
                .collect::<Option<_>>()
                .ok_or_else(|| EngineError::Internal("入力の値がありません".into()))?;
            let (out, node_warnings) = if node.enabled {
                let mut cx = ApplyCx::new(
                    mon,
                    &self.limits,
                    determinism::seed(node.seed, &source.0, node.op.id()),
                    plan.rules,
                );
                // 申告した解析を先に解く（段 10 で、入力の指紋 × ID × 版 × 引数をキーにしたキャッシュから引く）
                for need in &node.needs {
                    let a = self.solvable(need)?;
                    let v = ins.get(need.input).ok_or_else(|| {
                        EngineError::Internal("解析する入力の値がありません".into())
                    })?;
                    let acx = AnalyzeCx {
                        mon,
                        limits: &self.limits,
                    };
                    let out = solver
                        .solve(node.id, need, v, a.as_ref(), &acx)
                        .map_err(|e| e.at(node.id))?;
                    cx.preload(need, v, out)?;
                }
                let (out, mut mapped) = self
                    .apply_node(plan.rules, node, &ins, &mut cx)
                    .map_err(|e| e.at(node.id))?;
                // 申告（needs）と、操作が実際に使った解析が食い違えば操作の不具合（キャッシュの鍵が作れない）
                if cx.analyses_solved_in_place() != 0 {
                    return Err(EngineError::Internal(format!(
                        "{} が先に解いていない解析を使いました（needs と apply の食い違い）",
                        node.op.id()
                    )));
                }
                let mut all = cx.warnings;
                all.append(&mut mapped);
                (out, all)
            } else {
                (
                    ins.first().cloned().ok_or_else(|| {
                        EngineError::Internal("外したノードに入力がありません".into())
                    })?,
                    Vec::new(),
                )
            };
            if !node.inferred.shape.admits(&out.shape()) {
                return Err(EngineError::Internal(format!(
                    "{} の出力の形が推論と違います（推論 {:?}、実際 {:?}）",
                    node.op.id(),
                    node.inferred.shape,
                    out.shape()
                )));
            }
            // 持ち上げた操作はフレームごとに同じ警告を出すので、ノードごとに同じものは 1 つにする（出た順は保つ）
            for w in node_warnings {
                let w = w.at(node.id);
                if !warnings.contains(&w) {
                    warnings.push(w);
                }
            }
            values.insert(node.id, out);
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

    /// 申告した解析の実装（計画を作ったときに確かめてある）。
    fn solvable(&self, need: &Need) -> Result<Arc<dyn DynAnalysis>, EngineError> {
        self.analyses.get(need.id, need.v).ok_or_else(|| {
            EngineError::Internal(format!("解析 {} v{} がありません", need.id, need.v))
        })
    }

    /// ノード 1 つを実行し、メタデータを写す。返すのは値と、写したときの警告。
    fn apply_node(
        &self,
        rules: u32,
        node: &PlannedNode,
        ins: &[Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<(Value, Vec<Issue>), EngineError> {
        let sig = node.op.signature();
        let refs: Vec<&Value> = ins.iter().collect();
        let (value, space, time) = if node.lifted {
            let Value::Frames(frames) = &ins[0] else {
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
            for f in frames.frames() {
                cx.check()?;
                // 各フレームは、フレームの列のメタデータを持った 1 枚の画像として当てる（単体の画像と同じ見え方にする）
                let single = Value::Image(Arc::new(Image {
                    px: f.px.clone(),
                    meta: frames.meta.clone(),
                }));
                let mut frame_refs = refs.clone();
                frame_refs[0] = &single;
                let applied = node.op.apply(node.params.as_ref(), &frame_refs, cx)?;
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
            let applied = node.op.apply(node.params.as_ref(), &refs, cx)?;
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
}
