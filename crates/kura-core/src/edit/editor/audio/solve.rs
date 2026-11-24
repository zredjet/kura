//! 長い音声の流し解き（M17 段 3。docs/editing.md §18.3・§12.12）。
//!
//! 状態を運ぶ操作（`audio.eq`）や正規化は、引数・上流が変わるたびに申告した解析（状態の表・ゲートの計り）を入力の全体に
//! 流し直す。その推定時間（`Analysis::solve_ns_per_frame` の和）が [`SYNC_SOLVE_NS`] を超えるか、出力が 32 MiB を超える
//! （`is_short` でない）ときは、手ごとに錠の中で解かず：
//!
//! - 流し解きは、頭の計画の要る結果の鍵の列で見分ける仕事（[`SolveRun`]。`PresolveJob`）にして、`work: true` の頼みの
//!   区切り（`WORK_SLICE`）で進める（操作中でも進める）。同じ鍵の列を要る間は続け、違えば作り直す（最新の手だけ）。
//! - 解き終えるまでは、最後に公開した rev の計画（[`Held`]。その計画の申告した解析の結果をキャッシュの外の解析として握る）
//!   で頼まれた区間を描き、同じ古い rev で公開する（表示と試聴の先読みを途切れさせない）。波形の段・計りは進めない。
//! - 解き終えたら今の計画で描いて新しい rev を公開し（前の表示からの `changed` で今の交差フェード）、握る計画を替える。
//!
//! 前に公開した表示がない（開いた直後）・握る計画の結果がそろっていない・流して解けない解析があるときは、今までどおり錠の
//! 中で解く。

use std::time::Instant;

use kura_engine::audio::source::read_through;
use kura_engine::render::Plan;
use kura_engine::render::cache::Fingerprint;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::presolve::{MissingSolves, PresolveJob};
use kura_engine::render::region::Area;
use kura_engine::session::Snapshot;
use kura_engine::{EngineError, Monitor};

use super::super::{Editor, State, settle};
use super::is_short;
use super::slice::{Limit, Pace, WorkProgress};

/// 手ごとに錠の中で解く流し解きの推定時間の上限（ns）：NFR-11 のエンジンの中の 25 ms（段 2 の実測で決めた境。
/// docs/editing.md §18.5 の「手ごとに正で描く境」）。
pub const SYNC_SOLVE_NS: f64 = 25_000_000.0;

/// 1 回の step に渡すチャンクの数の上限（窓の 4 つ分。区切りの中で何度か step する）。
const MAX_STEP_CHUNKS: u64 = 64;

/// 最後に公開した rev の計画と、その計画の申告した解析の結果（キャッシュに外の解析として入れて握る）。
pub(crate) struct Held {
    pub(crate) rev: u64,
    pub(crate) plan: Plan,
    /// その rev の文書（表示の情報のノード・元の写しを作る）。
    pub(crate) doc: Snapshot,
    pub(crate) samples_id: String,
    /// 握っている結果（鍵・結果・数える大きさ）。キャッシュを空にしたあとも入れ直せるように持つ。
    pub(crate) tables: Vec<(Fingerprint, kura_engine::analysis::AnyOutput, u64)>,
    /// 計画の要る結果をすべて握れた（古い計画の道に使える）。
    pub(crate) complete: bool,
}

/// 裏で進める流し解き（頭の計画の要る結果の鍵の列で見分ける）。
pub(crate) struct SolveRun {
    pub(crate) needed: Vec<Fingerprint>,
    pub(crate) job: PresolveJob,
    pub(crate) pace: Pace,
}

impl SolveRun {
    /// 進み（0〜1。流し終えたフレームの割合）。
    pub(crate) fn progress(&self) -> f64 {
        let (done, total) = self.job.progress();
        if total == 0 {
            1.0
        } else {
            done as f64 / total as f64
        }
    }
}

/// 表示をどう描くか。
pub(crate) enum Gate {
    /// 今の計画で描く（要る結果がそろっている・境の内・握る計画がない）。
    Now,
    /// 流し解きを裏で進め、握る計画で描く。
    Background,
}

/// 区切りの結果。
pub(crate) enum Advanced {
    /// 解き終えた（今の計画で描ける）。
    Done,
    /// 続きがある。
    Pending,
}

impl Editor {
    /// 表示を今の計画で描くか、流し解きを裏にして握る計画で描くか（モジュールの説明）。仕事を作り直す・捨てるのもここ。
    pub(in crate::edit::editor) fn solve_gate(
        &self,
        st: &mut State,
        full: &kura_engine::Shape,
    ) -> Gate {
        let missing = match self
            .runtime
            .preview(|e| e.missing_solves(&st.plan, &st.inputs, &st.cache, st.output))
        {
            Ok(m) => m,
            Err(err) => {
                // 誤りは今の計画の描画で知らせる
                tracing::debug!(%err, "要る解析を求められません");
                self.drop_solve(st);
                return Gate::Now;
            }
        };
        if missing.tasks.is_empty() || missing.unstreamable {
            self.drop_solve(st);
            return Gate::Now;
        }
        let heavy = !is_short(full) || missing.estimated_ns() > SYNC_SOLVE_NS;
        let usable = st
            .audio
            .as_ref()
            .and_then(|a| a.held.as_ref())
            .is_some_and(|h| h.complete && h.rev != st.session.rev().0);
        if !heavy || !usable {
            self.drop_solve(st);
            return Gate::Now;
        }
        let needed: Vec<Fingerprint> = missing.needed.iter().map(|(k, _)| *k).collect();
        if let Some(a) = st.audio.as_mut()
            && a.solve.as_ref().is_none_or(|s| s.needed != needed)
        {
            a.solve = Some(SolveRun {
                needed,
                job: PresolveJob::new(MissingSolves { ..missing }),
                pace: Pace::default(),
            });
        }
        Gate::Background
    }

    /// 流し解きの仕事を捨てる。
    pub(in crate::edit::editor) fn drop_solve(&self, st: &mut State) {
        if let Some(a) = st.audio.as_mut() {
            a.solve = None;
        }
    }

    /// 流し解きを 1 回の区切り（`limit`）だけ進める。中止は `Canceled`（呼び手が Superseded か Canceled に）。予算の断り・
    /// 誤りは `progress.blocked`（次の区切りで続きから）。
    pub(in crate::edit::editor) fn advance_solve(
        &self,
        st: &mut State,
        limit: &mut Limit,
        progress: &mut WorkProgress,
        mon: &dyn Monitor,
    ) -> Result<Advanced, EngineError> {
        let Some(mut run) = st.audio.as_mut().and_then(|a| a.solve.take()) else {
            return Ok(Advanced::Done);
        };
        let r = self.advance_run(st, &mut run, limit, progress, mon);
        if let Some(a) = st.audio.as_mut() {
            a.solve = Some(run);
        }
        r
    }

    fn advance_run(
        &self,
        st: &mut State,
        run: &mut SolveRun,
        limit: &mut Limit,
        progress: &mut WorkProgress,
        mon: &dyn Monitor,
    ) -> Result<Advanced, EngineError> {
        loop {
            if run.job.is_done() {
                return Ok(Advanced::Done);
            }
            let n = limit.grant(&run.pace, MAX_STEP_CHUNKS);
            if n == 0 {
                return Ok(Advanced::Pending);
            }
            let Some((port, (a, b))) = run.job.next_span(n) else {
                return Ok(Advanced::Done);
            };
            let plan = st.plan.clone();
            let spec = EstimateSpec::new(0)
                .all_inputs(&plan, Source::Held)
                .port_area(port, Area::span(a, b));
            if let Err(err) =
                self.runtime
                    .admit_preview(&plan, spec, &st.inputs, &mut st.cache, &mut st.lease)
            {
                tracing::debug!(%err, "流し解きの区切りの確保が空いていないので、あとにします");
                progress.blocked = true;
                settle(st);
                return Ok(Advanced::Pending);
            }
            let started = Instant::now();
            let (inputs, cache) = (&st.inputs, &mut st.cache);
            // 流して読む読みは源の LRU に入れない（見えている区間のチャンクを追い出さない）
            let r = self
                .runtime
                .preview(|e| read_through(|| run.job.step(e, &plan, inputs, cache, n, mon)));
            settle(st);
            match r {
                Ok(p) => {
                    run.pace.note(p.chunks, started.elapsed());
                    progress.moved = progress.moved.saturating_add(p.chunks);
                    if p.done {
                        return Ok(Advanced::Done);
                    }
                }
                Err(e) if matches!(e.root(), EngineError::Canceled) => return Err(e),
                Err(err) => {
                    tracing::warn!(%err, "流し解きの区切りが失敗しました（次の区切りで続きから）");
                    progress.blocked = true;
                    return Ok(Advanced::Pending);
                }
            }
        }
    }

    /// 今の rev（`rev`、出力の指紋 `samples_id`）を公開したあと、その計画の申告した解析の結果を握る（外の解析としてキャッシュに
    /// 入れる。前に握っていたもののうち要らなくなったものは外す）。
    pub(in crate::edit::editor) fn hold_published(
        &self,
        st: &mut State,
        rev: u64,
        samples_id: &str,
    ) {
        let needed = match self
            .runtime
            .preview(|e| e.missing_solves(&st.plan, &st.inputs, &st.cache, st.output))
        {
            Ok(m) => m.needed,
            Err(err) => {
                tracing::debug!(%err, "握る解析を求められません");
                Vec::new()
            }
        };
        let mut tables = Vec::new();
        let mut complete = true;
        for (key, bytes) in needed {
            match st.cache.peek_analysis(&key) {
                Some(out) => {
                    st.cache.register_external_analysis(key, out.clone(), bytes);
                    tables.push((key, out, bytes));
                }
                None => complete = false,
            }
        }
        let old = st.audio.as_mut().and_then(|a| a.held.take());
        if let Some(old) = old {
            for (k, _, _) in old.tables {
                if !tables.iter().any(|(t, _, _)| *t == k) {
                    // ふつうの項目に戻す（取り消しで戻ったとき、追い出されていなければ解き直さない）
                    st.cache.demote_external_analysis(&k);
                }
            }
        }
        let held = Held {
            rev,
            plan: st.plan.clone(),
            doc: st.session.current().clone(),
            samples_id: samples_id.to_string(),
            tables,
            complete,
        };
        if let Some(a) = st.audio.as_mut() {
            a.held = Some(held);
            a.stale = false;
        }
        settle(st);
    }

    /// キャッシュを空にしたあと（保存の駐車）、握っている結果を入れ直す。
    pub(in crate::edit::editor) fn rehold(&self, st: &mut State) {
        let tables: Vec<_> = st
            .audio
            .as_ref()
            .and_then(|a| a.held.as_ref())
            .map(|h| h.tables.clone())
            .unwrap_or_default();
        for (k, out, bytes) in tables {
            st.cache.register_external_analysis(k, out, bytes);
        }
    }
}

/// 古い計画の道でハンドルの手を断るか：握る計画で描いた表示を出している間（`stale`）、その表示の計画の時間の写しの列が、
/// ハンドルを写す列（頭の計画。切り詰めは対象を外したもの）と違えば断る（利用者が見ている出力の座標を、頭の写しで引数に
/// 戻すと違う位置になる）。
pub(in crate::edit::editor) fn handle_blocked(
    st: &State,
    chain: &[kura_engine::render::TimeLink],
) -> Option<EngineError> {
    let a = st.audio.as_ref()?;
    if !a.stale {
        return None;
    }
    let shown = st.view.as_ref()?.plan().time_chain(st.output)?;
    let same = shown.len() == chain.len()
        && shown
            .iter()
            .zip(chain)
            .all(|(x, y)| x.time == y.time && x.enabled == y.enabled && x.node == y.node);
    if same {
        None
    } else {
        Some(EngineError::Invalid(kura_engine::Issues::one(
            kura_engine::Issue::error(
                kura_engine::IssueCode::BadParams,
                "変更を反映しています。終わってから動かしてください",
            ),
        )))
    }
}
