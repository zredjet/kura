//! ベンチの 2 つの経路（E30・NFR-11・12・16。docs/editing.md §12）。
//!
//! - エディタの経路（[`EditorPath`]）：`EditSession` の手 → `Engine::compile_snapshot`（前の計画から差分で）→ 描画。描画は 2 つ：
//!   - 表示の経路（[`EditorPath::render_view`]・[`EditorPath::render_preview`]・[`EditorPath::show`]。NFR-11・12 で測るもの）：
//!     段の計画（`Engine::plan_level`。段 0 は今の計画）→ `Engine::render_view`（前回の表示から変わった範囲を推論し、変わった
//!     タイルだけを描き直して継ぎはぎする。変わったタイルの一覧を作る）。等倍の全体の値は作らない。
//!   - 等倍の全体（[`EditorPath::render`]・[`EditorPath::step`]・[`EditorPath::digests`]）：`Engine::render_cached`（キャッシュ・
//!     差分の適用・変わった範囲）。NFR-16 の照合（ダイジェスト）に使う。
//!
//!   M12 のエディタと同じ流れで、予算の確保（`admit_cached`）は呼び手が手の適用（[`EditorPath::apply`]）・段の計画
//!   （[`EditorPath::plan_view`]）と描画の間に挟む（kura-core の `EngineRuntime::admit_preview` はプールの外で呼ぶため、段を
//!   分けてある）。
//! - 一括の経路（[`bulk_render`]）：正規の JSON → `Engine::load` → `compile` → `render`（キャッシュなし）。E4・NFR-16 の比べる相手。
//!
//! スレッドのプールは呼び手が作って `install` する（エンジンはプールを作らない）。時計も呼び手が渡す（`super::measure`）。

use std::collections::BTreeMap;

use crate::digest::Digest;
use crate::doc::{Graph, InputId, NodeId, OutputId, OutputSpec, PortRef};
use crate::error::EngineError;
use crate::monitor::{Monitor, NoMonitor};
use crate::render::cache::{CachedRender, FingerprintedInput, NodeCache, RenderOptions};
use crate::render::preview::{LevelFallback, LevelPlan};
use crate::render::view::{View, ViewRender, ViewRequest};
use crate::render::{Engine, InputValue, Plan, Rendered};
use crate::session::{CommitMode, Edit, EditSession, HistoryLimits, Rev, Snapshot};
use crate::shape::Shape;
use crate::value::Value;

/// エディタの手 1 つ。
#[derive(Debug, Clone)]
pub enum Step {
    /// 仮の値（スライダーを動かしている間・ドラッグの途中）。
    Preview(Edit),
    /// 確定する。
    Commit(Edit, CommitMode),
    /// 仮の値を確定する。
    CommitPreview(CommitMode),
    /// 1 手戻す。
    Undo,
    /// 1 手進める。
    Redo,
}

/// 表示の経路で描いた結果（[`EditorPath::render_view`]・[`EditorPath::render_preview`]）。
#[derive(Debug)]
pub struct LevelRender {
    /// 実際に描いた段（段の計画にできなければ 0）。
    pub level: u8,
    /// 段 0 に落ちた理由（段の計画にできたか、段 0 を求めたときは None）。
    pub fallback: Option<LevelFallback>,
    /// 表示の経路の結果（表示の値・変わったタイルの一覧・キャッシュと範囲の描画の数）。
    pub rendered: ViewRender,
}

/// エディタの経路の状態：セッション・入力・キャッシュ・今の計画・表示の計画と前回の表示。
pub struct EditorPath {
    session: EditSession,
    inputs: BTreeMap<InputId, FingerprintedInput>,
    shapes: BTreeMap<InputId, Shape>,
    cache: NodeCache,
    plan: Plan,
    focus: Option<NodeId>,
    /// 表示する出力（文書の最初の出力）。
    output: OutputId,
    /// 表示する段の計画（[`EditorPath::plan_view`]。手を当てたら捨てる）。
    view_plan: Option<LevelPlan>,
    /// 前回の表示（表示の経路の前の値）。
    view: Option<View>,
}

impl EditorPath {
    /// 文書を開く：セッションを作り、入力をキャッシュの外の値として数え（`register_external`）、最初の計画を作る（描かない）。
    /// `capacity` はキャッシュの上限、`focus` は編集中のノード（入力と出力を固定する）。
    pub fn open(
        engine: &Engine,
        graph: &Graph,
        inputs: BTreeMap<InputId, FingerprintedInput>,
        capacity: u64,
        focus: Option<NodeId>,
    ) -> Result<Self, EngineError> {
        let base = inputs.values().next().map(|i| i.source).unwrap_or_default();
        let session = EditSession::open(engine, graph, base, HistoryLimits::default())?;
        let shapes: BTreeMap<InputId, Shape> =
            inputs.iter().map(|(k, v)| (*k, v.value.shape())).collect();
        let mut cache = NodeCache::new(capacity);
        for v in inputs.values() {
            cache.register_external(&v.value);
        }
        let plan = engine.compile_snapshot(session.current(), &shapes, None)?;
        let output = graph
            .outputs
            .first()
            .map(|o| o.id)
            .ok_or_else(|| EngineError::Internal("出力のない文書です".into()))?;
        Ok(Self {
            session,
            inputs,
            shapes,
            cache,
            plan,
            focus,
            output,
            view_plan: None,
            view: None,
        })
    }

    /// 手を当てて、今の文書の計画を前の計画から差分で作る（描かない）。戻った・進んだ先、確定した・仮の値の番号を返す。
    /// 戻せない・進めないときは `Invalid` ではなく `Internal`（ベンチの手順の誤り）。
    pub fn apply(&mut self, engine: &Engine, step: Step) -> Result<Rev, EngineError> {
        let rev = match step {
            Step::Preview(edit) => self.session.preview(engine, edit)?,
            Step::Commit(edit, mode) => self.session.commit(engine, edit, mode)?.rev,
            Step::CommitPreview(mode) => self.session.commit_preview(mode)?.rev,
            Step::Undo => self
                .session
                .undo()
                .ok_or_else(|| EngineError::Internal("戻せる手がありません".into()))?,
            Step::Redo => self
                .session
                .redo()
                .ok_or_else(|| EngineError::Internal("進める手がありません".into()))?,
        };
        self.plan =
            engine.compile_snapshot(self.session.current(), &self.shapes, Some(&self.plan))?;
        self.view_plan = None;
        Ok(rev)
    }

    /// 今の計画をキャッシュを引きながら描く（変わった範囲の一覧を含む）。`digests` はダイジェストを取るか（プレビューでは
    /// 取らない。照合で取る）。
    pub fn render(
        &mut self,
        engine: &Engine,
        digests: bool,
        mon: &dyn Monitor,
    ) -> Result<CachedRender, EngineError> {
        let opts = RenderOptions {
            digests,
            focus: self.focus,
            verify_hits: false,
        };
        engine.render_cached(&self.plan, &self.inputs, &mut self.cache, &opts, mon)
    }

    /// 手を当てて描く（プレビュー：ダイジェストなし）。
    pub fn step(&mut self, engine: &Engine, step: Step) -> Result<CachedRender, EngineError> {
        self.apply(engine, step)?;
        self.render(engine, false, &NoMonitor)
    }

    /// 表示する段 `level` の計画を作る（`Engine::plan_level`。段 0 は今の計画のまま）。描く前の予算の確保
    /// （[`EditorPath::view_parts`]）と表示の経路（[`EditorPath::render_view`]）が使う。実際に描く段を返す（段の計画に
    /// できなければ 0）。
    pub fn plan_view(&mut self, engine: &Engine, level: u8) -> Result<u8, EngineError> {
        let lp = engine.plan_level(&self.plan, level)?;
        let actual = lp.plan.level();
        self.view_plan = Some(lp);
        Ok(actual)
    }

    /// 表示の経路で描く：[`EditorPath::plan_view`] の計画（なければ今の計画の段 0）を、前回の表示から変わったタイルだけ
    /// 描き直す（`Engine::render_view`。出力は文書の最初の出力の全体、focus は開いたときのノード）。描けたら前回の表示を
    /// 入れ替える。
    pub fn render_view(
        &mut self,
        engine: &Engine,
        mon: &dyn Monitor,
    ) -> Result<LevelRender, EngineError> {
        if self.view_plan.is_none() {
            self.plan_view(engine, 0)?;
        }
        let Some(lp) = &self.view_plan else {
            return Err(EngineError::Internal("表示の計画がありません".into()));
        };
        let req = ViewRequest::whole(self.output, self.focus);
        let rendered = engine.render_view(
            &lp.plan,
            &self.inputs,
            &mut self.cache,
            &req,
            self.view.as_ref(),
            mon,
        )?;
        self.view = Some(rendered.view.clone());
        Ok(LevelRender {
            level: rendered.level(),
            fallback: lp.fallback.clone(),
            rendered,
        })
    }

    /// プレビューの段 `level` で、表示の経路で描く（[`EditorPath::plan_view`] → [`EditorPath::render_view`]。NFR-11 の写真は
    /// 段 [`super::cases::PHOTO_LEVEL`]）。返す [`LevelRender::level`] が実際に描いた段で、呼び手は `level` と違えば
    /// [`super::PHOTO_FALLBACK`] を表に出す。
    pub fn render_preview(
        &mut self,
        engine: &Engine,
        level: u8,
    ) -> Result<LevelRender, EngineError> {
        self.plan_view(engine, level)?;
        self.render_view(engine, &NoMonitor)
    }

    /// 手を当てて、段 `level` で表示の経路で描く（[`EditorPath::apply`] → [`EditorPath::render_preview`]）。
    pub fn show(
        &mut self,
        engine: &Engine,
        step: Step,
        level: u8,
    ) -> Result<LevelRender, EngineError> {
        self.apply(engine, step)?;
        self.render_preview(engine, level)
    }

    /// 前回の表示（表示の経路で描いたことがなければ None）。
    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }

    /// 今の文書の出力のダイジェスト（出力の順。キャッシュの値を使い、ダイジェストだけ取る）。
    pub fn digests(&mut self, engine: &Engine) -> Result<Vec<Digest>, EngineError> {
        self.render(engine, true, &NoMonitor)?
            .outputs
            .iter()
            .map(|o| {
                o.digest
                    .ok_or_else(|| EngineError::Internal("ダイジェストを取っていません".into()))
            })
            .collect()
    }

    /// 表示の経路の予算の確保（`Engine::admit_cached`）に渡すもの：表示する計画（[`EditorPath::plan_view`] の。なければ今の
    /// 計画）・入力・キャッシュ。
    pub fn view_parts(
        &mut self,
    ) -> (
        &Plan,
        &BTreeMap<InputId, FingerprintedInput>,
        &mut NodeCache,
    ) {
        let plan = self.view_plan.as_ref().map_or(&self.plan, |lp| &lp.plan);
        (plan, &self.inputs, &mut self.cache)
    }

    /// 等倍の全体の描画の予算の確保（`Engine::admit_cached`）に渡すもの：今の計画・入力・キャッシュ。
    pub fn parts(
        &mut self,
    ) -> (
        &Plan,
        &BTreeMap<InputId, FingerprintedInput>,
        &mut NodeCache,
    ) {
        (&self.plan, &self.inputs, &mut self.cache)
    }

    /// 今の計画。
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// セッション。
    pub fn session(&self) -> &EditSession {
        &self.session
    }

    /// キャッシュ。
    pub fn cache(&self) -> &NodeCache {
        &self.cache
    }

    /// キャッシュ（確保を描いたあとに戻す `CacheLease::settle` に渡す）。
    pub fn cache_mut(&mut self) -> &mut NodeCache {
        &mut self.cache
    }

    /// 入力。
    pub fn inputs(&self) -> &BTreeMap<InputId, FingerprintedInput> {
        &self.inputs
    }

    /// 今の文書（仮の値があれば仮の値）。
    pub fn snapshot(&self) -> &Snapshot {
        self.session.current()
    }

    /// 今の文書の正規の JSON（一括の経路に渡す保存の形）。
    pub fn document(&self) -> Result<Vec<u8>, EngineError> {
        self.session.current().to_canonical_json()
    }
}

/// 入力 0 だけの、指紋の付いた入力（値のダイジェストを指紋にする）。
pub fn single_input(
    value: Value,
    source: Digest,
) -> Result<BTreeMap<InputId, FingerprintedInput>, EngineError> {
    Ok(BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(value, source)?,
    )]))
}

/// 指紋の付いた入力から、キャッシュなしの道（`Engine::render`）に渡す入力を作る（値は共有する）。
pub fn plain_inputs(
    inputs: &BTreeMap<InputId, FingerprintedInput>,
) -> BTreeMap<InputId, InputValue> {
    inputs
        .iter()
        .map(|(k, v)| {
            (
                *k,
                InputValue {
                    value: v.value.clone(),
                    source: v.source,
                },
            )
        })
        .collect()
}

/// 一括の経路：正規の JSON の文書を `engine`（エディタとは別のエンジンでよい）で読み直し、キャッシュなしで描く（出力の
/// ダイジェストを含む）。
pub fn bulk_render(
    engine: &Engine,
    document: &[u8],
    inputs: &BTreeMap<InputId, InputValue>,
    mon: &dyn Monitor,
) -> Result<Rendered, EngineError> {
    let graph = engine.load(document)?;
    let shapes: BTreeMap<InputId, Shape> =
        inputs.iter().map(|(k, v)| (*k, v.value.shape())).collect();
    let plan = engine.compile(&graph, &shapes)?;
    engine.render(&plan, inputs, mon)
}

/// 値の集まりが実際に持つバイト数（共有した実体は 1 度だけ。`NodeCache` の数え方）。
fn unique_bytes<'a>(values: impl IntoIterator<Item = &'a Value>) -> u64 {
    let mut counter = NodeCache::new(u64::MAX);
    for v in values {
        counter.register_external(v);
    }
    counter.bytes()
}

/// 描いた値で数えた、描く間に生きている値のバイト数の最大（入力を含む。共有した実体は 1 度だけ。NFR-17 の「見積もりが
/// 実際以上」の実際）。すべてのノードを出力にした文書を描いて値を得て（値は出力の数に依らない）、元の文書の計画の最後の
/// 使い手の表の時点で落とす。作業領域・読み書きは数えない（それは bench edit が子プロセスのピークで測る）。
pub fn live_peak(
    engine: &Engine,
    graph: &Graph,
    inputs: &BTreeMap<InputId, InputValue>,
) -> Result<u64, EngineError> {
    let template = graph
        .outputs
        .first()
        .cloned()
        .ok_or_else(|| EngineError::Internal("出力のない文書です".into()))?;
    let mut all = graph.clone();
    all.outputs = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(k, n)| {
            let id = u16::try_from(k)
                .map_err(|_| EngineError::Limit("ノードが多すぎて数えられません".into()))?;
            Ok(OutputSpec {
                id: OutputId(id),
                from: PortRef::Node { id: n.id },
                quantize: None,
                ..template.clone()
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    let shapes: BTreeMap<InputId, Shape> =
        inputs.iter().map(|(k, v)| (*k, v.value.shape())).collect();
    let plan = engine.compile(graph, &shapes)?;
    let all_plan = engine.compile(&all, &shapes)?;
    let rendered = engine.render(&all_plan, inputs, &NoMonitor)?;
    let values: BTreeMap<NodeId, Value> = graph
        .nodes
        .iter()
        .zip(rendered.outputs)
        .map(|(n, o)| (n.id, o.value))
        .collect();
    let input_values: Vec<Value> = inputs.values().map(|v| v.value.clone()).collect();
    let mut peak = unique_bytes(&input_values);
    let mut live: BTreeMap<NodeId, Value> = BTreeMap::new();
    for (i, n) in graph.nodes.iter().enumerate() {
        if let Some(v) = values.get(&n.id) {
            live.insert(n.id, v.clone());
        }
        peak = peak.max(unique_bytes(input_values.iter().chain(live.values())));
        for id in plan.released_after(i) {
            live.remove(id);
        }
    }
    Ok(peak)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::render::delta::OutputChange;
    use crate::testgen::InputGen;
    use crate::testing::single_node_graph;
    use crate::value::PixelFormat;

    fn noise(w: u32, h: u32) -> Value {
        InputGen::Noise {
            w,
            h,
            fmt: PixelFormat::Rgba8,
            seed: 3,
        }
        .generate()
        .unwrap()
    }

    fn bulk_digests(doc: &[u8], inputs: &BTreeMap<InputId, InputValue>) -> Vec<Digest> {
        let engine = Engine::new().unwrap();
        bulk_render(&engine, doc, inputs, &NoMonitor)
            .unwrap()
            .outputs
            .iter()
            .map(|o| o.digest)
            .collect()
    }

    /// 仮の値・確定・取り消し・やり直しのどの手のあとも、エディタの経路のダイジェストが一括の経路と同じ。番号はセッションの
    /// とおりで、戻せない手は `Internal`。
    #[test]
    fn editor_path_follows_the_session_and_matches_bulk() {
        let engine = Engine::new().unwrap();
        let g = single_node_graph(
            "image.canvas",
            1,
            json!({ "rect": { "x": 0, "y": 0, "w": 40, "h": 30 }, "fill": null }),
        )
        .unwrap();
        let node = g.nodes[0].id;
        let inputs = single_input(noise(40, 30), Digest([1; 32])).unwrap();
        let mut ed = EditorPath::open(&engine, &g, inputs, 1 << 20, Some(node)).unwrap();
        assert!(matches!(
            ed.apply(&engine, Step::Undo),
            Err(EngineError::Internal(_))
        ));
        let rect = |w: u32| Edit::SetParam {
            node,
            key: "rect".into(),
            value: json!({ "x": 0, "y": 0, "w": w, "h": 30 }),
        };
        let steps = [
            Step::Preview(rect(41)),
            Step::Preview(rect(42)),
            Step::CommitPreview(CommitMode::Step),
            Step::Commit(rect(50), CommitMode::Step),
            Step::Undo,
            Step::Redo,
            Step::Undo,
        ];
        let widths = [41, 42, 42, 50, 42, 50, 42];
        let plain = plain_inputs(ed.inputs());
        for (step, w) in steps.into_iter().zip(widths) {
            let out = ed.step(&engine, step).unwrap();
            assert_eq!(out.outputs[0].value.shape(), noise(w, 30).shape());
            let got = ed.digests(&engine).unwrap();
            assert_eq!(got, bulk_digests(&ed.document().unwrap(), &plain));
        }
        assert_eq!(ed.session().history_len(), 3);
        // 入力はキャッシュの外の値として数えている
        assert!(ed.cache().bytes() >= 40 * 30 * 4);
        // 表示の経路：段 2 の計画で描き、段 0 に戻すと全体を描き直す。予算の確保に渡すのは表示する計画
        let lv = ed.render_preview(&engine, 2).unwrap();
        assert_eq!((lv.level, lv.fallback.clone()), (2, None));
        let (plan, inputs, cache) = ed.view_parts();
        assert_eq!((plan.level(), inputs.len()), (2, 1));
        assert!(cache.capacity() > 0);
        let (plan, _, _) = ed.parts();
        assert_eq!(plan.level(), 0);
        let lv = ed.render_preview(&engine, 0).unwrap();
        assert_eq!(lv.level, 0);
        assert_eq!(lv.rendered.changed, OutputChange::All);
        let shown = ed.view().unwrap().value().exact().cloned().unwrap();
        let full = ed.render(&engine, false, &NoMonitor).unwrap();
        assert_eq!(
            shown.digest().unwrap(),
            full.outputs[0].value.digest().unwrap()
        );
        // 手を当てると段の計画を捨てる（表示の経路は等倍に戻る）
        let lv = ed.show(&engine, Step::Redo, 0).unwrap();
        assert_eq!(lv.level, 0);
        assert_eq!(ed.view_parts().0.level(), 0);
    }

    /// 生きている値の最大：反転の鎖は入力・前の値・今の値の 3 つ分（使い終えた値は落とす）。値を共有する操作は数え直さない。
    #[test]
    fn live_peak_counts_live_values_once() {
        let engine = Engine::new().unwrap();
        let mut g = Graph::new_single();
        for _ in 0..4 {
            g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
                .unwrap();
        }
        let input = noise(64, 64);
        let one = 64 * 64 * 4;
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input.clone(),
                source: Digest::default(),
            },
        )]);
        assert_eq!(live_peak(&engine, &g, &inputs).unwrap(), 3 * one);
        // 格子を書くだけの操作はタイルを入力と共有する
        let g =
            single_node_graph("meta.set_pivot", 1, json!({ "pivot": { "x": 1, "y": 2 } })).unwrap();
        assert_eq!(live_peak(&engine, &g, &inputs).unwrap(), one);
    }
}
