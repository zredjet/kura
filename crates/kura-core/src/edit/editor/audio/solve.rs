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

/// 1 回の step に渡すチャンクの数の上限（流れの窓 1 つ。区切りの中で何度か step する。区切りの前の確保の見積もりと同じ
/// 大きさにする）。
const MAX_STEP_CHUNKS: u64 = kura_engine::audio::stream::STREAM_CHUNKS as u64;

/// 中止・予算の断りのほかの誤り（源の読み・上限など）が 1 つの仕事でこの回数になったら、仕事を捨てて錠の中で解く今までの道に
/// 戻す（誤りを表示の失敗として知らせる。同じ鍵の列の間は戻さない）。
const MAX_SOLVE_ERRORS: u32 = 3;

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
    /// この仕事の誤りの数（誤りのあと、その仕事は最初から流し直すので、進んでも戻さない）。
    pub(crate) errors: u32,
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
    /// 誤りが続いた：仕事を捨てて、錠の中で解く今までの道で描く（誤りはそこで知らせる）。
    Failed,
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
        let needed: Vec<Fingerprint> = missing.needed.iter().map(|(k, _)| *k).collect();
        // 誤りが続いて捨てた鍵の列の間は、錠の中で解く道（誤りを知らせる）。列が変われば忘れる
        let failed = st.audio.as_mut().is_some_and(|a| {
            if a.solve_failed.as_ref().is_some_and(|f| *f != needed) {
                a.solve_failed = None;
            }
            a.solve_failed.is_some()
        });
        if !heavy || !usable || failed {
            self.drop_solve(st);
            return Gate::Now;
        }
        let Some(a) = st.audio.as_mut() else {
            return Gate::Now;
        };
        if a.solve.as_ref().is_none_or(|s| s.needed != needed) {
            let mut job = PresolveJob::new(MissingSolves {
                needed: missing.needed.clone(),
                ..missing
            });
            // キャッシュにある上流の結果は仕事の中の表にも持つ（追い出されても入れ子に解き直さない）
            job.seed(&missing.needed, &st.cache);
            let mut pace = Pace::default();
            if let Some(mut old) = a.solve.take() {
                // 同じ鍵を流している途中なら引き継ぐ（下流の申告を足した手などで上流の作りかけを捨てない）
                if job.adopt(&mut old.job) {
                    pace = old.pace;
                }
            }
            a.solve = Some(SolveRun {
                needed,
                job,
                pace,
                errors: 0,
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
        let plan = st.plan.clone();
        let r = self.advance_run(st, &plan, &mut run, limit, progress, mon);
        if let Some(a) = st.audio.as_mut() {
            a.solve = Some(run);
        }
        r
    }

    /// 計画 `plan` の流し解きの仕事 `run` を区切り `limit` だけ進める（表示と試聴の計画で分け合う。M17 段 6）。
    pub(in crate::edit::editor) fn advance_run(
        &self,
        st: &mut State,
        plan: &kura_engine::render::Plan,
        run: &mut SolveRun,
        limit: &mut Limit,
        progress: &mut WorkProgress,
        mon: &dyn Monitor,
    ) -> Result<Advanced, EngineError> {
        loop {
            if run.job.is_done() {
                return Ok(Advanced::Done);
            }
            // 今の仕事の残りで切る（区切りの前の確保の見積もりと、step が流す量を同じにする。仕事の境をまたがない）
            let left = run.job.left_chunks();
            let n = limit.grant(&run.pace, MAX_STEP_CHUNKS.min(left.max(1)));
            if n == 0 {
                return Ok(Advanced::Pending);
            }
            let n = if left == 0 { 0 } else { n };
            let Some((port, (a, b))) = run.job.next_span(n) else {
                return Ok(Advanced::Done);
            };
            let spec = EstimateSpec::new(0)
                .all_inputs(plan, Source::Held)
                .port_area(port, Area::span(a, b));
            if let Err(err) =
                self.runtime
                    .admit_preview(plan, spec, &st.inputs, &mut st.cache, &mut st.lease)
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
                .preview(|e| read_through(|| run.job.step(e, plan, inputs, cache, n, mon)));
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
                    run.errors = run.errors.saturating_add(1);
                    if run.errors >= MAX_SOLVE_ERRORS {
                        tracing::warn!(%err, "流し解きの誤りが続いたので、錠の中で解きます");
                        return Ok(Advanced::Failed);
                    }
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

    /// 仕事が解いた結果を、キャッシュから消えていれば外の解析として入れ直す（解き終えた区切りで今の計画を描く前。保存の駐車の
    /// `clear`・追い出しのあとでも錠の中で解き直さない。続く公開で握る結果に替わる）。
    pub(in crate::edit::editor) fn restore_solved(&self, st: &mut State) {
        let solved: Vec<_> = st
            .audio
            .as_ref()
            .and_then(|a| a.solve.as_ref())
            .map(|s| s.job.finished().to_vec())
            .unwrap_or_default();
        restore_finished(st, solved);
    }

    /// 誤りが続いた仕事を捨て、その鍵の列を覚える（列が変わるまで錠の中で解く道で描く）。
    pub(in crate::edit::editor) fn fail_solve(&self, st: &mut State) {
        if let Some(a) = st.audio.as_mut()
            && let Some(run) = a.solve.take()
        {
            a.solve_failed = Some(run.needed);
        }
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

/// 仕事が解いた結果のうちキャッシュにないものを、外の解析として入れ直す（表示と試聴の流し解きで分け合う）。
pub(in crate::edit::editor) fn restore_finished(
    st: &mut State,
    solved: Vec<kura_engine::render::presolve::Solved>,
) {
    for s in solved {
        if !st.cache.contains_analysis(&s.key) {
            st.cache.register_external_analysis(s.key, s.out, s.bytes);
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
