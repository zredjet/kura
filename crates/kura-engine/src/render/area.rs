//! 範囲の描画（docs/editing.md §12。段 10）：[`Engine::render_area`]・[`Engine::spoit`]。
//!
//! 1. 後ろから、各ノードの要る範囲を集める：範囲の描画を持つノード（`Op::REGION` で `reach` が `Global` でない）は、要る範囲を
//!    値のタイルに広げ、`Reach::backward` で入力 0 へ写す。持たないノードは入力を全体で要る。外したノードは入力 0 へそのまま。
//!    キャッシュに等倍で全体を描いた値（由来 Exact）があるノードは、そこで止める。
//! 2. 前から描く：当たりは値と警告を再生し、範囲の描画を持つノードは要るタイルだけを `apply_region` で描く（ほかのタイルは
//!    無効）。持たないノードは全体を描く：入力がどれも等倍で全体を描いた値なら `render_cached` の外れの道と同じ（継ぎ目の
//!    縮小の差分・前の出力への差分の適用（`Op::apply_appended`）・申告した解析のキャッシュ・由来 Exact で入れる）で、当たりと
//!    合わせて描画を前回として覚える（次の差分の適用に使う）。そうでなければ `apply` で素で描く（入れない）。
//! 3. 前の版の値（[`Base`]）があれば、出力のノードは「変わりうる範囲」と「前の値の無効なタイル」だけを描き、ほかは前の値の
//!    タイルを共有する（継ぎはぎ。由来 Patched）。
//!
//! - 呼び手の範囲（`AreaRequest`・`ViewRequest` の範囲・スポイトの位置）は、出力のキャンバス（フレームの数・音声の長さ）に
//!   切ってから使う（遠い座標の足し算があふれない）。
//! - 範囲の描画が無効なタイル（申告した依存の外）を読んだら、その描画を捨てて全体で描き直す（遅いだけで正しい。数は
//!   `AreaStats::fallbacks`）。継ぎはぎの描画が前の値の大きさ・形式・パレットの違いで断ったら（変わりうる範囲が小さすぎた）、
//!   前の値なしで描き直す（数は `AreaStats::rebased`）。
//! - 量子化した出力は、`render`・`render_cached` と同じく量子化を当てる（量子化の指紋でキャッシュを引く）。
//! - 継ぎはぎ・範囲の値はキャッシュに入れず、ダイジェストを取れない（[`PartialValue`]）。保存・一括・解析・スポイトには
//!   等倍で全体を描いた値か、前の値を使わない範囲の描画を使う（docs/editing.md §12.3・§12.5）。
//! - 警告は、描いたノードと当たったノードの分だけ（範囲によらず同じ）。要る範囲が空で描かなかったノードの警告は出ない
//!   （全体の警告は `render`・`render_cached` が正）。

use std::collections::BTreeMap;
use std::sync::Arc;

use super::cache::{
    Fingerprint, FingerprintedInput, NodeCache, Origin, RenderOptions, node_input_fingerprints,
    quantized_output,
};
use super::region::{
    Area, Base, FrameTiles, PartialValue, Rects, Spans, Tiles, frames_of, invalidate, tiles_in,
};
use super::{
    Engine, Plan, PlannedNode, apply_node, bad_input, check_node_shape, lookup, node_inputs,
    node_seed, op_shapes, pass_through, push_warnings,
};
use crate::digest::Digest;
use crate::doc::{InputId, NodeId, OutputId, PortRef};
use crate::error::{EngineError, Issue};
use crate::geom::{IPoint, IRect};

use crate::monitor::Monitor;
use crate::op::ApplyCx;
use crate::paint::Sample;
use crate::shape::{Fullness, Shape};
use crate::value::{Image, TILE, Value};

/// 範囲の描画で描く出力と範囲（キャンバスの座標）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaRequest {
    pub output: OutputId,
    pub area: Area,
}

/// 範囲の描画の数（1 回の描画の分）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AreaStats {
    /// `apply_region` で範囲だけを描いたノード。
    pub region_nodes: u64,
    /// `apply` で全体を描いたノード。
    pub full_nodes: u64,
    /// キャッシュの値（等倍で全体を描いたもの）を使ったノード。
    pub hits: u64,
    /// 要る範囲が空で描かなかったノード。
    pub skipped: u64,
    /// `apply_region` で描いたタイルの数（フレームの和）。
    pub drawn_tiles: u64,
    /// 前の値から共有した出力のタイルの数（継ぎはぎ）。
    pub reused_tiles: u64,
    /// 範囲の描画が無効なタイルを読み、全体で描き直した回数。
    pub fallbacks: u64,
    /// 継ぎはぎの描画が前の値（[`Base`]）の大きさ・形式・パレットの違いで断り、前の値なしで描き直した回数（変わりうる範囲が
    /// 小さすぎた印。遅いだけで値は正しい）。
    pub rebased: u64,
}

/// 範囲の描画の結果。
#[derive(Debug, Clone)]
pub struct AreaRender {
    pub output: OutputId,
    /// 描いた値（要る範囲のタイルが有効。ほかは無効か前の値）。
    pub value: PartialValue,
    /// 描いたノード・当たったノードの警告（`render` の警告の部分列で、順も同じ）。
    pub warnings: Vec<Issue>,
    pub stats: AreaStats,
}

/// スポイトで拾う位置（等倍の計画の出力の、フレーム `frame` のキャンバスの座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoitAt {
    pub output: OutputId,
    pub frame: u32,
    pub x: i64,
    pub y: i64,
}

/// 描き方の内側の誤り：無効なタイルを読んだ（全体で描き直す）か、継ぎはぎの前の値が今の値と合わなかった（前の値なしで
/// 描き直す）か、ふつうの誤り。
enum PassError {
    Hole,
    Rebase,
    Engine(EngineError),
}

impl From<EngineError> for PassError {
    fn from(e: EngineError) -> Self {
        Self::Engine(e)
    }
}

/// 出力のノードの継ぎはぎ：前の値から変わりうる範囲を無効にしたもの。
struct Patch {
    node: NodeId,
    base: Value,
}

impl Engine {
    /// 範囲の描画（エディタの表示用）：`req` の出力の範囲だけを描く。`base` があれば、その値に変わりうる範囲だけを描き直して
    /// 継ぎはぎする。キャッシュの等倍の値（由来 Exact）を使い、全体を描いたノードの値は入れる（範囲・継ぎはぎの値は入れない）。
    /// 範囲の描画は、全体の描画が失敗する入力でも通ることがある（範囲の外の中身で決まる誤り）。値は表示にだけ使う。
    pub fn render_area(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        req: &AreaRequest,
        base: Option<&Base<'_>>,
        mon: &dyn Monitor,
    ) -> Result<AreaRender, EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        cache.bind_engine(self.token)?;
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        self.check_inputs(plan, |id| inputs.get(&id).map(|v| &v.value))?;
        let (mut rebased, mut fallbacks) = (0, 0);
        let mut first = self.area_pass(plan, inputs, cache, req, base, mon, false);
        // 継ぎはぎの前の値の大きさ・形式・パレットが今と違い、範囲の描画が断った（変わりうる範囲が小さすぎた）：前の値なしで
        // 描き直す（PassError::Hole と同じく、遅いだけで正しい）
        if matches!(first, Err(PassError::Rebase)) {
            rebased += 1;
            first = self.area_pass(plan, inputs, cache, req, None, mon, false);
        }
        let result = match first {
            Ok(r) => Ok(r),
            Err(PassError::Engine(e)) => Err(e),
            // 範囲の描画が申告した依存の外を読んだ：全体で描き直す（継ぎはぎもしない）
            Err(PassError::Hole) => {
                fallbacks += 1;
                match self.area_pass(plan, inputs, cache, req, None, mon, true) {
                    Ok(r) => Ok(r),
                    Err(PassError::Engine(e)) => Err(e),
                    Err(PassError::Hole) => Err(EngineError::Internal(
                        "全体の描画で無効なタイルを読みました".into(),
                    )),
                    Err(PassError::Rebase) => Err(EngineError::Internal(
                        "前の値なしの描画で前の値の違いを断りました".into(),
                    )),
                }
            }
            Err(PassError::Rebase) => Err(EngineError::Internal(
                "前の値なしの描画で前の値の違いを断りました".into(),
            )),
        };
        result.map(|mut r| {
            r.stats.rebased += rebased;
            r.stats.fallbacks += fallbacks;
            r
        })
    }

    /// スポイト（E23）：等倍の計画の出力の 1 画素を、1×1 の範囲を描いて拾う（前の値は使わない）。画像の外・音声は None。
    /// 縮小した段の計画からは拾わない（`Unsupported`）。
    pub fn spoit(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        at: &SpoitAt,
        mon: &dyn Monitor,
    ) -> Result<Option<Sample>, EngineError> {
        if plan.level != 0 {
            return Err(EngineError::Unsupported(
                "スポイトは等倍の計画でだけ拾います（縮小の段からは拾いません）".into(),
            ));
        }
        let req = AreaRequest {
            output: at.output,
            area: Area::rect(at.frame, IRect::new(at.x, at.y, 1, 1)),
        };
        let r = self.render_area(plan, inputs, cache, &req, None, mon)?;
        r.value.sample(at.frame, at.x, at.y)
    }

    /// 範囲を持つノードか（`Op::REGION` で、申告した解析がなく、`reach` が `Global` でない）。
    fn regional(&self, node: &PlannedNode, in_shapes: &[Shape]) -> Result<bool, EngineError> {
        if !node.enabled || !node.op.region() || !node.needs.is_empty() {
            return Ok(false);
        }
        let reach = node
            .op
            .reach(node.params.as_ref(), &op_shapes(node, in_shapes))?;
        Ok(reach != super::region::Reach::Global)
    }

    #[allow(clippy::too_many_arguments)]
    fn area_pass(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        req: &AreaRequest,
        base: Option<&Base<'_>>,
        mon: &dyn Monitor,
        full: bool,
    ) -> Result<AreaRender, PassError> {
        let mut stats = AreaStats::default();
        let fps = self.fingerprints(plan, inputs)?;
        let input_fps: BTreeMap<InputId, Fingerprint> =
            inputs.iter().map(|(k, v)| (*k, v.fingerprint)).collect();
        let source = plan
            .primary
            .and_then(|id| inputs.get(&id))
            .map(|v| v.source)
            .unwrap_or_default();
        let Some((_, from, out_shape, quantize)) = plan.outputs.iter().find(|o| o.0 == req.output)
        else {
            return Err(bad_input(format!("出力 {} はありません", req.output.0)).into());
        };
        let input_values: BTreeMap<InputId, Value> =
            inputs.iter().map(|(k, v)| (*k, v.value.clone())).collect();
        // 量子化した出力は、render・render_cached と同じく量子化の指紋で引き、なければ量子化して入れる（等倍で全体を描いた
        // 音声だけ。そうでなければ量子化だけを当てる）
        let out_fp = fps
            .outputs
            .iter()
            .find(|o| o.0 == req.output)
            .map(|o| o.1)
            .ok_or_else(|| EngineError::Internal("出力の指紋がありません".into()))?;
        let quantized = |v: Value, exact: bool, cache: &mut NodeCache| match (quantize, &v) {
            (Some(q), Value::Audio(a)) if exact => {
                quantized_output(cache, a, q, out_fp, &source, false, mon)
            }
            (Some(q), Value::Audio(a)) => crate::audio::quantize(a, q, &source, mon),
            _ => Ok(v),
        };
        // 入力をそのまま出す出力は、入力の値（全体）
        let PortRef::Node { id: out_node } = from else {
            let v = lookup(&BTreeMap::new(), &input_values, from)
                .ok_or_else(|| EngineError::Internal("出力の値がありません".into()))?;
            return Ok(AreaRender {
                output: req.output,
                value: PartialValue::new(quantized(v, true, cache)?, Origin::Exact),
                warnings: Vec::new(),
                stats,
            });
        };
        // 音声・量子化は範囲の描画を持たないので全体で描く
        let full = full || quantize.is_some() || matches!(out_shape, Shape::Audio { .. });
        let shapes = Shapes::of(plan);
        let index: BTreeMap<NodeId, usize> = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        // 継ぎはぎする出力のノード（外したノードを通り抜けた先の、範囲を持つノード）と、描く範囲（呼び手の範囲は出力の
        // キャンバスに切ってから使う。遠い座標の足し算のあふれと、畳みで覆いが欠けるのを防ぐ）
        let mut target = if full {
            Area::All
        } else {
            clip_to_canvas(&req.area, out_shape)
        };
        let mut patch: Option<Patch> = None;
        if let (Some(b), false) = (base, full) {
            let mut at = *out_node;
            while let Some(n) = index.get(&at).map(|i| &plan.nodes[*i]) {
                match (n.enabled, n.inputs.first()) {
                    (false, Some(PortRef::Node { id })) => at = *id,
                    _ => break,
                }
            }
            if let Some(n) = index.get(&at).map(|i| &plan.nodes[*i])
                && self.regional(n, &shapes.inputs_of(n)?)?
                && b.value.shape() == n.inferred.shape
                && n.inferred.shape == *out_shape
            {
                let (draw, reused) = patch_target(b.value.value(), &target, b.changed)?;
                // 共有できるタイルがなければ継ぎはぎしない（前の値の形式・パレットが今と違いうる）
                if reused > 0 {
                    stats.reused_tiles = reused;
                    target = draw;
                    patch = Some(Patch {
                        node: at,
                        base: invalidate(b.value.value(), b.changed)?,
                    });
                }
            }
        }
        // 1. 後ろから要る範囲を集める。要る範囲が空でも、使い手があるノードは描く（範囲の描画を持つノードは無効なタイル
        //    だけの値を作る。使い手は形・メタデータだけを見る）
        let mut need: BTreeMap<NodeId, Area> = BTreeMap::from([(*out_node, target)]);
        let mut hits: BTreeMap<NodeId, (Value, Vec<Issue>, Vec<Issue>)> = BTreeMap::new();
        let mut regional: BTreeMap<NodeId, Area> = BTreeMap::new();
        for node in plan.nodes.iter().rev() {
            let Some(n_need) = need.get(&node.id).cloned() else {
                continue;
            };
            if node.enabled
                && let Some(fp) = fps.nodes.get(&node.id)
                && let Some(hit) = cache.hit(fp)
            {
                hits.insert(node.id, hit);
                continue;
            }
            let in_shapes = shapes.inputs_of(node)?;
            let mut add = |r: &PortRef, a: &Area| {
                if let PortRef::Node { id } = r {
                    need.entry(*id).or_default().union_with(a);
                }
            };
            if !node.enabled {
                if let Some(r) = node.inputs.first() {
                    add(r, &n_need);
                }
                continue;
            }
            if !full && self.regional(node, &in_shapes)? {
                let rounded = round_to_tiles(&n_need, &node.inferred.shape);
                let reach = node
                    .op
                    .reach(node.params.as_ref(), &op_shapes(node, &in_shapes))?;
                let in_need = reach.backward_in(&rounded, &node.inferred.shape);
                if let Some(r) = node.inputs.first() {
                    add(r, &in_need);
                }
                for r in node.inputs.iter().skip(1) {
                    add(r, &Area::All);
                }
                regional.insert(node.id, rounded);
            } else {
                for r in &node.inputs {
                    add(r, &Area::All);
                }
            }
        }
        // 2. 前から描く
        let mut values: BTreeMap<NodeId, Value> = BTreeMap::new();
        let mut exact: BTreeMap<NodeId, bool> = BTreeMap::new();
        let mut warnings = Vec::new();
        // 前の値のタイルを実際に使ったか（出力のノードがキャッシュに当たれば使わない）
        let mut patched = false;
        for (i, node) in plan.nodes.iter().enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled.into());
            }
            if !need.contains_key(&node.id) {
                stats.skipped += 1;
                continue;
            }
            let (out, is_exact) = if let Some((v, aw, mw)) = hits.remove(&node.id) {
                stats.hits += 1;
                // 当たったノードの描画を前回として覚える（render_cached と同じ。次の差分の適用の前の出力になる）
                if let Some(fp) = fps.nodes.get(&node.id) {
                    let in_fps = node_input_fingerprints(node, &fps.nodes, &input_fps)?;
                    cache.record_last(plan, node, in_fps, &source, *fp, &aw, &mw);
                }
                let mut all = aw;
                all.extend(mw);
                push_warnings(&mut warnings, node.id, all);
                (v, true)
            } else {
                let ins = node_inputs(node, &values, &input_values)?;
                let ins_exact = node.inputs.iter().all(|r| match r {
                    PortRef::Input { .. } => true,
                    PortRef::Node { id } => exact.get(id).copied().unwrap_or(false),
                });
                let base_v = patch
                    .as_ref()
                    .filter(|p| p.node == node.id)
                    .map(|p| &p.base);
                let tiles: Option<FrameTiles> = match regional.get(&node.id) {
                    Some(rounded) if node.enabled => {
                        let geometry = out_geometry(node, &ins)?;
                        let tiles: FrameTiles = geometry
                            .iter()
                            .map(|(f, o, w, h)| {
                                let t = match rounded.frame_rects(*f) {
                                    Some(r) => Tiles::covering(*w, *h, TILE, &r, *o),
                                    None => Tiles::none(*w, *h, TILE),
                                };
                                (*f, t)
                            })
                            .collect();
                        // すべてのタイルが要り、入力がどれも全体の値なら、全体の描画と同じなので全体で描く（等倍の値として
                        // キャッシュに入れられる）
                        let whole = base_v.is_none()
                            && tiles.values().all(Tiles::is_full)
                            && ins.iter().all(Value::is_complete);
                        (!whole).then_some(tiles)
                    }
                    _ => None,
                };
                if !node.enabled {
                    (pass_through(&ins)?, ins_exact)
                } else if let Some(tiles) = tiles {
                    stats.drawn_tiles += tiles.values().map(|t| t.count() as u64).sum::<u64>();
                    patched |= base_v.is_some();
                    let run = self.run_region(plan, node, &ins, &tiles, base_v, &source, mon);
                    // 申告した依存の外（無効なタイル）を読んだら、範囲の描画を捨てる
                    if ins.iter().any(holes_read) {
                        return Err(PassError::Hole);
                    }
                    let (v, aw, mw) = match run {
                        Ok(r) => r,
                        // 継ぎはぎの前の値の大きさ・形式・パレットが違う（範囲の描画の部品が内部の誤りで断る）：前の値なしで
                        // 描き直す（render_area）。前の値なしでも同じ誤りなら、そのとき返す
                        Err(EngineError::Internal(_)) if base_v.is_some() => {
                            return Err(PassError::Rebase);
                        }
                        Err(e) => return Err(e.at(node.id).into()),
                    };
                    stats.region_nodes += 1;
                    let mut all = aw;
                    all.extend(mw);
                    push_warnings(&mut warnings, node.id, all);
                    (v, false)
                } else {
                    // 全体で描くノードの入力は、どれも全体の値であること（範囲の値なら全体で描き直す）
                    if !ins.iter().all(Value::is_complete) {
                        return Err(PassError::Hole);
                    }
                    let run = match fps.nodes.get(&node.id).copied().filter(|_| ins_exact) {
                        // 入力がどれも等倍で全体を描いた値なら、render_cached の外れの道と同じ（継ぎ目の縮小の差分・差分の
                        // 適用・解析のキャッシュ・由来 Exact で入れる）。描画を前回として覚える
                        Some(fp) => {
                            let in_fps = node_input_fingerprints(node, &fps.nodes, &input_fps)?;
                            let run = self.draw_miss(
                                plan,
                                node,
                                &ins,
                                &in_fps,
                                &source,
                                fp,
                                cache,
                                &RenderOptions::default(),
                                mon,
                            )?;
                            cache.record_last(
                                plan,
                                node,
                                in_fps,
                                &source,
                                fp,
                                &run.apply_warnings,
                                &run.map_warnings,
                            );
                            run
                        }
                        None => self.run_node(
                            plan,
                            node,
                            &ins,
                            &source,
                            mon,
                            &mut |need, v, a, acx| a.run_any(&need.params, v, acx),
                        )?,
                    };
                    stats.full_nodes += 1;
                    let mut all = run.apply_warnings;
                    all.extend(run.map_warnings);
                    push_warnings(&mut warnings, node.id, all);
                    (run.value, ins_exact)
                }
            };
            check_node_shape(node, &out)?;
            values.insert(node.id, out);
            exact.insert(node.id, is_exact);
            for id in plan.released_after(i) {
                if *id != *out_node {
                    values.remove(id);
                }
            }
        }
        let value = values
            .remove(out_node)
            .ok_or_else(|| EngineError::Internal("出力の値がありません".into()))?;
        let out_exact = exact.get(out_node).copied().unwrap_or(false) && value.is_complete();
        let value = quantized(value, out_exact, cache)?;
        let origin = if plan.level != 0 {
            // 縮小の段の値は近似（保存・解析・照合に使わない）
            Origin::Scaled
        } else if out_exact {
            Origin::Exact
        } else if patched {
            Origin::Patched
        } else {
            Origin::Partial
        };
        if !patched {
            stats.reused_tiles = 0;
        }
        Ok(AreaRender {
            output: req.output,
            value: PartialValue::new(value, origin),
            warnings,
            stats,
        })
    }

    /// 範囲を持つノード 1 つを、`tiles` のタイルだけ描く（全体の描画と同じ `apply_node` を通る：持ち上げ・写し方の確かめ・
    /// メタデータの写しは同じで、操作の呼び方だけが `apply_region`。持ち上げたときはフレームごとのタイルと前の値のフレーム）。
    /// 返すのは値と、操作の警告と、写したときの警告。
    #[allow(clippy::too_many_arguments)]
    fn run_region(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        source: &Digest,
        mon: &dyn Monitor,
    ) -> Result<(Value, Vec<Issue>, Vec<Issue>), EngineError> {
        let mut cx = ApplyCx::new(mon, &self.limits, node_seed(node, source), plan.rules);
        let none = || {
            EngineError::Internal(format!(
                "{} は範囲の描画を持つと申告しましたが、apply_region が None を返しました",
                node.op.id()
            ))
        };
        let params = node.params.as_ref();
        let (value, map_warnings) =
            apply_node(plan.rules, node, ins, &mut cx, &mut |refs, frame, cx| {
                let Some(i) = frame else {
                    return node
                        .op
                        .apply_region(params, refs, tiles, base, cx)
                        .ok_or_else(none)?;
                };
                // 持ち上げた操作：フレーム i のタイルと、前の値のフレーム i（入力のフレームの列のメタデータを持った 1 枚の画像）
                let Some(Value::Frames(frames)) = ins.first() else {
                    return Err(EngineError::Internal(
                        "持ち上げの入力がフレームの列ではありません".into(),
                    ));
                };
                let frame_base = match base {
                    Some(Value::Frames(b)) if b.frames().len() == frames.frames().len() => {
                        b.frames().get(i).map(|bf| {
                            Value::Image(Arc::new(Image {
                                px: bf.px.clone(),
                                meta: frames.meta.clone(),
                            }))
                        })
                    }
                    Some(_) => {
                        return Err(EngineError::Internal(
                            "範囲の描画の前の値の形が違います".into(),
                        ));
                    }
                    None => None,
                };
                let n = u32::try_from(i).unwrap_or(u32::MAX);
                let frame_tiles: FrameTiles = tiles
                    .get(&n)
                    .map(|t| BTreeMap::from([(0, t.clone())]))
                    .unwrap_or_default();
                node.op
                    .apply_region(params, refs, &frame_tiles, frame_base.as_ref(), cx)
                    .ok_or_else(none)?
            })?;
        Ok((value, cx.warnings, map_warnings))
    }
}

/// 値の画素のどこかで、無効なタイルを読んだことがあれば true。
fn holes_read(v: &Value) -> bool {
    frames_of(v).iter().any(|(_, px, _)| px.holes_read())
}

/// 計画の値の形（入力とノード）。
struct Shapes {
    inputs: BTreeMap<InputId, Shape>,
    nodes: BTreeMap<NodeId, Shape>,
}

impl Shapes {
    fn of(plan: &Plan) -> Self {
        Self {
            inputs: plan.inputs.clone(),
            nodes: plan
                .nodes
                .iter()
                .map(|n| (n.id, n.inferred.shape))
                .collect(),
        }
    }

    /// ノードの入力の形（計画の順）。
    fn inputs_of(&self, node: &PlannedNode) -> Result<Vec<Shape>, EngineError> {
        node.inputs
            .iter()
            .map(|r| lookup(&self.nodes, &self.inputs, r))
            .collect::<Option<_>>()
            .ok_or_else(|| EngineError::Internal("ノードの入力の形がありません".into()))
    }
}

/// 要る範囲を、出力の値のタイル（[`TILE`]）の格子に広げる（範囲の描画はタイルごとに描くので、入力に要る範囲はタイルの
/// 矩形から逆写しする）。1 枚の画像とキャンバスいっぱいのフレームの列は格子が 0 から始まるので大きさを知らなくてよい。
/// トリムしたフレームの列（位置が値にしかない）は、そのフレームの全体にする（段 10 はフレーム単位の全体まで）。
fn round_to_tiles(area: &Area, shape: &Shape) -> Area {
    let Area::Pixels(map) = area else {
        return area.clone();
    };
    let aligned = matches!(
        shape,
        Shape::Image { .. }
            | Shape::Frames {
                full: Fullness::Full,
                ..
            }
    );
    let t = i64::from(TILE);
    let out = map
        .iter()
        .map(|(f, rects)| {
            if rects.is_whole() || !aligned {
                return (*f, Rects::whole());
            }
            let mut r = Rects::new();
            for q in rects.rects() {
                let x0 = q.x.max(0).div_euclid(t) * t;
                let y0 = q.y.max(0).div_euclid(t) * t;
                let x1 = q.right().max(0).saturating_add(t - 1).div_euclid(t) * t;
                let y1 = q.bottom().max(0).saturating_add(t - 1).div_euclid(t) * t;
                if x0 < x1 && y0 < y1 {
                    r.push(IRect::new(
                        x0,
                        y0,
                        u32::try_from(x1 - x0).unwrap_or(u32::MAX),
                        u32::try_from(y1 - y0).unwrap_or(u32::MAX),
                    ));
                }
            }
            (*f, r)
        })
        .collect();
    Area::pixels(out)
}

/// 呼び手の範囲を、出力の形のキャンバスに切る：画像・フレームの列は [0, 幅) × [0, 高さ) とフレームの数（大きさの決まって
/// いない軸は上限で切る。フレームはキャンバスからはみ出さない）、音声は長さまで。フレーム全体・`All` はそのまま。
fn clip_to_canvas(area: &Area, shape: &Shape) -> Area {
    let side = |d: crate::shape::Dim| u32::try_from(d.max()).unwrap_or(u32::MAX);
    match (area, shape) {
        (Area::Pixels(map), Shape::Image { w, h, .. } | Shape::Frames { w, h, .. }) => {
            let count = match shape {
                Shape::Frames { count, .. } => count.max(),
                _ => 1,
            };
            let canvas = IRect::new(0, 0, side(*w), side(*h));
            Area::pixels(
                map.iter()
                    .filter(|(f, _)| u64::from(**f) < count)
                    .map(|(f, rects)| {
                        if rects.is_whole() {
                            return (*f, Rects::whole());
                        }
                        let mut out = Rects::new();
                        for r in rects.clipped(&canvas) {
                            out.push(r);
                        }
                        (*f, out)
                    })
                    .collect(),
            )
        }
        (Area::Samples(spans), Shape::Audio { frames, .. }) => {
            let n = frames.max();
            let mut out = Spans::new();
            for (a, b) in spans.spans() {
                out.push((*a).min(n), (*b).min(n));
            }
            Area::samples(out)
        }
        _ => area.clone(),
    }
}

/// 範囲を持つノードの出力の、フレームごとの（番号, キャンバスの中の位置, 幅, 高さ）。画素ごとに依る操作（`Reach::Same`）は
/// 入力の値と同じ形、ほかは推論した形（大きさが決まっている。決まっていなければ `reach` が `Global`）。
fn out_geometry(
    node: &PlannedNode,
    ins: &[Value],
) -> Result<Vec<(u32, IPoint, u32, u32)>, EngineError> {
    let origin = IPoint { x: 0, y: 0 };
    let exact = |d: crate::shape::Dim| d.exact().and_then(|v| u32::try_from(v).ok());
    match node.inferred.shape {
        Shape::Image { w, h, .. } => match (exact(w), exact(h)) {
            (Some(w), Some(h)) => Ok(vec![(0, origin, w, h)]),
            _ => Ok(frames_of(ins.first().ok_or_else(missing)?)
                .into_iter()
                .map(|(i, px, o)| (i, o, px.width(), px.height()))
                .collect()),
        },
        Shape::Frames { .. } if node.lifted => {
            let (Shape::Frames { count, w, h, .. }, Some(first)) =
                (node.inferred.shape, ins.first())
            else {
                return Err(missing());
            };
            let n = match first {
                Value::Frames(fr) => fr.frames().len(),
                _ => 0,
            };
            let (Some(w), Some(h)) = (exact(w), exact(h)) else {
                return Err(EngineError::Internal(
                    "持ち上げた範囲の描画の大きさが決まっていません".into(),
                ));
            };
            let _ = count;
            Ok((0..n)
                .map(|i| (u32::try_from(i).unwrap_or(u32::MAX), origin, w, h))
                .collect())
        }
        // 持ち上げないフレームの列の操作は、画素ごとに依るもの（形・位置は入力のまま）
        Shape::Frames { .. } => Ok(frames_of(ins.first().ok_or_else(missing)?)
            .into_iter()
            .map(|(i, px, o)| (i, o, px.width(), px.height()))
            .collect()),
        Shape::Audio { .. } => Err(EngineError::Internal(
            "音声には範囲の描画を当てられません".into(),
        )),
    }
}

fn missing() -> EngineError {
    EngineError::Internal("範囲の描画の入力がありません".into())
}

/// 継ぎはぎで描く範囲：要る範囲のタイルのうち、前の値で無効か、変わりうる範囲に掛かるもの。返すのは (描く範囲, 前の値から
/// 共有するタイルの数)。
fn patch_target(base: &Value, need: &Area, changed: &Area) -> Result<(Area, u64), EngineError> {
    let need_tiles = tiles_in(base, need, TILE);
    let changed_tiles = tiles_in(base, changed, TILE);
    let mut out = BTreeMap::new();
    let mut reused = 0u64;
    for (f, px, origin) in frames_of(base) {
        let (Some(n), Some(c)) = (need_tiles.get(&f), changed_tiles.get(&f)) else {
            continue;
        };
        let mut keep = px.valid_tiles();
        keep.subtract(c)?;
        keep.intersect_with(n)?;
        reused += keep.count() as u64;
        let mut draw = n.clone();
        draw.subtract(&keep)?;
        out.insert(f, draw.to_rects(origin));
    }
    Ok((Area::pixels(out), reused))
}
