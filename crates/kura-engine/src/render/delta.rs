//! 変わった範囲（docs/editing.md §12。段 10。E31）。
//!
//! 2 段で求める：
//! 1. 描く前の推論（[`Engine::delta`]）：前後の計画を NodeId で対応させ、ノードごとに変わりうる範囲を保守的に求める。
//!    新しい・消えたノード、操作の ID・版、入力のつなぎ、種、規則・段、元、推論した形が違えば全体。外す・入れるは
//!    `effect`、引数の違いは `param_change`、上流の違いは `Reach::forward` で写す（引数と上流の両方が変われば和。三角不等式）。
//!    上流の全体は、画素だけの変化と分かる形（Indexed8 以外）のときだけ入力の矩形に開いて写す（Indexed8 はパレットだけが
//!    変わりうるので全体のまま）。
//! 2. 描いたあとの絞り込み（[`refine`]）：手元に持っている前の出力の値と今の値のタイルを `Arc::ptr_eq` で比べ、
//!    共有したままのタイル（前の値で有効なもの）を外す（番地を覚えて比べない。前の値がなければ推論のまま。docs/editing.md §12.6）。報告の単位は
//!    [`VIEW_TILE`]。
//!
//! `render_cached` は前回の計画・入力の指紋・出力の値を [`NodeCache`] に持ち、結果に [`Changed`] を付ける。

use std::collections::BTreeMap;
use std::sync::Arc;

use super::cache::{CachedOutput, Fingerprint, FingerprintedInput, NodeCache};
use super::region::{Area, Spans, Tiles, VIEW_TILE, frames_of};
use super::{Engine, Plan, PlannedNode, lookup, op_shapes};
use crate::digest::Digest;
use crate::doc::{InputId, NodeId, OutputId, PortRef};
use crate::error::EngineError;
use crate::geom::IRect;
use crate::shape::Shape;
use crate::value::{PixelFormat, TILE, Value};

/// ノードと出力ごとの、前の計画から変わりうる範囲（保守的。キャンバスの座標・音声の区間）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    /// 今の計画のノードごとの範囲。
    pub nodes: BTreeMap<NodeId, Area>,
    /// 今の計画の出力ごとの範囲。
    pub outputs: Vec<(OutputId, Area)>,
}

/// 出力 1 つの変わった範囲（描いたあとに報告するもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputChange {
    /// 全体（前の値がない・形が変わった・全体に依る操作が作り直した）。
    All,
    /// 画像：フレームの番号ごとの、変わったタイル（1 辺 [`VIEW_TILE`]、フレームの画像の座標）。変わったタイルのない
    /// フレームは持たない（空の表は何も変わっていない）。
    Tiles(BTreeMap<u32, Tiles>),
    /// 音声：変わった区間（空は何も変わっていない）。
    Samples(Spans),
}

impl OutputChange {
    /// 何も変わっていなければ true。
    pub fn is_empty(&self) -> bool {
        match self {
            Self::All => false,
            Self::Tiles(map) => map.values().all(Tiles::is_empty),
            Self::Samples(s) => s.is_empty(),
        }
    }
}

/// `render_cached` の、前回の描画から変わった範囲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// 解像度の段（0 は等倍）。
    pub level: u8,
    /// 出力ごとの変わった範囲（計画の出力の順）。
    pub per_output: Vec<(OutputId, OutputChange)>,
    /// メタデータ（格子・ピボット・スライス・ループ点など）か、フレームの表示時間・繰り返しが変わったら true（前の値が
    /// なければ true）。
    pub meta_changed: bool,
}

/// 前回の `render_cached`（ノードのキャッシュが持つ。出力の値は、変わったタイルを `Arc::ptr_eq` で比べるために持つ）。
pub(crate) struct PrevRender {
    plan: Plan,
    inputs: BTreeMap<InputId, (Fingerprint, Digest)>,
    outputs: Vec<(OutputId, Value)>,
}

/// 入力の（指紋, 元）。推論はこれだけを見る（前回の描画・表示は入力の値を持たない）。
pub(super) fn stamps(
    inputs: &BTreeMap<InputId, FingerprintedInput>,
) -> BTreeMap<InputId, (Fingerprint, Digest)> {
    inputs
        .iter()
        .map(|(k, v)| (*k, (v.fingerprint, v.source)))
        .collect()
}

impl Engine {
    /// 前の計画 `prev`（入力 `prev_inputs`）から今の計画 `next`（入力 `next_inputs`）へ、ノードと出力ごとに変わりうる範囲
    /// （描かない。保守的：前後を全体で描いた差は、必ずこの範囲に入る。E31）。入力の値は指紋と元で比べる。
    pub fn delta(
        &self,
        prev: &Plan,
        prev_inputs: &BTreeMap<InputId, FingerprintedInput>,
        next: &Plan,
        next_inputs: &BTreeMap<InputId, FingerprintedInput>,
    ) -> Result<Delta, EngineError> {
        infer(prev, &stamps(prev_inputs), next, &stamps(next_inputs))
    }
}

/// 入力とノードごとの値（`PortRef` で引く）。
struct Ports<T> {
    inputs: BTreeMap<InputId, T>,
    nodes: BTreeMap<NodeId, T>,
}

impl<T: Clone> Ports<T> {
    fn get(&self, r: &PortRef) -> Option<T> {
        lookup(&self.nodes, &self.inputs, r)
    }
}

/// 計画の値の形（ノードと入力）。
fn shapes_of(plan: &Plan) -> Ports<Shape> {
    Ports {
        inputs: plan.inputs.clone(),
        nodes: plan
            .nodes
            .iter()
            .map(|n| (n.id, n.inferred.shape))
            .collect(),
    }
}

/// すべてが変わったとみなす推論。
fn everything(next: &Plan) -> Delta {
    Delta {
        nodes: next.nodes.iter().map(|n| (n.id, Area::All)).collect(),
        outputs: next.outputs.iter().map(|o| (o.0, Area::All)).collect(),
    }
}

/// 推論の本体（入力は（指紋, 元）で比べる。[`stamps`]）。
pub(super) fn infer(
    prev: &Plan,
    prev_in: &BTreeMap<InputId, (Fingerprint, Digest)>,
    next: &Plan,
    next_in: &BTreeMap<InputId, (Fingerprint, Digest)>,
) -> Result<Delta, EngineError> {
    // 規則・段・エンジンが違えば、メタデータの写し方・値の大きさ・登録表が違いうる
    if prev.rules != next.rules || prev.level != next.level || prev.engine != next.engine {
        return Ok(everything(next));
    }
    // 元（主の入力の中身のハッシュ）は、すべてのノードの種に入る
    let source = |plan: &Plan, ins: &BTreeMap<InputId, (Fingerprint, Digest)>| {
        plan.primary.and_then(|id| ins.get(&id)).map(|v| v.1)
    };
    if source(prev, prev_in) != source(next, next_in) {
        return Ok(everything(next));
    }
    let mut areas: Ports<Area> = Ports {
        inputs: BTreeMap::new(),
        nodes: BTreeMap::new(),
    };
    for (id, shape) in &next.inputs {
        let same = prev.inputs.get(id) == Some(shape)
            && prev_in.get(id).map(|v| v.0) == next_in.get(id).map(|v| v.0);
        let area = if same { Area::Empty } else { Area::All };
        areas.inputs.insert(*id, area);
    }
    let prev_nodes: BTreeMap<NodeId, &Arc<PlannedNode>> =
        prev.nodes.iter().map(|n| (n.id, n)).collect();
    let (prev_shapes, next_shapes) = (shapes_of(prev), shapes_of(next));
    for n in &next.nodes {
        let ins: Vec<Area> = n
            .inputs
            .iter()
            .map(|r| areas.get(r).unwrap_or(Area::All))
            .collect();
        let in_shapes: Vec<Shape> = n
            .inputs
            .iter()
            .map(|r| next_shapes.get(r))
            .collect::<Option<_>>()
            .ok_or_else(|| EngineError::Internal("ノードの入力の形がありません".into()))?;
        let area = match prev_nodes.get(&n.id) {
            Some(p) => {
                let prev_in_shapes: Option<Vec<Shape>> =
                    p.inputs.iter().map(|r| prev_shapes.get(r)).collect();
                node_delta(p, n, &ins, &in_shapes, prev_in_shapes.as_deref())?
            }
            None => Area::All,
        };
        areas.nodes.insert(n.id, area.clone());
    }
    let nodes = areas.nodes.clone();
    let prev_outputs: BTreeMap<OutputId, _> = prev.outputs.iter().map(|o| (o.0, o)).collect();
    let outputs = next
        .outputs
        .iter()
        .map(|(id, from, shape, q)| {
            let same = prev_outputs
                .get(id)
                .is_some_and(|p| p.1 == *from && p.2 == *shape && p.3 == *q);
            let area = if same {
                areas.get(from).unwrap_or(Area::All)
            } else {
                Area::All
            };
            (*id, area)
        })
        .collect();
    Ok(Delta { nodes, outputs })
}

/// 持ち上げる操作の 1 フレームの範囲を、フレームの列に広げる。
fn lifted(node: &PlannedNode, in_shapes: &[Shape], area: Area) -> Area {
    match (node.lifted, in_shapes.first()) {
        (true, Some(Shape::Frames { count, .. })) => area.lift(*count),
        _ => area,
    }
}

/// 上流の範囲 `input`（入力 0）を、ノードの申告で前へ写す（2 つ目からの入力が変われば全体）。
///
/// 上流の `All` は「画素の外（パレット）まで変わったかもしれない」を含む。入力の矩形に開いて写す（位置を写す申告で、広げた
/// キャンバスの縁などを外す）のは、画素だけの変化と分かるとき（[`pixels_only`]）に限り、ほかは `All` のまま（段 10 の検証の
/// D2。docs/editing.md §12.6：上流でパレットだけが変わると、canvas の縁の番号・出力のパレットも変わるのに、入力を写した矩形
/// だけになっていた）。
fn forward(node: &PlannedNode, ins: &[Area], in_shapes: &[Shape]) -> Result<Area, EngineError> {
    if ins.iter().skip(1).any(|a| !a.is_empty()) {
        return Ok(Area::All);
    }
    let Some(first) = ins.first() else {
        return Ok(Area::Empty);
    };
    if first.is_empty() {
        return Ok(Area::Empty);
    }
    let reach = node
        .op
        .reach(node.params.as_ref(), &op_shapes(node, in_shapes))?;
    Ok(match in_shapes.first() {
        Some(s) if !first.is_all() || pixels_only(s) => reach.forward_in(first, s),
        Some(_) => Area::All,
        None => reach.forward(first),
    })
}

/// 形が同じ値どうしの違いが、画素（標本）だけに限られる形なら true。形（大きさ・形式・フレームの数・キャンバスいっぱいか）は
/// `node_delta` が前後で同じことを確かめている。パレットは形に現れないので、Indexed8 は形が同じでもパレットだけが違いうる
/// （false）。メタデータの違いは画素の範囲に入れない（`meta_changed` で別に報告する）。
fn pixels_only(s: &Shape) -> bool {
    match s {
        Shape::Image { fmt, .. } | Shape::Frames { fmt, .. } => *fmt != PixelFormat::Indexed8,
        Shape::Audio { .. } => true,
    }
}

/// ノード 1 つの変わりうる範囲。
fn node_delta(
    p: &PlannedNode,
    n: &PlannedNode,
    ins: &[Area],
    in_shapes: &[Shape],
    prev_in_shapes: Option<&[Shape]>,
) -> Result<Area, EngineError> {
    let same_frame = p.op.id() == n.op.id()
        && p.op.version() == n.op.version()
        && p.inputs == n.inputs
        && p.seed == n.seed
        && p.lifted == n.lifted
        && p.inferred.shape == n.inferred.shape
        && prev_in_shapes == Some(in_shapes);
    if !same_frame {
        return Ok(Area::All);
    }
    let first = ins.first().cloned().unwrap_or_default();
    let shapes = op_shapes(n, in_shapes);
    Ok(match (p.enabled, n.enabled) {
        (false, false) => first,
        // 外した：前の出力は op(前の入力)、今は今の入力。差は「op が入力 0 から変えた範囲」と「入力 0 の差」の和
        (true, false) => {
            let effect = lifted(p, in_shapes, p.op.effect(p.params.as_ref(), &shapes)?);
            effect.union(&first)
        }
        (false, true) => {
            let effect = lifted(n, in_shapes, n.op.effect(n.params.as_ref(), &shapes)?);
            effect.union(&first)
        }
        (true, true) if p.key == n.key => forward(n, ins, in_shapes)?,
        (true, true) => {
            let change = lifted(
                n,
                in_shapes,
                n.op.param_change(p.params.as_ref(), n.params.as_ref(), &shapes)?,
            );
            change.union(&forward(n, ins, in_shapes)?)
        }
    })
}

/// 描いたあとの絞り込み：前の出力 `prev` と今の出力 `next` のタイルを `Arc::ptr_eq` で比べ、共有したままのタイル（前の値で
/// 有効で、同じ実体）を推論 `inferred` から外したもの（報告するタイルは、中身が変わったタイルと前の値で無効だったタイルを含み、
/// 推論の範囲の中にある）。形・形式・パレット・フレームの位置が違えば `All`。
pub fn refine(prev: &Value, next: &Value, inferred: &Area) -> OutputChange {
    if inferred.is_empty() {
        return match next {
            Value::Audio(_) => OutputChange::Samples(Spans::new()),
            _ => OutputChange::Tiles(BTreeMap::new()),
        };
    }
    match (prev, next) {
        (Value::Audio(a), Value::Audio(b)) => {
            let same = a.rate() == b.rate()
                && a.frames() == b.frames()
                && a.channels() == b.channels()
                && a.planes()
                    .iter()
                    .zip(b.planes())
                    .all(|(x, y)| Arc::ptr_eq(x, y));
            match (same, inferred) {
                (true, _) => OutputChange::Samples(Spans::new()),
                (false, Area::Samples(s)) => OutputChange::Samples(s.clone()),
                _ => OutputChange::All,
            }
        }
        (Value::Audio(_), _) | (_, Value::Audio(_)) => OutputChange::All,
        _ => {
            let (pf, nf) = (frames_of(prev), frames_of(next));
            if pf.len() != nf.len() {
                return OutputChange::All;
            }
            let mut out = BTreeMap::new();
            for ((_, a, ao), (i, b, bo)) in pf.into_iter().zip(nf) {
                if ao != bo
                    || a.width() != b.width()
                    || a.height() != b.height()
                    || a.format() != b.format()
                    || a.palette() != b.palette()
                {
                    return OutputChange::All;
                }
                let guess = match inferred.frame_rects(i) {
                    Some(r) => Tiles::covering(b.width(), b.height(), VIEW_TILE, &r, bo),
                    None => continue,
                };
                let mut changed = Tiles::none(b.width(), b.height(), VIEW_TILE);
                for (tx, ty) in guess.iter() {
                    if !shared_view_tile(a, b, &guess.tile_rect(tx, ty)) {
                        changed.insert(tx, ty);
                    }
                }
                if !changed.is_empty() {
                    out.insert(i, changed);
                }
            }
            OutputChange::Tiles(out)
        }
    }
}

/// 配信のタイル（画像の座標の矩形）に重なる値のタイルが、どれも同じ実体で、前の値 `a` で有効なら true。無効にしたタイルは
/// 実体を残す（`Raster::invalidate`）ので、前の値で無効だったタイルは、同じ実体でも共有とみなさない（前回は配信していない。
/// 段 10 の検証の D3。docs/editing.md §12.6 の絞り込み）。
fn shared_view_tile(a: &crate::value::Raster, b: &crate::value::Raster, r: &IRect) -> bool {
    let t = i64::from(TILE);
    let (tx0, ty0) = ((r.x / t) as u32, (r.y / t) as u32);
    let (tx1, ty1) = (((r.right() - 1) / t) as u32, ((r.bottom() - 1) / t) as u32);
    (ty0..=ty1).all(|ty| (tx0..=tx1).all(|tx| b.shares_tile(a, tx, ty) && !a.is_hole(tx, ty)))
}

/// 推論だけの報告（前の値がないとき）。
fn from_inferred(next: &Value, inferred: &Area) -> OutputChange {
    match (next, inferred) {
        (_, Area::All) => OutputChange::All,
        (Value::Audio(_), Area::Samples(s)) => OutputChange::Samples(s.clone()),
        (Value::Audio(_), Area::Empty) => OutputChange::Samples(Spans::new()),
        (Value::Audio(_), _) => OutputChange::All,
        (_, area) => OutputChange::Tiles(
            frames_of(next)
                .into_iter()
                .filter_map(|(i, px, o)| {
                    let r = area.frame_rects(i)?;
                    let t = Tiles::covering(px.width(), px.height(), VIEW_TILE, &r, o);
                    (!t.is_empty()).then_some((i, t))
                })
                .collect(),
        ),
    }
}

/// メタデータ（と、フレームの表示時間・繰り返し）が同じなら true。
pub(super) fn same_meta(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Image(x), Value::Image(y)) => x.meta == y.meta,
        (Value::Frames(x), Value::Frames(y)) => {
            x.meta == y.meta
                && x.loop_count == y.loop_count
                && x.frames().len() == y.frames().len()
                && x.frames()
                    .iter()
                    .zip(y.frames())
                    .all(|(f, g)| f.duration_ms == g.duration_ms)
        }
        (Value::Audio(x), Value::Audio(y)) => x.meta == y.meta,
        _ => false,
    }
}

/// `render_cached` のあと：前回の描画から変わった範囲を求め、今回の描画を前回として覚える。
pub(crate) fn track(
    plan: &Plan,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
    cache: &mut NodeCache,
    outputs: &[CachedOutput],
) -> Changed {
    let now = stamps(inputs);
    let prev = cache.take_prev();
    let delta = prev
        .as_ref()
        .and_then(|p| infer(&p.plan, &p.inputs, plan, &now).ok());
    let mut meta_changed = prev.is_none();
    let per_output = outputs
        .iter()
        .map(|o| {
            let inferred = delta
                .as_ref()
                .and_then(|d| d.outputs.iter().find(|(id, _)| *id == o.id))
                .map_or(Area::All, |(_, a)| a.clone());
            let before = prev
                .as_ref()
                .and_then(|p| p.outputs.iter().find(|(id, _)| *id == o.id))
                .map(|(_, v)| v);
            let change = match before {
                Some(v) => {
                    meta_changed |= !same_meta(v, &o.value);
                    refine(v, &o.value, &inferred)
                }
                None => {
                    meta_changed = true;
                    from_inferred(&o.value, &inferred)
                }
            };
            (o.id, change)
        })
        .collect();
    cache.set_prev(PrevRender {
        plan: plan.clone(),
        inputs: now,
        outputs: outputs.iter().map(|o| (o.id, o.value.clone())).collect(),
    });
    Changed {
        level: plan.level,
        per_output,
        meta_changed,
    }
}
