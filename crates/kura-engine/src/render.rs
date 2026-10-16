//! 実行器（docs/editing.md §12）。文書を検証・推論して計画にし（compile）、元の値に当てる（render）。
//!
//! - load：文書を読み、スキーマ・知らない操作・新しすぎる版を確かめる（E8）。
//! - compile：形の正しさ → 操作の版の解決（旧版・移行）→ 引数の読み込みと範囲 → 形の推論 → 上限。描かない。
//! - render：入力の値を計画の形と上限に照らしてから、ノードを順に実行する。フレームの列への持ち上げと、
//!   メタデータの写しはここで行う。実際の形・写し方が推論と合わなければ内部の誤り（操作の不具合）として止める。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::determinism;
use crate::digest::Digest;
use crate::doc::{self, Graph, InputId, InputRole, NodeId, OutputId, PortRef};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::Size;
use crate::limits::Limits;
use crate::mapping::{SpaceMap, TimeMap, map_audio_meta, map_image_meta};
use crate::monitor::Monitor;
use crate::op::{
    AnyParams, ApplyCx, DynOp, InferCx, Inferred, Lift, MetaAccess, Registry, Resolved,
};
use crate::shape::{Dim, Fullness, Shape, ValueKind};
use crate::value::{Frames, Image, Raster, Value};

/// 操作エンジン。登録表と上限を持つ。
pub struct Engine {
    registry: Registry,
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
}

/// 検証と推論を済ませた計画。
pub struct Plan {
    /// 文書の全体の規則の版（メタデータの写し方を選ぶ。§8.4）。
    rules: u32,
    inputs: BTreeMap<InputId, Shape>,
    nodes: Vec<PlannedNode>,
    outputs: Vec<(OutputId, PortRef, Shape)>,
    primary: Option<InputId>,
}

impl Plan {
    /// 出力の形（描かずに分かる）。
    pub fn output_shapes(&self) -> Vec<(OutputId, Shape)> {
        self.outputs.iter().map(|(id, _, s)| (*id, *s)).collect()
    }

    /// ノードの推論の結果（エディタの表示用）。
    pub fn node_shape(&self, id: NodeId) -> Option<Shape> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.inferred.shape)
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

    /// 上限を指定したエンジン。
    pub fn with_limits(limits: Limits) -> Result<Self, EngineError> {
        let registry = crate::ops::registry().map_err(EngineError::Internal)?;
        Ok(Self { registry, limits })
    }

    /// 登録表を指定したエンジン（版の契約のテストで、試験用の操作を入れるときに使う）。
    pub fn with_registry(registry: Registry, limits: Limits) -> Self {
        Self { registry, limits }
    }

    /// 登録表。
    pub fn registry(&self) -> &Registry {
        &self.registry
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
                // 出力の大きさが中身で決まる操作（入力は決まっているのに出力が AtMost）は、フレームごとに大きさが変わりうるので持ち上げない
                let content_dependent = (fw.exact().is_some() && w.exact().is_none())
                    || (fh.exact().is_some() && h.exact().is_none());
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
            shapes.insert(node.id, inferred.shape);
            nodes.push(PlannedNode {
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
            .map(|o| lookup(&shapes, &inputs, &o.from).map(|s| (o.id, o.from, s)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                Issues::one(Issue::error(IssueCode::BadGraph, "出力の形が分かりません"))
            })?;
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

    /// 計画を入力に当てて描く。入力の値は計画の形と上限に照らしてから使う。
    pub fn render(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, InputValue>,
        mon: &dyn Monitor,
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
                let mut cx = ApplyCx {
                    mon,
                    limits: &self.limits,
                    seed: determinism::seed(node.seed, &source.0, node.op.id()),
                    warnings: Vec::new(),
                };
                let (out, mut mapped) = self.apply_node(plan.rules, node, &ins, &mut cx)?;
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
            warnings.extend(node_warnings.into_iter().map(|w| w.at(node.id)));
            values.insert(node.id, out);
        }
        let outputs = plan
            .outputs
            .iter()
            .map(|(id, from, _)| {
                let value = lookup(&values, &input_values, from)
                    .ok_or_else(|| EngineError::Internal("出力の値がありません".into()))?;
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

/// 入力 0 のメタデータを、文書の規則の版の写し方で写して出力に付ける。
/// 規則の版を上げたら、古い版の写し方も残して、ここで版ごとに選ぶ（§8.4）。
fn with_mapped_meta(
    rules: u32,
    out: Value,
    input: &Value,
    space: &SpaceMap,
    time: &TimeMap,
) -> Result<(Value, Vec<Issue>), EngineError> {
    match rules {
        1 => Ok(with_mapped_meta_r1(out, input, space, time)),
        // 形の確かめで知らない版は断っているので、ここには来ない
        other => Err(EngineError::Internal(format!(
            "規則の版 {other} の写し方がありません"
        ))),
    }
}

/// 規則 1 のメタデータの写し方。
fn with_mapped_meta_r1(
    out: Value,
    input: &Value,
    space: &SpaceMap,
    time: &TimeMap,
) -> (Value, Vec<Issue>) {
    match (out, image_meta_and_size(input), input) {
        (Value::Image(img), Some((meta, size)), _) => {
            let (meta, warnings) = map_image_meta(meta, size, space);
            let mut img = Arc::unwrap_or_clone(img);
            img.meta = meta;
            (Value::Image(Arc::new(img)), warnings)
        }
        (Value::Frames(fr), Some((meta, size)), _) => {
            let (meta, warnings) = map_image_meta(meta, size, space);
            let mut fr = Arc::unwrap_or_clone(fr);
            fr.meta = meta;
            (Value::Frames(Arc::new(fr)), warnings)
        }
        (Value::Audio(a), _, Value::Audio(src)) => {
            let (meta, warnings) = map_audio_meta(&src.meta, time, a.frames() as u64);
            let mut a = Arc::unwrap_or_clone(a);
            a.meta = meta;
            (Value::Audio(Arc::new(a)), warnings)
        }
        (other, _, _) => (other, Vec::new()),
    }
}

/// 形の推論の段階で `Dim` が決まっているかを確かめる補助。
pub fn exact_size(w: Dim, h: Dim) -> Option<Size> {
    Some(Size {
        w: u32::try_from(w.exact()?).ok()?,
        h: u32::try_from(h.exact()?).ok()?,
    })
}
