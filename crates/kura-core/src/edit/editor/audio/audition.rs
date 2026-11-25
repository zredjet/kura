//! 試聴だけの重ね（M17 段 6。docs/editing.md §18.7）：一時的に外して聞く音（試聴の計画）。
//!
//! - **試聴の計画**：頭の文書（仮の値を含む）の写しで、頼まれたノードを外した計画（[`compile_without`]）。文書・rev・取り消しの
//!   列は変えない。(rev, ノード) で覚える。外したノードの指紋は入力 0 の指紋なので、外したノードの引数を変えても試聴の出力の
//!   指紋（`auditionId`）は変わらない。
//! - **描く**：表示を描いたあと、試聴の計画で、短い音声は全体・長い音声は頼まれた区間（[`AuditionRequest::spans`]。再生の
//!   先読み）を、試聴用の前回の表示を土台に描き、id で引ける置き場に公開する（`/edit/{s}/audition/{id}/pcm/{a}-{b}`）。
//!   計画が要る解析の結果はキャッシュに外の解析として握る（表示の描画に追い出されて解き直さない）。
//! - **流し解き**：試聴の計画に重い流し解き（推定が [`SYNC_SOLVE_NS`] を超えるか、長い音声）が要るときは、裏の仕事の区切りで
//!   表示より先に進める（聞こえる音を先に替える）。試聴の握る計画は持たない：解き終えるまでは `auditionId` を表示の
//!   `samplesId` にして、外していない音を鳴らす（[`AuditionPhase::Solving`]）。
//! - 文書にないノード・外すと出力の形（レート・チャンネル数・長さ）が変わるノード・外すと計画を作れない文書は外せない
//!   （[`AuditionPhase::Unavailable`]）。もう外してあるノードは表示と同じ音。
//! - 頼みに試聴がなくなったら、試聴の状態（計画・流し解き・握った結果・公開した値）を片付ける。

use kura_engine::analysis::AnyOutput;
use kura_engine::doc::NodeId;
use kura_engine::monitor::Ticket;
use kura_engine::render::Plan;
use kura_engine::render::cache::Fingerprint;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::presolve::{MissingSolves, PresolveJob};
use kura_engine::render::region::Area;
use kura_engine::render::view::{View, ViewRequest};
use kura_engine::{EngineError, Shape};

use super::super::view::AudioViewport;
use super::super::{Editor, State, compile_without, settle};
use super::measure::{LoudnessState, MeasureTarget};
use super::slice::{Limit, Pace, WorkProgress};
use super::solve::{Advanced, SYNC_SOLVE_NS, SolveRun, restore_finished};
use super::{AudioViewInfo, audio_shape, is_short, samples_id, valid_runs, view_area};

/// 試聴の頼み（表示の頼みに添える）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditionRequest {
    /// 外すノード。
    pub bypass: NodeId,
    /// 描いて配ってほしい区間（再生の先読み。長い音声だけ使う。表示と同じく 4 つ・48 チャンクまで）。
    pub spans: Vec<(u64, u64)>,
}

/// 試聴の様子。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AuditionPhase {
    /// 外した音を配れる（`auditionId` の置き場から取る）。
    Ready,
    /// 外した音の流し解きを裏で進めている（進み 0〜1）。その間の `auditionId` は表示の `samplesId`（外していない音）。
    Solving(f64),
    /// 外せない（文書にないノード・外すと出力の形が変わる・計画を作れない）。`auditionId` は表示の `samplesId`。
    Unavailable,
}

/// 試聴の情報（表示の情報に添える）。
#[derive(Debug, Clone, PartialEq)]
pub struct AuditionInfo {
    /// 外すノード。
    pub node: NodeId,
    pub phase: AuditionPhase,
    /// 試聴の値の有効な区間（`/edit/{s}/audition/{auditionId}/pcm` で取れる所。チャンクに揃う。外したノードがもう外して
    /// あるときは表示の `valid`）。
    pub valid: Vec<(u64, u64)>,
    /// 外して聞く音のラウドネス（`edit_measure` の `audition` で計ったもの。`auditionId` で覚える。計っていなければ
    /// `Idle`。もう外してあるノードは出力のもの）。
    pub loudness: LoudnessState,
}

/// 外したときの計画。
#[derive(Clone)]
pub(crate) enum Built {
    /// もう外してある（表示と同じ音）。
    Same,
    /// 外した計画。
    Plan(Plan),
    /// 外せない。
    Unavailable,
}

/// 開いている音声のエディタの試聴の状態（`AudioState::audition`）。
#[derive(Default)]
pub(crate) struct AuditionState {
    /// 計画を作った (rev, 外すノード) と、その計画。
    pub(crate) built: Option<(u64, NodeId, Built)>,
    /// 試聴の前回の表示（`render_view` の土台。表示の `st.view` とは分ける）。
    pub(crate) view: Option<View>,
    /// 裏で進める試聴の計画の流し解き。
    pub(crate) solve: Option<SolveRun>,
    /// 誤りが続いて捨てた流し解きの要る鍵の列（その間は錠の中で解く）。
    pub(crate) solve_failed: Option<Vec<Fingerprint>>,
    /// 試聴の計画が要る解析の結果（キャッシュに外の解析として握る。キャッシュを空にしたあとも入れ直す）。
    pub(crate) tables: Vec<(Fingerprint, AnyOutput, u64)>,
}

impl AuditionState {
    /// 流し解きの進み（なければ 0）。
    fn progress(&self) -> f64 {
        self.solve.as_ref().map_or(0.0, SolveRun::progress)
    }
}

/// この頼みで試聴をどう扱うか（[`Editor::prepare_audition`] が決める）。
pub(crate) enum Prep {
    /// 表示と同じ音（もう外してある）。
    Same(NodeId),
    /// 外せない。
    Unavailable(NodeId),
    /// 外した計画で描く（要る結果がそろっている・境の内）。
    Now(NodeId, Plan),
    /// 外した計画の流し解きを裏で進める（解き終えるまで外していない音）。
    Background(NodeId, Plan),
}

/// 描けなかったわけ（呼び手が `Superseded`・`Canceled` にする）。
pub(crate) struct Interrupted;

impl Editor {
    /// 頼みに合わせて試聴の計画を用意し、描き方を決める（頼みに試聴がなければ片付けて None）。
    pub(in crate::edit::editor) fn prepare_audition(
        &self,
        st: &mut State,
        avp: &AudioViewport,
        full: &Shape,
    ) -> Option<Prep> {
        let Some(req) = avp.audition.as_ref() else {
            self.end_audition(st);
            return None;
        };
        let node = req.bypass;
        let rev = st.session.rev().0;
        let current = st
            .audio
            .as_ref()
            .and_then(|a| a.audition.built.as_ref())
            .filter(|(r, n, _)| *r == rev && *n == node)
            .map(|(_, _, b)| b.clone());
        let built = match current {
            Some(b) => b,
            None => {
                let built = self.build_audition(st, node, full);
                if let Some(a) = st.audio.as_mut() {
                    if a.audition
                        .built
                        .as_ref()
                        .is_some_and(|(_, n, _)| *n != node)
                    {
                        // 外すノードが替わった：土台の表示と流し解きを捨てる
                        a.audition.view = None;
                        a.audition.solve = None;
                        a.audition.solve_failed = None;
                    }
                    a.audition.built = Some((rev, node, built.clone()));
                }
                built
            }
        };
        let plan = match built {
            Built::Same => {
                self.drop_audition_work(st);
                return Some(Prep::Same(node));
            }
            Built::Unavailable => {
                self.drop_audition_work(st);
                return Some(Prep::Unavailable(node));
            }
            Built::Plan(plan) => plan,
        };
        Some(self.audition_gate(st, node, plan, full))
    }

    /// 外したときの計画を作る（モジュールの説明）。
    fn build_audition(&self, st: &State, node: NodeId, full: &Shape) -> Built {
        let doc = st.session.current();
        match self
            .runtime
            .preview(|e| compile_without(e, doc, &st.shapes, node))
        {
            None => {
                if doc.to_graph().nodes.iter().any(|n| n.id == node) {
                    Built::Same
                } else {
                    Built::Unavailable
                }
            }
            Some(Ok(plan)) => {
                let shape = plan
                    .output_shapes()
                    .into_iter()
                    .find(|(o, _)| *o == st.output)
                    .map(|(_, s)| s);
                if shape.as_ref() == Some(full) {
                    Built::Plan(plan)
                } else {
                    // 外すと出力の形が変わる（再生の形が合わなくなる）
                    Built::Unavailable
                }
            }
            Some(Err(issues)) => {
                tracing::debug!(?issues, "ノードを外した文書の計画を作れません");
                Built::Unavailable
            }
        }
    }

    /// 試聴の計画を今描くか、流し解きを裏で進めるか（表示の `solve_gate` と同じ境。握る計画がなくても裏にする）。
    fn audition_gate(&self, st: &mut State, node: NodeId, plan: Plan, full: &Shape) -> Prep {
        let missing = match self
            .runtime
            .preview(|e| e.missing_solves(&plan, &st.inputs, &st.cache, st.output))
        {
            Ok(m) => m,
            Err(err) => {
                // 誤りは描画で知らせる
                tracing::debug!(%err, "試聴の計画の要る解析を求められません");
                self.drop_audition_solve(st);
                return Prep::Now(node, plan);
            }
        };
        if missing.tasks.is_empty() || missing.unstreamable {
            self.drop_audition_solve(st);
            return Prep::Now(node, plan);
        }
        let heavy = !is_short(full) || missing.estimated_ns() > SYNC_SOLVE_NS;
        let needed: Vec<Fingerprint> = missing.needed.iter().map(|(k, _)| *k).collect();
        let failed = st.audio.as_mut().is_some_and(|a| {
            if a.audition
                .solve_failed
                .as_ref()
                .is_some_and(|f| *f != needed)
            {
                a.audition.solve_failed = None;
            }
            a.audition.solve_failed.is_some()
        });
        if !heavy || failed {
            self.drop_audition_solve(st);
            return Prep::Now(node, plan);
        }
        let Some(a) = st.audio.as_mut() else {
            return Prep::Now(node, plan);
        };
        if a.audition.solve.as_ref().is_none_or(|s| s.needed != needed) {
            let mut job = PresolveJob::new(MissingSolves {
                needed: missing.needed.clone(),
                ..missing
            });
            job.seed(&missing.needed, &st.cache);
            let mut pace = Pace::default();
            if let Some(mut old) = a.audition.solve.take()
                && job.adopt(&mut old.job)
            {
                pace = old.pace;
            }
            a.audition.solve = Some(SolveRun {
                needed,
                job,
                pace,
                errors: 0,
            });
        }
        Prep::Background(node, plan)
    }

    /// 試聴の流し解きを、表示より先に区切り `limit` だけ進める（解き終えたら今描く。誤りが続いたら錠の中で解く）。
    pub(in crate::edit::editor) fn advance_audition(
        &self,
        st: &mut State,
        prep: &mut Option<Prep>,
        limit: Option<&mut Limit>,
        progress: &mut WorkProgress,
        ticket: &Ticket,
    ) -> Result<(), EngineError> {
        let (Some(Prep::Background(node, plan)), Some(limit)) = (prep.as_ref(), limit) else {
            return Ok(());
        };
        let (node, plan) = (*node, plan.clone());
        let Some(mut run) = st.audio.as_mut().and_then(|a| a.audition.solve.take()) else {
            return Ok(());
        };
        let r = self.advance_run(st, &plan, &mut run, limit, progress, ticket);
        match r {
            Ok(Advanced::Done) => {
                restore_finished(st, run.job.finished().to_vec());
                *prep = Some(Prep::Now(node, plan));
            }
            Ok(Advanced::Failed) => {
                tracing::warn!("試聴の流し解きの誤りが続いたので、錠の中で解きます");
                if let Some(a) = st.audio.as_mut() {
                    a.audition.solve_failed = Some(run.needed);
                }
                *prep = Some(Prep::Now(node, plan));
            }
            Ok(Advanced::Pending) => {
                if let Some(a) = st.audio.as_mut() {
                    a.audition.solve = Some(run);
                }
            }
            Err(e) => {
                if let Some(a) = st.audio.as_mut() {
                    a.audition.solve = Some(run);
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// 表示の情報に試聴を添える（`Now` なら試聴の計画で描いて公開する）。描いている途中で取って代わられたら `Interrupted`。
    pub(in crate::edit::editor) fn attach_audition(
        &self,
        st: &mut State,
        info: &mut AudioViewInfo,
        prep: Option<Prep>,
        avp: &AudioViewport,
        full: &Shape,
        ticket: &Ticket,
    ) -> Result<(), Interrupted> {
        info.audition_id = info.samples_id.clone();
        let Some(prep) = prep else {
            info.audition = None;
            return Ok(());
        };
        let (node, plan) = match prep {
            Prep::Same(node) => {
                info.audition = Some(AuditionInfo {
                    node,
                    phase: AuditionPhase::Ready,
                    valid: info.valid.clone(),
                    loudness: info.loudness.clone(),
                });
                return Ok(());
            }
            Prep::Unavailable(node) => {
                info.audition = Some(AuditionInfo {
                    node,
                    phase: AuditionPhase::Unavailable,
                    valid: Vec::new(),
                    loudness: LoudnessState::Idle,
                });
                return Ok(());
            }
            Prep::Background(node, _) => {
                let progress = st.audio.as_ref().map_or(0.0, |a| a.audition.progress());
                info.audition = Some(AuditionInfo {
                    node,
                    phase: AuditionPhase::Solving(progress),
                    valid: Vec::new(),
                    loudness: LoudnessState::Idle,
                });
                info.work = true;
                self.count_resident(st);
                return Ok(());
            }
            Prep::Now(node, plan) => (node, plan),
        };
        let spans = avp
            .audition
            .as_ref()
            .map(|r| r.spans.clone())
            .unwrap_or_default();
        let area = view_area(
            full,
            &AudioViewport {
                spans,
                ..AudioViewport::idle()
            },
        );
        let frames = audio_shape(full).map_or(0, |(_, _, n)| n);
        let id = match self.audition_id(st, &plan) {
            Some(id) => id,
            None => {
                info.audition = Some(AuditionInfo {
                    node,
                    phase: AuditionPhase::Unavailable,
                    valid: Vec::new(),
                    loudness: LoudnessState::Idle,
                });
                return Ok(());
            }
        };
        let mut valid = Vec::new();
        if !matches!(area, Area::Empty) {
            let spec = EstimateSpec::new(0)
                .all_inputs(&plan, Source::Held)
                .area(st.output, area.clone());
            if let Err(err) =
                self.runtime
                    .admit_preview(&plan, spec, &st.inputs, &mut st.cache, &mut st.lease)
            {
                // 予算が空いていない：次の裏の仕事の頼みで描き直す（それまで外していない音）
                tracing::debug!(%err, "試聴の描画の確保が空いていないので、あとにします");
                settle(st);
                info.audition = Some(AuditionInfo {
                    node,
                    phase: AuditionPhase::Solving(0.0),
                    valid: Vec::new(),
                    loudness: LoudnessState::Idle,
                });
                info.work = true;
                return Ok(());
            }
            let req = ViewRequest {
                output: st.output,
                area,
                focus: None,
            };
            let prev = st.audio.as_ref().and_then(|a| a.audition.view.as_ref());
            let (inputs, cache) = (&st.inputs, &mut st.cache);
            let r = self
                .runtime
                .preview(|e| e.render_view(&plan, inputs, cache, &req, prev, ticket));
            settle(st);
            let vr = match r {
                Ok(vr) => vr,
                Err(e) if matches!(e.root(), EngineError::Canceled) => return Err(Interrupted),
                Err(err) => {
                    tracing::warn!(%err, "外した音を描けません");
                    info.audition = Some(AuditionInfo {
                        node,
                        phase: AuditionPhase::Unavailable,
                        valid: Vec::new(),
                        loudness: LoudnessState::Idle,
                    });
                    return Ok(());
                }
            };
            valid = valid_runs(vr.view.value(), frames);
            self.publish_audition(id.clone(), vr.view.clone());
            if let Some(a) = st.audio.as_mut() {
                a.audition.view = Some(vr.view);
            }
            self.pin_audition(st, &plan);
        }
        // 外して聞く音が替わった（操作中でない確定の表示）：前の外して聞く音の計りかけを捨てる。表示の work には計りかけも数える
        let settled = !avp.interacting && !st.session.is_provisional();
        let loudness = match st.audio.as_mut() {
            Some(a) => {
                if settled {
                    a.measure.retarget_role(MeasureTarget::Audition, &id);
                }
                if a.measure.running(&id) {
                    info.work = true;
                }
                a.measure.state(&id)
            }
            None => LoudnessState::Idle,
        };
        info.audition_id = id;
        info.audition = Some(AuditionInfo {
            node,
            phase: AuditionPhase::Ready,
            valid,
            loudness,
        });
        self.count_resident(st);
        Ok(())
    }

    /// 試聴の計画の出力の指紋（`auditionId`）。
    fn audition_id(&self, st: &State, plan: &Plan) -> Option<String> {
        match self.runtime.preview(|e| e.fingerprints(plan, &st.inputs)) {
            Ok(fps) => fps
                .outputs
                .iter()
                .find(|(o, _)| *o == st.output)
                .map(|(_, fp)| samples_id(fp)),
            Err(err) => {
                tracing::warn!(%err, "外した音の指紋を求められません");
                None
            }
        }
    }

    /// 試聴の計画が要る解析の結果を握る（外の解析としてキャッシュに入れる。要らなくなったものは、表示が握っていなければ
    /// ふつうの項目に戻す）。
    fn pin_audition(&self, st: &mut State, plan: &Plan) {
        let needed = match self
            .runtime
            .preview(|e| e.missing_solves(plan, &st.inputs, &st.cache, st.output))
        {
            Ok(m) => m.needed,
            Err(err) => {
                tracing::debug!(%err, "試聴の握る解析を求められません");
                Vec::new()
            }
        };
        let mut tables = Vec::new();
        for (key, bytes) in needed {
            if let Some(out) = st.cache.peek_analysis(&key) {
                st.cache.register_external_analysis(key, out.clone(), bytes);
                tables.push((key, out, bytes));
            }
        }
        let old = st
            .audio
            .as_mut()
            .map(|a| std::mem::replace(&mut a.audition.tables, tables))
            .unwrap_or_default();
        self.release_audition_tables(st, old);
        settle(st);
    }

    /// 試聴が握っていた結果のうち、今の試聴・表示の握る計画が握っていないものを、ふつうの項目に戻す。
    fn release_audition_tables(&self, st: &mut State, old: Vec<(Fingerprint, AnyOutput, u64)>) {
        for (k, _, _) in old {
            let kept = st.audio.as_ref().is_some_and(|a| {
                a.audition.tables.iter().any(|(t, _, _)| *t == k)
                    || a.held
                        .as_ref()
                        .is_some_and(|h| h.tables.iter().any(|(t, _, _)| *t == k))
            });
            if !kept {
                st.cache.demote_external_analysis(&k);
            }
        }
    }

    /// 試聴の流し解きを捨てる。
    fn drop_audition_solve(&self, st: &mut State) {
        if let Some(a) = st.audio.as_mut() {
            a.audition.solve = None;
        }
    }

    /// 試聴の流し解き・握った結果・土台の表示を捨てる（外せない・もう外してあるとき）。
    fn drop_audition_work(&self, st: &mut State) {
        let old = st
            .audio
            .as_mut()
            .map(|a| {
                a.audition.solve = None;
                a.audition.view = None;
                std::mem::take(&mut a.audition.tables)
            })
            .unwrap_or_default();
        self.release_audition_tables(st, old);
    }

    /// 頼みに試聴がなくなった：試聴の状態と公開した値を片付ける。
    fn end_audition(&self, st: &mut State) {
        let active = st
            .audio
            .as_ref()
            .is_some_and(|a| a.audition.built.is_some() || !a.audition.tables.is_empty());
        if !active {
            return;
        }
        self.drop_audition_work(st);
        if let Some(a) = st.audio.as_mut() {
            a.audition = AuditionState::default();
        }
        self.clear_audition();
        settle(st);
    }

    /// キャッシュを空にしたあと（保存の駐車）、試聴が握っている結果を入れ直す。
    pub(in crate::edit::editor) fn rehold_audition(&self, st: &mut State) {
        let tables: Vec<_> = st
            .audio
            .as_ref()
            .map(|a| a.audition.tables.clone())
            .unwrap_or_default();
        for (k, out, bytes) in tables {
            st.cache.register_external_analysis(k, out, bytes);
        }
    }
}
