//! 照らし直しのジョブ（`kind = 'project'`。M15 段 2。docs/data-model.md §4「プロジェクトと使った記録（M15）」、ADR-204）。
//!
//! 登録したプロジェクトを順に見て、取り込める形式のファイルを集め（[`walk::walk`]）、500 件ずつ、大きさと更新時刻が前と
//! 違うものだけハッシュしてから、すべての行を中身の持ち主（今 → 元 → 過去の版 → 持ち出した出力）と照らして書く（止めても
//! ハッシュした分は残り、次の回で使い直す）。見えなくなったファイルの行は最後まで照らした回だけが消す（使った記録は今あるもの。
//! 読み切れなかったフォルダの下は消さない）。フォルダが見つからなければ記録を残して `missing` にする。照らしの間に登録を外す・
//! 場所を選び直すと、そのプロジェクトは書かずに終える。一時停止・中止・譲るは区切り（64 件か 256 MiB、大きなファイルは 64 MiB
//! ごと）で止め、次は初めから。

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{Engine, absolute, walk};
use crate::Result;
use crate::import::store::{OwnerRole, find_owner};
use crate::jobs::{Control, JobContext, JobEnd, JobHandler};
use crate::time::now_ms;

/// 1 つのトランザクションで書く行の数。
const CHUNK: usize = 500;
/// 並べてハッシュする件数と合計の大きさの上限（区切りごとに止める指示を見る）。
const HASH_BATCH: usize = 64;
const HASH_BATCH_BYTES: u64 = 256 << 20;
/// 1 つのファイルを読む塊の大きさと、止める指示を見る間隔。
const READ_CHUNK: usize = 4 << 20;
const STOP_EVERY: u64 = 64 << 20;
/// ハッシュに使うスレッドの数の上限（大域の rayon のプールをふさがない専用のプール）。
const HASH_THREADS: usize = 4;

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

/// 止める指示があるか（並べたハッシュの中から見る）。
fn halted(ctx: &JobContext) -> bool {
    !matches!(ctx.control(), Control::Continue) || ctx.should_yield()
}

/// 1 つのプロジェクトの照らしの終わり方。
enum Step {
    Done,
    Stopped(JobEnd),
}

/// 照らすプロジェクトの見分け（照らしの間の外す・場所の選び直し・ID の使い回しに気づく）。
#[derive(Debug, Clone)]
struct Key {
    id: i64,
    root: String,
    created_at: i64,
}

/// 照らす行 1 つ。
struct Row {
    path: String,
    size: i64,
    mtime_ns: i64,
    hash: Option<[u8; 32]>,
    /// 前の回の行（大きさ・更新時刻・ハッシュ）。変わったファイルが読めなかったときは、これをそのまま書き戻す（前の結び付きを
    /// 保ち、次の回は大きさか時刻が違うので読み直す。新しい大きさ・時刻と前のハッシュを組にすると、読めるようになっても
    /// 大きさ・時刻が同じとみなして前のハッシュのまま当たり続けた。PR #1 の Codex のレビューの指摘。M15 段 8）。
    known: Option<(i64, i64, [u8; 32])>,
}

/// ファイルを読んでハッシュする（`halt` が真になれば途中でやめて None）。
fn hash_stoppable(
    path: &Path,
    halt: &(dyn Fn() -> bool + Sync),
) -> std::io::Result<Option<[u8; 32]>> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut since = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        since += n as u64;
        if since >= STOP_EVERY {
            since = 0;
            if halt() {
                return Ok(None);
            }
        }
    }
    Ok(Some(*hasher.finalize().as_bytes()))
}

/// 照らすプロジェクトがまだ同じか（同じ ID・ルート・作った時刻）。
fn alive(conn: &Connection, key: &Key) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM projects WHERE id = ?1 AND root = ?2 AND created_at = ?3")?
        .query_row(params![key.id, key.root, key.created_at], |_| Ok(()))
        .optional()?
        .is_some())
}

fn mark_missing(ctx: &JobContext, key: &Key, now: i64) -> Result<()> {
    let key = key.clone();
    ctx.library().db().write(move |tx| {
        tx.prepare_cached(
            "UPDATE projects SET missing = 1, updated_at = ?4 WHERE id = ?1 AND root = ?2 AND created_at = ?3",
        )?
        .execute(params![key.id, key.root, key.created_at, now])?;
        Ok(())
    })
}

/// 塊の中のハッシュのない行を、64 件か 256 MiB ずつ並べてハッシュする（大きなファイルは 1 件ずつ）。止めたら Some。
fn hash_rows(
    ctx: &JobContext,
    pool: Option<&rayon::ThreadPool>,
    root: &str,
    rows: &mut [Row],
    result: &mut ScanResult,
) -> Option<JobEnd> {
    let todo: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].hash.is_none())
        .collect();
    let mut start = 0;
    while start < todo.len() {
        if let Some(end) = interrupt(ctx) {
            return Some(end);
        }
        let mut end = start;
        let mut bytes = 0u64;
        while end < todo.len() && end - start < HASH_BATCH {
            let size = u64::try_from(rows[todo[end]].size).unwrap_or(0);
            if end > start && bytes + size > HASH_BATCH_BYTES {
                break;
            }
            bytes += size;
            end += 1;
        }
        let batch = &todo[start..end];
        let paths: Vec<std::path::PathBuf> = batch
            .iter()
            .map(|&i| absolute(root, &rows[i].path))
            .collect();
        let halt = || halted(ctx);
        let work = || -> Vec<std::io::Result<Option<[u8; 32]>>> {
            paths.par_iter().map(|p| hash_stoppable(p, &halt)).collect()
        };
        let hashes = match pool {
            Some(pool) => pool.install(work),
            None => paths.iter().map(|p| hash_stoppable(p, &halt)).collect(),
        };
        let mut stopped = false;
        for (&i, h) in batch.iter().zip(hashes) {
            match h {
                Ok(Some(h)) => {
                    result.hashed += 1;
                    rows[i].hash = Some(h);
                }
                Ok(None) => stopped = true,
                // 照らしの間に消えたファイルは記録しない（前の行は最後に消える）。読めないファイルは前の行のまま残す
                // （大きさ・時刻も前のもの。`Row::known`）
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    result.skipped += 1;
                    if let Some((size, mtime_ns, h)) = rows[i].known {
                        rows[i].size = size;
                        rows[i].mtime_ns = mtime_ns;
                        rows[i].hash = Some(h);
                    }
                }
            }
        }
        if stopped {
            return Some(interrupt(ctx).unwrap_or(JobEnd::Yield));
        }
        start = end;
    }
    None
}

/// ハッシュのある行を書く（中身の持ち主と照らす）。プロジェクトが変わっていれば書かずに false。一致した数を足す。
fn write_rows(
    ctx: &JobContext,
    key: &Key,
    rows: &[Row],
    stamp: i64,
    result: &mut ScanResult,
) -> Result<bool> {
    let chunk: Vec<(String, i64, i64, [u8; 32])> = rows
        .iter()
        .filter_map(|r| r.hash.map(|h| (r.path.clone(), r.size, r.mtime_ns, h)))
        .collect();
    let written = chunk.len() as u64;
    let key = key.clone();
    let matched = ctx.library().db().write(move |tx| {
        if !alive(tx, &key)? {
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
                key.id,
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
        Some(m) => {
            result.files += written;
            result.matched += m;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// 1 つのプロジェクトを照らす。`k`・`total` は全体の進みの位置。
fn scan_one(
    ctx: &JobContext,
    pool: Option<&rayon::ThreadPool>,
    id: i64,
    (k, total): (usize, usize),
    result: &mut ScanResult,
) -> Result<Step> {
    let db = ctx.library().db();
    // ジョブの初めの一覧ではなく、今の行を読む（照らしの前に選び直した・外したものに合わせる）
    let Ok(p) = db.read(move |c| super::get(c, id)) else {
        return Ok(Step::Done);
    };
    let key = Key {
        id: p.id,
        root: p.root.clone(),
        created_at: p.created_at,
    };
    let progress = |inner: f64, message: &str| {
        ctx.progress(
            (k as f64 + inner.clamp(0.0, 1.0)) / total.max(1) as f64,
            message,
        );
    };
    progress(0.0, &format!("{} を照らしています", p.name));
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
        mark_missing(ctx, &key, now)?;
        return Ok(Step::Done);
    }
    let engine: Engine = walk::detect(root);
    let decided: Cell<Option<JobEnd>> = Cell::new(None);
    let stop = || {
        let end = interrupt(ctx);
        let stop = end.is_some();
        if stop {
            decided.set(end);
        }
        stop
    };
    let walked = match walk::walk(root, engine, &stop) {
        Ok(Some(w)) => w,
        // 止めた理由は止めたときに覚えたもの（読み直すと譲る相手が終わっていて「理由なし」になりうる）
        Ok(None) => return Ok(Step::Stopped(decided.take().unwrap_or(JobEnd::Yield))),
        Err(err) => {
            ctx.log(&format!("{}：フォルダを読めません（{err}）", p.name));
            result.missing += 1;
            mark_missing(ctx, &key, now)?;
            return Ok(Step::Done);
        }
    };
    result.skipped += walked.skipped as u64;
    if !walked.unreadable.is_empty() {
        ctx.log(&format!(
            "{}：読み切れなかったフォルダ {} 件（前の記録は残します）",
            p.name,
            walked.unreadable.len()
        ));
    }
    // 前の回に見たファイル（大きさ・更新時刻が同じならハッシュを使い直す）
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
        .iter()
        .map(|f| {
            let size = i64::try_from(f.size).unwrap_or(i64::MAX);
            let before = known
                .get(&f.path)
                .and_then(|(s, m, h)| <[u8; 32]>::try_from(h.as_slice()).ok().map(|h| (*s, *m, h)));
            let same = before.and_then(|(s, m, h)| (s == size && m == f.mtime_ns).then_some(h));
            Row {
                path: f.path.clone(),
                size,
                mtime_ns: f.mtime_ns,
                hash: same,
                known: before,
            }
        })
        .collect();
    let chunks = rows.len().div_ceil(CHUNK).max(1);
    for (c, chunk) in rows.chunks_mut(CHUNK).enumerate() {
        progress(
            c as f64 / chunks as f64,
            &format!("{} のファイルを読んでいます", p.name),
        );
        let stopped = hash_rows(ctx, pool, &p.root, chunk, result);
        // 止めてもハッシュした分は書く（次の回で大きさと時刻で使い直す）
        if !write_rows(ctx, &key, chunk, stamp, result)? {
            return Ok(Step::Done);
        }
        if let Some(end) = stopped {
            return Ok(Step::Stopped(end));
        }
    }
    // 照らしの間にフォルダが外れていたら、見えなかった行を消さずに「見つからない」にする
    if !root.is_dir() {
        ctx.log(&format!(
            "{}：照らしの間にフォルダが見つからなくなりました",
            p.name
        ));
        result.missing += 1;
        mark_missing(ctx, &key, now)?;
        return Ok(Step::Done);
    }
    // 見えなくなったファイルの行を消し（読み切れなかったフォルダの下は残す）、照らし終えた印を付ける
    let engine_name = engine.as_str();
    let fin = key.clone();
    let removed = db.write(move |tx| {
        if !alive(tx, &fin)? {
            return Ok(0);
        }
        let stale: Vec<String> = tx
            .prepare_cached("SELECT path FROM project_files WHERE project_id = ?1 AND seen_at < ?2")?
            .query_map(params![fin.id, stamp], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut removed = 0usize;
        let mut delete =
            tx.prepare_cached("DELETE FROM project_files WHERE project_id = ?1 AND path = ?2")?;
        for path in stale.iter().filter(|p| !walked.under_unreadable(p)) {
            removed += delete.execute(params![fin.id, path])?;
        }
        tx.prepare_cached(
            "UPDATE projects SET missing = 0, engine = ?2, scanned_at = ?3, updated_at = ?4 WHERE id = ?1",
        )?
        .execute(params![fin.id, engine_name, stamp, now])?;
        Ok(removed)
    })?;
    result.removed += removed as u64;
    progress(1.0, &format!("{} を照らしました", p.name));
    Ok(Step::Done)
}

impl JobHandler for ProjectHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let ids: Vec<i64> = ctx
            .library()
            .db()
            .read(super::list)?
            .into_iter()
            .map(|p| p.id)
            .collect();
        // ハッシュは専用の小さなプールで（大域のプールをふさがない）。作れなければ 1 つずつ
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get())
                    .clamp(1, HASH_THREADS),
            )
            .thread_name(|i| format!("kura-project-{i}"))
            .build()
            .ok();
        let mut result = ScanResult::default();
        for (k, id) in ids.iter().enumerate() {
            if let Some(end) = interrupt(ctx) {
                return Ok(end);
            }
            if let Step::Stopped(end) =
                scan_one(ctx, pool.as_ref(), *id, (k, ids.len()), &mut result)?
            {
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
