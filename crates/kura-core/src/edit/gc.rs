//! 置き換えた版のファイルの片付け（GC。`JobKind::Gc`。M12 の段 3。docs/editing.md §14 の安全網 1・2、docs/data-model.md §4
//! 「保存と版（M12）」）。
//!
//! 1. **版**：消し待ち（waiting）の版のうち、置き換えから 7 日たち、かつ置き換えより前に作ったバックアップが残っていないもの
//!    （[`cutoff`]。M12 の利用者の決定 2）を、置き換えの古い順に片付ける。ほかの hold の版・アイテムの今・アンカーと台帳の行を
//!    共有しているものは、照合せずに released にする（台帳の行はほかが持つので、ファイルは消えない）。共有していないものは、
//!    消す前に版の文書と入力の記録から作り直して照らし（`save::verify_version`）、記録のエンコーダで書いたバイト列が出力と
//!    同じなら消し、違えば kept で残す。今は照らせないもの（コーデックがない・大きすぎる・OS のデコーダが要る）は消し待ちの
//!    まま次回へ。
//! 2. **消す手順**（ゴミ箱を空にする処理も同じ部品を使う。`Library::empty_trash`）：台帳の門（`Library::ingest_exclusive`）を
//!    取り、トランザクション 1 で候補がまだ消し待ちで同じ台帳の行を指していることを確かめ直して（作り直している間に保存・
//!    版に戻すで同じ中身が使われたら、共有になったので released にするだけ。S12）'delete' を予告してコミット、トランザクション
//!    2 で版を released（`file_blob_id` は NULL）にして `ledger::release`（どこからも参照されない行だけ消える）、コミットのあとで
//!    実ファイルと派生のファイルを消し（`ledger::remove_files`）、予告を消す（`ledger::clear_pending`）。途中で落ちたら、開くとき
//!    の `ledger::settle_pending` が片付ける（S13）。
//! 3. **アンカー**：どの版の入力からも参照されないアンカーの行とファイルを、同じ手順で片付ける（不変条件「参照されない
//!    アンカーはない」）。
//! 4. **文書の部品**：根（版・下書きの文書と、終わっていない保存のジョブ・7 日以内に失敗・中止した保存のジョブ（やり直せる
//!    ように）の入力の文書。`save::gc_roots`）から辿れない部品を消す（[`sweep_parts`]）。
//!
//! 譲る相手は取り込み・サムネイル・保存・解析・書き出し・ダウンロード・キャプチャ（`JobKind::yields_to`）。候補ごとに一時停止・
//! 中止・譲るを見て、カーソル（置き換えの時刻と版の行の ID）を候補ごとのトランザクションで途中経過に書く（再開はカーソルの
//! 次から。今は照らせなかった候補は、このジョブではもう見ない）。積むのは、ライブラリを開いたあと・保存のジョブがコミット
//! したあと・バックアップの 1 時間ごとの確かめで、候補（[`due`]）があるときだけ。

use std::collections::BTreeSet;
use std::sync::Arc;

use kura_engine::Digest;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::save::{self, Flow, Verdict};
use super::versions::{self, FileState};
use super::{EngineServices, anchors, store};
use crate::jobs::{Control, JobContext, JobEnd, JobHandler, JobId, save_partial};
use crate::library::{Layout, Library, backup, hex, ledger};
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 置き換えたファイルを残す長さ（7 日。バックアップの保持と同じ。docs/editing.md §14 の安全網 1）。
pub const GRACE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 文書の部品が孤立したかもしれない印（下書きを置き換えた・捨てた、保存のジョブが版を足さずに終わった）を置く settings の
/// キー。GC が部品を片付けたら消す（終わっていない保存のジョブがあれば残す：その根が終わってから孤立しうるので）。
pub const PARTS_KEY: &str = "gc.parts";

/// 1 回の問い合わせで読む候補の数。
const BATCH: usize = 16;

/// 今の時刻（UTC の Unix ミリ秒）を返す口。
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;
/// 残っているバックアップの作成時刻（名前の UTC。`backup::list`）を返す口。
pub type BackupsFn = Arc<dyn Fn(&Layout) -> Result<Vec<i64>> + Send + Sync>;

/// GC の時計とバックアップの一覧（テストと `kura-devtools edit crash-loop` の子が差し替える。アプリはいつも
/// [`GcClock::system`]）。
#[derive(Clone)]
pub struct GcClock {
    now: NowFn,
    backups: BackupsFn,
}

impl std::fmt::Debug for GcClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcClock").finish_non_exhaustive()
    }
}

impl Default for GcClock {
    fn default() -> Self {
        Self::system()
    }
}

impl GcClock {
    /// 本物の時計と、ライブラリの `backups/` の一覧。
    pub fn system() -> Self {
        Self::new(Arc::new(now_ms), Arc::new(backup_times))
    }

    /// 本物の時計を `offset_ms` だけ進めたもの（バックアップは本物の一覧）。`kura-devtools edit crash-loop` の子だけが使う
    /// （置き換えたばかりの版を 7 日たったものとして片付けさせる）。
    pub fn shifted(offset_ms: i64) -> Self {
        Self::new(
            Arc::new(move || now_ms().saturating_add(offset_ms)),
            Arc::new(backup_times),
        )
    }

    /// 時計とバックアップの一覧を渡して作る。
    pub fn new(now: NowFn, backups: BackupsFn) -> Self {
        Self { now, backups }
    }

    /// 今の時刻。
    pub fn now(&self) -> i64 {
        (self.now)()
    }

    /// 今の時刻とバックアップの一覧から、消してよい置き換えの時刻の上限（[`cutoff`]）。
    pub fn cutoff(&self, layout: &Layout) -> Result<i64> {
        Ok(cutoff(self.now(), &(self.backups)(layout)?))
    }
}

/// ライブラリの `backups/` に残っているバックアップの作成時刻（名前の UTC。秒の単位）。
fn backup_times(layout: &Layout) -> Result<Vec<i64>> {
    Ok(backup::list(layout)?
        .into_iter()
        .map(|b| b.created_at)
        .collect())
}

/// 消してよい置き換えの時刻の上限：置き換えから 7 日たち（`replaced_at <= now - 7日`）、かつ置き換えより前に作った
/// バックアップが残っていない（残っているうち最も古いものの作成時刻が置き換えより後：`replaced_at < oldest`）。バックアップが
/// なければ 7 日だけで決める（M12 の利用者の決定 2）。バックアップの名前は秒の単位なので、同じ秒の置き換えは「前に作った」と
/// みなす側（残す側）に倒れる。
pub fn cutoff(now: i64, backups: &[i64]) -> i64 {
    let by_age = now.saturating_sub(GRACE_MS);
    match backups.iter().min() {
        Some(oldest) => by_age.min(oldest.saturating_sub(1)),
        None => by_age,
    }
}

/// GC の候補があるか（積むか）：上限 `cutoff` までに置き換えた消し待ちの版（部分索引 `idx_item_versions_waiting` で引く）か、
/// 文書の部品の印。
pub fn due(conn: &Connection, cutoff: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM item_versions WHERE file_state = 'waiting' AND replaced_at <= ?1)",
    )?;
    if stmt.query_row([cutoff], |r| r.get::<_, bool>(0))? {
        return Ok(true);
    }
    parts_marked(conn)
}

/// 上限 `cutoff` までに置き換えた消し待ちの版の数（進捗と、整合性の確かめの「7 日を過ぎた消し待ち」）。
pub fn overdue(conn: &Connection, cutoff: i64) -> Result<u64> {
    let mut stmt = conn.prepare_cached(
        "SELECT count(*) FROM item_versions WHERE file_state = 'waiting' AND replaced_at <= ?1",
    )?;
    let n: i64 = stmt.query_row([cutoff], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// 照合が合わずに残した版（kept）の数（整合性の確かめ。版の表を走査するので、設定の「いま確かめる」だけで呼ぶ）。
pub fn kept(conn: &Connection) -> Result<u64> {
    let mut stmt =
        conn.prepare_cached("SELECT count(*) FROM item_versions WHERE file_state = 'kept'")?;
    let n: i64 = stmt.query_row([], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// 文書の部品が孤立したかもしれない印を置く（呼び手のトランザクションの中で）。
pub fn mark_parts(conn: &Connection) -> Result<()> {
    crate::settings::set(conn, PARTS_KEY, &true)
}

/// 文書の部品の印があるか。
pub fn parts_marked(conn: &Connection) -> Result<bool> {
    Ok(crate::settings::get::<bool>(conn, PARTS_KEY)?.unwrap_or(false))
}

/// 候補があれば GC を積む（`enqueue` は `JobManager::ensure_queued` か `JobContext::ensure_queued` を呼ぶ口）。積んだら true。
pub fn queue_if_due(
    library: &Library,
    clock: &GcClock,
    enqueue: impl FnOnce() -> Result<Option<JobId>>,
) -> Result<bool> {
    let cutoff = clock.cutoff(library.layout())?;
    if !library.db().read(|c| due(c, cutoff))? {
        return Ok(false);
    }
    Ok(enqueue()?.is_some())
}

// ---- 文書の部品 ----

/// 部品の片付けの範囲。
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    /// すべて：親の辺のない部品（文書の根と、孤立したノード・切れ目）から始める（`edit_objects` を走査する。GC のジョブだけ）。
    All,
    /// 渡した根の部品から始める（ゴミ箱を空にするとき：消すアイテムの版・下書きの文書）。
    Roots(&'a [i64]),
}

/// 根から辿れない文書の部品を消し、消した部品の数を返す（呼び手のトランザクションの中で）。根は版・下書きが指す部品と、
/// `live`（終わっていない保存のジョブの入力の根のダイジェスト）。範囲 `scope` の部品のうち、どこからも参照されない
/// （版・下書き・ほかの部品の辺のどれも指さない）ものを消し、その子へ進む。`edit_edges.child_id` は ON DELETE を持たないので、
/// 親から順に消す（親を消すと親の辺は CASCADE で消え、子は残った辺がなくなったときに消える）。
pub fn sweep_parts(tx: &Connection, scope: Scope<'_>, live: &[Digest]) -> Result<u64> {
    let mut live_ids = BTreeSet::new();
    for digest in live {
        if let Some(id) = store::doc_id_by_digest(tx, digest)? {
            live_ids.insert(id);
        }
    }
    let mut level: BTreeSet<i64> = match scope {
        Scope::All => {
            // 意図した走査（GC のジョブで、部品の印か消し待ちの候補があるときだけ。親の辺は idx_edit_edges_child で引く）
            let mut stmt = tx.prepare_cached(
                "SELECT o.id FROM edit_objects o
                  WHERE NOT EXISTS (SELECT 1 FROM edit_edges e WHERE e.child_id = o.id)",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        }
        Scope::Roots(ids) => ids.iter().copied().collect(),
    };
    let mut referenced = tx.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM edit_edges WHERE child_id = ?1)
             OR EXISTS (SELECT 1 FROM item_versions WHERE doc_id = ?1)
             OR EXISTS (SELECT 1 FROM edit_drafts WHERE doc_id = ?1)",
    )?;
    let mut children = tx.prepare_cached("SELECT child_id FROM edit_edges WHERE parent_id = ?1")?;
    let mut delete = tx.prepare_cached("DELETE FROM edit_objects WHERE id = ?1")?;
    let mut removed = 0u64;
    // 根 → ノード → 切れ目（部品の深さは 2 まで。壊れた形でも止まるよう上限を置く）
    for _ in 0..8 {
        if level.is_empty() {
            break;
        }
        let mut next = BTreeSet::new();
        for id in level {
            if live_ids.contains(&id) || referenced.query_row([id], |r| r.get::<_, bool>(0))? {
                continue;
            }
            let kids = children.query_map([id], |r| r.get::<_, i64>(0))?;
            for kid in kids {
                next.insert(kid?);
            }
            removed += delete.execute([id])? as u64;
        }
        level = next;
    }
    Ok(removed)
}

/// GC の部品の片付け（トランザクションの中で）：すべての範囲を片付け（保存のジョブの根は、終わっていないものと `now` の 7 日
/// 前から後に失敗・中止したもの）、終わっていない保存のジョブがなければ印を消す。
fn sweep_all_parts(tx: &Connection, now: i64) -> Result<u64> {
    let live = save::gc_roots(tx, now.saturating_sub(GRACE_MS))?;
    let removed = sweep_parts(tx, Scope::All, &live)?;
    tx.prepare_cached(
        "DELETE FROM settings WHERE key = ?1
            AND NOT EXISTS (SELECT 1 FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit')",
    )?
    .execute([PARTS_KEY])?;
    Ok(removed)
}

// ---- 版の候補 ----

/// GC の候補（消し待ちの版と、そのファイルの台帳の行）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    id: i64,
    item_id: i64,
    version: i64,
    replaced_at: i64,
    blob_id: i64,
    hash: [u8; 32],
    ext: String,
    /// ほかの hold の版・アイテムの今・アンカーと台帳の行を共有している（照合せずに released にしてよい）。
    shared: bool,
}

/// 再開のカーソル（置き換えの時刻と版の行の ID。この組より後の候補から）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cursor {
    replaced_at: i64,
    id: i64,
}

/// 台帳の行 `blob_id`（中身 `hash`）を、ほかの hold の版・アイテムの今・アンカーが参照しているか（3 つとも索引で引く）。
const SHARED: &str = "EXISTS (SELECT 1 FROM items i WHERE i.hash = ?2)
     OR EXISTS (SELECT 1 FROM item_versions w WHERE w.file_blob_id = ?1 AND w.file_state = 'hold')
     OR EXISTS (SELECT 1 FROM edit_anchors a WHERE a.blob_id = ?1)";

/// 上限 `cutoff` までに置き換えた消し待ちの版を、カーソル `after` の次から置き換えの古い順に `limit` 個（部分索引
/// `idx_item_versions_waiting` で引く。共有の判定は [`SHARED`] と同じ 3 つの索引）。
fn candidates(
    conn: &Connection,
    cutoff: i64,
    after: Option<Cursor>,
    limit: usize,
) -> Result<Vec<Candidate>> {
    let after = after.unwrap_or(Cursor {
        replaced_at: i64::MIN,
        id: i64::MIN,
    });
    let mut stmt = conn.prepare_cached(
        "SELECT v.id, v.item_id, v.version, v.replaced_at, v.file_blob_id, b.hash, b.ext,
                EXISTS (SELECT 1 FROM items i WHERE i.hash = b.hash)
             OR EXISTS (SELECT 1 FROM item_versions w WHERE w.file_blob_id = v.file_blob_id AND w.file_state = 'hold')
             OR EXISTS (SELECT 1 FROM edit_anchors a WHERE a.blob_id = v.file_blob_id)
           FROM item_versions v JOIN blobs b ON b.id = v.file_blob_id
          WHERE v.file_state = 'waiting' AND v.replaced_at <= ?1 AND (v.replaced_at, v.id) > (?2, ?3)
          ORDER BY v.replaced_at, v.id LIMIT ?4",
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt.query_map(params![cutoff, after.replaced_at, after.id, limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Vec<u8>>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, bool>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, item_id, version, replaced_at, blob_id, hash, ext, shared) = row?;
        let hash = <[u8; 32]>::try_from(hash.as_slice()).map_err(|_| {
            CoreError::Invalid(format!(
                "台帳の行 {blob_id} のハッシュが 32 バイトではありません"
            ))
        })?;
        out.push(Candidate {
            id,
            item_id,
            version,
            replaced_at,
            blob_id,
            hash,
            ext,
            shared,
        });
    }
    Ok(out)
}

/// 候補がまだ消し待ちで、同じ台帳の行を指しているか（門の中で確かめ直す）。
fn still_waiting(tx: &Connection, c: &Candidate) -> Result<bool> {
    let row: Option<(String, Option<i64>)> = tx
        .prepare_cached("SELECT file_state, file_blob_id FROM item_versions WHERE id = ?1")?
        .query_row([c.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(row.is_some_and(|(state, blob)| {
        state == FileState::Waiting.as_str() && blob == Some(c.blob_id)
    }))
}

/// 候補の台帳の行を、ほかが参照しているか。
fn is_shared(tx: &Connection, c: &Candidate) -> Result<bool> {
    let mut stmt = tx.prepare_cached(&format!("SELECT {SHARED}"))?;
    Ok(stmt.query_row(params![c.blob_id, &c.hash[..]], |r| r.get(0))?)
}

/// 候補の版を released にする（ファイルを持たない。消し待ちで同じ台帳の行を指しているときだけ）。変えたら true。
fn release_row(tx: &Connection, c: &Candidate) -> Result<bool> {
    let n = tx
        .prepare_cached(
            "UPDATE item_versions SET file_state = 'released', file_blob_id = NULL
              WHERE id = ?1 AND file_state = 'waiting' AND file_blob_id = ?2",
        )?
        .execute(params![c.id, c.blob_id])?;
    Ok(n == 1)
}

/// どの版の入力からも参照されないアンカーと、そのファイルの台帳の行（`edit_anchors` は元 × 読み方の設定ごとの小さな表なので
/// 走査する。入力は idx_item_version_inputs_anchor で引く）。
fn unreferenced_anchors(conn: &Connection) -> Result<Vec<(i64, [u8; 32], String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id
          WHERE NOT EXISTS (SELECT 1 FROM item_version_inputs n WHERE n.anchor_id = a.id)",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, hash, ext) = row?;
        if let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) {
            out.push((id, hash, ext));
        }
    }
    Ok(out)
}

// ---- ジョブ ----

/// 片付けの段（途中経過の `phase`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    #[default]
    Versions,
    Anchors,
    Parts,
}

/// GC のジョブの途中経過と結果（`jobs.result_json`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct GcState {
    v: u32,
    phase: Phase,
    cursor: Option<Cursor>,
    /// 照合して消した版。
    released: u64,
    /// 共有していたので照合せずに released にした版。
    shared: u64,
    /// 照合が合わずに残した版。
    kept: u64,
    /// 今は照らせず、消し待ちのまま次回へ回した版。
    deferred: u64,
    /// 片付けたアンカー。
    anchors: u64,
    /// 片付けた文書の部品。
    parts: u64,
    /// 消した台帳の行（実ファイル）。
    files: u64,
}

impl GcState {
    fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    fn message(&self) -> String {
        format!(
            "版 {} 件を片付けました（照合 {}・共有 {}、残した {}・次回へ {}）、アンカー {}、文書の部品 {}",
            self.released + self.shared,
            self.released,
            self.shared,
            self.kept,
            self.deferred,
            self.anchors,
            self.parts
        )
    }
}

/// 候補 1 つの、門の中での確かめ（トランザクション 1）の結果。
enum Pre {
    /// もう消し待ちでない・別の台帳の行を指す（保存・版に戻す・ゴミ箱を空にするで変わった）。
    Gone,
    /// 共有になった（作り直している間に保存・版に戻すで同じ中身が使われた。S12）：released にした。
    Shared,
    /// 'delete' を予告した（予告の行の ID）。
    Announced(i64),
}

/// テストが段の途中で割り込む所（S12：作り直している間に同じ中身を使う）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GcStage {
    /// 照合が一致したあと、台帳の門を取る前。
    Verified { version_id: i64 },
}

/// テストの割り込み。
#[cfg(test)]
pub(crate) type GcHook = Arc<dyn Fn(GcStage) + Send + Sync>;

/// S13 の試験の、強制終了を写す段（その段の直後に、後始末を通らずに失敗する）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GcFault {
    /// 'delete' の予告をコミットしたあと（台帳の行はまだある）。
    AfterAnnounce,
    /// 台帳の行を解放したあと、ファイルを消す前。
    AfterRelease,
}

/// GC のジョブの処理。エンジンの部品（照合の作り直し）は、このライブラリのもの。
pub struct GcHandler {
    engine: Arc<EngineServices>,
    clock: GcClock,
    #[cfg(test)]
    hook: Option<GcHook>,
    #[cfg(test)]
    fault: Option<GcFault>,
}

impl GcHandler {
    /// このライブラリのエンジンの部品と時計で片付ける。
    pub fn new(engine: Arc<EngineServices>, clock: GcClock) -> Self {
        Self {
            engine,
            clock,
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            fault: None,
        }
    }

    /// 段ごとに `hook` を呼ぶ（テストの割り込み）。
    #[cfg(test)]
    pub(crate) fn with_hook(mut self, hook: GcHook) -> Self {
        self.hook = Some(hook);
        self
    }

    /// 段 `fault` の直後で、後始末を通らずに失敗させる（テストの強制終了）。
    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: GcFault) -> Self {
        self.fault = Some(fault);
        self
    }

    #[cfg(test)]
    fn at(&self, stage: GcStage) {
        if let Some(hook) = &self.hook {
            hook(stage);
        }
    }

    #[cfg(test)]
    fn crash_at(&self, at: GcFault) -> Result<()> {
        if self.fault == Some(at) {
            return Err(CoreError::Invalid(format!("テスト用の強制終了（{at:?}）")));
        }
        Ok(())
    }
}

/// 一時停止・中止・譲るの指示があれば、その終わり方。
fn interrupted(ctx: &JobContext, st: &GcState) -> Option<JobEnd> {
    match ctx.control() {
        Control::Pause => Some(JobEnd::Paused),
        Control::Cancel => Some(JobEnd::Canceled {
            result_json: Some(st.json()),
        }),
        Control::Continue if ctx.should_yield() => Some(JobEnd::Yield),
        Control::Continue => None,
    }
}

/// 照合の中で止めたときの終わり方（中止なら途中経過を結果に）。
fn stopped(end: JobEnd, st: &GcState) -> JobEnd {
    match end {
        JobEnd::Canceled { .. } => JobEnd::Canceled {
            result_json: Some(st.json()),
        },
        other => other,
    }
}

impl JobHandler for GcHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let library = ctx.library();
        let db = library.db();
        let job = ctx.id();
        let mut st: GcState = ctx
            .partial()
            .and_then(|p| serde_json::from_str::<GcState>(p).ok())
            .filter(|s| s.v == 1)
            .unwrap_or_default();
        st.v = 1;
        if st.phase == Phase::Versions {
            let cutoff = self.clock.cutoff(library.layout())?;
            let total = db.read(|c| overdue(c, cutoff))?.max(1);
            let mut seen = 0u64;
            loop {
                let cursor = st.cursor;
                let batch = db.read(|c| candidates(c, cutoff, cursor, BATCH))?;
                if batch.is_empty() {
                    break;
                }
                for cand in batch {
                    if let Some(end) = interrupted(ctx, &st) {
                        return Ok(end);
                    }
                    if let Flow::Stop(end) = self.collect(ctx, &cand, &mut st)? {
                        return Ok(stopped(end, &st));
                    }
                    seen += 1;
                    ctx.progress(
                        (seen as f64 / total as f64).min(1.0) * 0.8,
                        "置き換えた版を片付けています",
                    );
                }
            }
            st.phase = Phase::Anchors;
            let json = st.json();
            db.write(move |tx| save_partial(tx, job, &json, 0.8, "アンカーを片付けています"))?;
        }
        if st.phase == Phase::Anchors {
            if let Some(end) = interrupted(ctx, &st) {
                return Ok(end);
            }
            self.sweep_anchors(ctx, &mut st)?;
        }
        if let Some(end) = interrupted(ctx, &st) {
            return Ok(end);
        }
        // 文書の部品（1 つのトランザクション。印もここで消す）
        let mut done = st.clone();
        done.phase = Phase::Parts;
        let now = self.clock.now();
        let (parts, done) = db.write(move |tx| {
            let parts = sweep_all_parts(tx, now)?;
            done.parts += parts;
            save_partial(tx, job, &done.json(), 1.0, "片付けました")?;
            Ok((parts, done))
        })?;
        if parts > 0 {
            ctx.log(&format!("孤立した文書の部品を {parts} 個消しました"));
        }
        let message = done.message();
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: done.json(),
            message: Some(message),
        })
    }
}

impl GcHandler {
    /// 候補 1 つ：共有していれば released にし、していなければ照合して、一致すれば消し、合わなければ kept、照らせなければ
    /// 次回へ。カーソルは候補ごとのトランザクションで途中経過に書く（照合の途中で止めたら書かず、再開でやり直す）。
    fn collect(&self, ctx: &JobContext, cand: &Candidate, st: &mut GcState) -> Result<Flow<()>> {
        let library = ctx.library();
        let db = library.db();
        let job = ctx.id();
        let cursor = Cursor {
            replaced_at: cand.replaced_at,
            id: cand.id,
        };
        if cand.shared {
            let _gate = library.ingest_exclusive();
            let (c, mut next) = (cand.clone(), st.clone());
            next.cursor = Some(cursor);
            let (done, next) = db.write(move |tx| {
                let done = if !still_waiting(tx, &c)? {
                    true
                } else if is_shared(tx, &c)? {
                    if release_row(tx, &c)? {
                        next.shared += 1;
                    }
                    true
                } else {
                    false
                };
                if done {
                    save_partial(tx, job, &next.json(), 0.0, "置き換えた版を片付けています")?;
                }
                Ok((done, next))
            })?;
            if done {
                *st = next;
                return Ok(Flow::Go(()));
            }
            // 共有でなくなった（ほかの持ち主が先に消えた）：照合してから消す
        }
        let version = db
            .read(|c| versions::get(c, cand.item_id, cand.version))?
            .filter(|v| {
                v.id == cand.id
                    && v.file_state == FileState::Waiting
                    && v.file_blob_id == Some(cand.blob_id)
            });
        let Some(version) = version else {
            // 読むまでの間に、保存・版に戻す・ゴミ箱を空にするで変わった
            let mut next = st.clone();
            next.cursor = Some(cursor);
            let json = next.json();
            db.write(move |tx| save_partial(tx, job, &json, 0.0, "置き換えた版を片付けています"))?;
            *st = next;
            return Ok(Flow::Go(()));
        };
        let verdict = match save::verify_version(&self.engine, ctx, &version)? {
            Flow::Go(verdict) => verdict,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        match verdict {
            Verdict::Defer(why) => {
                ctx.log(&format!(
                    "アイテム {} の版 {} は今は照らせないので、次回にします：{why}",
                    cand.item_id, cand.version
                ));
                let mut next = st.clone();
                next.cursor = Some(cursor);
                next.deferred += 1;
                let json = next.json();
                db.write(move |tx| {
                    save_partial(tx, job, &json, 0.0, "置き換えた版を片付けています")
                })?;
                *st = next;
            }
            Verdict::Mismatch(why) => {
                ctx.log(&format!(
                    "アイテム {} の版 {} は作り直しが記録と合わないので、ファイルを残します：{why}",
                    cand.item_id, cand.version
                ));
                let (c, mut next) = (cand.clone(), st.clone());
                next.cursor = Some(cursor);
                *st = db.write(move |tx| {
                    let n = tx
                        .prepare_cached(
                            "UPDATE item_versions SET file_state = 'kept'
                              WHERE id = ?1 AND file_state = 'waiting' AND file_blob_id = ?2",
                        )?
                        .execute(params![c.id, c.blob_id])?;
                    next.kept += n as u64;
                    save_partial(tx, job, &next.json(), 0.0, "置き換えた版を片付けています")?;
                    Ok(next)
                })?;
            }
            Verdict::Match => {
                #[cfg(test)]
                self.at(GcStage::Verified {
                    version_id: cand.id,
                });
                self.remove(ctx, cand, cursor, st)?;
            }
        }
        Ok(Flow::Go(()))
    }

    /// 照合が一致した候補を消す：門を取り、確かめ直して 'delete' を予告してコミット → released にして台帳の行を解放して
    /// コミット → 実ファイルと派生のファイルを消す → 予告を消す。
    fn remove(
        &self,
        ctx: &JobContext,
        cand: &Candidate,
        cursor: Cursor,
        st: &mut GcState,
    ) -> Result<()> {
        let library = ctx.library();
        let db = library.db();
        let job = ctx.id();
        let _gate = library.ingest_exclusive();
        let (c, mut next) = (cand.clone(), st.clone());
        next.cursor = Some(cursor);
        // トランザクション 1：確かめ直して予告する
        let (pre, next) = db.write(move |tx| {
            if !still_waiting(tx, &c)? {
                save_partial(tx, job, &next.json(), 0.0, "置き換えた版を片付けています")?;
                return Ok((Pre::Gone, next));
            }
            if is_shared(tx, &c)? {
                if release_row(tx, &c)? {
                    next.shared += 1;
                }
                save_partial(tx, job, &next.json(), 0.0, "置き換えた版を片付けています")?;
                return Ok((Pre::Shared, next));
            }
            let id = ledger::announce(
                tx,
                &c.hash,
                &c.ext,
                ledger::PendingKind::Delete,
                Some(job),
                now_ms(),
            )?;
            Ok((Pre::Announced(id), next))
        })?;
        let pending = match pre {
            Pre::Gone | Pre::Shared => {
                *st = next;
                return Ok(());
            }
            Pre::Announced(id) => id,
        };
        #[cfg(test)]
        self.crash_at(GcFault::AfterAnnounce)?;
        // トランザクション 2：released にして台帳の行を解放する（共有でないことは門の中で確かめた）
        let (c, mut next) = (cand.clone(), next);
        let (released, next) = db.write(move |tx| {
            let released = if release_row(tx, &c)? {
                next.released += 1;
                ledger::release(tx, &[c.hash])?
            } else {
                Vec::new()
            };
            next.files += released.len() as u64;
            save_partial(tx, job, &next.json(), 0.0, "置き換えた版を片付けています")?;
            Ok((released, next))
        })?;
        *st = next;
        #[cfg(test)]
        self.crash_at(GcFault::AfterRelease)?;
        // コミットのあと：ファイルを消してから予告を消す（消せなければ予告を残し、次に開くときの照合が消す）
        if ledger::remove_files(library.layout(), &released) == 0 {
            db.write(move |tx| ledger::clear_pending(tx, &[pending]).map(|_| ()))?;
        }
        if !released.is_empty() {
            ctx.log(&format!(
                "アイテム {} の版 {} のファイル（{}）を消しました",
                cand.item_id,
                cand.version,
                hex(&cand.hash)
            ));
        }
        Ok(())
    }

    /// どの版の入力からも参照されないアンカーを、消す手順で片付ける（ふつうは 0 個。ゴミ箱を空にする処理が片付けるので）。
    fn sweep_anchors(&self, ctx: &JobContext, st: &mut GcState) -> Result<()> {
        let library = ctx.library();
        let db = library.db();
        let job = ctx.id();
        let _gate = library.ingest_exclusive();
        let (ids, pending) = db.write(move |tx| {
            let rows = unreferenced_anchors(tx)?;
            let mut pending = Vec::with_capacity(rows.len());
            for (_, hash, ext) in &rows {
                pending.push(ledger::announce(
                    tx,
                    hash,
                    ext,
                    ledger::PendingKind::Delete,
                    Some(job),
                    now_ms(),
                )?);
            }
            Ok((
                rows.into_iter().map(|(id, ..)| id).collect::<Vec<_>>(),
                pending,
            ))
        })?;
        let mut next = st.clone();
        next.phase = Phase::Parts;
        let (released, next) = db.write(move |tx| {
            let hashes = anchors::sweep_unreferenced(tx, &ids)?;
            next.anchors += hashes.len() as u64;
            let released = ledger::release(tx, &hashes)?;
            next.files += released.len() as u64;
            save_partial(tx, job, &next.json(), 0.9, "文書の部品を片付けています")?;
            Ok((released, next))
        })?;
        *st = next;
        if ledger::remove_files(library.layout(), &released) == 0 && !pending.is_empty() {
            db.write(move |tx| ledger::clear_pending(tx, &pending).map(|_| ()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use crate::edit::testing::{insert_item, insert_version};
    use kura_engine::Graph;
    use kura_engine::doc_parts::split;
    use serde_json::json;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    /// 上限：7 日たち、かつ置き換えより前のバックアップが残っていない（いちばん古いバックアップの 1 ms 前まで）。境界を含む。
    #[test]
    fn cutoff_is_seven_days_and_before_the_oldest_backup() {
        let now = 100 * DAY;
        assert_eq!(cutoff(now, &[]), now - 7 * DAY);
        let c = cutoff(now, &[]);
        assert!(now - 7 * DAY <= c, "ちょうど 7 日は消してよい");
        assert!(now - 7 * DAY + 1 > c, "7 日に 1 ms 足りなければ残す");
        // いちばん古いバックアップより前に置き換えたものだけ（同じ時刻は残す）
        let oldest = 90 * DAY;
        assert_eq!(cutoff(now, &[95 * DAY, oldest, 99 * DAY]), oldest - 1);
        // バックアップが新しければ 7 日だけで決まる
        assert_eq!(cutoff(now, &[99 * DAY]), now - 7 * DAY);
        assert_eq!(cutoff(i64::MIN, &[i64::MIN]), i64::MIN);
    }

    /// 候補の問い合わせ：消し待ちで上限までのものを、カーソルの次から置き換えの古い順に。共有（アイテムの今・hold の版・
    /// アンカー）を見分ける。部分索引で引き、版の表を走査しない。
    #[test]
    fn candidates_follow_the_cursor_and_see_sharing() {
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        insert_version(&conn, a, 0, [10; 32], "hold", None);
        insert_version(&conn, a, 1, [11; 32], "waiting", None);
        insert_version(&conn, a, 2, [12; 32], "waiting", None);
        insert_version(&conn, a, 3, [1; 32], "hold", None);
        conn.execute_batch(
            "UPDATE item_versions SET replaced_at = 50 WHERE version = 1;
             UPDATE item_versions SET replaced_at = 40 WHERE version = 2;",
        )
        .unwrap();
        let all = candidates(&conn, 100, None, 10).unwrap();
        assert_eq!(
            all.iter()
                .map(|c| (c.version, c.shared))
                .collect::<Vec<_>>(),
            vec![(2, false), (1, false)]
        );
        assert!(due(&conn, 100).unwrap());
        assert!(due(&conn, 45).unwrap());
        assert!(!due(&conn, 39).unwrap());
        assert_eq!(overdue(&conn, 45).unwrap(), 1);
        let after = Cursor {
            replaced_at: all[0].replaced_at,
            id: all[0].id,
        };
        let rest = candidates(&conn, 100, Some(after), 10).unwrap();
        assert_eq!(rest.iter().map(|c| c.version).collect::<Vec<_>>(), vec![1]);
        assert_eq!(candidates(&conn, 100, None, 1).unwrap().len(), 1);
        // 版 1 の中身を別のアイテムの今にすると共有
        insert_item(&conn, 2, [11; 32]);
        let all = candidates(&conn, 100, None, 10).unwrap();
        assert_eq!(
            all.iter()
                .map(|c| (c.version, c.shared))
                .collect::<Vec<_>>(),
            vec![(2, false), (1, true)]
        );
        let plan = query_plan(
            &conn,
            "SELECT v.id, v.item_id, v.version, v.replaced_at, v.file_blob_id, b.hash, b.ext,
                    EXISTS (SELECT 1 FROM items i WHERE i.hash = b.hash)
                 OR EXISTS (SELECT 1 FROM item_versions w WHERE w.file_blob_id = v.file_blob_id AND w.file_state = 'hold')
                 OR EXISTS (SELECT 1 FROM edit_anchors a WHERE a.blob_id = v.file_blob_id)
               FROM item_versions v JOIN blobs b ON b.id = v.file_blob_id
              WHERE v.file_state = 'waiting' AND v.replaced_at <= 100 AND (v.replaced_at, v.id) > (1, 2)
              ORDER BY v.replaced_at, v.id LIMIT 16",
        );
        assert!(plan.contains("idx_item_versions_waiting"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        for index in [
            "sqlite_autoindex_items_1",
            "idx_item_versions_file_blob",
            "idx_edit_anchors_blob",
        ] {
            assert!(plan.contains(index), "{index}: {plan}");
        }
        for sql in [
            "SELECT EXISTS (SELECT 1 FROM item_versions WHERE file_state = 'waiting' AND replaced_at <= 5)",
            "SELECT count(*) FROM item_versions WHERE file_state = 'waiting' AND replaced_at <= 5",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(plan.contains("idx_item_versions_waiting"), "{plan}");
            assert!(!plan.contains("SCAN item_versions"), "{plan}");
        }
    }

    /// 部品の印：置けば候補があることになり、終わっていない保存のジョブがなければ片付けで消える。
    #[test]
    fn the_parts_mark_makes_gc_due_until_swept() {
        let conn = test_conn();
        assert!(!due(&conn, i64::MAX).unwrap());
        mark_parts(&conn).unwrap();
        assert!(due(&conn, 0).unwrap());
        // 保存のジョブが待っていれば、印は残す（その根があとで孤立しうる）
        conn.execute(
            "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('edit', 'queued', '{}', 1)",
            [],
        )
        .unwrap();
        sweep_all_parts(&conn, 10).unwrap();
        assert!(parts_marked(&conn).unwrap());
        conn.execute("UPDATE jobs SET state = 'failed', finished_at = 5", [])
            .unwrap();
        sweep_all_parts(&conn, 10).unwrap();
        assert!(!parts_marked(&conn).unwrap());
        assert!(!due(&conn, 0).unwrap());
    }

    fn doc_with(n: usize, axis: &str) -> Graph {
        let actions: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                json!({ "type": "fill", "at": { "x": i, "y": 3 }, "color": { "rgba": [0, 0, 0, 0] },
                        "tolerance": 30, "connectivity": "eight", "contiguous": true, "frame": 0 })
            })
            .collect();
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
            .unwrap();
        g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
            .unwrap();
        g
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// 部品の片付け：版・下書き・終わっていない保存のジョブの根から辿れるものは残し、孤立したもの（置き換えた下書き・失敗した
    /// ジョブ）を消す。文書どうしで共有する切れ目・ノードは、どれかの根から辿れれば残す。孤立したノード・切れ目も消す。
    #[test]
    fn sweep_parts_keeps_reachable_parts_and_removes_orphans() {
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        insert_version(&conn, a, 0, [10; 32], "hold", None);
        // 版 1 の文書（筆 200 本）・下書き（210 本：前の切れ目を共有）・待っているジョブの文書・孤立した文書（失敗した保存）
        let v1 = split(&doc_with(200, "vertical")).unwrap();
        let draft = split(&doc_with(210, "vertical")).unwrap();
        let queued = split(&doc_with(3, "horizontal")).unwrap();
        let orphan = split(&doc_with(130, "horizontal")).unwrap();
        let v1_id = store::put_doc(&conn, &v1, 1).unwrap();
        let draft_id = store::put_doc(&conn, &draft, 1).unwrap();
        store::put_doc(&conn, &queued, 1).unwrap();
        let orphan_id = store::put_doc(&conn, &orphan, 1).unwrap();
        insert_version(&conn, a, 1, [1; 32], "hold", None);
        conn.execute(
            "UPDATE item_versions SET doc_id = ?1 WHERE version = 1",
            [v1_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edit_drafts (item_id, doc_id, base_hash, base_version, updated_at) VALUES (1, ?1, x'00', 1, 1)",
            [draft_id],
        )
        .unwrap();
        // 孤立したノード（根が消えた形）
        conn.execute(
            "INSERT INTO edit_objects (digest, kind, body, created_at) VALUES (zeroblob(32), 'node', x'7b7d', 1)",
            [],
        )
        .unwrap();
        let before = count(&conn, "SELECT count(*) FROM edit_objects");
        let live = vec![queued.root];
        // 範囲を根に絞ると、渡した根（孤立した文書）とその下だけを消す
        let removed = sweep_parts(&conn, Scope::Roots(&[orphan_id, v1_id]), &live).unwrap();
        // 孤立した文書（130 本＝切れ目 64+64+2）のうち、ほかと共有しないもの：根・描画のノード・最後の切れ目（前の 2 つの
        // 切れ目は版 1 と同じ中身、反転のノードは待っているジョブの文書と同じ中身）
        assert_eq!(removed, 3);
        assert!(store::load_doc_bytes(&conn, v1_id).is_ok());
        assert!(store::load_doc_bytes(&conn, draft_id).is_ok());
        // すべての範囲：孤立したノードと、版 1 が指さなくなった空の文書（テストの補助が先に置いたもの）も消す。ジョブの根は残す
        let removed = sweep_parts(&conn, Scope::All, &live).unwrap();
        assert_eq!(removed, 2);
        let left = count(&conn, "SELECT count(*) FROM edit_objects");
        assert_eq!(left, before - 3 - 2);
        let queued_id = store::doc_id_by_digest(&conn, &queued.root)
            .unwrap()
            .unwrap();
        assert!(store::load_doc_bytes(&conn, queued_id).is_ok());
        // ジョブが終われば、その文書も孤立して消える。下書きを消すと、版 1 と共有しない最後の切れ目・ノード・根が消える
        let removed = sweep_parts(&conn, Scope::All, &[]).unwrap();
        assert!(removed > 0);
        assert!(
            store::doc_id_by_digest(&conn, &queued.root)
                .unwrap()
                .is_none()
        );
        conn.execute("DELETE FROM edit_drafts", []).unwrap();
        let removed = sweep_parts(&conn, Scope::All, &[]).unwrap();
        // 下書きの根・描画のノード・最後の切れ目（210 本の 4 つ目。前の 3 つは版 1 と共有）。反転のノードは共有
        assert_eq!(removed, 3);
        assert!(store::load_doc_bytes(&conn, v1_id).is_ok());
        assert!(crate::db::violations(&conn).unwrap().is_empty());
        assert!(versions::check_invariants(&conn).unwrap().is_empty());
        // 問い合わせは索引で引く（すべての範囲の出発点だけ部品の表を走査する）
        let plan = query_plan(
            &conn,
            "SELECT EXISTS (SELECT 1 FROM edit_edges WHERE child_id = 1)
                 OR EXISTS (SELECT 1 FROM item_versions WHERE doc_id = 1)
                 OR EXISTS (SELECT 1 FROM edit_drafts WHERE doc_id = 1)",
        );
        for index in [
            "idx_edit_edges_child",
            "idx_item_versions_doc",
            "idx_edit_drafts_doc",
        ] {
            assert!(plan.contains(index), "{index}: {plan}");
        }
        assert!(
            !plan.replace("SCAN CONSTANT ROW", "").contains("SCAN"),
            "{plan}"
        );
        let plan = query_plan(&conn, "SELECT child_id FROM edit_edges WHERE parent_id = 1");
        assert!(plan.contains("PRIMARY KEY"), "{plan}");
        let plan = query_plan(
            &conn,
            "SELECT o.id FROM edit_objects o WHERE NOT EXISTS (SELECT 1 FROM edit_edges e WHERE e.child_id = o.id)",
        );
        assert!(plan.contains("idx_edit_edges_child"), "{plan}");
    }

    /// 部品の片付けで残す保存のジョブの根：終わっていないものと、`since` 以後に失敗・中止したもの（版に戻すは文書を持たない）。
    /// どちらも状態の索引で引く。
    #[test]
    fn gc_roots_keep_unfinished_and_recently_ended_saves() {
        let conn = test_conn();
        let job = |state: &str, finished: Option<i64>, action: serde_json::Value| {
            conn.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at, finished_at) VALUES ('edit', ?1, ?2, 1, ?3)",
                params![
                    state,
                    json!({ "v": 1, "itemId": 1, "base": { "hash": "00".repeat(32), "version": 0 }, "action": action })
                        .to_string(),
                    finished
                ],
            )
            .unwrap();
        };
        let doc = |b: u8| json!({ "type": "overwrite", "doc": hex(&[b; 32]) });
        job("queued", None, doc(1));
        job("paused", None, doc(2));
        job("failed", Some(100), doc(3));
        job("canceled", Some(50), doc(4));
        job("done", Some(200), doc(5));
        job("running", None, json!({ "type": "revert", "to": 0 }));
        let roots = save::gc_roots(&conn, 60).unwrap();
        assert_eq!(
            roots,
            vec![Digest([1; 32]), Digest([2; 32]), Digest([3; 32])]
        );
        assert_eq!(
            save::live_roots(&conn).unwrap(),
            vec![Digest([1; 32]), Digest([2; 32])]
        );
        let plan = query_plan(
            &conn,
            "SELECT input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit'
             UNION ALL
             SELECT input_json FROM jobs
              WHERE state IN ('failed', 'canceled') AND kind = 'edit' AND finished_at >= 1",
        );
        assert!(plan.contains("idx_jobs_queue"), "{plan}");
        assert!(!plan.contains("SCAN jobs"), "{plan}");
    }

    /// 参照されないアンカーの問い合わせは入力を索引で引く（アンカーの表は小さいので走査する）。
    #[test]
    fn unreferenced_anchors_use_the_input_index() {
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        let used = crate::edit::testing::insert_anchor(&conn, [10; 32], [20; 32]);
        let free = crate::edit::testing::insert_anchor(&conn, [10; 32], [21; 32]);
        insert_version(&conn, a, 0, [10; 32], "hold", None);
        insert_version(&conn, a, 1, [1; 32], "hold", Some(used));
        let found = unreferenced_anchors(&conn).unwrap();
        assert_eq!(found, vec![(free, [21; 32], "wav".to_string())]);
        let plan = query_plan(
            &conn,
            "SELECT a.id, b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id
              WHERE NOT EXISTS (SELECT 1 FROM item_version_inputs n WHERE n.anchor_id = a.id)",
        );
        assert!(plan.contains("idx_item_version_inputs_anchor"), "{plan}");
    }

    /// 途中経過の形（再開のカーソルと数）。知らない項目は読み捨て、足りない項目は 0。
    #[test]
    fn state_round_trips() {
        let st = GcState {
            v: 1,
            phase: Phase::Anchors,
            cursor: Some(Cursor {
                replaced_at: 5,
                id: 7,
            }),
            released: 2,
            ..GcState::default()
        };
        let text = st.json();
        assert!(text.contains(r#""phase":"anchors""#), "{text}");
        assert!(text.contains(r#""replacedAt":5"#), "{text}");
        assert_eq!(serde_json::from_str::<GcState>(&text).unwrap(), st);
        let partial: GcState = serde_json::from_str(r#"{"v":1,"kept":3,"later":true}"#).unwrap();
        assert_eq!((partial.kept, partial.phase), (3, Phase::Versions));
    }
}
