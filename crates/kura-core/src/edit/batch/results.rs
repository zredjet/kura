//! 一括の結果（M14 段 5。docs/editing.md §14.2「結果」）：結果のシートが出す、向き・レシピ・状態と理由ごとの群（アイテムは ID の
//! 列だけ）・別名で作ったアイテム・元に戻せるか（[`result`]）と、見えている分だけの 1 件ずつの理由（[`rows`]）。

use rusqlite::{OptionalExtension, params};

use super::{BatchInput, ModeInput, input_of, undo};
use crate::db::Db;
use crate::jobs::{JobId, JobState};
use crate::{CoreError, Result};

/// 結果の群の状態の並び（当てた → 変わらない → 同じ中身 → 飛ばした → 失敗 → 中止 → 待ち）。
const STATES: [&str; 7] = [
    "saved",
    "unchanged",
    "existing",
    "skipped",
    "failed",
    "canceled",
    "pending",
];

/// 一括の向き。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchAction {
    Overwrite,
    /// 別名（`name` は名前の型）。
    SaveAs {
        name: String,
    },
    /// 一括 `of` の［元に戻す…］（`save_as` は元の一括が別名か N → 1、`merge` は N → 1 か）。
    Undo {
        of: JobId,
        save_as: bool,
        merge: bool,
    },
    /// N → 1（スプライトシート。`name` は名前の型）。
    Merge {
        name: String,
        sheet: kura_engine::selection::SheetParams,
    },
}

/// 一括のジョブの結果（[`result`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchResult {
    pub job: JobId,
    pub state: JobState,
    pub action: BatchAction,
    /// 当てたレシピの名前（積んだときの写し。元に戻す一括は None）。
    pub recipe: Option<String>,
    pub include_drafts: bool,
    /// 対象の行の数。
    pub total: i64,
    /// 状態と理由ごとの群（状態の並び、同じ状態は理由の最初に出た順）。
    pub groups: Vec<ResultGroup>,
    /// 別名で作ったアイテム（行の順。消えたものは除く）。
    pub created: Vec<i64>,
    pub undo: UndoInfo,
}

/// 状態と理由ごとの群。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultGroup {
    pub state: String,
    /// 理由の code（当てた・変わらないは None）。
    pub code: Option<String>,
    /// 対象のアイテム（行の順）。
    pub items: Vec<i64>,
}

/// 元に戻せるか。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UndoInfo {
    /// この一括を元に戻す一括（古い順）。
    pub by: Vec<JobId>,
    /// 元に戻せる件数（終わった一括の、中身を変えて、まだ戻していない行。元に戻す一括そのもの・終わっていない一括・戻す一括が
    /// 終わっていないときは 0）。
    pub available: i64,
}

/// 1 件の行（[`rows`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRow {
    pub ord: i64,
    pub item_id: i64,
    pub state: String,
    pub code: Option<String>,
    pub message: Option<String>,
    /// 足した版（`item_versions.id`）。
    pub version_id: Option<i64>,
    /// 別名で作ったアイテム。
    pub out_item_id: Option<i64>,
    pub finished_at: Option<i64>,
}

/// 一括のジョブ `job` の結果（ないジョブは NotFound、一括でなければ Invalid）。
pub fn result(db: &Db, job: JobId) -> Result<BatchResult> {
    let input: BatchInput = input_of(db, job)?;
    let action = match &input.mode {
        ModeInput::Overwrite => BatchAction::Overwrite,
        ModeInput::SaveAs { name } => BatchAction::SaveAs { name: name.clone() },
        ModeInput::Undo { of, save_as, merge } => BatchAction::Undo {
            of: *of,
            save_as: *save_as,
            merge: *merge,
        },
        ModeInput::Merge { name, sheet } => BatchAction::Merge {
            name: name.clone(),
            sheet: *sheet,
        },
    };
    db.read(|c| {
        let state: String = c
            .prepare_cached("SELECT state FROM jobs WHERE id = ?1")?
            .query_row([job], |r| r.get(0))
            .optional()?
            .ok_or_else(|| CoreError::NotFound(format!("ジョブ {job}")))?;
        let state = JobState::parse(&state)
            .ok_or_else(|| CoreError::Invalid(format!("ジョブ {job} の状態 {state} を知りません")))?;
        let mut groups: Vec<ResultGroup> = Vec::new();
        let mut created = Vec::new();
        let mut total = 0i64;
        let mut stmt = c.prepare_cached(
            "SELECT item_id, state, code, out_item_id FROM batch_items WHERE job_id = ?1 ORDER BY ord",
        )?;
        let rows = stmt.query_map([job], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?;
        for row in rows {
            let (item, state, code, made) = row?;
            total += 1;
            // N → 1 はどの行も同じアイテムを指すので 1 つにする
            if let Some(made) = made
                && state == "saved"
                && !created.contains(&made)
            {
                created.push(made);
            }
            match groups
                .iter_mut()
                .find(|g| g.state == state && g.code == code)
            {
                Some(g) => g.items.push(item),
                None => groups.push(ResultGroup {
                    state,
                    code,
                    items: vec![item],
                }),
            }
        }
        // 状態の並びに揃える（同じ状態の中は最初に出た順のまま）
        groups.sort_by_key(|g| STATES.iter().position(|s| *s == g.state).unwrap_or(STATES.len()));
        let mut info = UndoInfo::default();
        if let Some((save_as, merge)) = match action {
            BatchAction::Overwrite => Some((false, false)),
            BatchAction::SaveAs { .. } => Some((true, false)),
            BatchAction::Merge { .. } => Some((true, true)),
            BatchAction::Undo { .. } => None,
        } {
            let undos = undo::undo_jobs(c, job, None)?;
            info.by = undos.iter().map(|(id, _)| *id).collect();
            let finished = |s: JobState| matches!(s, JobState::Done | JobState::Failed | JobState::Canceled);
            if finished(state) && undos.iter().all(|(_, s)| finished(*s)) {
                info.available = undo::targets(c, job, save_as, merge, &info.by)?.len() as i64;
            }
        }
        Ok(BatchResult {
            job,
            state,
            action: action.clone(),
            recipe: input.recipe.as_ref().map(|r| r.name.clone()),
            include_drafts: input.include_drafts,
            total,
            groups,
            created,
            undo: info,
        })
    })
}

/// 一括のジョブ `job` の行（行の順。`state`・`code` があればそれだけ。`offset` から `limit` 件。limit は 1〜1,000）。
pub fn rows(
    db: &Db,
    job: JobId,
    state: Option<&str>,
    code: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<BatchRow>> {
    let limit = limit.clamp(1, 1_000);
    let offset = offset.max(0);
    let (state, code) = (state.map(str::to_string), code.map(str::to_string));
    db.read(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT ord, item_id, state, code, message, version_id, out_item_id, finished_at
               FROM batch_items
              WHERE job_id = ?1 AND (?2 IS NULL OR state = ?2) AND (?3 IS NULL OR code = ?3)
              ORDER BY ord LIMIT ?4 OFFSET ?5",
        )?;
        let rows = stmt.query_map(params![job, state, code, limit, offset], |r| {
            Ok(BatchRow {
                ord: r.get(0)?,
                item_id: r.get(1)?,
                state: r.get(2)?,
                code: r.get(3)?,
                message: r.get(4)?,
                version_id: r.get(5)?,
                out_item_id: r.get(6)?,
                finished_at: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    })
}
