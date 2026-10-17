//! E26（docs/editing.md §17）：解析と操作が一致する。2 段で確かめる。
//!
//! 1. **構造の網**（登録したすべての操作の例に自動で当たる。E18 と同じ形）
//!    - 操作が申告した解析（§11.2 の口）を計画の申告どおりに解き、実行器が渡した値と `Engine::analyze_dyn` で別に
//!      求めた値が正規の形で一致する。`NEEDS` の（ID, 版）はどれも、どれかの例の計画に載る（載らなければ失敗）。
//!      `NEEDS` のある操作で計画が空になる例は、理由つきの許可の一覧 `EMPTY_PLAN` に載せる（一覧にない例の計画が空なら
//!      失敗。一覧にあるのに計画が空でない例・ない例も失敗）。引数によって申告をやめて自分で計る形を、例ごとに見逃さない。
//!    - 申告した値を使っている（自分で計り直していない）：偽の値を渡すと出力が変わる。偽の値は結果の欄ごとに作る
//!      （分類ごとのファイルの `FAKES`。JSON の結果は欄の一覧が結果の欄と同じであること。バイナリの結果は型付きの値を
//!      変える）。各例で、対の行の `reads`（操作が読む欄）のどれか（かすべて）を変えた偽の値で出力が変わる。`reads` の
//!      欄はどれも、どれかの例で出力を変え、`reads` にない欄はどの例でも出力を変えない。描画のエラーは、操作の apply
//!      から来たもの（値を読んだ証拠）として数え、Solver が解けない・偽の値が読めない・偽の値が元と同じはテストの誤り
//!      （panic）にする。
//!    - 音声の操作は、核（`Op::audio_kernel`）の経路も `KernelOf` で包んで実行器に載せ、記録と偽の値で同じ照合をする
//!      （apply の経路と同じ申告を同じ値で解き、同じ偽の値で同じ出力になる）。
//!    - `src/ops/**` が、申告で受け取るべき計算（解析の `run`・`analysis::` のモジュールの関数・エンジンの解析の口・
//!      `imaging::bounds` の不透明な範囲）を直接呼ばない（ソースの走査。`analysis` の別名も断る。部品として使ってよい
//!      ものは理由つきの許可の一覧 `DIRECT_OK`）。
//! 2. **意味の対の表**：操作と解析の意味の一致（トリムの矩形＝不透明な範囲、正規化の利得＝計測から解いた利得 など）を、
//!    対ごとの検査で確かめる。行は（操作 ID, 版, 解析 ID, 版）。検査は行を受け取り、行の版の操作を当て、解析は登録表の
//!    （ID, 版）で引く（型の名前では引かない）。覆いの検査：申告のある（操作, 版, 解析, 版）はすべて対の表に載る（旧版の
//!    操作を含む）。表にある（操作, 解析）は、操作の今の版と解析の今の版の行を持つ（解析の版を上げて旧版を legacy に
//!    残しても、今の版どうしの意味の一致を確かめる）。登録したすべての解析は、対の表か「対のない解析」の表に載る。
//!
//! 対の表・対のない解析の表・偽の値の作り方は分類ごとのファイル（`tests/analysis/pairs_image.rs`・`pairs_audio.rs`）に
//! 置く。解析を足す班は、自分の分類のファイルに行を足す。「対のない解析」は、どの操作とも意味の対を持たない解析（波形の
//! 段など）で、版の契約（§8.4）の「表示だけ」（どの操作も申告しない解析）とは別の分け方。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "analysis/pairs_audio.rs"]
mod pairs_audio;
#[path = "analysis/pairs_image.rs"]
mod pairs_image;

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kura_engine::analysis::{
    Analysis, AnalysisOutput, AnalysisRequest, AnalyzeCx, AnyOutput, DynAnalysis, Encoding, Need,
    Solve, Solver,
};
use kura_engine::audio::render_all;
use kura_engine::doc::{InputId, NodeId};
use kura_engine::op::{
    Applied, ApplyCx, DynExample, DynOp, Example, InferCx, Inferred, Numeric, Op, Signature,
};
use kura_engine::render::{Engine, InputValue};
use kura_engine::shape::Shape;
use kura_engine::testing::{render_with, single_node_graph, source_of};
use kura_engine::value::AudioMeta;
use kura_engine::{Digest, EngineError, Issues, Limits, NoMonitor, Value};

// ---- 対の表の行と、行の版で操作・解析を引く口 ----

/// 意味の対の 1 行：操作 `op` の版 `op_v` と、解析 `analysis` の版 `analysis_v`。
pub struct Pair {
    pub op: &'static str,
    pub op_v: u16,
    pub analysis: &'static str,
    pub analysis_v: u16,
    /// 操作のこの版がこの解析のこの版を申告しているとき、受け取った結果から読む欄（正規の形の欄の名前）。申告のない
    /// 対は空。構造の網が、欄ごとの偽の値でこの一覧が過不足ないことを確かめる。
    pub reads: &'static [&'static str],
    /// 一致を確かめる。行を受け取り、行の版の操作を当て、行の版の解析を引く。
    pub check: fn(&Engine, &Pair) -> Result<(), String>,
}

impl Pair {
    /// 行の版の操作を値に当てる。
    pub fn apply(
        &self,
        engine: &Engine,
        params: serde_json::Value,
        input: &Value,
    ) -> Result<Value, String> {
        apply_op(engine, self.op, self.op_v, params, input)
    }

    /// 行の版の操作の例。
    pub fn examples(&self, engine: &Engine) -> Result<Vec<DynExample>, String> {
        find_op(engine, self.op, self.op_v)?
            .examples()
            .map_err(|e| e.to_string())
    }

    /// 行の版の解析の例の入力（登録表で引く）。名前つき。
    pub fn analysis_inputs(&self, engine: &Engine) -> Result<Vec<(String, Value)>, String> {
        let a = engine
            .analyses()
            .get(self.analysis, self.analysis_v)
            .ok_or_else(|| format!("{} v{} が登録されていない", self.analysis, self.analysis_v))?;
        a.examples()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|ex| Ok((ex.name.to_string(), ex.input.generate().map_err(err)?)))
            .collect()
    }

    /// 行の版の解析で解く（登録表の（ID, 版）で引き、型 `A` の結果として読む）。
    pub fn analyze<A: Analysis>(
        &self,
        engine: &Engine,
        v: &Value,
        p: &A::Params,
    ) -> Result<A::Output, String> {
        if A::ID != self.analysis {
            return Err(format!(
                "行の解析は {} で、{} ではない",
                self.analysis,
                A::ID
            ));
        }
        analyze_at::<A>(engine, self.analysis_v, v, p)
    }

    /// 行の版の操作が申告する解析 `id` の版。
    pub fn declared(&self, engine: &Engine, id: &str) -> Result<u16, String> {
        find_op(engine, self.op, self.op_v)?
            .static_needs()
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, v)| *v)
            .ok_or_else(|| format!("{} v{} は {id} を申告していない", self.op, self.op_v))
    }
}

pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 登録された操作のその版（旧版を含む）。
pub fn find_op(engine: &Engine, id: &str, v: u16) -> Result<Arc<dyn DynOp>, String> {
    engine
        .registry()
        .all()
        .into_iter()
        .find(|o| o.id() == id && o.version() == v)
        .ok_or_else(|| format!("{id} v{v} が登録されていない"))
}

/// 操作の今の版（対の検査が補助に使う操作は今の版で当てる）。
pub fn current_op(engine: &Engine, id: &str) -> Result<u16, String> {
    engine
        .registry()
        .current_version(id)
        .ok_or_else(|| format!("{id} の今の版がない"))
}

/// 操作 `id` の版 `v` を、渡したエンジンで値に当てる。
pub fn apply_op(
    engine: &Engine,
    id: &str,
    v: u16,
    params: serde_json::Value,
    input: &Value,
) -> Result<Value, String> {
    render_with(
        engine,
        &single_node_graph(id, v, params).map_err(err)?,
        input.clone(),
        Digest::default(),
        &NoMonitor,
    )
    .map_err(|e| format!("{id} v{v}: {e}"))
}

/// 解析 `A::ID` の版 `v` で解く。登録表の（ID, 版）で引き、型 `A` は引数の書き方と結果の読み方だけに使う。
pub fn analyze_at<A: Analysis>(
    engine: &Engine,
    v: u16,
    value: &Value,
    p: &A::Params,
) -> Result<A::Output, String> {
    let params = serde_json::to_value(p).map_err(err)?;
    let out = engine
        .analyze_dyn(
            &AnalysisRequest {
                id: A::ID.to_string(),
                v: Some(v),
                params,
            },
            value,
            &NoMonitor,
        )
        .map_err(|e| format!("{} v{v}: {e}", A::ID))?;
    A::Output::from_canonical(&out.output)
        .map_err(|e| format!("{} v{v} の結果を読めない: {e}", A::ID))
}

fn pairs() -> Vec<&'static Pair> {
    pairs_image::PAIRS
        .iter()
        .chain(pairs_audio::PAIRS)
        .collect()
}

/// 対のない解析（どの操作とも意味の対を持たないもの）と、その理由。
fn unpaired() -> Vec<(&'static str, &'static str)> {
    pairs_image::UNPAIRED
        .iter()
        .chain(pairs_audio::UNPAIRED)
        .copied()
        .collect()
}

// ---- 偽の値 ----

/// 偽の値の作り方 1 つ：解析の結果の欄 `field` だけを変える。
pub struct FakeField {
    pub field: &'static str,
    pub perturb: Perturb,
}

/// 欄の変え方。
pub enum Perturb {
    /// 正規の JSON の欄を変える（JSON の結果の解析）。
    Json(fn(&mut serde_json::Value)),
    /// 型付きの値を変える（バイナリの結果の解析。正規の形は書き換えず、型に戻した値を変える）。型が違えば None。
    Typed(fn(&AnyOutput) -> Option<AnyOutput>),
}

/// 解析の偽の値の作り方（結果の欄ごと）。
fn fakes(id: &str) -> Option<&'static [FakeField]> {
    pairs_image::FAKES
        .iter()
        .chain(pairs_audio::FAKES)
        .find(|(a, _)| *a == id)
        .map(|(_, f)| *f)
}

/// 記録した 1 件：ノード・申告・解析した値・渡した結果の正規の形。
type Seen = (NodeId, Need, Value, Vec<u8>);

/// 渡した解析の値を記録する。
#[derive(Default)]
struct Recording {
    seen: Mutex<Vec<Seen>>,
}

impl Solver for Recording {
    fn solve(
        &self,
        node: NodeId,
        need: &Need,
        v: &Value,
        analysis: &dyn DynAnalysis,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError> {
        let out = Solve.solve(node, need, v, analysis, cx)?;
        let bytes = analysis.encode_any(&out)?;
        self.seen
            .lock()
            .unwrap()
            .push((node, need.clone(), v.clone(), bytes));
        Ok(out)
    }
}

/// 申告 `target` だけ、欄 `fields` を変えた偽の値を渡す。解けなかったら印を付ける（apply のエラーと分ける）。
struct Fake<'a> {
    target: Need,
    fields: Vec<&'a FakeField>,
    solver_failed: AtomicBool,
}

impl<'a> Fake<'a> {
    fn new(target: &Need, fields: Vec<&'a FakeField>) -> Self {
        Self {
            target: target.clone(),
            fields,
            solver_failed: AtomicBool::new(false),
        }
    }

    fn failed(&self) -> bool {
        self.solver_failed.load(Ordering::SeqCst)
    }
}

impl Solver for Fake<'_> {
    fn solve(
        &self,
        node: NodeId,
        need: &Need,
        v: &Value,
        analysis: &dyn DynAnalysis,
        cx: &AnalyzeCx<'_>,
    ) -> Result<AnyOutput, EngineError> {
        let out = Solve
            .solve(node, need, v, analysis, cx)
            .inspect_err(|_| self.solver_failed.store(true, Ordering::SeqCst))?;
        if *need != self.target {
            return Ok(out);
        }
        let id = need.id;
        let names: Vec<&str> = self.fields.iter().map(|f| f.field).collect();
        let original = analysis
            .encode_any(&out)
            .unwrap_or_else(|e| panic!("{id} の結果を正規の形にできない: {e}"));
        let fake = match analysis.encoding() {
            Encoding::Json => {
                let mut json: serde_json::Value = serde_json::from_slice(&original)
                    .unwrap_or_else(|e| panic!("{id} の結果が JSON で読めない: {e}"));
                for f in &self.fields {
                    let Perturb::Json(perturb) = f.perturb else {
                        panic!(
                            "JSON の結果の解析 {id} の欄 {} の偽の値は Perturb::Json で作る",
                            f.field
                        );
                    };
                    perturb(&mut json);
                }
                let bytes = kura_engine::digest::canonical(&json).unwrap_or_else(|e| {
                    panic!(
                        "{id} の偽の値（{names:?}）を正規の形にできない（tests/analysis/pairs_*.rs の FAKES の誤り）: {e}"
                    )
                });
                analysis.decode_any(&bytes).unwrap_or_else(|e| {
                    panic!(
                        "{id} の偽の値（{names:?}）が読めない（tests/analysis/pairs_*.rs の FAKES の誤り）: {e}"
                    )
                })
            }
            Encoding::Binary => {
                let mut cur = out;
                for f in &self.fields {
                    let Perturb::Typed(perturb) = f.perturb else {
                        panic!(
                            "バイナリの結果の解析 {id} の欄 {} の偽の値は、型付きの値を変える Perturb::Typed で作る（正規の形は書き換えない）",
                            f.field
                        );
                    };
                    cur = perturb(&cur).unwrap_or_else(|| {
                        panic!("{id} の欄 {} の偽の値の型が違う（FAKES の誤り）", f.field)
                    });
                }
                cur
            }
        };
        let again = analysis
            .encode_any(&fake)
            .unwrap_or_else(|e| panic!("{id} の偽の値（{names:?}）を正規の形にできない: {e}"));
        assert!(
            again != original,
            "{id} の偽の値（{names:?}）の正規の形が元と同じ（tests/analysis/pairs_*.rs の FAKES の誤り）"
        );
        Ok(fake)
    }
}

/// 偽の値で描いた結果の分け方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// 出力が変わらない。
    Same,
    /// 出力が変わった。
    Changed,
    /// 操作の apply が断った（値を読んだ証拠）。
    Rejected,
}

/// 偽の値で描いた結果を分ける。内部の誤り・Solver が解けないのは、テストか実行器の誤りとして止める。
fn outcome(
    got: &Result<Value, EngineError>,
    plain: &Value,
    solver_failed: bool,
    ctx: &str,
) -> Outcome {
    match got {
        Ok(v) if v.digest().unwrap() == plain.digest().unwrap() => Outcome::Same,
        Ok(_) => Outcome::Changed,
        Err(EngineError::Internal(m)) => panic!("{ctx}：内部の誤り: {m}"),
        Err(e) if solver_failed => panic!("{ctx}：Solver が解けなかった（テストの誤り）: {e}"),
        Err(_) => Outcome::Rejected,
    }
}

fn render(
    engine: &Engine,
    op: &dyn DynOp,
    ex: &DynExample,
    input: &Value,
    source: Digest,
    solver: &dyn Solver,
) -> Result<Value, EngineError> {
    let g = single_node_graph(op.id(), op.version(), ex.params.clone())?;
    let plan = engine.compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))?;
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source,
        },
    )]);
    let mut r = engine.render_with_solver(&plan, &inputs, &NoMonitor, solver)?;
    Ok(r.outputs.swap_remove(0).value)
}

// ---- 核の経路 ----

/// 核の経路を実行器に載せる包み：`apply` は `O::audio_kernel` の核を作って描く（申告・推論・例は `O` のまま）。
/// 実行器が先に解いた（偽の）値を、核の経路も `ApplyCx::analysis` で受け取る。
pub struct KernelOf<O>(PhantomData<fn() -> O>);

impl<O: Op> Op for KernelOf<O> {
    const ID: &'static str = O::ID;
    const VERSION: u16 = O::VERSION;
    type Params = O::Params;
    const NEEDS: &'static [(&'static str, u16)] = O::NEEDS;

    fn signature() -> Signature {
        O::signature()
    }

    fn infer(p: &O::Params, inputs: &[Shape], cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        O::infer(p, inputs, cx)
    }

    fn needs(p: &O::Params, inputs: &[Shape]) -> Result<Vec<Need>, Issues> {
        O::needs(p, inputs)
    }

    fn apply(
        p: &O::Params,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        let kernel = O::audio_kernel(p, inputs, cx)
            .ok_or_else(|| EngineError::Internal(format!("{} に核がありません", O::ID)))??;
        let meta = match inputs.first() {
            Some(Value::Audio(a)) => a.meta.clone(),
            _ => AudioMeta::default(),
        };
        Ok(Applied::value(render_all(kernel.as_ref(), meta, cx.mon)?))
    }

    fn examples() -> Vec<Example<O::Params>> {
        O::examples()
    }

    fn numeric_for(p: &O::Params) -> Numeric {
        O::numeric_for(p)
    }
}

/// その例で核を作るか（音声を出す操作か）。核の中の解析はその場で解く。
fn has_kernel(op: &dyn DynOp, params: &serde_json::Value, input: &Value) -> bool {
    let p = op.parse(params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, 1);
    op.audio_kernel(p.as_ref(), &[input], &mut cx).is_some()
}

/// 核の経路を描くエンジン（`KernelOf` で包んだその操作だけ。解析は既定の登録表）。
fn kernel_engine(op: &dyn DynOp) -> Engine {
    let r = pairs_audio::kernel_registry(op.id(), op.version())
        .unwrap_or_else(|| {
            panic!(
                "{} v{} は解析を申告して核を持つのに、核の経路の登録がない（tests/analysis/pairs_audio.rs の kernel_registry に足す）",
                op.id(),
                op.version()
            )
        })
        .unwrap();
    Engine::with_registries(
        r,
        kura_engine::analysis::registry().unwrap(),
        Limits::default(),
    )
    .unwrap()
}

/// 音声の標本（ビットで比べる）。音声でなければ None。
fn samples(v: &Value) -> Option<(u32, Vec<Vec<u32>>)> {
    let Value::Audio(a) = v else { return None };
    Some((
        a.rate(),
        a.planes()
            .iter()
            .map(|p| p.iter().map(|x| x.to_bits()).collect())
            .collect(),
    ))
}

/// apply の経路と核の経路が同じ結果（どちらも同じ標本か、どちらもエラー）。
fn same_result(a: &Result<Value, EngineError>, b: &Result<Value, EngineError>) -> bool {
    match (a, b) {
        (Ok(x), Ok(y)) => samples(x).is_some() && samples(x) == samples(y),
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

// ---- 構造の網 ----

/// `NEEDS` のある操作で、計画が空になる（何も申告しない）例と理由（操作, 版, 例の名前, 理由）。一覧にない例の計画が
/// 空なら失敗にする（引数によって申告をやめ、apply が自分で計る形を見逃さない）。一覧にあるのに計画が空でない例・
/// ない例も失敗にする（一覧を古くしない）。
const EMPTY_PLAN: &[(&str, u16, &str, &str)] = &[(
    "image.trim_alpha",
    1,
    "rgb-is-untouched",
    "アルファのない形式（Rgb8）は透明を表せず、何もしないので解かない",
)];

/// 構造の網：登録したすべての操作（旧版を含む）の例で、申告した解析を計画どおりに解き、実行器が渡した値＝別に求めた値、
/// 渡した値を使っている（欄ごとの偽の値で出力が変わる）。NEEDS はどれも例の計画に載り、NEEDS のある操作で計画が空の
/// 例は `EMPTY_PLAN` に載る。核の経路も同じ照合をする。
#[test]
fn e26_declared_analyses_match_and_are_used() {
    let engine = Engine::new().unwrap();
    let pairs = pairs();
    let mut failures = Vec::new();
    // （操作, 版）→ どれかの例の計画に載った申告
    let mut planned_by: BTreeMap<(&str, u16), BTreeSet<(&str, u16)>> = BTreeMap::new();
    // EMPTY_PLAN の行のうち、その例が見つかったもの
    let mut empty_seen: BTreeSet<usize> = BTreeSet::new();
    // （操作, 版, 解析, 版, 欄）→ その欄だけの偽の値で出力が変わった（か apply が断った）例の数
    let mut moved: BTreeMap<(&str, u16, &str, u16, &str), usize> = BTreeMap::new();
    let (mut examples, mut changed, mut rejected, mut kernels) = (0, 0, 0, 0);
    for op in engine.registry().all() {
        let op = op.as_ref();
        let mut kernel: Option<Engine> = None;
        for ex in op.examples().unwrap() {
            let ctx = format!("{} v{} {}", op.id(), op.version(), ex.name);
            let input = ex.inputs[0].generate().unwrap();
            let source = source_of(&ex.inputs).unwrap();
            let g = single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap();
            let plan = engine
                .compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))
                .unwrap();
            let node = g.nodes[0].id;
            let planned = plan.node_needs(node).unwrap().to_vec();
            let rec = Recording::default();
            let plain = render(&engine, op, &ex, &input, source, &rec)
                .unwrap_or_else(|e| panic!("{ctx}: {e}"));
            let seen = rec.seen.into_inner().unwrap();
            // 計画の申告どおりに、その順で解いた
            let solved: Vec<Need> = seen.iter().map(|(_, n, _, _)| n.clone()).collect();
            assert_eq!(solved, planned, "{ctx}");
            let mine = planned_by.entry((op.id(), op.version())).or_default();
            for need in &planned {
                assert!(
                    op.static_needs().contains(&(need.id, need.v)),
                    "{ctx}：NEEDS にない申告"
                );
                mine.insert((need.id, need.v));
            }
            // NEEDS のある操作で計画が空の例は、理由つきで EMPTY_PLAN に（一覧の行は計画が空の例だけ）
            let listed = EMPTY_PLAN
                .iter()
                .position(|(o, v, n, _)| (*o, *v, *n) == (op.id(), op.version(), ex.name));
            if let Some(i) = listed {
                empty_seen.insert(i);
            }
            match (planned.is_empty(), listed) {
                (true, None) if !op.static_needs().is_empty() => failures.push(format!(
                    "{ctx}：NEEDS があるのに計画が空（申告しない理由があれば EMPTY_PLAN に理由つきで載せる。needs が返さずに apply が自分で計っていない？）"
                )),
                (false, Some(_)) => failures.push(format!(
                    "{ctx}：EMPTY_PLAN にあるのに計画が空でない（EMPTY_PLAN から消す）"
                )),
                _ => {}
            }
            if planned.is_empty() {
                continue;
            }
            examples += 1;
            // 申告ごとに：渡した値＝別に求めた値。対の行と偽の値の作り方を引く（なければ案内して止める）
            let mut checks: Vec<(&Need, &Pair, &'static [FakeField])> = Vec::new();
            for (at, need, v, bytes) in &seen {
                assert_eq!(*at, node);
                // 解析した値はノードの入力
                assert_eq!(v.digest().unwrap(), input.digest().unwrap(), "{ctx}");
                let alone = engine
                    .analyze_dyn(
                        &AnalysisRequest {
                            id: need.id.to_string(),
                            v: Some(need.v),
                            params: need.params.clone(),
                        },
                        v,
                        &NoMonitor,
                    )
                    .unwrap();
                assert_eq!(
                    &alone.output, bytes,
                    "{ctx}：実行器が渡した {} の値が、別に求めた値と違う",
                    need.id
                );
                let row = pairs
                    .iter()
                    .find(|p| {
                        (p.op, p.op_v, p.analysis, p.analysis_v)
                            == (op.id(), op.version(), need.id, need.v)
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{ctx}：申告 {} v{} の行が意味の対の表にない（tests/analysis/pairs_*.rs に足す）",
                            need.id, need.v
                        )
                    });
                assert!(
                    !row.reads.is_empty(),
                    "{ctx}：対の行の reads（操作が読む欄）が空"
                );
                let analysis = engine.analyses().get(need.id, need.v).unwrap();
                let fields = fakes(need.id).unwrap_or_else(|| match analysis.encoding() {
                    Encoding::Json => panic!(
                        "解析 {} の偽の値の作り方がありません（tests/analysis/pairs_*.rs の FAKES に、結果の欄ごとに足す）",
                        need.id
                    ),
                    Encoding::Binary => panic!(
                        "バイナリの結果の解析 {} の偽の値の作り方がありません（正規の形の JSON の書き換えは使えない。tests/analysis/pairs_*.rs の FAKES に、型付きの値を変える Perturb::Typed で欄ごとに足す）",
                        need.id
                    ),
                });
                if analysis.encoding() == Encoding::Json {
                    let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
                    let keys: BTreeSet<&str> = json
                        .as_object()
                        .unwrap_or_else(|| panic!("{} の結果が JSON のオブジェクトでない", need.id))
                        .keys()
                        .map(String::as_str)
                        .collect();
                    let names: BTreeSet<&str> = fields.iter().map(|f| f.field).collect();
                    assert_eq!(
                        keys, names,
                        "{}：偽の値の欄（FAKES）が結果の欄と違う",
                        need.id
                    );
                }
                checks.push((need, row, fields));
            }
            // 核の経路（音声を出す操作）：同じ申告を同じ値で解き、同じ出力
            let keng = if has_kernel(op, &ex.params, &input) {
                Some(&*kernel.get_or_insert_with(|| kernel_engine(op)))
            } else {
                None
            };
            if let Some(k) = keng {
                kernels += 1;
                let krec = Recording::default();
                let kplain = render(k, op, &ex, &input, source, &krec);
                assert!(
                    same_result(&Ok(plain.clone()), &kplain),
                    "{ctx}：核の経路の出力が apply の経路と違う: {:?}",
                    kplain.err()
                );
                let strip = |s: &[Seen]| -> Vec<(NodeId, Need, Vec<u8>)> {
                    s.iter()
                        .map(|(n, need, _, b)| (*n, need.clone(), b.clone()))
                        .collect()
                };
                assert_eq!(
                    strip(&krec.seen.into_inner().unwrap()),
                    strip(&seen),
                    "{ctx}：核の経路が解いた申告・値が apply の経路と違う"
                );
            }
            for (need, row, fields) in checks {
                let read: Vec<&FakeField> = row
                    .reads
                    .iter()
                    .map(|r| {
                        fields
                            .iter()
                            .find(|f| f.field == *r)
                            .unwrap_or_else(|| panic!("{ctx}：reads の欄 {r} の偽の値がない"))
                    })
                    .collect();
                // reads の欄をすべて変えたもの（None）と、欄ごと。この例で、reads の欄のどれか（かすべて）を変えた
                // 偽の値で出力が変わる（欄どうしが打ち消す例があるので、すべてを変えたものだけでは見ない）
                let mut cases: Vec<(Option<&'static str>, Vec<&FakeField>)> = vec![(None, read)];
                cases.extend(fields.iter().map(|f| (Some(f.field), vec![f])));
                let mut used = false;
                for (field, set) in cases {
                    let what = field.map_or_else(
                        || format!("reads の欄 {:?} をすべて変えたもの", row.reads),
                        |f| format!("欄 {f} だけを変えたもの"),
                    );
                    let at = format!("{ctx}：{} の偽の値（{what}）", need.id);
                    let fake = Fake::new(need, set.clone());
                    let got = render(&engine, op, &ex, &input, source, &fake);
                    let result = outcome(&got, &plain, fake.failed(), &at);
                    if let Some(k) = keng {
                        let kfake = Fake::new(need, set);
                        let kgot = render(k, op, &ex, &input, source, &kfake);
                        outcome(&kgot, &plain, kfake.failed(), &format!("{at}（核の経路）"));
                        assert!(
                            same_result(&got, &kgot),
                            "{at}で、核の経路が apply の経路と違う（核が申告した値を使っていない？）"
                        );
                    }
                    match result {
                        Outcome::Same => continue,
                        Outcome::Changed => changed += 1,
                        Outcome::Rejected => rejected += 1,
                    }
                    used = true;
                    if let Some(f) = field {
                        assert!(
                            row.reads.contains(&f),
                            "{at}で出力が変わるのに、対の行の reads にない（reads に足す）"
                        );
                        *moved
                            .entry((op.id(), op.version(), need.id, need.v, f))
                            .or_default() += 1;
                    }
                }
                assert!(
                    used,
                    "{ctx}：申告した {} の値を使っていない（reads の欄の偽の値で出力が変わらない。自分で計り直している？）",
                    need.id
                );
            }
        }
    }
    for (i, (op, v, name, _)) in EMPTY_PLAN.iter().enumerate() {
        if !empty_seen.contains(&i) {
            failures.push(format!(
                "EMPTY_PLAN の {op} v{v} {name} は、登録された操作の例にない（EMPTY_PLAN から消す）"
            ));
        }
    }
    // NEEDS の（ID, 版）はどれも、どれかの例の計画に載る（needs が返さずに apply が自分で計る形を見逃さない）
    for op in engine.registry().all() {
        for (id, v) in op.static_needs() {
            if !planned_by
                .get(&(op.id(), op.version()))
                .is_some_and(|s| s.contains(&(*id, *v)))
            {
                failures.push(format!(
                    "{} v{} の NEEDS の {id} v{v} が、どの例の計画にも載らない（needs が返さない？ 申告する例を足す）",
                    op.id(),
                    op.version()
                ));
            }
        }
    }
    // reads の欄はどれも、どれかの例で出力を変える
    for p in &pairs {
        for f in p.reads {
            if moved
                .get(&(p.op, p.op_v, p.analysis, p.analysis_v, *f))
                .is_none_or(|n| *n == 0)
            {
                failures.push(format!(
                    "{} v{} が読む {} v{} の欄 {f}：その欄だけの偽の値で、どの例の出力も変わらない（自分で計り直している？ その欄が効く例を足す）",
                    p.op, p.op_v, p.analysis, p.analysis_v
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert!(
        examples > 0 && changed > 0,
        "申告のある操作の例がない（例 {examples}・出力が変わった {changed}・apply が断った {rejected}・核 {kernels}）"
    );
}

// ---- 意味の対 ----

/// 意味の対の表のすべての検査（行の版で）。
#[test]
fn e26_pairs_agree() {
    let engine = Engine::new().unwrap();
    let mut failures = Vec::new();
    for p in pairs() {
        if let Err(e) = (p.check)(&engine, p) {
            failures.push(format!(
                "{} v{} ↔ {} v{}: {e}",
                p.op, p.op_v, p.analysis, p.analysis_v
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// 覆いの検査の失敗の一覧。
fn coverage_failures(
    engine: &Engine,
    pairs: &[&Pair],
    unpaired: &[(&'static str, &'static str)],
) -> Vec<String> {
    let ops = engine.registry();
    let analyses = engine.analyses();
    let mut failures = Vec::new();
    let key = |p: &Pair| (p.op, p.op_v, p.analysis, p.analysis_v);
    // 申告のある（操作, 版, 解析, 版）は、旧版の操作も含めてすべて対の表に（reads つきで）
    for op in ops.all() {
        for (id, v) in op.static_needs() {
            match pairs
                .iter()
                .find(|p| key(p) == (op.id(), op.version(), *id, *v))
            {
                None => failures.push(format!(
                    "{} v{} は {id} v{v} を申告しているのに、意味の対の表にその版の行がない",
                    op.id(),
                    op.version()
                )),
                Some(p) if p.reads.is_empty() => failures.push(format!(
                    "{} v{} ↔ {id} v{v}：申告のある行の reads（操作が読む欄）が空",
                    p.op, p.op_v
                )),
                Some(_) => {}
            }
        }
    }
    for (i, p) in pairs.iter().enumerate() {
        let op = ops
            .all()
            .into_iter()
            .find(|o| o.id() == p.op && o.version() == p.op_v);
        if op.is_none() {
            failures.push(format!("対の表の {} v{} は登録されていない", p.op, p.op_v));
        }
        if analyses.get(p.analysis, p.analysis_v).is_none() {
            failures.push(format!(
                "対の表の {} v{} は登録されていない",
                p.analysis, p.analysis_v
            ));
        }
        let declared = op.is_some_and(|o| o.static_needs().contains(&(p.analysis, p.analysis_v)));
        if !declared && !p.reads.is_empty() {
            failures.push(format!(
                "{} v{} ↔ {} v{}：申告のない行に reads がある",
                p.op, p.op_v, p.analysis, p.analysis_v
            ));
        }
        for f in p.reads {
            if !fakes(p.analysis).is_some_and(|fs| fs.iter().any(|x| x.field == *f)) {
                failures.push(format!("{} の欄 {f} の偽の値が FAKES にない", p.analysis));
            }
        }
        if pairs[..i].iter().any(|q| key(q) == key(p)) {
            failures.push(format!(
                "対の表に {} v{} ↔ {} v{} が 2 つある",
                p.op, p.op_v, p.analysis, p.analysis_v
            ));
        }
    }
    // 表にある（操作, 解析）は、操作の今の版と解析の今の版の行を持つ。今の版の操作は解析の今の版を申告する
    // （`Engine::new` が確かめる。このテストはその後に来る）ので、この行は今の版どうしの意味の一致を確かめる
    let ids: BTreeSet<(&str, &str)> = pairs.iter().map(|p| (p.op, p.analysis)).collect();
    for (op, a) in ids {
        let (Some(ov), Some(av)) = (ops.current_version(op), analyses.current_version(a)) else {
            continue;
        };
        if !pairs.iter().any(|p| key(p) == (op, ov, a, av)) {
            let declares = ops
                .all()
                .into_iter()
                .any(|o| o.id() == op && o.static_needs().iter().any(|(id, _)| *id == a));
            let how = if declares {
                // 申告のある対：解析の版を上げたら、申告する操作の版も必ず上げる（§8.4）
                "申告のある対。解析の版を上げたら、申告する操作の版も必ず上げ（§8.4）、今の版どうしの行を足す"
            } else {
                "版を上げたら今の版どうしの行を足す。意味がずれるなら操作の版も上げる"
            };
            failures.push(format!(
                "{op} の今の版 {ov} と {a} の今の版 {av} の対の行がない（{how}）"
            ));
        }
    }
    // 登録したすべての解析は、対の表か対のない解析の表に（両方ではない）
    for a in analyses.current() {
        let id = a.id();
        let paired = pairs.iter().any(|p| p.analysis == id);
        let listed = unpaired.iter().any(|(u, _)| *u == id);
        if !paired && !listed {
            failures.push(format!(
                "解析 {id} が、意味の対の表にも対のない解析の表にもない（tests/analysis/pairs_*.rs に足す）"
            ));
        }
        if paired && listed {
            failures.push(format!("解析 {id} が両方の表にある"));
        }
    }
    for (id, _) in unpaired {
        if analyses.current_version(id).is_none() {
            failures.push(format!("対のない解析の表の {id} は登録されていない"));
        }
    }
    failures
}

/// 覆いの検査：申告のある（操作, 版, 解析, 版）はすべて対の表に。表にある（操作, 解析）は今の版どうしの行を持つ。
/// 登録したすべての解析は対の表か対のない解析の表に載る。表に、登録されていない操作・解析の版の行はない。
#[test]
fn e26_tables_cover_every_declaration_and_analysis() {
    let engine = Engine::new().unwrap();
    let failures = coverage_failures(&engine, &pairs(), &unpaired());
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

// ---- ソースの走査：操作が申告で受け取るべき計算を直接呼ばない ----

/// 禁止の名前の種類（どこに出たら断るか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ng {
    /// 型・トレイト：`.` の後を除き、どこに出ても断る（束縛や欄の名前にはならない。`non_snake_case` が断る）。
    Type,
    /// 関数・メソッド：呼び出し（`(` が続く）・use の中（`as` の別名を除く）・パスの途中（`::` が続く・前にある）の
    /// ときだけ断る。欄の読み取り（`.` の後で、呼び出しでない）と束縛（`let`・パターン・引数）の名前は除く。
    Fn,
    /// 関数かモジュール：`Fn` と同じ。ただし `名前::名前` と続く（モジュールのパスの一部）ときは除く。
    FnOrModule,
}

/// 直接使ってはいけない名前と、その種類と理由。
const DIRECT_NG: &[(&str, Ng, &str)] = &[
    (
        "Analysis",
        Ng::Type,
        "解析のトレイト（解析の run を直接呼ぶのに要る）",
    ),
    ("DynAnalysis", Ng::Type, "型を消した解析"),
    (
        "AnalyzeCx",
        Ng::Type,
        "解析の文脈（解析の run を直接呼ぶのに要る）",
    ),
    (
        "Engine",
        Ng::Type,
        "エンジン（操作がエンジンを作る理由はない。作れば解析の口で自分で計れる）",
    ),
    ("run_any", Ng::Fn, "型を消した解析の run"),
    (
        "analyze",
        Ng::Fn,
        "解析を計る口（エンジンの型付きの口・解析のモジュールの関数）",
    ),
    ("analyze_dyn", Ng::Fn, "エンジンの解析の口"),
    ("analyze_many", Ng::Fn, "エンジンの解析の口（まとめて解く）"),
    (
        "opaque_bounds",
        Ng::FnOrModule,
        "不透明な範囲（image.opaque_bounds の中身。申告して受け取る）",
    ),
    (
        "frames_opaque_bounds",
        Ng::Fn,
        "フレームの列の不透明な範囲（image.opaque_bounds の中身）",
    ),
];

/// 部品として使ってよいもの（ファイル, 名前, 理由）。使われていない行は失敗にする（一覧を古くしない）。
const DIRECT_OK: &[(&str, &str, &str)] = &[(
    "sprite/split_grid.rs",
    "opaque_bounds",
    "skipEmpty の空のセルの判定。切り出したセル（計画の段階でない値）ごとに見るので申告できない。部品として使い、image.opaque_bounds の結果とは比べない",
)];

/// ソースから、コメント・文字列・文字の中身を空白にし、テストのモジュール（`#[cfg(test)]` と続く属性のあとの
/// `mod <名前> { … }`）を除いたもの（行は保つ）。
fn code_of(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == 'r' && matches!(next, Some('"' | '#')) && !out.chars().last().is_some_and(ident) {
            let mut j = i + 1;
            let mut hashes = 0;
            while b.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&'"') {
                j += 1;
                while j < b.len()
                    && !(b[j] == '"' && (0..hashes).all(|h| b.get(j + 1 + h) == Some(&'#')))
                {
                    if b[j] == '\n' {
                        out.push('\n');
                    }
                    j += 1;
                }
                out.push(' ');
                i = j + 1 + hashes;
                continue;
            }
        }
        if c == '"' {
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' {
                    i += 1;
                } else if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '\'' {
            // 文字（'x'・'\n'・'\u{…}'）。ライフタイム（'a）はそのまま
            if next == Some('\\') {
                let mut j = i + 3;
                while j < b.len() && b[j] != '\'' {
                    j += 1;
                }
                out.push(' ');
                i = j + 1;
                continue;
            }
            if b.get(i + 2) == Some(&'\'') {
                out.push(' ');
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    without_test_modules(&out)
}

/// 空白（CRLF の `\r` を含む）を読み飛ばした位置。
fn skip_ws(b: &[char], mut i: usize) -> usize {
    while b.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// `b[i]` から始まる名前（英数字と `_`）の終わりと、その名前。
fn word_at(b: &[char], i: usize) -> (usize, String) {
    let w: String = b
        .iter()
        .skip(i)
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
        .collect();
    (i + w.chars().count(), w)
}

/// `b[i]` から始まる属性 `#[…]` の終わり（`]` の次）と、中身（空白を除く）。属性でなければ None。
fn attribute_at(b: &[char], i: usize) -> Option<(usize, String)> {
    if b.get(i) != Some(&'#') {
        return None;
    }
    let open = skip_ws(b, i + 1);
    if b.get(open) != Some(&'[') {
        return None;
    }
    let mut depth = 0usize;
    for (j, c) in b.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let body = b[open + 1..j].iter().filter(|c| !c.is_whitespace());
                    return Some((j + 1, body.collect()));
                }
            }
            _ => {}
        }
    }
    None
}

/// `b[i]` から `#[cfg(test)]`・続く属性・空白・可視性のあとに `mod <名前> {` が来るなら、対応する `}` の次の位置。
fn test_module_end(b: &[char], i: usize) -> Option<usize> {
    let (mut j, body) = attribute_at(b, i)?;
    if body != "cfg(test)" {
        return None;
    }
    while let Some((end, _)) = attribute_at(b, skip_ws(b, j)) {
        j = end;
    }
    let (mut j, mut w) = word_at(b, skip_ws(b, j));
    if w == "pub" {
        j = skip_ws(b, j);
        if b.get(j) == Some(&'(') {
            j += b[j..].iter().position(|c| *c == ')')? + 1;
        }
        (j, w) = word_at(b, skip_ws(b, j));
    }
    if w != "mod" {
        return None;
    }
    let (j, name) = word_at(b, skip_ws(b, j));
    let open = skip_ws(b, j);
    if name.is_empty() || b.get(open) != Some(&'{') {
        return None;
    }
    let mut depth = 0usize;
    for (k, c) in b.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// コメント・文字列を除いたコードから、テストのモジュールを除く（改行は保つ。対応する `}` がなければ除かない）。
fn without_test_modules(code: &str) -> String {
    let b: Vec<char> = code.chars().collect();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    while i < b.len() {
        if let Some(end) = test_module_end(&b, i) {
            out.extend(b[i..end].iter().filter(|c| **c == '\n'));
            i = end;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `analysis::` に続くパス（`code` の `at` から）の末尾の名前の（位置, 直前の区切り, 名前）。`{…}` の中の名前を含み、
/// `as` の別名は除く。`*` も名前。`名前::<…>`（型引数）は、その名前を末尾とする。
fn path_leaves(code: &str, prev: &str, at: usize, out: &mut Vec<(usize, String, String)>) {
    let s = code[at..].trim_start();
    let at = code.len() - s.len();
    if s.starts_with('{') {
        let mut depth = 0;
        let mut item = at + 1;
        for (i, c) in s.char_indices() {
            match c {
                '{' => depth += 1,
                '}' | ',' if depth == 1 => {
                    path_leaves(code, prev, item, out);
                    item = at + i + 1;
                    if c == '}' {
                        return;
                    }
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        return;
    }
    if s.starts_with('*') {
        out.push((at, prev.to_string(), "*".to_string()));
        return;
    }
    let id: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if id.is_empty() {
        return;
    }
    match s[id.len()..].trim_start().strip_prefix("::") {
        Some(rest) if !rest.trim_start().starts_with('<') => {
            path_leaves(code, &id, code.len() - rest.len(), out);
        }
        _ => out.push((at, prev.to_string(), id)),
    }
}

/// 解析のパスの末尾が、解析のモジュールの項目（関数・モジュール・`*`）か、解析の `run` か。型の関連関数
/// （`PeaksParams::default` など。直前が型の名前）は除く。
fn module_item(prev: &str, leaf: &str) -> bool {
    let lower = |s: &str| s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_');
    leaf == "*" || leaf == "run" || (lower(leaf) && lower(prev))
}

/// 文字列の末尾の名前（英数字と `_`。空白は読み飛ばす）。
fn last_word(s: &str) -> &str {
    let s = s.trim_end();
    let start = s
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    &s[start..]
}

/// 文字列の先頭の名前（英数字と `_`）。
fn first_word(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// 操作のソース 1 つ（`code_of` を通したもの）の、直接の計算の呼び出し（行, 名前, 理由）。同じ位置は 1 度だけ。
fn direct_calls(code: &str) -> Vec<(usize, String, String)> {
    // （位置, 名前, 理由）
    let mut found: Vec<(usize, String, String)> = Vec::new();
    let mut pos = 0;
    // いま読んでいる use の文の終わり（`;`）の位置
    let mut use_end = 0;
    let bytes = code.as_bytes();
    while pos < bytes.len() {
        let c = bytes[pos] as char;
        if !(c.is_ascii_alphabetic() || c == '_') {
            pos += 1;
            continue;
        }
        let start = pos;
        while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
            pos += 1;
        }
        let name = &code[start..pos];
        let before = code[..start].trim_end();
        let after = code[pos..].trim_start();
        let path = after.strip_prefix("::");
        if name == "use" {
            use_end = code[pos..].find(';').map_or(code.len(), |i| pos + i);
            continue;
        }
        // use の中の名前（`as` に続く別名は束縛なので除く）
        let in_use = start < use_end && last_word(before) != "as";
        if let Some((_, kind, why)) = DIRECT_NG.iter().find(|(n, _, _)| *n == name) {
            let call = after.starts_with('(');
            let in_path = path.is_some() || before.ends_with("::");
            // `名前::名前`（モジュールのパスの一部）。`名前::<…>` は型引数なので含めない
            let module = path.is_some_and(|r| !r.trim_start().starts_with('<'));
            let hit = match kind {
                Ng::Type => !before.ends_with('.'),
                Ng::Fn => call || in_path || in_use,
                Ng::FnOrModule => !module && (call || in_path || in_use),
            };
            if hit {
                found.push((start, name.to_string(), why.to_string()));
            }
        }
        // `analysis` の別名（`use crate::analysis as X;`・`use crate::{analysis as X};`）：別名のパスは追えないので断る
        if name == "analysis" && in_use && first_word(after) == "as" {
            found.push((
                start,
                name.to_string(),
                "解析のモジュールの別名（別名のパスは走査が追えない。`analysis::…` と書く）"
                    .to_string(),
            ));
        }
        // `analysis::…` のパス（`cx.analysis::<T>` の型引数は除く）：末尾が解析のモジュールの項目（関数・モジュール・
        // `*`）か `run` なら直接の計算
        if name == "analysis"
            && let Some(rest) = path
            && !rest.trim_start().starts_with('<')
        {
            let mut leaves = Vec::new();
            path_leaves(code, name, code.len() - rest.len(), &mut leaves);
            for (at, prev, leaf) in leaves {
                if module_item(&prev, &leaf) {
                    found.push((
                        at,
                        leaf,
                        "解析のモジュールの関数・モジュール（申告して受け取る）".to_string(),
                    ));
                }
            }
        }
    }
    // 位置の順に。同じ名前を 2 つの規則が見つけたら、先に見つけたほう（パスの規則）だけ
    found.sort_by_key(|(at, _, _)| *at);
    found.dedup_by_key(|(at, _, _)| *at);
    let line = |at: usize| code[..at].matches('\n').count() + 1;
    found
        .into_iter()
        .map(|(at, name, why)| (line(at), name, why))
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// 操作（`src/ops/**`）は、申告で受け取るべき計算（解析の run・`analysis::` のモジュールの関数・不透明な範囲）を
/// 直接呼ばない（.claude/rules/engine.md）。部品として使ってよいものは `DIRECT_OK` に理由つきで載せる。
#[test]
fn e26_ops_do_not_compute_declarable_analyses_themselves() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ops");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty());
    let mut used = BTreeSet::new();
    let mut failures = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let code = code_of(&std::fs::read_to_string(f).unwrap());
        for (line, name, why) in direct_calls(&code) {
            match DIRECT_OK
                .iter()
                .position(|(file, n, _)| rel == *file && name == *n)
            {
                Some(i) => {
                    used.insert(i);
                }
                None => failures.push(format!("src/ops/{rel}:{line}: {name}：{why}")),
            }
        }
    }
    for (i, (file, name, _)) in DIRECT_OK.iter().enumerate() {
        if !used.contains(&i) {
            failures.push(format!(
                "許可の一覧の src/ops/{file} の {name} は、もう使われていない（DIRECT_OK から消す）"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// 走査が、知っている迂回の形を見つけ、申告の口・束縛や欄の名前・文字列・コメント・テストのモジュールは見つけない。
#[test]
fn direct_call_scan_finds_known_bypasses() {
    let found = |src: &str| -> Vec<String> {
        direct_calls(&code_of(src))
            .into_iter()
            .map(|(_, n, _)| n)
            .collect()
    };
    // 見つける
    for (src, want) in [
        (
            "let b = <OpaqueBounds as crate::analysis::Analysis>::run(&p, input, &crate::analysis::AnalyzeCx { mon: cx.mon, limits: cx.limits })?;",
            vec!["Analysis", "AnalyzeCx"],
        ),
        (
            "let m = crate::analysis::loudness::analyze(audio, cx.mon)?;",
            vec!["analyze"],
        ),
        (
            "use crate::analysis::loudness;\nlet m = loudness::measure(a, mon)?;",
            vec!["loudness"],
        ),
        (
            "use crate::analysis::{Need, audio::silence::{scan, SilenceParams}, loudness::{self as l}};",
            vec!["scan", "self"],
        ),
        ("use crate::analysis::*;", vec!["*"]),
        (
            "let b = crate::analysis::image::opaque_bounds::OpaqueBounds::run(&p, v, acx)?;",
            vec!["run"],
        ),
        (
            "let m = crate::analysis::loudness::measure::<f32>(a, mon)?;",
            vec!["measure"],
        ),
        (
            "let r = crate::imaging::bounds::frames_opaque_bounds(fr, t, cx.mon)?.1;",
            vec!["frames_opaque_bounds"],
        ),
        (
            "use crate::imaging::bounds::opaque_bounds;\nlet r = opaque_bounds(px, t, mon)?;",
            vec!["opaque_bounds", "opaque_bounds"],
        ),
        (
            "use crate::imaging::bounds::{union_rect, frames_opaque_bounds as fob};",
            vec!["frames_opaque_bounds"],
        ),
        (
            "let r = crate::imaging::bounds::opaque_bounds::<u8>(px, t, mon)?;\nlet s = img.opaque_bounds(t)?;",
            vec!["opaque_bounds", "opaque_bounds"],
        ),
        // エンジンを作って型付きの口で計る（needs を条件で空にする迂回）
        (
            "let m = Arc::new(crate::render::Engine::new()?.analyze::<LoudnessGating>(input, &NoParams {}, cx.mon)?);",
            vec!["Engine", "analyze"],
        ),
        (
            "use crate::Engine;\nlet e = Engine::with_limits(cx.limits.clone())?;\nlet m = e.analyze(input, &p, cx.mon)?;\nlet n = e.analyze_many(&reqs, input, cx.mon)?;",
            vec!["Engine", "Engine", "analyze", "analyze_many"],
        ),
        // analysis の別名（別名のパスは追えないので、別名を付けたところで断る）
        (
            "use crate::analysis as an;\nlet m = an::loudness::measure(a, cx.mon)?;",
            vec!["analysis"],
        ),
        (
            "use crate::{analysis as an, op::Op};\nlet s = an::audio::silence::scan(a, &p, mon)?;",
            vec!["analysis"],
        ),
        ("use super::super::analysis\r\n    as an;", vec!["analysis"]),
        // テストのモジュールより後ろのコードは見る
        (
            "#[cfg(test)]\nmod tests { use crate::analysis::loudness; }\nfn here(a: &Audio) { crate::analysis::loudness::analyze(a, mon) }",
            vec!["analyze"],
        ),
        // テストのモジュールでないもの（`#[cfg(test)]` の付いた関数・外のファイルのモジュール）は見る
        (
            "#[cfg(test)]\nfn probe() { crate::analysis::loudness::analyze(a, mon); }",
            vec!["analyze"],
        ),
    ] {
        assert_eq!(found(src), want, "{src}");
    }
    // 見つけない
    for src in [
        // 申告の口と、型の関連関数
        "let p = crate::analysis::audio::peaks::PeaksParams::default();\n\
         let n = crate::analysis::Need::of::<LoudnessGating>(0, &NoParams {})?;",
        "use crate::analysis::image::opaque_bounds::{OpaqueBounds, OpaqueBoundsParams};\n\
         use crate::analysis::{Need, NoParams};\n\
         const NEEDS: &[(&str, u16)] = &[(\"image.opaque_bounds\", 1)];\n\
         let b = cx.analysis::<OpaqueBounds>(0, input, &OpaqueBoundsParams { threshold: 0 })?; // opaque_bounds(x)\n\
         /* crate::analysis::loudness::measure */ let c = 'x';",
        // 申告で受け取った値の束縛・欄の名前
        "let opaque_bounds = cx.analysis::<OpaqueBounds>(0, input, &OpaqueBoundsParams { threshold: 0 })?;\n\
         let r = opaque_bounds.union.unwrap_or(clear);\n\
         let f = s.opaque_bounds.frames.get(i);",
        "let mut opaque_bounds: Arc<Bounds> = got;\n\
         let analyze = true;\n\
         match x { Some(opaque_bounds) => opaque_bounds.union, None => None }\n\
         let g = |opaque_bounds: &Bounds| opaque_bounds.union;\n\
         let s = Seen { opaque_bounds, analyze: a.analyze };\n\
         struct Seen { opaque_bounds: Arc<Bounds>, analyze: bool }\n\
         fn take(opaque_bounds: &Bounds) -> IRect { opaque_bounds.union }",
        "use crate::imaging::pixels::can_be_transparent as opaque_bounds;",
        // テストのモジュール（属性・空白・CRLF を挟んでも、名前が tests でなくても）
        "#[cfg(test)]\nmod tests { use crate::analysis::loudness; }",
        "#[cfg(test)]\n#[allow(clippy::float_cmp)]\nmod tests {\n    use crate::analysis::loudness;\n    fn f() { let s = \"}\"; let c = '{'; loudness::analyze(a, mon); }\n}",
        "#[cfg(test)]\r\n#[allow(clippy::float_cmp)]\r\n\r\nmod tests {\r\n    use crate::analysis::loudness;\r\n}\r\n",
        "#[ cfg( test ) ]\n/// 照合\n#[allow(clippy::float_cmp)] #[rustfmt::skip]\npub(crate) mod oracle_tests { fn g() { crate::analysis::loudness::analyze(a, mon); } }",
    ] {
        assert_eq!(found(src), Vec::<String>::new(), "{src}");
    }
}
