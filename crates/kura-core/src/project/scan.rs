//! 照らし直しのジョブ（`kind = 'project'`。M15 段 2。docs/data-model.md §4「プロジェクトと使った記録（M15）」、ADR-204）。
//!
//! 登録したプロジェクトを順に見て、取り込める形式のファイルを集め（[`walk::walk`]）、大きさと更新時刻が前と違うものだけ
//! ハッシュし、すべての行を中身の持ち主（今 → 元 → 過去の版 → 持ち出した出力）と照らして 500 件ずつ書く。見えなくなった
//! ファイルの行は最後に消す（使った記録は今あるもの）。フォルダが見つからなければ記録を残して `missing` にする。一時停止・
//! 中止・譲るは区切りで止め、次は初めから（変わっていないファイルはハッシュし直さない）。

use std::collections::BTreeMap;
use std::path::Path;

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Engine, Project, absolute, walk};
use crate::Result;
use crate::import::hash_file;
use crate::import::store::{OwnerRole, find_owner};
use crate::jobs::{Control, JobContext, JobEnd, JobHandler};
use crate::time::now_ms;

/// 1 つのトランザクションで書く行の数。
const CHUNK: usize = 500;
/// 並べてハッシュする件数（区切りごとに止める指示を見る）。
const HASH_BATCH: usize = 64;

/// 照らし直しの結果（ジョブの `result_json`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    /// 照らしたプロジェクトの数（見つからなかったものを含む）。
    pub projects: u64,
    /// フォルダが見つからなかったプロジェクトの数。
    pub missing: u64,
    /// 照らしたファイルの数。
    pub files: u64,
    /// ハッシュしたファイルの数（大きさか更新時刻が変わったもの・新しいもの）。
    pub hashed: u64,
    /// アイテムと一致したファイルの数。
    pub matched: u64,
    /// 見えなくなって消した行の数。
    pub removed: u64,
    /// 読めなかったフォルダ・ファイルの数。
    pub skipped: u64,
}

/// 中身 `hash` の持ち主：今 → 元 → 過去の版（取り込みの重複の判定と同じ `find_owner`）→ 持ち出した出力（ID の小さい
/// アイテム）。アイテムの ID と、何と一致したか（`project_files.matched` の値）。
pub fn match_hash(conn: &Connection, hash: &[u8; 32]) -> Result<Option<(i64, &'static str)>> {
    if let Some(owner) = find_owner(conn, hash)? {
        let how = match owner.role {
            OwnerRole::Current => "current",
            OwnerRole::Original => "original",
            OwnerRole::Past { .. } => "version",
        };
        return Ok(Some((owner.id, how)));
    }
    let output: Option<i64> = conn
        .prepare_cached(
            "SELECT item_id FROM item_outputs WHERE hash = ?1 ORDER BY item_id LIMIT 1",
        )?
        .query_row([&hash[..]], |r| r.get(0))
        .optional()?;
    Ok(output.map(|id| (id, "output")))
}

/// 照らし直しのジョブの処理。
#[derive(Debug, Default)]
pub struct ProjectHandler;

/// 止める指示（一時停止・中止・譲る）があれば、その終わり方。
fn interrupt(ctx: &JobContext) -> Option<JobEnd> {
    match ctx.control() {
        Control::Pause => Some(JobEnd::Paused),
        Control::Cancel => Some(JobEnd::Canceled { result_json: None }),
        Control::Continue if ctx.should_yield() => Some(JobEnd::Yield),
        Control::Continue => None,
    }
}

/// 1 つのプロジェクトの照らしの終わり方。
enum Step {
    Done,
    Stopped(JobEnd),
}

/// 照らす行 1 つ。
struct Row {
    path: String,
    size: i64,
    mtime_ns: i64,
    hash: Option<[u8; 32]>,
}

fn mark_missing(ctx: &JobContext, p: &Project, now: i64) -> Result<()> {
    let id = p.id;
    ctx.library().db().write(move |tx| {
        tx.prepare_cached("UPDATE projects SET missing = 1, updated_at = ?2 WHERE id = ?1")?
            .execute(params![id, now])?;
        Ok(())
    })
}

fn scan_one(ctx: &JobContext, p: &Project, result: &mut ScanResult) -> Result<Step> {
    let db = ctx.library().db();
    let now = now_ms();
    // この回の印：前の回より必ず大きくする（同じミリ秒で照らし直しても、見えなくなった行を見分けられるように）
    let stamp = now.max(p.scanned_at.unwrap_or(0) + 1);
    result.projects += 1;
    let root = Path::new(&p.root);
    if !root.is_dir() {
        ctx.log(&format!(
            "{}：フォルダが見つかりません（{}）",
            p.name, p.root
        ));
        result.missing += 1;
        mark_missing(ctx, p, now)?;
        return Ok(Step::Done);
    }
    let engine: Engine = walk::detect(root);
    let stop = || interrupt_flag(ctx);
    let walked = match walk::walk(root, engine, &stop) {
        Ok(Some(w)) => w,
        Ok(None) => return Ok(Step::Stopped(interrupt(ctx).unwrap_or(JobEnd::Paused))),
        Err(err) => {
            ctx.log(&format!("{}：フォルダを読めません（{err}）", p.name));
            result.missing += 1;
            mark_missing(ctx, p, now)?;
            return Ok(Step::Done);
        }
    };
    result.skipped += walked.skipped as u64;
    // 前の回に見たファイル（大きさ・更新時刻が同じならハッシュを使い直す）
    let id = p.id;
    let known: BTreeMap<String, (i64, i64, Vec<u8>)> = db.read(move |c| {
        let mut stmt = c.prepare_cached(
            "SELECT path, size_bytes, mtime_ns, hash FROM project_files WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    })?;
    let mut rows: Vec<Row> = walked
        .files
        .into_iter()
        .map(|f| {
            let size = i64::try_from(f.size).unwrap_or(i64::MAX);
            let hash = known
                .get(&f.path)
                .filter(|(s, m, h)| *s == size && *m == f.mtime_ns && h.len() == 32)
                .and_then(|(_, _, h)| <[u8; 32]>::try_from(h.as_slice()).ok());
            Row {
                path: f.path,
                size,
                mtime_ns: f.mtime_ns,
                hash,
            }
        })
        .collect();
    let todo: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].hash.is_none())
        .collect();
    for (k, batch) in todo.chunks(HASH_BATCH).enumerate() {
        if let Some(end) = interrupt(ctx) {
            return Ok(Step::Stopped(end));
        }
        ctx.progress(
            (k * HASH_BATCH) as f64 / todo.len().max(1) as f64,
            &format!("{} のファイルを読んでいます", p.name),
        );
        let hashes: Vec<Option<[u8; 32]>> = batch
            .par_iter()
            .map(|&i| hash_file(&absolute(&p.root, &rows[i].path)).ok())
            .collect();
        for (&i, h) in batch.iter().zip(hashes) {
            if h.is_some() {
                result.hashed += 1;
            } else {
                result.skipped += 1;
            }
            rows[i].hash = h;
        }
    }
    // 読めなかった（照らしの間に消えた）ファイルは記録しない（前の行は最後に消える）
    rows.retain(|r| r.hash.is_some());
    result.files += rows.len() as u64;
    for chunk in rows.chunks(CHUNK) {
        if let Some(end) = interrupt(ctx) {
            return Ok(Step::Stopped(end));
        }
        let chunk: Vec<(String, i64, i64, [u8; 32])> = chunk
            .iter()
            .filter_map(|r| r.hash.map(|h| (r.path.clone(), r.size, r.mtime_ns, h)))
            .collect();
        let matched = db.write(move |tx| {
            // 照らしの間に登録を外したら書かない
            let alive: Option<i64> = tx
                .prepare_cached("SELECT id FROM projects WHERE id = ?1")?
                .query_row([id], |r| r.get(0))
                .optional()?;
            if alive.is_none() {
                return Ok(None);
            }
            let mut matched = 0u64;
            let mut upsert = tx.prepare_cached(
                "INSERT INTO project_files (project_id, path, size_bytes, mtime_ns, hash, item_id, matched, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(project_id, path) DO UPDATE SET size_bytes = excluded.size_bytes,
                   mtime_ns = excluded.mtime_ns, hash = excluded.hash, item_id = excluded.item_id,
                   matched = excluded.matched, seen_at = excluded.seen_at",
            )?;
            for (path, size, mtime, hash) in &chunk {
                let owner = match_hash(tx, hash)?;
                if owner.is_some() {
                    matched += 1;
                }
                upsert.execute(params![
                    id,
                    path,
                    size,
                    mtime,
                    &hash[..],
                    owner.map(|(item, _)| item),
                    owner.map(|(_, how)| how),
                    stamp
                ])?;
            }
            Ok(Some(matched))
        })?;
        match matched {
            Some(m) => result.matched += m,
            None => return Ok(Step::Done),
        }
    }
    // 見えなくなったファイルの行を消し、照らし終えた印を付ける
    let engine_name = engine.as_str();
    let removed = db.write(move |tx| {
        let removed = tx
            .prepare_cached("DELETE FROM project_files WHERE project_id = ?1 AND seen_at < ?2")?
            .execute(params![id, stamp])?;
        tx.prepare_cached(
            "UPDATE projects SET missing = 0, engine = ?2, scanned_at = ?3, updated_at = ?4 WHERE id = ?1",
        )?
        .execute(params![id, engine_name, stamp, now])?;
        Ok(removed)
    })?;
    result.removed += removed as u64;
    Ok(Step::Done)
}

fn interrupt_flag(ctx: &JobContext) -> bool {
    !matches!(ctx.control(), Control::Continue) || ctx.should_yield()
}

impl JobHandler for ProjectHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let projects = ctx.library().db().read(super::list)?;
        let mut result = ScanResult::default();
        let total = projects.len().max(1) as f64;
        for (k, p) in projects.iter().enumerate() {
            if let Some(end) = interrupt(ctx) {
                return Ok(end);
            }
            ctx.progress(k as f64 / total, &format!("{} を照らしています", p.name));
            if let Step::Stopped(end) = scan_one(ctx, p, &mut result)? {
                return Ok(end);
            }
        }
        let message = format!(
            "{} 件のプロジェクトを照らしました（ファイル {}・一致 {}）",
            result.projects, result.files, result.matched
        );
        Ok(JobEnd::Done {
            result_json: serde_json::to_string(&result)?,
            message: Some(message),
        })
    }
}
