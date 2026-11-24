//! 流し解きを区切りで進める仕事（M17 段 3。docs/editing.md §18.3）。
//!
//! 出力を描くのに要る申告した解析のうち、キャッシュにないもの（[`SolveTask`]。[`Engine::missing_solves`]）を、上流から順に、
//! 入力のポートをチャンクの区間ごとに流して解く（[`PresolveJob::step`]）。描画の中の流し解き（`Engine::presolve`）と同じ
//! 実装（`solve_port`・`finish_solving`）を通り、結果は区切り方によらず `Analysis::run` とビット単位で同じ（E35）。
//!
//! - 窓は `stream_port` の窓（頭から順、`STREAM_CHUNKS` まで）。中止を見るのは窓を描く側だけで、受け手（`AudioStream::feed`）
//!   には中止しない監視を渡す（受け手の状態は窓の境でしか止まらない）。続きは受け手が食べ終えた位置から流す。
//! - 解けた結果は実行器と同じ鍵（`analysis_fingerprint`）でキャッシュに入れ、仕事の中の表（`Presolved`）にも区切りをまたいで
//!   持つ（後ろの仕事の窓を描くとき、上流の結果を窓の中で解き直さない。キャッシュから追い出されても同じ）。
//! - 入力に直結した申告（描画の中では値に `run` を当てる）も流して解く（E35 で同じ値）。
//! - 仕事を作った計画と違う計画で進めると誤り（鍵を引き直して照らす）。
//! - 内部のポート（ノードの入力 0・出力）を流す前に解くもの（[`Engine::missing_port_solves`]。M17 段 4）も同じ走査を、出力では
//!   なくそのポートから行う（`Engine::stream_node_port` は、これが空でなければ錠の中で全体を解かずに断る）。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use super::area::{PassCx, Presolved};
use super::audio::{Solving, StreamStats, port_analysis_key, start_solving};
use super::cache::{Fingerprint, FingerprintedInput, NodeCache};
use super::port::{PortSide, node_port};
use super::region::Spans;
use super::{Engine, Plan, bad_input};
use crate::analysis::{AnyOutput, Need};
use crate::doc::{InputId, NodeId, OutputId, PortRef};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::AUDIO_CHUNK;

/// 流して解く申告した解析 1 つ。
#[derive(Debug, Clone)]
pub struct SolveTask {
    /// 申告したノード。
    pub node: NodeId,
    /// 解析の鍵（`analysis_fingerprint`）。
    pub key: Fingerprint,
    /// 申告（入力の番号・解析の ID と版・正規の引数）。
    pub need: Need,
    /// 入力のポートの長さ（フレーム）。
    pub frames: u64,
    /// 流し解きの推定（ns / フレーム。`Analysis::solve_ns_per_frame`）。
    pub ns_per_frame: f64,
    port: PortRef,
    shape: Shape,
}

impl SolveTask {
    /// 流し解きの推定時間（ns）。
    pub fn estimated_ns(&self) -> f64 {
        self.frames as f64 * self.ns_per_frame
    }
}

/// 出力を描くのに要る、キャッシュにない申告した解析（[`Engine::missing_solves`]）。
#[derive(Debug, Clone, Default)]
pub struct MissingSolves {
    /// 流して解けるもの（上流から順。同じ鍵は 1 つ）。
    pub tasks: Vec<SolveTask>,
    /// 流して解けないもの（長さの分からない入力・流れを持たない解析）があれば true（区切りでは解けない。描画の中で解く）。
    pub unstreamable: bool,
    /// 出力を描くのに要るすべての解析（キャッシュにあるものも。上流から順。同じ鍵は 1 つ）の鍵と、キャッシュで数える大きさ
    /// （解析の見積もり）。エディタが仕事を見分け（同じものを要る間は続ける）、公開した計画の結果を握るのに使う。
    pub needed: Vec<(Fingerprint, u64)>,
}

impl MissingSolves {
    /// 推定時間の和（ns）。
    pub fn estimated_ns(&self) -> f64 {
        self.tasks.iter().map(SolveTask::estimated_ns).sum()
    }

    /// 鍵の列（上流から順）。
    pub fn keys(&self) -> Vec<Fingerprint> {
        self.tasks.iter().map(|t| t.key).collect()
    }
}

/// 解けた解析（鍵・結果・キャッシュで数える大きさ）。
#[derive(Debug, Clone)]
pub struct Solved {
    pub key: Fingerprint,
    pub out: AnyOutput,
    pub bytes: u64,
}

/// 区切り 1 回の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// すべて解けた。
    pub done: bool,
    /// 流し終えたフレーム（仕事の和）。
    pub frames_done: u64,
    /// 流すフレームの和。
    pub frames_total: u64,
    /// この区切りで流したチャンクの数。
    pub chunks: u64,
    /// この区切りの流れの数（窓・範囲の描画の和）。
    pub stream: StreamStats,
}

/// 区切りで進める流し解き（借用を持たない。`Send`・`Sync`：流れ（`AudioStream` は `Sync` でない）は錠に入れ、`step` の中は
/// `&mut self` で錠を取らずに触る）。[`Engine::missing_solves`] の仕事から作る。
pub struct PresolveJob {
    tasks: Vec<SolveTask>,
    at: usize,
    running: Mutex<Option<Solving>>,
    presolved: Presolved,
    finished: Vec<Solved>,
    frames_done: u64,
}

impl PresolveJob {
    /// 仕事（上流から順）から作る。
    pub fn new(missing: MissingSolves) -> Self {
        Self {
            tasks: missing.tasks,
            at: 0,
            running: Mutex::new(None),
            presolved: Presolved::new(),
            finished: Vec::new(),
            frames_done: 0,
        }
    }

    /// キャッシュにある要る結果（`missing.needed` のうち仕事にならなかったもの）を仕事の中の表に入れる（キャッシュから追い
    /// 出されても、後ろの仕事の窓の描画が上流を入れ子に解き直さない）。`needed` は [`MissingSolves::needed`]。
    pub fn seed(&mut self, needed: &[(Fingerprint, u64)], cache: &NodeCache) {
        for (key, _) in needed {
            if let Some(out) = cache.peek_analysis(key) {
                self.presolved.insert(*key, out);
            }
        }
    }

    /// 前の仕事 `old` が進めていた流しを、同じ鍵の仕事を最初に解くなら引き継ぐ（下流の申告を足した手などで、上流の作りかけを
    /// 捨てない）。引き継いだら true。
    pub fn adopt(&mut self, old: &mut PresolveJob) -> bool {
        let (Some(first), Some(prev)) = (self.tasks.get(self.at), old.tasks.get(old.at)) else {
            return false;
        };
        if first.key != prev.key {
            return false;
        }
        let taken = old
            .running
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(solving) = taken else {
            return false;
        };
        *self
            .running
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Some(solving);
        for (k, v) in &old.presolved {
            self.presolved.entry(*k).or_insert_with(|| v.clone());
        }
        true
    }

    /// 今の仕事の残りのチャンクの数（なければ 0）。
    pub fn left_chunks(&self) -> u64 {
        let Some(t) = self.tasks.get(self.at) else {
            return 0;
        };
        t.frames
            .saturating_sub(self.pos())
            .div_ceil(AUDIO_CHUNK as u64)
    }

    /// 仕事（上流から順）。
    pub fn tasks(&self) -> &[SolveTask] {
        &self.tasks
    }

    /// 鍵の列（上流から順）。
    pub fn keys(&self) -> Vec<Fingerprint> {
        self.tasks.iter().map(|t| t.key).collect()
    }

    /// すべて解けたか。
    pub fn is_done(&self) -> bool {
        self.at >= self.tasks.len()
    }

    /// 解けた解析（解けた順）。
    pub fn finished(&self) -> &[Solved] {
        &self.finished
    }

    /// （流し終えたフレーム, 流すフレームの和）。
    pub fn progress(&self) -> (u64, u64) {
        let running = self.pos();
        let total = self.tasks.iter().map(|t| t.frames).sum::<u64>();
        (self.frames_done.saturating_add(running), total)
    }

    /// 進めている仕事を最後まで流したときの結果の大きさの見積もり（作りかけの結果を持つ量の上の見積もり。なければ 0）と、
    /// 解けた結果の大きさの和。
    pub fn held_bytes(&self, engine: &Engine) -> u64 {
        let solved = self
            .finished
            .iter()
            .fold(0u64, |a, s| a.saturating_add(s.bytes));
        let running = match (self.is_running(), self.tasks.get(self.at)) {
            (true, Some(t)) => engine
                .analyses
                .get(t.need.id, t.need.v)
                .and_then(|a| a.estimate(&t.need.params, &t.shape).ok())
                .map_or(0, |e| e.bytes),
            _ => 0,
        };
        solved.saturating_add(running)
    }

    /// 進めている仕事の食べ終えた位置（なければ 0）。
    fn pos(&self) -> u64 {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or(0, |r| r.pos)
    }

    /// 仕事を進めている途中か。
    fn is_running(&self) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// 次に流す入力のポートとその区間（`chunks` チャンクまで。区切りの前の確保の見積もり `EstimateSpec::port_area` に使う。
    /// 仕事が残っていなければ None）。
    pub fn next_span(&self, chunks: u64) -> Option<(PortRef, (u64, u64))> {
        let t = self.tasks.get(self.at)?;
        let pos = self.pos();
        let c = AUDIO_CHUNK as u64;
        let end = pos
            .saturating_add(chunks.max(1).saturating_mul(c))
            .min(t.frames);
        Some((t.port, (pos, end)))
    }

    /// `chunks` チャンクまで流して進める（窓の途中では止めない。区間はチャンクに揃える）。解き終えた仕事の結果はキャッシュ
    /// （`analysis_fingerprint` の鍵）と仕事の中の表に入れ、次の仕事へ進む（同じ区切りの中で続ける）。中止されたら、渡し終えた
    /// 窓の終わりまで進めて `Canceled` を返す（次の `step` で続きから）。計画・入力は仕事を作ったときと同じもの（鍵を照らす）。
    pub fn step(
        &mut self,
        engine: &Engine,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        chunks: u64,
        mon: &dyn Monitor,
    ) -> Result<Progress, EngineError> {
        engine.check_stream_plan(plan, inputs, cache)?;
        let cx = PassCx::new(engine, plan, inputs)?;
        let c = AUDIO_CHUNK as u64;
        let mut left = chunks;
        let mut used = 0u64;
        let mut stream = StreamStats::default();
        let running = self
            .running
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        while let Some(task) = self.tasks.get(self.at) {
            let key = port_analysis_key(&cx, &task.port, &task.need)?;
            if key != task.key {
                return Err(EngineError::Internal(
                    "流し解きの仕事を作った計画と違う計画です".into(),
                ));
            }
            let a = engine.solvable(&task.need)?;
            if running.is_none() {
                // ほかの描画が解いていれば使う
                if let Some(out) = cache.peek_analysis(&task.key) {
                    let bytes = a
                        .estimate(&task.need.params, &task.shape)
                        .map_or(0, |e| e.bytes);
                    self.presolved.insert(task.key, out.clone());
                    self.finished.push(Solved {
                        key: task.key,
                        out,
                        bytes,
                    });
                    self.frames_done = self.frames_done.saturating_add(task.frames);
                    self.at += 1;
                    continue;
                }
                *running = Some(start_solving(a.as_ref(), &task.need, &task.shape)?);
            }
            let Some(solving) = running.as_mut() else {
                return Err(EngineError::Internal("流し解きの流れがありません".into()));
            };
            if solving.pos < task.frames {
                if left == 0 {
                    break;
                }
                let n = left.min((task.frames - solving.pos).div_ceil(c));
                let end = solving
                    .pos
                    .saturating_add(n.saturating_mul(c))
                    .min(task.frames);
                let mut spans = Spans::new();
                spans.push(solving.pos, end);
                let r = engine.solve_port(
                    &cx,
                    cache,
                    &task.port,
                    &task.shape,
                    Some(&spans),
                    solving,
                    true,
                    &mut self.presolved,
                    &mut stream.area,
                    &mut stream.windows,
                    mon,
                );
                left = left.saturating_sub(n);
                used = used.saturating_add(n);
                if let Err(e) = r {
                    // 中止のほかの誤り（源の読み・上限など）は、受け手が窓の途中まで進んでいることがある（`TableBuilder::feed`
                    // は組ごとに食べる）ので、この仕事を最初からやり直す
                    if !matches!(e.root(), EngineError::Canceled) {
                        *running = None;
                    }
                    return Err(e);
                }
            }
            if solving.pos < task.frames {
                break;
            }
            if solving.info.is_none() {
                // 長さ 0：空の範囲の窓で形とメタデータを決める
                engine.solve_port(
                    &cx,
                    cache,
                    &task.port,
                    &task.shape,
                    None,
                    solving,
                    true,
                    &mut self.presolved,
                    &mut stream.area,
                    &mut stream.windows,
                    mon,
                )?;
            }
            let Some(solving) = running.take() else {
                return Err(EngineError::Internal("流し解きの流れがありません".into()));
            };
            let out = engine.finish_solving(
                cache,
                a.as_ref(),
                &task.need,
                &task.shape,
                task.key,
                solving,
                mon,
            )?;
            let bytes = a
                .estimate(&task.need.params, &task.shape)
                .map_or(0, |e| e.bytes);
            self.presolved.insert(task.key, out.clone());
            self.finished.push(Solved {
                key: task.key,
                out,
                bytes,
            });
            self.frames_done = self.frames_done.saturating_add(task.frames);
            self.at += 1;
        }
        let (frames_done, frames_total) = self.progress();
        Ok(Progress {
            done: self.is_done(),
            frames_done,
            frames_total,
            chunks: used,
            stream,
        })
    }
}

impl Engine {
    /// 計画の出力 `output` を描くのに要る申告した解析のうち、キャッシュ（外の解析を含む）にないもの（上流から順。同じ鍵は
    /// 1 つ）。キャッシュの使った順は変えない。
    pub fn missing_solves(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &NodeCache,
        output: OutputId,
    ) -> Result<MissingSolves, EngineError> {
        self.check_solve_plan(plan, inputs)?;
        let Some((_, from, _, _)) = plan.outputs.iter().find(|o| o.0 == output) else {
            return Err(bad_input(format!("出力 {} はありません", output.0)));
        };
        let cx = PassCx::new(self, plan, inputs)?;
        self.missing_from(&cx, cache, *from)
    }

    /// ノード `node` の入力 0・出力のポート（`side`）を流すのに要る申告した解析のうち、キャッシュにないもの（
    /// [`Engine::missing_solves`] と同じ走査を、出力ではなくそのポートから。`Engine::stream_node_port` はこれが空でなければ
    /// 断るので、先にこの仕事を [`PresolveJob`] で解く）。キャッシュの使った順は変えない。
    pub fn missing_port_solves(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &NodeCache,
        node: NodeId,
        side: PortSide,
    ) -> Result<MissingSolves, EngineError> {
        self.check_solve_plan(plan, inputs)?;
        let from = node_port(plan, node, side)?;
        let cx = PassCx::new(self, plan, inputs)?;
        self.missing_from(&cx, cache, from)
    }

    /// 解く仕事を数える口の前の確かめ（このエンジンの計画・入力の数）。
    fn check_solve_plan(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
    ) -> Result<(), EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        Ok(())
    }

    /// ポート `from` の値を描くのに要る申告した解析のうち、キャッシュにないもの（`from` に届くノードを計画の順に見る。
    /// [`Engine::missing_solves`]・[`Engine::missing_port_solves`] と、`Engine::stream_node_port` の判定が共有する走査）。
    pub(super) fn missing_from(
        &self,
        cx: &PassCx<'_>,
        cache: &NodeCache,
        from: PortRef,
    ) -> Result<MissingSolves, EngineError> {
        let plan = cx.plan;
        // ポートに届くノード
        let mut reach = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(p) = stack.pop() {
            let PortRef::Node { id } = p else { continue };
            if !reach.insert(id) {
                continue;
            }
            if let Some(n) = cx.index.get(&id).and_then(|i| plan.nodes.get(*i)) {
                stack.extend(n.inputs.iter().copied());
            }
        }
        let mut out = MissingSolves::default();
        let mut seen = BTreeSet::new();
        for node in plan.nodes.iter().filter(|n| reach.contains(&n.id)) {
            for need in &node.needs {
                let Some(r) = node.inputs.get(need.input) else {
                    return Err(EngineError::Internal(
                        "解析する入力のポートがありません".into(),
                    ));
                };
                let key = port_analysis_key(cx, r, need)?;
                if !seen.insert(key) {
                    continue;
                }
                let a = self.solvable(need)?;
                let shape = cx.shapes.port(r);
                let bytes = match shape {
                    Some(s) => a.estimate(&need.params, &s).map_or(0, |e| e.bytes),
                    None => 0,
                };
                out.needed.push((key, bytes));
                if cache.contains_analysis(&key) {
                    continue;
                }
                let streamable = match shape {
                    Some(
                        s @ Shape::Audio {
                            frames: Dim::Exact(_),
                            ..
                        },
                    ) => a.stream_any(&need.params, &s)?.is_some(),
                    _ => false,
                };
                let Some(
                    shape @ Shape::Audio {
                        frames: Dim::Exact(frames),
                        ..
                    },
                ) = shape.filter(|_| streamable)
                else {
                    out.unstreamable = true;
                    continue;
                };
                out.tasks.push(SolveTask {
                    node: node.id,
                    key,
                    need: need.clone(),
                    frames,
                    ns_per_frame: a.solve_ns_per_frame(&need.params, &shape)?,
                    port: *r,
                    shape,
                });
            }
        }
        Ok(out)
    }

    /// 流す口の前の確かめ（`stream_audio` と同じ：このエンジンの等倍の計画・入力の数と形・キャッシュの縛り）。
    pub(super) fn check_stream_plan(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
    ) -> Result<(), EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        if plan.level != 0 {
            return Err(EngineError::Unsupported(
                "縮小の段の計画は流せません（等倍の計画で描きます）".into(),
            ));
        }
        cache.bind_engine(self.token)?;
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        self.check_inputs(plan, |id| inputs.get(&id).map(|v| &v.value))
    }
}
