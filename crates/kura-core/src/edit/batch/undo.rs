//! 一括の［元に戻す…］（M14 段 5。docs/editing.md §14.2「元に戻す」、ADR-190 (7)・ADR-195）。
//!
//! 一括 `of` を戻す一括のジョブ（`JobKind::Batch`。入力の向きは `undo`。レシピは持たない）。行は、元の一括で中身を変えた行
//! （版を足した saved・unchanged）の順で、上書きならそのアイテム、別名なら作ったアイテム。1 件ずつ：
//! - 飛ばす決まりは一括と同じ（保存していない下書きのあるアイテムはいつも飛ばす：下書きの土台を黙って古くしない）。頭が一括の
//!   版のままでなければ（そのあと保存した・版に戻した・別の一括を当てた）`skipped`（Changed）。
//! - 上書き：一括の前の版（元の行の `base_version`）へ戻す（版に戻すと同じ `Saver::revert_reuse`。前の版のファイルがあれば
//!   働き手が台帳の門を持ってそのままコミットし（ファイルを書かないので区切りの同期は要らない）、なければ作り直して、
//!   まとまりでコミットする）。戻した版は `kind = 'revert'`、行の `version_id` が指す。
//! - 別名：作ったアイテムをゴミ箱へ入れる（行と同じトランザクション。ゴミ箱から戻せる）。
//!
//! 積むのは元の一括が終わってから（待ち・動いている・一時停止なら Busy）。戻す一括そのものは戻さない。前に戻した分（ほかの
//! 戻す一括で saved・unchanged になった行）は入れない。同じ一括を戻す一括が終わっていなければ Busy。やり直し（`job_retry`）も
//! 同じ決まりで断る（[`check_retry`]。ADR-196）。止め方・ちょうど 1 回・強制終了は一括と同じ（行と版・ゴミ箱が同じトランザクション。
//! 作り直しを止めたら作業用の PCM の鍵を返し、待ち・一時停止のまま中止したら捨てる）。

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    BatchHandler, BatchInput, BatchUnit, INPUT_V, ModeInput, Outcome, Skip, Task, input_of,
    insert_rows, row_record,
};
use crate::db::Db;
use crate::edit::EngineServices;
use crate::edit::save::{Base, Flow, Reverted, UnitCx, revert_work_keys};
use crate::edit::versions::{self, Head};
use crate::jobs::{Control, JobId, JobKind, JobManager, JobState};
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 一括 `of` を元に戻す一括のジョブを積む（モジュールの説明）。ないジョブは NotFound、一括でない・戻す一括を戻す・戻すものが
/// ないなら Invalid、元の一括か、同じ一括を戻す一括が終わっていなければ Busy。エディタで開いているアイテムは、積んだ時点で
/// `skipped`（Open）にする（[`super::start_batch`] と同じ）。
pub fn start_undo(db: &Db, jobs: &JobManager, engine: &EngineServices, of: JobId) -> Result<JobId> {
    let input = input_of(db, of)?;
    let (save_as, merge) = match input.mode {
        ModeInput::Overwrite => (false, false),
        ModeInput::SaveAs { .. } => (true, false),
        ModeInput::Merge { .. } => (true, true),
        ModeInput::Undo { .. } => {
            return Err(CoreError::Invalid(
                "元に戻した一括は、もう一度戻せません".into(),
            ));
        }
    };
    let undo = BatchInput {
        v: INPUT_V,
        recipe: None,
        mode: ModeInput::Undo { of, save_as, merge },
        include_drafts: false,
    };
    let input_json = serde_json::to_string(&undo)?;
    let now = now_ms();
    engine.with_open_item(|open| {
        jobs.enqueue_then(JobKind::Batch, input_json, move |tx, job| {
            let state = job_state(tx, of)?;
            if !state.is_some_and(finished) {
                return Err(CoreError::Busy(
                    "一括が終わってから元に戻せます（一時停止中なら中止してから）".into(),
                ));
            }
            let earlier = undo_jobs(tx, of, Some(job))?;
            if earlier.iter().any(|(_, s)| !finished(*s)) {
                return Err(CoreError::Busy(
                    "この一括を元に戻す一括が、まだ終わっていません".into(),
                ));
            }
            let earlier: Vec<JobId> = earlier.into_iter().map(|(id, _)| id).collect();
            let targets = targets(tx, of, save_as, merge, &earlier)?;
            if insert_rows(tx, job, &targets, open, now)? == 0 {
                return Err(CoreError::Invalid(
                    "元に戻すアイテムがありません（この一括で変えたものは、もう戻したか消えました）".into(),
                ));
            }
            Ok(())
        })
    })
}

/// 失敗・中止した一括をもう一度動かす前の確かめ（`JobManager::retry` が queued に戻すのと同じトランザクションで呼ぶ）：
/// 元に戻す一括なら、元の一括が終わっていて、同じ一括を戻すほかの一括がすべて終わっていること。ふつうの一括なら、それを
/// 戻す一括がすべて終わっていること（積むときの [`start_undo`] の決まりを、やり直しでも崩さない）。違えば Busy。
pub(crate) fn check_retry(tx: &Connection, job: JobId, input_json: &str) -> Result<()> {
    let input: BatchInput = serde_json::from_str(input_json)?;
    match input.mode {
        ModeInput::Undo { of, .. } => {
            if !job_state(tx, of)?.is_some_and(finished) {
                return Err(CoreError::Busy(
                    "元の一括が終わってから、元に戻す一括をやり直せます".into(),
                ));
            }
            if undo_jobs(tx, of, Some(job))?
                .iter()
                .any(|(_, s)| !finished(*s))
            {
                return Err(CoreError::Busy(
                    "同じ一括を元に戻すほかの一括が、まだ終わっていません".into(),
                ));
            }
        }
        ModeInput::Overwrite | ModeInput::SaveAs { .. } | ModeInput::Merge { .. } => {
            if undo_jobs(tx, job, None)?.iter().any(|(_, s)| !finished(*s)) {
                return Err(CoreError::Busy(
                    "この一括を元に戻す一括が終わってから、やり直せます".into(),
                ));
            }
        }
    }
    Ok(())
}

/// 終わった状態（済んだ・失敗した・中止した）か。
fn finished(state: JobState) -> bool {
    matches!(
        state,
        JobState::Done | JobState::Failed | JobState::Canceled
    )
}

/// ジョブ `job` の状態（ないジョブは None）。
fn job_state(conn: &Connection, job: JobId) -> Result<Option<JobState>> {
    let state: Option<String> = conn
        .prepare_cached("SELECT state FROM jobs WHERE id = ?1")?
        .query_row([job], |r| r.get(0))
        .optional()?;
    Ok(state.as_deref().and_then(JobState::parse))
}

/// 一括 `of` を戻す一括のジョブと状態（古い順。`except` は除く）。
pub(crate) fn undo_jobs(
    conn: &Connection,
    of: JobId,
    except: Option<JobId>,
) -> Result<Vec<(JobId, JobState)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, state FROM jobs
          WHERE kind = 'batch' AND json_extract(input_json, '$.mode.type') = 'undo'
            AND json_extract(input_json, '$.mode.of') = ?1
          ORDER BY id",
    )?;
    let rows = stmt.query_map([of], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, state) = row?;
        if Some(id) == except {
            continue;
        }
        let state = JobState::parse(&state).ok_or_else(|| {
            CoreError::Invalid(format!("ジョブ {id} の状態 {state} を知りません"))
        })?;
        out.push((id, state));
    }
    Ok(out)
}

/// 戻すアイテム（元の一括の行の順）：版を足した行（saved・unchanged）の、上書きならアイテム、別名なら作ったアイテム（消えたもの
/// は除く）。N → 1（`merge`）は作ったアイテム 1 つ（行はどれも同じアイテムを指し、版は持たない）。前に戻した一括 `earlier` で
/// saved・unchanged になったものは除く。
pub(crate) fn targets(
    conn: &Connection,
    of: JobId,
    save_as: bool,
    merge: bool,
    earlier: &[JobId],
) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT item_id, out_item_id FROM batch_items
          WHERE job_id = ?1 AND state IN ('saved', 'unchanged') AND (version_id IS NOT NULL OR ?2)
          ORDER BY ord",
    )?;
    let rows = stmt.query_map(params![of, merge], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
    })?;
    let mut done = conn.prepare_cached(
        "SELECT 1 FROM batch_items WHERE job_id = ?1 AND item_id = ?2 AND state IN ('saved', 'unchanged')",
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (item, made) = row?;
        let target = if save_as { made } else { Some(item) };
        let Some(target) = target else { continue };
        if out.contains(&target) {
            continue;
        }
        let mut undone = false;
        for job in earlier {
            if done
                .query_row(params![job, target], |_| Ok(()))
                .optional()?
                .is_some()
            {
                undone = true;
                break;
            }
        }
        if !undone {
            out.push(target);
        }
    }
    Ok(out)
}

impl BatchHandler {
    /// 元に戻す一括の 1 件（飛ばす決まりは見たあと。モジュールの説明）。
    pub(super) fn prepare_undo(
        &self,
        unit: &BatchUnit<'_>,
        task: Task,
        head: Head,
        of: JobId,
        save_as: bool,
        merge: bool,
    ) -> std::result::Result<Outcome, Skip> {
        let db = unit.ctx.library().db();
        let item = task.item_id;
        let failed = |e: CoreError| Skip::failed(&e);
        // 元の一括の行：足した版の番号と、その前の版（上書きは同じアイテム、別名は作ったアイテムで引く。N → 1 の作ったアイテムは
        // 版を持たないので版 0 のまま）
        let sql = if merge {
            "SELECT 0, NULL FROM batch_items b
              WHERE b.out_item_id = ?2 AND b.job_id = ?1 AND b.state = 'saved' LIMIT 1"
        } else if save_as {
            "SELECT v.version, b.base_version FROM batch_items b JOIN item_versions v ON v.id = b.version_id
              WHERE b.out_item_id = ?2 AND b.job_id = ?1 AND b.state IN ('saved', 'unchanged')"
        } else {
            "SELECT v.version, b.base_version FROM batch_items b JOIN item_versions v ON v.id = b.version_id
              WHERE b.job_id = ?1 AND b.item_id = ?2 AND b.state IN ('saved', 'unchanged')"
        };
        let row: Option<(i64, Option<i64>)> = db
            .read(|c| {
                Ok(c.prepare_cached(sql)?
                    .query_row(params![of, item], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?)
            })
            .map_err(failed)?;
        let Some((at, before)) = row else {
            return Err(Skip::skipped(
                "Gone",
                "元の一括で足した版がありません".into(),
            ));
        };
        if head.version != at {
            return Err(Skip::skipped(
                "Changed",
                format!(
                    "一括のあとに変わったので飛ばしました（一括の版 {at}、今は版 {}）",
                    head.version
                ),
            ));
        }
        // 止める指示を見てから変える（行は pending のまま）
        if unit.control() != Control::Continue {
            return Ok(Outcome::Stopped(Vec::new()));
        }
        // 作り直しを一時停止・譲るで止めたときに残しうる作業用の PCM の鍵（待ち・一時停止のまま中止したら捨てる。一括と同じ）
        let kept = |to: i64| {
            db.read(|c| revert_work_keys(c, item, to))
                .unwrap_or_else(|err| {
                    tracing::warn!(%err, item, "戻す 1 件の作業用の PCM の鍵を求められません");
                    Vec::new()
                })
        };
        let base = Base {
            hash: head.hash,
            version: head.version,
        };
        let job = unit.ctx.id();
        let classify = |e: CoreError| match e {
            CoreError::Conflict(_) if task.attempt == 0 => Ok(Outcome::Retry(task)),
            e @ CoreError::Conflict(_) => Err(Skip::skipped("Conflict", e.to_string())),
            e => Err(failed(e)),
        };
        if save_as {
            let (ord, now) = (task.ord, now_ms());
            let trashed = db.write(move |tx| {
                let h = versions::head(tx, item)?
                    .ok_or_else(|| CoreError::NotFound(format!("アイテム {item}")))?;
                if h.trashed {
                    return Err(CoreError::Conflict(
                        "戻す間にゴミ箱に入れられました".into(),
                    ));
                }
                if h.hash != base.hash || h.version != base.version {
                    return Err(CoreError::Conflict(format!(
                        "戻す間にアイテムが変わりました（版 {} → {}）",
                        base.version, h.version
                    )));
                }
                crate::items::trash(tx, &[item], now)?;
                let n = tx
                    .prepare_cached(
                        "UPDATE batch_items SET state = 'saved', base_hash = ?3, base_version = ?4, decoding = 0,
                                code = NULL, message = NULL, finished_at = ?5
                          WHERE job_id = ?1 AND ord = ?2 AND state = 'pending'",
                    )?
                    .execute(params![job, ord, &base.hash[..], base.version, now])?;
                if n != 1 {
                    return Err(CoreError::Conflict(
                        "元に戻す一括の対象の行がありません（中止されたか、アイテムが消えました）".into(),
                    ));
                }
                Ok(())
            });
            return match trashed {
                Ok(()) => {
                    unit.log(&format!("アイテム {item} をゴミ箱に入れました"));
                    Ok(Outcome::Done { saved: None })
                }
                Err(e) => classify(e),
            };
        }
        let Some(to) = before else {
            return Err(failed(CoreError::Invalid(format!(
                "アイテム {item} の一括の前の版が分かりません"
            ))));
        };
        let record = row_record(job, task.ord, base, false);
        match self.saver.revert_reuse(unit, item, base, to, &record) {
            Ok(Reverted::Committed(c)) => Ok(Outcome::Done { saved: c.saved }),
            Ok(Reverted::Rebuild(plan)) => match self.saver.prepare_rebuild(unit, *plan) {
                Ok(Flow::Go(w)) => Ok(Outcome::Ready(task, Box::new(w))),
                // 作り直しの途中で止めた（一時停止なら作業用の PCM を残しうる。鍵を渡して、中止のときに捨てる）
                Ok(Flow::Stop(_)) => Ok(Outcome::Stopped(kept(to))),
                Err(e) => classify(e),
            },
            Err(e) => classify(e),
        }
    }
}
