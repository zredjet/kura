//! 保存のジョブ（`JobKind::Edit`。M12 の段 2。docs/editing.md §14、docs/data-model.md §4「保存と版（M12）」）。
//!
//! 上書き（アイテム ID を保ったまま中身を差し替えて版を足す）・別名で保存（新しいアイテムを作り、元のアイテムの元を
//! 引き継いで版 1 に文書を置く）・版に戻す（元に戻す・途中の版 k に戻す。版 k の中身で新しい版を足す。[`revert`]）。
//!
//! 手順（docs/editing.md §14 の表）：
//! 0. 確かめ：このジョブが頭にした版があれば、前回コミットしたので Done（再開）。前回 OS のデコーダで元を読んでいる途中で
//!    落ちていれば（途中経過の `decoding` が真）、同じファイルで落ち続けないよう失敗にする。文書を読み、土台を確かめる。
//! 1. 予算：元（版 0。なければ今）の形をヘッダから読み（アンカーがあればアンカーの形）、計画を作って一括の予算を確保する
//!    （アンカーを作りうるなら、読む持ち越し ＋ 入力の値 ＋ アンカーを読み直す分との大きいほう。`admit_bulk_with`）。OS の
//!    デコーダを通りうるときは、見出しを読む前に `decoding` を真にしてコミットし、2 で読み終えるまで保つ（[`read`]）。
//! 2. 読む：アンカーがあればアンカーから（記録はアンカーの元の読み取りの記録）、なければ元を読む。アンカーが使えなければ、
//!    元を記録の識別子のデコーダで読み直して値を照らす（同じなら警告を残して続け、違えば Mismatch）。読み終えたら印を偽に戻す。
//! 3. アンカー：再現を約束しないデコーダで読み、まだアンカーがなければ、`staging/edit-<job>-anchor.<png|wav>.part` に書いて
//!    `sync_all` し、読み直して照らしてハッシュを取る（M12 の決定 7。描いたのと同じ読み取りから）。
//! 4. 描く（`Engine::render`）。5. 書く：エンコードしてハッシュ H を取り、`staging/edit-<job>-out.<ext>.part` に書いて
//!    `sync_all`。ここで予算と値を手放す。5 までは一時停止・中止で止まる（staging を消す）。
//! 6. 予告：台帳の門（`Library::ingest_exclusive`）を取り、トランザクション 1 で土台と、読んだアンカーの行が残っていることを
//!    確かめ直し（ゴミ箱を空にして片付けられていれば Conflict）、H の持ち主で Conflict か existing を決め、拡張子を決め
//!    （台帳に行があればその拡張子）、台帳にない出力とアンカーを予告する（`ledger::announce`）。
//! 7. 配置：`files/` へ同じボリュームの中で改名する（同じ中身がもうあれば staging を消す）。
//! 8. コミット：トランザクション 2 で土台を確かめ直し、版 0（最初の保存）・アンカー・アイテムの置き換え（か新しいアイテム）・
//!    前の頭・新しい頭・入力の記録を書き、下書きを片付け（`drafts::settle_after_save`）、自分の予告を消し、途中経過に結果を
//!    書く。ここで Conflict になったら（門の間にゴミ箱に入れられた）、門を持ったまま、台帳に載っていない置いたファイルを
//!    消して予告を消してから失敗にする。
//! 9. 後始末：門を放し、staging の残りを消し、サムネイル・解析・プロキシのジョブを積む（GC の時計を渡されていれば、GC の
//!    候補があるときだけ GC も。M12 の段 3）。
//!
//! 1 件ずつの単位（M14 段 2）：手順はジョブの文脈によらない [`Saver`] が、1 件の場（[`UnitCx`]：ライブラリ・止める指示・進み・
//! ログ・読む前の印・持ち主のジョブ・書きかけの印）で動かす。0 の再開の確かめと 9 の後のジョブを積むのは呼び手（保存のジョブは
//! [`EditHandler`]。一括のジョブは段 4）。1〜5 は [`Saver::prepare`]（版に戻すの作り直しは [`Saver::prepare_rebuild`]）で、
//! 1 件ごとに並べてよい。6〜8 は [`Saver::commit`]（版に戻すの使い直しは [`Saver::revert_reuse`]）で、門を持つので順に呼ぶ。
//! 8 の結果は呼び手の [`Record`] がトランザクション 2 の中で書く（保存のジョブは途中経過の `committed`）。
//!
//! 積む口（[`start_save`]）は、文書の部品を置くのと同じトランザクションでジョブを積む（GC の部品の片付けが、積む前の部品を
//! 消さないように）。上書き・別名が版を足さずに終わったら（失敗・中止・同じ中身のアイテムがある）、その文書の部品が孤立しうる
//! ので GC の部品の印（`gc::mark_parts`）を置く。GC の照合（[`verify_version`]）は版に戻すの作り直しと同じ読み・描き・書きを使う。
//!
//! 版に戻すときは、版 k の中身が台帳にあってファイルがあれば、門を持って 1 つのトランザクションで戻す（描かない）。なければ
//! 版 k の文書と入力の記録から 1〜5 で作り直し（値のダイジェストを記録と照らす）、6〜9 は上と同じ（[`revert`]）。
//!
//! 強制終了：1〜5 は staging の掃除とジョブの再開で初めから。6 のあとは開くときの予告の照合（`ledger::settle_pending`）が
//! 置いたファイルを消して予告を消し、ジョブは初めから。8 のあとは 0 がこのジョブの版を見つけて Done（版は 1 つだけ）。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::budget::value_bytes;
use kura_engine::codec::anchor::{self, AnchorContext};
use kura_engine::codec::formats::{format_by_ext, format_by_id};
use kura_engine::codec::recorded::{self, Recorded, RecordedInput};
use kura_engine::codec::{DecodeStamp, EncodedStamp, Tier};
use kura_engine::digest::canonical;
use kura_engine::doc::{
    DecodeOptions, Expand, FormatChoice, InputId, InputRole as DocRole, OutputId,
};
use kura_engine::doc_parts::split;
use kura_engine::render::InputValue;
use kura_engine::render::estimate::{EstimateSpec, Sink, Source};
use kura_engine::{Digest, Dim, EngineError, Graph, Limits, Monitor, Shape, Value};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::anchors::{self, Anchor, NewAnchor};
use super::gc::{self, GcClock};
use super::versions::{
    self, FileState, Head, InputRole, MediaRecord, NewInput, NewVersion, OriginalFile, VersionKind,
    VersionRecord,
};
use super::work_pcm::WorkKey;
use super::{EngineServices, drafts, from_engine, store};
use crate::db::Db;
use crate::import::place::{Staged, place};
use crate::import::store::{NewItem, Owner, OwnerRole, find_owner, insert_item};
use crate::import::{MediaKind, format_id, hash_file, mime_for};
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, JobState, save_partial,
};
use crate::library::{Layout, hex, ledger};
use crate::media::MediaInfo;
use crate::text::nfc;
use crate::time::now_ms;
use crate::{CoreError, Result};

/// ジョブの入力の形の版。
const INPUT_V: u32 = 1;

/// 保存の土台：エディタを開いたときの今（`items.hash`）と頭の版。保存のときに今・頭の版が違えば Conflict。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Base {
    pub hash: [u8; 32],
    pub version: i64,
}

/// 保存の操作。
#[derive(Debug, Clone, PartialEq)]
pub enum SaveAction {
    /// 上書き（`graph` は元に対する累積の文書）。
    Overwrite { graph: Graph },
    /// 別名で保存（新しいアイテムの表示名 `title` と、元のファイル名の語幹 `stem`。拡張子は出力の形式で決める）。
    SaveAs {
        graph: Graph,
        title: String,
        stem: String,
    },
    /// 版 `to`（0 が元。頭の版より前）に戻す：版 `to` の中身で新しい版を足す（docs/editing.md §14「版に戻す」）。
    Revert { to: i64 },
}

/// 保存の頼み（[`start_save`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct SaveRequest {
    pub item_id: i64,
    pub base: Base,
    pub action: SaveAction,
}

/// ジョブの入力（小さく：文書は先に DB に置き、根のダイジェストだけを書く）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobInput {
    v: u32,
    item_id: i64,
    base: BaseInput,
    action: ActionInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BaseInput {
    hash: Digest,
    version: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
enum ActionInput {
    Overwrite {
        doc: Digest,
    },
    SaveAs {
        doc: Digest,
        title: String,
        stem: String,
    },
    Revert {
        to: i64,
    },
}

/// 保存できる文書の形か：入力が元 1 つ（Primary）、出力が 1 つのファイル（`Expand::One`）。
pub fn check_doc(g: &Graph) -> Result<()> {
    if !matches!(g.inputs.as_slice(), [slot] if slot.role == DocRole::Primary) {
        return Err(CoreError::Invalid(
            "保存できる文書は、入力が元 1 つ（Primary）のものだけです".into(),
        ));
    }
    if !matches!(g.outputs.as_slice(), [out] if out.expand == Expand::One) {
        return Err(CoreError::Invalid(
            "保存できる文書は、出力が 1 つのファイルのものだけです".into(),
        ));
    }
    Ok(())
}

/// 保存のジョブを積む。文書を内容アドレスの部品に分けて先に DB に置き（`store::put_doc`）、ジョブの入力には根のダイジェスト
/// だけを書く。アイテムがなければ NotFound。版に戻す（`Revert`）は、戻す先が 0 以上・頭の版より前でなければ Invalid。同じ
/// アイテムの保存のジョブ（上書き・別名・版に戻す）が終わっていなければ（待ち・実行中・一時停止）Busy（土台がずれて Conflict
/// になるため。エディタを閉じて開き直しても効くよう、部品を置くのと同じトランザクションで DB を見る。[`pending_save`]）。
pub fn start_save(db: &Db, jobs: &JobManager, req: &SaveRequest) -> Result<JobId> {
    let item_id = req.item_id;
    let names = match &req.action {
        SaveAction::SaveAs { title, stem, .. } => {
            let (title, stem) = (nfc(title.trim()), nfc(stem.trim()));
            if title.is_empty() || stem.is_empty() || stem.contains(['/', '\0']) {
                return Err(CoreError::Invalid(
                    "別名で保存の名前が正しくありません（空・「/」を含むもの）".into(),
                ));
            }
            Some((title, stem))
        }
        SaveAction::Overwrite { .. } | SaveAction::Revert { .. } => None,
    };
    if let SaveAction::Overwrite { graph } | SaveAction::SaveAs { graph, .. } = &req.action {
        check_doc(graph)?;
    }
    let head = db
        .read(|c| versions::head(c, item_id))?
        .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
    let (action, parts) = match (&req.action, names) {
        (SaveAction::Overwrite { graph }, _) => {
            let parts = split_graph(graph)?;
            (ActionInput::Overwrite { doc: parts.root }, Some(parts))
        }
        (SaveAction::SaveAs { graph, .. }, Some((title, stem))) => {
            let parts = split_graph(graph)?;
            (
                ActionInput::SaveAs {
                    doc: parts.root,
                    title,
                    stem,
                },
                Some(parts),
            )
        }
        (SaveAction::Revert { to }, _) => {
            revert::check_target(*to, head.version)?;
            (ActionInput::Revert { to: *to }, None)
        }
        (SaveAction::SaveAs { .. }, None) => {
            return Err(CoreError::Invalid("別名で保存の名前がありません".into()));
        }
    };
    let input = JobInput {
        v: INPUT_V,
        item_id,
        base: BaseInput {
            hash: Digest(req.base.hash),
            version: req.base.version,
        },
        action,
    };
    // 文書の部品を置くのと同じトランザクションで積む（間に GC の部品の片付けが入っても、積んだジョブの根として残る）。同じ
    // アイテムの保存が終わっていなければ、何も置かずに断る
    jobs.enqueue_with(JobKind::Edit, serde_json::to_string(&input)?, move |tx| {
        if let Some(job) = pending_save(tx, item_id)? {
            return Err(CoreError::Busy(format!(
                "アイテム {item_id} の前の保存（ジョブ {job}）が終わるまで待ってください"
            )));
        }
        if let Some(parts) = parts {
            store::put_doc(tx, &parts, now_ms())?;
        }
        Ok(())
    })
}

/// アイテム `item_id` を待っている終わっていない（待ち・実行中・一時停止の）保存のジョブか、一括のジョブ（M14 段 4。その
/// アイテムの行がまだ pending）。あればその 1 つ。保存のジョブは状態の索引 `idx_jobs_queue` で引いて入力を読んでアイテムを比べ
/// （読めない入力は飛ばす。そのジョブは動くときに失敗する）、一括は `idx_batch_items_item` で引く。エディタを開くとき
/// （`SavePending`）と保存を積むとき（Busy）に見る（一括で待っているアイテムを、一括の土台がずれるまで開かない）。
pub fn pending_save(conn: &Connection, item_id: i64) -> Result<Option<JobId>> {
    if let Some(job) = pending_edit_job(conn, item_id)? {
        return Ok(Some(job));
    }
    super::batch::pending_batch(conn, item_id)
}

/// アイテム `item_id` の終わっていない保存のジョブ（上書き・別名・版に戻す。一括は数えない：一括の 1 件が自分を数えないように）。
pub(crate) fn pending_edit_job(conn: &Connection, item_id: i64) -> Result<Option<JobId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit'",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, input) = row?;
        if serde_json::from_str::<JobInput>(&input).is_ok_and(|i| i.item_id == item_id) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// 終わっていない保存のジョブの向き（[`PendingSave`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingSaveMode {
    Overwrite,
    /// 別名で保存（新しいアイテムの表示名 `title`）。
    SaveAs {
        title: String,
    },
    Revert,
}

/// 終わっていない（待ち・実行中・一時停止の）保存のジョブ 1 件（画面が進捗・一時停止・再開・中止を出す。ADR-189）。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingSave {
    pub job: JobId,
    pub item_id: i64,
    /// 保存しているアイテムの表示名（別名で保存では元にしたアイテム。行がなければ None）。
    pub item_title: Option<String>,
    pub mode: PendingSaveMode,
    pub state: JobState,
    pub progress: Option<f64>,
    pub message: Option<String>,
}

/// 終わっていない保存のジョブを古い順に（アプリを開き直した・ライブラリを替えたあとも、一時停止した保存を画面から再開・中止
/// できるように）。読めない入力は飛ばす（そのジョブは動くときに失敗する）。
pub fn pending_saves(conn: &Connection) -> Result<Vec<PendingSave>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, state, progress, message, input_json FROM jobs
          WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit' ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, JobId>(0)?,
            r.get::<_, JobState>(1)?,
            r.get::<_, Option<f64>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut title = conn.prepare_cached("SELECT title FROM items WHERE id = ?1")?;
    let mut out = Vec::new();
    for row in rows {
        let (job, state, progress, message, input) = row?;
        let Ok(input) = serde_json::from_str::<JobInput>(&input) else {
            continue;
        };
        let mode = match input.action {
            ActionInput::Overwrite { .. } => PendingSaveMode::Overwrite,
            ActionInput::SaveAs { title, .. } => PendingSaveMode::SaveAs { title },
            ActionInput::Revert { .. } => PendingSaveMode::Revert,
        };
        out.push(PendingSave {
            job,
            item_id: input.item_id,
            item_title: title.query_row([input.item_id], |r| r.get(0)).optional()?,
            mode,
            state,
            progress,
            message,
        });
    }
    Ok(out)
}

/// 文書を内容アドレスの部品に分ける。
fn split_graph(graph: &Graph) -> Result<kura_engine::doc_parts::DocParts> {
    split(graph).map_err(|e| from_engine("文書を部品に分けられません", e))
}

/// 終わっていない（待ち・実行中・一時停止の）保存のジョブの入力が指す文書の根と、一括のジョブ（M14）の入力が指すレシピの文書の根
/// （版の不変条件で、部品があることを確かめる）。
/// 版に戻すジョブは文書を持たない。読めない入力は飛ばす（そのジョブは動くときに失敗する）。
pub fn live_roots(conn: &Connection) -> Result<Vec<Digest>> {
    let mut stmt = conn.prepare_cached(
        "SELECT input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind IN ('edit', 'batch')",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    roots_of(rows)
}

/// GC の部品の片付けで残す保存のジョブと一括のジョブ（M14。レシピの文書の写し）の根（docs/data-model.md §4「保存と版（M12）」）：
/// 終わっていないジョブと、`since` 以後に
/// 失敗・中止したジョブ（利用者がやり直せるように。前回 OS のデコーダで読んでいる途中で落ちた保存は、失敗にしてやり直しで
/// 読み直す。docs/editing.md §14 の段 0）。どちらも `idx_jobs_queue`（状態）で引く。
pub fn gc_roots(conn: &Connection, since: i64) -> Result<Vec<Digest>> {
    let mut stmt = conn.prepare_cached(
        "SELECT input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind IN ('edit', 'batch')
         UNION ALL
         SELECT input_json FROM jobs
          WHERE state IN ('failed', 'canceled') AND kind IN ('edit', 'batch') AND finished_at >= ?1",
    )?;
    let rows = stmt.query_map([since], |r| r.get::<_, String>(0))?;
    roots_of(rows)
}

/// ジョブの入力の列から、文書の根のダイジェスト（重なりなし）。
fn roots_of(rows: impl Iterator<Item = rusqlite::Result<String>>) -> Result<Vec<Digest>> {
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        match serde_json::from_str::<JobInput>(&row).map(|i| i.action) {
            Ok(ActionInput::Overwrite { doc } | ActionInput::SaveAs { doc, .. }) => out.push(doc),
            Ok(ActionInput::Revert { .. }) => {}
            // 一括のジョブ（M14 段 4）の入力の写しが指すレシピの文書
            Err(_) => out.extend(super::batch::roots_of_input(&row)),
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// ---- 純粋な関数 ----

/// 保存の向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Overwrite,
    SaveAs,
    /// 版に戻す（持ち主の決め方は上書きと同じ：アイテムの今を差し替える）。
    Revert,
}

/// 出力の中身 H の持ち主から決めること。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 保存する（上書きで、持ち主がいないか、ほかのアイテム・自分の元・過去の版と同じ中身。台帳の行を共有する）。
    Proceed,
    /// 上書きの出力が自分の今と同じ：版だけ足してファイルは変えない。
    Unchanged,
    /// 上書きの出力が別のアイテム（ゴミ箱のものを含む）の今と同じ：保存しない。
    Conflict(String),
    /// 別名で保存の出力が、どれかのアイテム（元のアイテム自身を含む）の今・元・過去の版と同じ：新しいアイテムを作らず、
    /// そのアイテムを示す（M12 の決定 3・12）。
    Existing {
        item_id: i64,
        role: OwnerRole,
        trashed: bool,
    },
}

/// H の持ち主 `owner`（`import::store::find_owner`。今 → 元 → 過去の版の順）と、保存するアイテム `me`・向きから決める
/// （版に戻すのは上書きと同じ）。
pub fn decide(owner: Option<&Owner>, me: i64, mode: Mode) -> Decision {
    let Some(o) = owner else {
        return Decision::Proceed;
    };
    match mode {
        Mode::Overwrite | Mode::Revert => match o.role {
            OwnerRole::Current if o.id == me => Decision::Unchanged,
            OwnerRole::Current => Decision::Conflict(format!(
                "保存した結果が、別のアイテム（{}{}）の今と同じ中身になります",
                o.id,
                if o.trashed { "・ゴミ箱" } else { "" }
            )),
            OwnerRole::Original | OwnerRole::Past { .. } => Decision::Proceed,
        },
        Mode::SaveAs => Decision::Existing {
            item_id: o.id,
            role: o.role,
            trashed: o.trashed,
        },
    }
}

/// 出力のファイルの拡張子：台帳に同じ中身の行があればその拡張子（items のトリガが食い違いを断るため。`files/` の 1 ファイル
/// に 1 行）、形式が今と同じなら今の拡張子（`apng` を `png` にしない）、ほかは形式の代表の拡張子。知らない形式は None。
pub fn output_ext(format_id: &str, current_ext: &str, ledger_ext: Option<&str>) -> Option<String> {
    if let Some(ext) = ledger_ext {
        return Some(ext.to_string());
    }
    if format_by_ext(current_ext).is_some_and(|f| f.id == format_id) {
        return Some(current_ext.to_string());
    }
    format_by_id(format_id)
        .and_then(|f| f.exts.first())
        .map(|e| (*e).to_string())
}

/// 出力のメディア情報（items の列）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Media {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// 値の形からメディア情報を決める：画像は幅・高さ、フレームの列はキャンバスの幅・高さ、音声は長さ
/// （round(フレーム × 1000 / 標本化周波数)。あふれないよう u128 で）。
pub fn media_of(shape: &Shape) -> Media {
    let px = |d: Dim| i64::try_from(d.max()).ok();
    match *shape {
        Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => Media {
            width: px(w),
            height: px(h),
            duration_ms: None,
        },
        Shape::Audio { rate, frames, .. } => {
            let duration_ms = (rate > 0)
                .then(|| {
                    let rate = u128::from(rate);
                    (u128::from(frames.max()) * 1000 + rate / 2) / rate
                })
                .and_then(|ms| i64::try_from(ms).ok());
            Media {
                width: None,
                height: None,
                duration_ms,
            }
        }
    }
}

/// 土台が今と違う理由（同じなら None）：ゴミ箱に入れられた・今か頭の版が変わった（M12 の決定：編集の途中でゴミ箱に入れられた
/// アイテムの保存は Conflict）。
fn base_conflict(head: &Head, base: &Base) -> Option<String> {
    if head.trashed {
        return Some("編集している間に、アイテムがゴミ箱に入れられました".into());
    }
    if head.hash != base.hash || head.version != base.version {
        return Some(format!(
            "編集を始めたあとに、別の保存でアイテムが変わりました（版 {} → {}）",
            base.version, head.version
        ));
    }
    None
}

// ---- ジョブ ----

/// S9 の試験の、強制終了を写す段（その段の直後に、後始末を通らずに失敗する）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // 段の「あと」を名前で言う（設計の S9 の 4 段の名前のまま）
pub(crate) enum Fault {
    /// 描いて書いたあと（staging に出力がある）。
    AfterRender,
    /// 予告のあと（台帳の予告がコミットされ、まだ置いていない）。
    AfterAnnounce,
    /// 置いたあと（`files/` に台帳にないファイルがある）。
    AfterPlace,
    /// コミットのあと（版があり、後始末の前）。
    AfterCommit,
}

/// テストが段の途中で割り込む所（中止・門の間にゴミ箱に入れる試験）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// 書いて staging に置いたあと、止める指示を見る前。
    Written,
    /// 置いたあと、トランザクション 2 の前（台帳の門を持っている）。
    Placed,
    /// 音声を流して書く回で窓を 1 つ書き手に渡したあと（`fed` はそれまでに渡したフレームの数（回をまたいで足す）、`frames`
    /// は 1 回の長さ。Ogg Vorbis は `fed < frames` が後ろ向きの回）。保存・版に戻す・GC の照合のどれでも呼ぶ（M13 段 3 の
    /// 検証の直し）。
    Window { fed: u64, frames: u64 },
    /// 流して書いたものの R2 の読み直しで、1 区切りを読んだあと。
    Rehash,
    /// 流して書く保存で、計画の確保の直前（`resident` は見積もりの入力の源の LRU（`Source::Lazy`）、`promote` はアンカーに
    /// 昇格するか）。M13 段 4C の検証の直し。
    Admit { resident: u64, promote: bool },
}

/// テストの割り込み（段ごとに呼ばれる。止めて待つ・DB を書き換えるなど）。
#[cfg(test)]
pub(crate) type Hook = Arc<dyn Fn(Stage) + Send + Sync>;

/// 保存の手順を 1 件ずつ動かすもの（M14 段 2。ジョブの文脈によらない：[`UnitCx`]）。準備（段 0〜5：[`Saver::prepare`]・
/// [`Saver::prepare_rebuild`]）は 1 件ごとに別のスレッドで並べてよい。コミット（段 6〜8：[`Saver::commit`]・
/// [`Saver::revert_reuse`]）は台帳の門を持つので、呼び手が 1 件ずつ順に呼ぶ。エンジンの部品（実行の場・コーデックの組）は、
/// このライブラリのもの。
pub(crate) struct Saver {
    engine: Arc<EngineServices>,
    #[cfg(test)]
    pub(crate) fault: Option<Fault>,
    #[cfg(test)]
    pub(crate) hook: Option<Hook>,
}

impl Saver {
    /// このライブラリのエンジンの部品で保存する。
    pub(crate) fn new(engine: Arc<EngineServices>) -> Self {
        Self {
            engine,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            hook: None,
        }
    }

    /// 段 `fault` の直後で、後始末を通らずに失敗させる（テストの強制終了）。
    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: Fault) -> Self {
        self.fault = Some(fault);
        self
    }

    #[cfg(test)]
    fn crash_at(&self, at: Fault) -> bool {
        self.fault == Some(at)
    }

    /// テストの割り込み。
    #[cfg(test)]
    fn at(&self, stage: Stage) {
        if let Some(hook) = &self.hook {
            hook(stage);
        }
    }
}

/// 保存のジョブの処理（`JobKind::Edit`）：入力を読み、再開を確かめ、[`Saver`] で 1 件を準備してコミットし、後のジョブを積む。
pub struct EditHandler {
    saver: Saver,
    /// GC の時計（あれば、コミットのあとに候補があれば GC を積む。`LibrarySession` が GC と同じものを渡す）。
    gc: Option<GcClock>,
}

impl EditHandler {
    /// このライブラリのエンジンの部品で保存する。
    pub fn new(engine: Arc<EngineServices>) -> Self {
        Self {
            saver: Saver::new(engine),
            gc: None,
        }
    }

    /// コミットのあとに、GC の候補（`clock` で数える）があれば GC のジョブを積む。
    pub fn with_gc(mut self, clock: GcClock) -> Self {
        self.gc = Some(clock);
        self
    }

    /// 段 `fault` の直後で、後始末を通らずに失敗させる（テストの強制終了）。
    #[cfg(test)]
    pub(crate) fn with_fault(mut self, fault: Fault) -> Self {
        self.saver.fault = Some(fault);
        self
    }

    /// 段ごとに `hook` を呼ぶ（テストの割り込み）。
    #[cfg(test)]
    pub(crate) fn with_hook(mut self, hook: Hook) -> Self {
        self.saver.hook = Some(hook);
        self
    }
}

#[cfg(test)]
fn crashed(at: Fault) -> CoreError {
    CoreError::Invalid(format!("テスト用の強制終了（{at:?}）"))
}

/// 段の結果：続けるか、止めるか（一時停止・中止）。
pub(crate) enum Flow<T> {
    Go(T),
    Stop(JobEnd),
}

/// 止める指示があれば、その止め方。
fn stop_end(ctx: &dyn UnitCx) -> Option<JobEnd> {
    match ctx.control() {
        Control::Continue => None,
        Control::Pause => Some(JobEnd::Paused),
        Control::Cancel => Some(JobEnd::Canceled { result_json: None }),
    }
}

/// エンジンの結果：中止（止める指示があるとき）は止め方に、ほかの誤りは CoreError に。
fn engine_flow<T>(
    ctx: &dyn UnitCx,
    what: &str,
    r: std::result::Result<T, EngineError>,
) -> Result<Flow<T>> {
    match r {
        Ok(v) => Ok(Flow::Go(v)),
        Err(e) if matches!(e.root(), EngineError::Canceled) => match stop_end(ctx) {
            Some(end) => Ok(Flow::Stop(end)),
            None => Err(from_engine(what, e)),
        },
        Err(e) => Err(from_engine(what, e)),
    }
}

/// [`engine_flow`] の結果を取り出す（止めるなら呼び手から `Ok(Flow::Stop(_))` で返す）。
macro_rules! go {
    ($ctx:expr, $what:expr, $r:expr) => {
        match engine_flow($ctx, $what, $r)? {
            Flow::Go(v) => v,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        }
    };
}

// 段 1・2 の読みと、版に戻す・GC の照合と、音声を流して書く道（`go!` を使うので、マクロの定義のあとに置く）
mod read;
mod revert;
mod stream;
mod unit;

pub use revert::Verdict;
pub(crate) use revert::{Reverted, verify_version};
pub(crate) use unit::{Committed, JobUnit, Record, UnitCx, UnitMonitor};

/// このジョブの staging のファイル（落とすと消す。置いたものは改名で消えている）。
#[derive(Default)]
struct Staging(Vec<PathBuf>);

impl Staging {
    fn add(&mut self, path: PathBuf) {
        self.0.push(path);
    }

    /// 消さずに残す（テストの強制終了。開くときの staging の掃除が消す）。
    #[cfg(test)]
    fn keep(mut self) {
        self.0.clear();
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        for path in &self.0 {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    tracing::warn!(%err, path = %path.display(), "保存の書きかけを消せませんでした（次に開くときに消します）");
                }
            }
        }
    }
}

/// 今回作ったアンカー（staging に書いて照らした）。
#[derive(Debug, Clone)]
struct NewAnchorFile {
    part: PathBuf,
    hash: [u8; 32],
    size: u64,
    /// 入れ物の拡張子（`png`・`wav`。台帳に同じ中身があればその拡張子で置く）。
    ext: &'static str,
}

/// 元の読み方。
#[derive(Debug, Clone)]
enum AnchorUse {
    /// アンカーなし（再現できるデコーダで読んだ・アンカーの行がない）。
    None,
    /// あったアンカーから読んだ。
    Existing(Anchor),
    /// あったアンカーが使えず（壊れている・ファイルがない・別の値）、元を記録の識別子のデコーダで読み直したら記録と同じ値
    /// だった。入力の記録は同じ値のアンカーを指したまま（docs/editing.md §9.4）。
    Fallback(Anchor),
    /// 今回作った。
    New(NewAnchorFile),
}

impl AnchorUse {
    /// 入力の記録が指すアンカーのファイルの中身。
    fn file_hash(&self) -> Option<[u8; 32]> {
        match self {
            Self::None => None,
            Self::Existing(a) | Self::Fallback(a) => Some(a.hash),
            Self::New(a) => Some(a.hash),
        }
    }

    /// 結果の `anchor`：アンカーから読んだ（used）・作った（created）・元から読んだ（null。使えずに読み直したときも）。
    fn label(&self) -> Option<&'static str> {
        match self {
            Self::None | Self::Fallback(_) => None,
            Self::Existing(_) => Some("used"),
            Self::New(_) => Some("created"),
        }
    }

    /// 前からあったアンカー（予告のときに、まだ同じ台帳の行を指して残っているかを確かめ直す）。
    fn existing(&self) -> Option<&Anchor> {
        match self {
            Self::Existing(a) | Self::Fallback(a) => Some(a),
            Self::None | Self::New(_) => None,
        }
    }
}

/// 何を書いたか（段 8 の分かれ目）。
#[derive(Debug, Clone)]
enum Act {
    /// 文書を描いた版（上書き・別名で保存）。`names` は別名で保存の (表示名, 語幹)。`parts` は DB にまだない文書の部品
    /// （一括で継ぎ足した文書。トランザクション 2 で置く。保存のジョブは積む前に置いたので None）。
    Edit {
        doc: Digest,
        names: Option<(String, String)>,
        parts: Option<Box<kura_engine::doc_parts::DocParts>>,
    },
    /// 版 k に戻す版（版 k の文書から作り直した）。
    Revert(Box<revert::Target>),
}

/// 描いて書き終えたもの（段 5 まで）。
#[derive(Debug, Clone)]
struct Prepared {
    job: i64,
    /// 版に書く持ち主（`UnitCx::version_job`。一括は None）。
    version_job: Option<i64>,
    mode: Mode,
    item_id: i64,
    base: Base,
    act: Act,
    kind: MediaKind,
    /// 拡張子の手がかり：出力の形式がこの拡張子の形式と同じなら、この拡張子のまま（`apng` を `png` にしない。上書き・別名は
    /// 今の拡張子、版に戻すのは版 k の拡張子）。
    ext_hint: String,
    original: OriginalFile,
    source_format: String,
    opts: DecodeOptions,
    /// 元を読んだ記録（アンカーから読んだときはアンカーの元の読み取りの記録）。
    decoded: DecodeStamp,
    anchor: AnchorUse,
    out_part: PathBuf,
    out_hash: [u8; 32],
    out_size: u64,
    out_format: String,
    value_digest: Digest,
    media: Media,
    encoded: EncodedStamp,
    warnings: Vec<String>,
    /// 版の記録に足す一括の出どころ（`VersionRecord::batch`。保存のジョブは None）。
    batch: Option<serde_json::Value>,
}

/// 段 0 で決めたもの（流して書く道 `prepare_stream` に渡す）。
struct Decided<'a> {
    mode: Mode,
    item_id: i64,
    base: Base,
    doc: Digest,
    names: Option<(String, String)>,
    parts: Option<Box<kura_engine::doc_parts::DocParts>>,
    batch: Option<serde_json::Value>,
    graph: &'a Graph,
    head: Head,
    original: OriginalFile,
    source_format: &'static str,
    opts: DecodeOptions,
    found: Option<Anchor>,
    /// 読み方の設定のダイジェスト（エディタが貸す入力を引く鍵）。
    options_digest: [u8; 32],
    out_format: String,
    enc_opts: serde_json::Value,
    /// 出力の拡張子の候補の最初（staging の名前）。
    ext0: &'static str,
    limits: Limits,
    may_make_anchor: bool,
    runtime: Arc<crate::edit::EngineRuntime>,
    codecs: Arc<kura_engine::codec::CodecSet>,
}

/// 保存する形式の問題（[`save_format`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatProblem {
    /// この環境では書けない（MP3 で ffmpeg がない、など）。`fallback` は代わりの既定（音声は Ogg Vorbis、書けなければ WAV）。
    Unavailable {
        format: String,
        fallback: Option<String>,
    },
}

/// 文書の出力の形式の選び `choice`（`Keep` は元の形式 `source_format`）で、出力の形 `out_shape` を書く形式と設定（M13 段 3。
/// docs/editing.md §14）。書き手がなければ [`FormatProblem::Unavailable`]（代わりは音声なら Ogg Vorbis（書けるとき）、
/// なければ WAV、画像なら PNG）。エディタの保存が積む前に確かめる（積んだあとに書き手が消えたら、ジョブが `Unsupported`）。
pub fn save_format(
    codecs: &kura_engine::codec::CodecSet,
    choice: &FormatChoice,
    source_format: &str,
    out_shape: &Shape,
) -> std::result::Result<(String, serde_json::Value), FormatProblem> {
    let (format, opts) = match choice {
        FormatChoice::To { format, options } => (
            format.clone(),
            if options.is_null() {
                json!({})
            } else {
                options.clone()
            },
        ),
        FormatChoice::Keep => (source_format.to_string(), json!({})),
    };
    if codecs.encoder(&format, out_shape).is_some() {
        return Ok((format, opts));
    }
    let fallback = match out_shape {
        Shape::Audio { .. } => ["ogg", "wav"]
            .into_iter()
            .find(|f| codecs.encoder(f, out_shape).is_some()),
        _ => ["png"]
            .into_iter()
            .find(|f| codecs.encoder(f, out_shape).is_some()),
    }
    .map(str::to_string);
    Err(FormatProblem::Unavailable { format, fallback })
}

/// 書ける形式 1 つ（[`writable_formats`]）：形式・既定の書き手・段・バイト列の再現を約束するか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WritableFormat {
    pub format: String,
    pub codec: String,
    pub tier: Tier,
    pub reproducible: bool,
}

/// 種類 `kind` のアイテムの形 `shape` の値を書ける形式の一覧（形式の表の順。M13 段 3。段 6 の形式を選ぶシートが使う）：
/// `CodecSet::capabilities` のうち、その種類の形式で、段の順で最初にこの形を書ける書き手があるもの。
pub fn writable_formats(
    codecs: &kura_engine::codec::CodecSet,
    kind: MediaKind,
    shape: &Shape,
) -> Vec<WritableFormat> {
    codecs
        .capabilities()
        .into_iter()
        .filter(|c| MediaKind::from(c.item_kind) == kind)
        .filter_map(|c| {
            let e = codecs.encoder(c.format, shape)?;
            Some(WritableFormat {
                format: c.format.to_string(),
                codec: e.id().to_string(),
                tier: e.tier(),
                reproducible: e.reproducible(),
            })
        })
        .collect()
}

/// 予告のトランザクションで決めたこと。
#[derive(Debug, Clone)]
struct Go {
    ext: String,
    /// H が台帳にあった（ファイルを共有する）。
    reused: bool,
    /// 上書きの出力が今と同じ（版だけ足す）。
    unchanged: bool,
    /// 今回作ったアンカーを置く拡張子。
    anchor_ext: Option<String>,
    /// この 1 件が書いた予告の行の ID（トランザクション 2 はこれだけを消す。同じ持ち主の前の 1 件が消せずに残した予告を
    /// 巻き込まない。ADR-192 (5)）。
    announced: Vec<i64>,
}

/// 予告のトランザクションの結果。
enum Tx1 {
    Conflict(String),
    Existing {
        item_id: i64,
        role: OwnerRole,
        trashed: bool,
    },
    Go(Go),
}

const DECODING: &str = r#"{"v":1,"decoding":true}"#;
const DECODED: &str = r#"{"v":1,"decoding":false}"#;
const CRASHED: &str = r#"{"v":1,"crashed":true}"#;

fn partial_json(partial: Option<&str>) -> Option<serde_json::Value> {
    partial.and_then(|p| serde_json::from_str(p).ok())
}

impl JobHandler for EditHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let result = self.run_job(ctx);
        // 版を足さずに終わった上書き・別名（失敗・中止・同じ中身のアイテムがある）：積む前に置いた文書の部品が孤立しうるので、
        // GC の部品の印を置く（一時停止・譲るはジョブが続くので置かない）
        let ended = !matches!(result, Ok(JobEnd::Paused | JobEnd::Yield));
        let with_doc = serde_json::from_str::<JobInput>(ctx.input())
            .is_ok_and(|i| !matches!(i.action, ActionInput::Revert { .. }));
        if ended && with_doc {
            let job = ctx.id();
            let db = ctx.library().db();
            let marked = db.read(|c| versions::by_job(c, job)).and_then(|v| match v {
                Some(_) => Ok(()),
                None => db.write(|tx| gc::mark_parts(tx)),
            });
            if let Err(err) = marked {
                tracing::warn!(%err, job, "文書の部品の印を置けませんでした");
            }
        }
        result
    }

    /// 待ち・一時停止のまま中止された上書き・別名（`run` を通らないので上の印が置かれない）：積む前に置いた文書の部品が孤立
    /// しうるので、中止と同じトランザクションで GC の部品の印を置く（版に戻すは文書を持たない）。一時停止で残した作業用の PCM
    /// は、入力から鍵を求めて捨てる（GC のジョブ・閉じるのを待たずに消す。M13 段 3 の検証の直し）。
    fn canceled_while_idle(&self, tx: &Connection, _job: JobId, input_json: &str) -> Result<()> {
        let input = serde_json::from_str::<JobInput>(input_json).ok();
        let with_doc = input
            .as_ref()
            .is_some_and(|i| !matches!(i.action, ActionInput::Revert { .. }));
        if with_doc {
            gc::mark_parts(tx)?;
        }
        if let Some(input) = &input {
            match paused_work_keys(tx, input) {
                Ok(keys) => {
                    for key in &keys {
                        self.saver.engine.work_pcm().discard(key);
                    }
                }
                // 鍵を求められなくても中止は止めない（残したものは期限・上限・閉じるときに消える）
                Err(err) => tracing::warn!(%err, "中止した保存の作業用の PCM の鍵を求められません"),
            }
        }
        Ok(())
    }
}

/// 一時停止した保存が残しうる作業用の PCM の鍵（中止の後始末で捨てる。M13 段 3 の検証の直し）：上書き・別名は元・形式・文書の
/// 入力の読み方で、段の順（decoder None。エディタ・試聴と同じ鍵）と、アンカーがあればその記録のデコーダのもの（アンカーが
/// 使えずに読み直したとき）。版に戻すは版の入力の記録（形式・読み方・記録のデコーダ）のもの。
fn paused_work_keys(conn: &Connection, input: &JobInput) -> Result<Vec<WorkKey>> {
    let Some(original) = versions::original(conn, input.item_id)? else {
        return Ok(Vec::new());
    };
    match &input.action {
        ActionInput::Overwrite { doc } | ActionInput::SaveAs { doc, .. } => {
            let Some(format) = format_id(&original.ext) else {
                return Ok(Vec::new());
            };
            let Some(doc_id) = store::doc_id_by_digest(conn, doc)? else {
                return Ok(Vec::new());
            };
            let bytes = store::load_doc_bytes(conn, doc_id)?;
            let graph =
                kura_engine::doc::load(&bytes).map_err(|e| from_engine("文書を読めません", e))?;
            let options = graph
                .inputs
                .first()
                .map(|slot| slot.decode.clone())
                .unwrap_or_default();
            let any = WorkKey {
                source: original.hash,
                format: format.to_string(),
                options,
                decoder: None,
            };
            let found = anchors::find(
                conn,
                &original.hash,
                &anchors::options_digest(&any.options)?,
            )?;
            let mut keys = vec![any.clone()];
            if let Some(a) = found {
                keys.push(WorkKey {
                    decoder: Some(a.decoded.codec),
                    ..any
                });
            }
            Ok(keys)
        }
        ActionInput::Revert { to } => {
            let Some(v) = versions::get(conn, input.item_id, *to)? else {
                return Ok(Vec::new());
            };
            Ok(versions::inputs(conn, v.id)?
                .iter()
                .filter_map(|i| serde_json::from_str::<RecordedInput>(&i.recorded).ok())
                .map(|rec| WorkKey {
                    source: rec.source.0,
                    format: rec.format,
                    options: rec.options,
                    decoder: Some(rec.decoded.codec),
                })
                .collect())
        }
    }
}

impl EditHandler {
    /// ジョブの本体（[`JobHandler::run`] が、版を足さずに終わったときの部品の印を置く）。
    fn run_job(&self, ctx: &JobContext) -> Result<JobEnd> {
        let input: JobInput = serde_json::from_str(ctx.input())?;
        if input.v != INPUT_V {
            return Err(CoreError::Invalid(format!(
                "保存のジョブの入力の版 {} を知りません",
                input.v
            )));
        }
        let db = ctx.library().db();
        let job = ctx.id();
        // 0. 再開：このジョブの版があれば、前回コミットした
        if let Some(v) = db.read(|c| versions::by_job(c, job))? {
            let result = partial_json(ctx.partial())
                .and_then(|p| p.get("committed").cloned())
                .unwrap_or_else(|| {
                    json!({
                        "outcome": "saved", "itemId": v.item_id, "version": v.version, "versionId": v.id,
                        "hash": hex(&v.output_hash), "ext": v.ext, "resumed": true,
                    })
                });
            ctx.log("前回コミットした版があるので、そのまま終えます");
            // コミットのあと後始末の前に落ちていれば、保存のあとのジョブを積んでいない
            if let Some(h) = db.read(|c| versions::head(c, v.item_id))? {
                self.queue_follow_ups(ctx, h.kind);
            }
            return Ok(JobEnd::Done {
                result_json: result.to_string(),
                message: Some("保存しました".into()),
            });
        }
        if partial_json(ctx.partial()).is_some_and(|p| p.get("decoding") == Some(&json!(true))) {
            // 前回、OS のデコーダで元を読んでいる途中で落ちた：同じファイルで落ち続けないよう失敗にする（印は消すので、
            // 利用者がやり直せば読み直す）
            db.write(move |tx| {
                save_partial(
                    tx,
                    job,
                    CRASHED,
                    0.0,
                    "前回、元を読んでいる途中でアプリが終わりました",
                )
            })?;
            return Err(CoreError::Tool(
                "前回、OS のデコーダで元を読んでいる途中でアプリが終わったので、保存をやめました（もう一度保存すると読み直します）"
                    .into(),
            ));
        }
        let base = Base {
            hash: input.base.hash.0,
            version: input.base.version,
        };
        let unit = JobUnit(ctx);
        // 結果は途中経過の `committed` に、版と同じトランザクションで書く（再開の 0 が版と一緒に読む）
        let record: Record = Arc::new(move |tx, outcome| {
            save_partial(
                tx,
                job,
                &json!({ "v": 1, "committed": outcome }).to_string(),
                1.0,
                "保存しました",
            )
        });
        let (mode, doc, names) = match input.action.clone() {
            ActionInput::Overwrite { doc } => (Mode::Overwrite, doc, None),
            ActionInput::SaveAs { doc, title, stem } => (Mode::SaveAs, doc, Some((title, stem))),
            ActionInput::Revert { to } => {
                let plan = match self
                    .saver
                    .revert_reuse(&unit, input.item_id, base, to, &record)?
                {
                    Reverted::Committed(done) => return Ok(self.done(ctx, done)),
                    Reverted::Rebuild(plan) => plan,
                };
                let written = match self.saver.prepare_rebuild(&unit, *plan)? {
                    Flow::Go(w) => w,
                    Flow::Stop(end) => return Ok(end),
                };
                let done = self.saver.commit(&unit, written, record)?;
                return Ok(self.done(ctx, done));
            }
        };
        let written = match self
            .saver
            .prepare(&unit, mode, input.item_id, base, doc, names)?
        {
            Flow::Go(w) => w,
            Flow::Stop(end) => return Ok(end),
        };
        let done = self.saver.commit(&unit, written, record)?;
        Ok(self.done(ctx, done))
    }

    /// コミットのあと：版を足したら後のジョブを積み、ジョブの終わりにする。
    fn done(&self, ctx: &JobContext, c: Committed) -> JobEnd {
        if let Some(kind) = c.saved {
            self.queue_follow_ups(ctx, kind);
        }
        JobEnd::Done {
            result_json: c.outcome.to_string(),
            message: Some(c.message),
        }
    }
}

/// [`Saver::prepare_graph`] に渡す、文書のほかのもの。
#[derive(Debug, Clone, Default)]
pub(crate) struct GraphExtra {
    /// 別名で保存の (表示名, 語幹)。上書きは None。
    pub(crate) names: Option<(String, String)>,
    /// 版の記録に足す一括の出どころ（`VersionRecord::batch`）。
    pub(crate) batch: Option<serde_json::Value>,
}

/// 上書き・別名で保存の準備に渡すもの（文書の根・DB にまだない部品・別名の名前・版の記録に足す一括の出どころ）。
struct DocArgs {
    mode: Mode,
    item_id: i64,
    base: Base,
    doc: Digest,
    parts: Option<Box<kura_engine::doc_parts::DocParts>>,
    names: Option<(String, String)>,
    batch: Option<serde_json::Value>,
}

/// 描いて書き終えた 1 件（段 5 まで。[`Saver::commit`] に渡す。落とすと staging を消す）。
pub(crate) struct Written {
    p: Prepared,
    staging: Staging,
}

impl Written {
    /// staging に書いた出力（区切りの同期を取るファイル。一括）。
    pub(crate) fn part(&self) -> &Path {
        &self.p.out_part
    }

    /// 準備した土台（一括の行に書く）。
    pub(crate) fn base(&self) -> Base {
        self.p.base
    }
}

impl Saver {
    /// 書き終えたあと（段 5 の終わり）：止める指示があれば止める（呼び手が staging を落として消す）。テストの強制終了なら
    /// staging を残して失敗する。続けるなら None。
    fn after_written<T>(
        &self,
        ctx: &dyn UnitCx,
        _staging: &mut Staging,
    ) -> Option<Result<Flow<T>>> {
        #[cfg(test)]
        self.at(Stage::Written);
        if let Some(end) = stop_end(ctx) {
            return Some(Ok(Flow::Stop(end)));
        }
        #[cfg(test)]
        if self.crash_at(Fault::AfterRender) {
            std::mem::take(_staging).keep();
            return Some(Err(crashed(Fault::AfterRender)));
        }
        None
    }

    /// 上書き・別名で保存の段 0〜5：文書（`doc`。積む前に DB に置いたもの）を読み、土台を確かめ、元を読み（アンカーを作り）、
    /// 描いて staging に書く。`names` は別名で保存の (表示名, 語幹)。止める指示（一時停止・中止）は `Flow::Stop`。
    pub(crate) fn prepare(
        &self,
        ctx: &dyn UnitCx,
        mode: Mode,
        item_id: i64,
        base: Base,
        doc: Digest,
        names: Option<(String, String)>,
    ) -> Result<Flow<Written>> {
        let db = ctx.library().db();
        let doc_id = db
            .read(|c| store::doc_id_by_digest(c, &doc))?
            .ok_or_else(|| CoreError::NotFound(format!("保存する文書 {doc}")))?;
        let bytes = db.read(|c| store::load_doc_bytes(c, doc_id))?;
        let graph = self
            .engine
            .runtime()?
            .bulk(|e| e.load(&bytes))
            .map_err(|e| from_engine("文書を読めません", e))?;
        drop(bytes);
        self.prepare_doc(
            ctx,
            DocArgs {
                mode,
                item_id,
                base,
                doc,
                parts: None,
                names,
                batch: None,
            },
            &graph,
        )
    }

    /// 上書き・別名で保存の段 0〜5 を、DB にまだない文書 `graph` で（一括で継ぎ足した文書。M14 段 4）。文書の部品は
    /// トランザクション 2 で版と一緒に置く（止めた・断った 1 件の部品が DB に残らない）。
    pub(crate) fn prepare_graph(
        &self,
        ctx: &dyn UnitCx,
        mode: Mode,
        item_id: i64,
        base: Base,
        graph: &Graph,
        extra: GraphExtra,
    ) -> Result<Flow<Written>> {
        let GraphExtra { names, batch } = extra;
        let parts = split_graph(graph)?;
        let doc = parts.root;
        self.prepare_doc(
            ctx,
            DocArgs {
                mode,
                item_id,
                base,
                doc,
                parts: Some(Box::new(parts)),
                names,
                batch,
            },
            graph,
        )
    }

    /// 段 0〜5 の本体。
    fn prepare_doc(&self, ctx: &dyn UnitCx, a: DocArgs, graph: &Graph) -> Result<Flow<Written>> {
        let DocArgs {
            mode,
            item_id,
            base,
            doc,
            parts,
            names,
            batch,
        } = a;
        let library = ctx.library();
        let db = library.db();
        let layout = library.layout();
        let job = ctx.job();
        let tag = ctx.tag();
        let runtime = self.engine.runtime()?.clone();
        let codecs = self.engine.codecs();
        check_doc(graph)?;
        let head = db
            .read(|c| versions::head(c, item_id))?
            .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
        if let Some(why) = base_conflict(&head, &base) {
            return Err(CoreError::Conflict(why));
        }
        let original = db
            .read(|c| versions::original(c, item_id))?
            .ok_or_else(|| CoreError::NotFound(format!("アイテム {item_id}")))?;
        let source_format = format_id(&original.ext).ok_or_else(|| {
            CoreError::Unsupported(format!("元の形式（{}）を読めません", original.ext))
        })?;
        let opts = graph
            .inputs
            .first()
            .map(|slot| slot.decode.clone())
            .unwrap_or_default();
        let options_digest = anchors::options_digest(&opts)?;
        let found = db.read(|c| anchors::find(c, &original.hash, &options_digest))?;
        let mon = UnitMonitor(ctx);
        let limits: Limits = runtime.bulk(|e| e.limits().clone());
        let original_path = layout.content_path(&original.hash, &original.ext);
        let (out_format, enc_opts) = match graph.outputs.first().map(|o| &o.format) {
            Some(FormatChoice::To { format, options }) => (
                format.clone(),
                if options.is_null() {
                    json!({})
                } else {
                    options.clone()
                },
            ),
            Some(FormatChoice::Keep) | None => (source_format.to_string(), json!({})),
        };
        let desc = format_by_id(&out_format).ok_or_else(|| {
            CoreError::Unsupported(format!("出力の形式 {out_format} を知りません"))
        })?;
        if MediaKind::from(desc.item_kind) != head.kind {
            return Err(CoreError::Invalid(format!(
                "保存でアイテムの種類は変えられません（{} → {out_format}）",
                head.kind.as_str()
            )));
        }
        let may_make_anchor = codecs
            .decoders_for(source_format)
            .iter()
            .any(|d| !d.reproducible());
        // 音声で、出力の形式の書き手が流して書けるなら、値に組まずに流して書く（M13 段 3。save/stream.rs）
        if stream::applies(&codecs, head.kind, &out_format) {
            return self.prepare_stream(
                ctx,
                Decided {
                    mode,
                    item_id,
                    base,
                    doc,
                    names,
                    parts,
                    batch,
                    graph,
                    head,
                    original,
                    source_format,
                    opts,
                    found,
                    options_digest,
                    out_format,
                    enc_opts,
                    ext0: desc.exts.first().copied().unwrap_or("bin"),
                    limits,
                    may_make_anchor,
                    runtime: runtime.clone(),
                    codecs: codecs.clone(),
                },
            );
        }

        // 1・2. 形をヘッダから読み、計画を作って予算を確保し、読む（アンカーがあればアンカーから。使えなければ元を記録の
        // デコーダで読み直す。OS のデコーダを通りうるなら、見出しを読む前から読み終えるまで `decoding` の印を保つ。read.rs）
        ctx.progress(0.05, "元の大きさを確かめています");
        let cx = read::ReadCx {
            ctx,
            runtime: &runtime,
            codecs: &codecs,
            layout,
            original: &original_path,
            limits: &limits,
            mon: &mon,
        };
        let mut warnings: Vec<String> = Vec::new();
        let plan_for = |how: &read::Reading<'_>, shape: &Shape| -> Result<Flow<_>> {
            let shapes = BTreeMap::from([(InputId(0), *shape)]);
            let plan = runtime
                .bulk(|e| e.compile(graph, &shapes))
                .map_err(|issues| CoreError::Invalid(format!("文書を描けません：{issues}")))?;
            let out_shape = plan
                .output_shapes()
                .first()
                .map(|(_, s)| *s)
                .ok_or_else(|| CoreError::Invalid("文書に出力がありません".into()))?;
            if codecs.encoder(&out_format, &out_shape).is_none() {
                return Err(CoreError::Unsupported(format!(
                    "この結果は {out_format} に書けません（保存する形式を選び直してください。音声の既定は Ogg Vorbis）"
                )));
            }
            let may_anchor = may_make_anchor && matches!(how, read::Reading::Any { .. });
            let spec = EstimateSpec::new(0)
                .digests(true)
                .input(InputId(0), how.source(&codecs))
                .input_len(InputId(0), file_len(how.path(&original_path)))
                .output(
                    OutputId(0),
                    Sink::Encode {
                        codecs: &codecs,
                        format: &out_format,
                        opts: &enc_opts,
                    },
                );
            // アンカーを作るなら：読む持ち越し ＋ 入力の値 ＋ 書いて読み直す分（読み直した値。docs/editing.md §9.4 の「値の 2 倍」）
            let anchor_need = |est: &kura_engine::render::estimate::MemoryEstimate| {
                if may_anchor {
                    est.carried_decode
                        .saturating_add(value_bytes(shape))
                        .saturating_add(anchor::work_bytes(shape))
                } else {
                    0
                }
            };
            let (_estimate, lease) = go!(
                ctx,
                "保存のメモリを確保できません",
                runtime.admit_bulk_with(&plan, spec, anchor_need, &mon)
            );
            Ok(Flow::Go((plan, lease)))
        };
        let read = match found {
            Some(a) => {
                let rec = RecordedInput {
                    source: Digest(original.hash),
                    format: source_format.to_string(),
                    options: opts.clone(),
                    decoded: a.decoded.clone(),
                    anchor: Some(Digest(a.hash)),
                };
                match read::read_recorded_input(
                    &cx,
                    &rec,
                    Some(a),
                    "保存できません",
                    true,
                    &mut warnings,
                    plan_for,
                )? {
                    Flow::Go(read) => Flow::Go(read.or_mismatch()?),
                    Flow::Stop(end) => Flow::Stop(end),
                }
            }
            None => {
                let how = read::Reading::Any {
                    format: source_format,
                    opts: &opts,
                };
                match read::read_once(&cx, &how, |shape| plan_for(&how, shape))? {
                    Flow::Go(read::Attempt::Read {
                        value,
                        decoded,
                        prep,
                    }) => Flow::Go((value, decoded, AnchorUse::None, prep)),
                    Flow::Go(read::Attempt::Unusable(why)) => {
                        return Err(CoreError::Mismatch(format!("保存できません：{why}")));
                    }
                    Flow::Stop(end) => Flow::Stop(end),
                }
            }
        };
        let (value, decoded, found_use, (plan, lease)) = match read {
            Flow::Go(read) => read,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };

        // 3. アンカー
        let mut staging = Staging::default();
        std::fs::create_dir_all(layout.staging_dir())?;
        let anchor_use = match found_use {
            AnchorUse::None if anchor::needs_anchor(&decoded) => {
                ctx.progress(0.35, "元の読み取りを残しています");
                let ext = if matches!(value, Value::Audio(_)) {
                    "wav"
                } else {
                    "png"
                };
                let part = layout
                    .staging_dir()
                    .join(format!("edit-{tag}-anchor.{ext}.part"));
                staging.add(part.clone());
                let acx = AnchorContext {
                    source: Some(Digest(original.hash)),
                    format: Some(source_format.to_string()),
                    options: opts.clone(),
                };
                let written = runtime
                    .bulk(|_| write_anchor(ctx, &part, &value, &decoded, &acx, &limits, &mon));
                go!(ctx, "元の読み取りを残せません", written);
                let hash = hash_file(&part)?;
                let size = std::fs::metadata(&part)?.len();
                AnchorUse::New(NewAnchorFile {
                    part,
                    hash,
                    size,
                    ext,
                })
            }
            other => other,
        };

        // 4. 描く
        ctx.progress(0.5, "描いています");
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value,
                source: Digest(original.hash),
            },
        )]);
        let rendered = go!(
            ctx,
            "描けません",
            runtime.bulk(|e| e.render(&plan, &inputs, &mon))
        );
        drop(inputs);
        warnings.extend(rendered.warnings.iter().map(|w| w.message.clone()));
        let out = rendered
            .outputs
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::Invalid("描いた出力がありません".into()))?;

        // 5. 書く
        ctx.progress(0.75, "書いています");
        let encoded = go!(
            ctx,
            "書けません",
            runtime.bulk(|_| codecs.encode_bytes(&out.value, &out_format, &enc_opts, &mon))
        );
        let (bytes, encoded) = encoded;
        let media = media_of(&out.value.shape());
        let value_digest = out.digest;
        drop(out);
        drop(lease);
        let out_hash = *blake3::hash(&bytes).as_bytes();
        let out_size = bytes.len() as u64;
        let ext0 = desc.exts.first().copied().unwrap_or("bin");
        let out_part = layout
            .staging_dir()
            .join(format!("edit-{tag}-out.{ext0}.part"));
        staging.add(out_part.clone());
        write_synced(ctx, &out_part, &bytes)?;
        drop(bytes);
        if let Some(stop) = self.after_written(ctx, &mut staging) {
            return stop;
        }
        Ok(Flow::Go(Written {
            p: Prepared {
                job,
                version_job: ctx.version_job(),
                mode,
                item_id,
                base,
                act: Act::Edit { doc, names, parts },
                kind: head.kind,
                ext_hint: head.ext,
                original,
                source_format: source_format.to_string(),
                opts,
                decoded,
                anchor: anchor_use,
                out_part,
                out_hash,
                out_size,
                out_format,
                value_digest,
                media,
                encoded,
                warnings,
                batch,
            },
            staging,
        }))
    }

    /// 段 0〜5 の流して書く道（M13 段 3。音声）：遅延の入力から計画の出力を窓ごとに描いて流して書く（save/stream.rs）。
    /// 一時停止で止めるときは作業用の PCM を残す印を立てる（再開は初めから。残っているものを使い回す）。
    fn prepare_stream(&self, ctx: &dyn UnitCx, d: Decided<'_>) -> Result<Flow<Written>> {
        let mut work: Option<crate::edit::work_pcm::WorkHandle> = None;
        let r = self.prepare_stream_in(ctx, d, &mut work);
        if matches!(r, Ok(Flow::Stop(JobEnd::Paused)))
            && let Some(w) = work.as_mut()
        {
            w.retain_on_drop();
        }
        r
    }

    fn prepare_stream_in(
        &self,
        ctx: &dyn UnitCx,
        d: Decided<'_>,
        work: &mut Option<crate::edit::work_pcm::WorkHandle>,
    ) -> Result<Flow<Written>> {
        let library = ctx.library();
        let layout = library.layout();
        let job = ctx.job();
        let tag = ctx.tag();
        let mon = UnitMonitor(ctx);
        let (runtime, codecs) = (d.runtime.as_ref(), d.codecs.as_ref());
        let original_path = layout.content_path(&d.original.hash, &d.original.ext);
        let cx = stream::StreamCx {
            ctx,
            runtime,
            codecs,
            engine: &self.engine,
            layout,
            original: &original_path,
            original_hash: d.original.hash,
            format: d.source_format,
            opts: &d.opts,
            limits: &d.limits,
            mon: &mon,
        };
        // 進捗の重み：入力 1（作業用のファイル 1・照らしの回 0.5）、書く回 × 1、昇格の照らし 0.5、R2 の読み直し 0.25
        let probe = Shape::Audio {
            rate: 48_000,
            channels: 2,
            frames: Dim::Exact(48_000),
        };
        let rounds = codecs
            .encoder(&d.out_format, &probe)
            .map_or(1, |e| e.stream_passes(&d.out_format, &d.enc_opts));
        let promote_weight = if d.may_make_anchor && d.found.is_none() {
            0.5
        } else {
            0.0
        };
        let mut passes =
            stream::Passes::new(&cx, 1.0 + f64::from(rounds) + promote_weight + 0.25, true);
        passes.report(0.0, 0.0, "元の大きさを確かめています");
        let mut warnings: Vec<String> = Vec::new();
        // 1. 入力
        // エディタが同じ元・同じ読み方の入力を貸していれば、元を読み直さずに使う（照らしていなければ照らしの回を回す）
        let lent = match stream::borrowed(
            &cx,
            d.item_id,
            &d.options_digest,
            d.found.as_ref(),
            &mut passes,
        )? {
            Flow::Go(lent) => lent,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        let input = match lent {
            Some((input, lent_work)) => {
                *work = lent_work;
                input
            }
            None => {
                let want = match &d.found {
                    Some(a) => stream::Want::Recorded {
                        decoded: &a.decoded,
                        platform: true,
                    },
                    None => stream::Want::Any,
                };
                let read = stream::read_input(
                    &cx,
                    d.found.clone(),
                    want,
                    "保存できません",
                    &mut warnings,
                    &mut passes,
                    work,
                )?;
                match read {
                    Flow::Go(read) => read.or_mismatch()?,
                    Flow::Stop(end) => return Ok(Flow::Stop(end)),
                }
            }
        };
        // 2. 計画（遅延の値の形で）と予算
        let shape = input.value.shape();
        let shapes = BTreeMap::from([(InputId(0), shape)]);
        let plan = runtime
            .bulk(|e| e.compile(d.graph, &shapes))
            .map_err(|issues| CoreError::Invalid(format!("文書を描けません：{issues}")))?;
        let out_shape = plan
            .output_shapes()
            .first()
            .map(|(_, s)| *s)
            .ok_or_else(|| CoreError::Invalid("文書に出力がありません".into()))?;
        if !codecs.encode_streams(&d.out_format, &out_shape) {
            return Err(CoreError::Unsupported(format!(
                "この結果は {} に書けません（保存する形式を選び直してください。既定は Ogg Vorbis）",
                d.out_format
            )));
        }
        let promote =
            matches!(input.anchor, AnchorUse::None) && anchor::needs_anchor(&input.decoded);
        let tasks = runtime.bulk_threads() as u64;
        let promote_need = if promote {
            stream::verify_bytes(&shape, tasks, 32 << 20)
        } else {
            0
        };
        #[cfg(test)]
        self.engine.at(Stage::Admit {
            resident: input.resident,
            promote,
        });
        let spec = EstimateSpec::new(0)
            .stream(OutputId(0))
            .digests(true)
            .input(
                InputId(0),
                Source::Lazy {
                    resident: input.resident,
                },
            )
            .output(
                OutputId(0),
                Sink::Encode {
                    codecs,
                    format: &d.out_format,
                    opts: &d.enc_opts,
                },
            );
        let (_estimate, lease) = go!(
            ctx,
            "保存のメモリを確保できません",
            runtime.admit_bulk_with(&plan, spec, |_| promote_need, &mon)
        );
        // 3. アンカーの昇格（Platform・External で読んだ作業用のファイルを、写して照らす）
        let mut staging = Staging::default();
        std::fs::create_dir_all(layout.staging_dir())?;
        let anchor_use = if promote {
            let w = work.as_ref().ok_or_else(|| {
                CoreError::Invalid("再現しない読みなのに作業用のファイルがありません".into())
            })?;
            let pass = stream::Pass {
                passes: &passes,
                weight: promote_weight,
                message: "元の読み取りを残しています",
            };
            let a = match stream::promote(&cx, w, &input.decoded, &tag, &mut staging, pass)? {
                Flow::Go(a) => a,
                Flow::Stop(end) => return Ok(Flow::Stop(end)),
            };
            passes.finish(promote_weight);
            a
        } else {
            passes.finish(promote_weight);
            input.anchor.clone()
        };
        // 4. 描いて書く
        let out_part = layout
            .staging_dir()
            .join(format!("edit-{tag}-out.{}.part", d.ext0));
        staging.add(out_part.clone());
        let written = match stream::write_output(
            &cx,
            &plan,
            &input,
            None,
            &d.out_format,
            &d.enc_opts,
            &out_part,
            &passes,
            f64::from(rounds),
            rounds,
        )? {
            Flow::Go(w) => w,
            Flow::Stop(end) => return Ok(Flow::Stop(end)),
        };
        passes.finish(f64::from(rounds));
        drop(lease);
        // 5. R2：書いたものを 1 回読んで、台帳の BLAKE3 と書き出しの記録のダイジェストを取る（重み 0.25 の回）
        let (out_hash, file_digest, out_size) = match stream::hash_part(&out_part, &passes, &mon) {
            Ok(h) => h,
            Err(CoreError::Canceled) => match stop_end(ctx) {
                Some(end) => return Ok(Flow::Stop(end)),
                None => return Err(CoreError::Canceled),
            },
            Err(e) => return Err(e),
        };
        warnings.extend(written.warnings);
        let encoded = written.encoded.stamp(file_digest);
        if let Some(stop) = self.after_written(ctx, &mut staging) {
            return stop;
        }
        Ok(Flow::Go(Written {
            p: Prepared {
                job,
                version_job: ctx.version_job(),
                mode: d.mode,
                item_id: d.item_id,
                base: d.base,
                act: Act::Edit {
                    doc: d.doc,
                    names: d.names,
                    parts: d.parts,
                },
                kind: d.head.kind,
                ext_hint: d.head.ext,
                original: d.original,
                source_format: d.source_format.to_string(),
                opts: d.opts,
                decoded: input.decoded,
                anchor: anchor_use,
                out_part,
                out_hash,
                out_size,
                out_format: d.out_format,
                value_digest: written.encoded.value_digest,
                media: media_of(&out_shape),
                encoded,
                warnings,
                batch: d.batch,
            },
            staging,
        }))
    }

    /// 段 6〜9：台帳の門を持って予告 → 配置 → コミット（トランザクション 2 の中で `record` が結果を書く）し、門と staging を
    /// 片付ける。後のジョブは呼び手が積む（[`Committed::saved`]）。土台・持ち主の競合は `Conflict`（置いたファイルと予告は
    /// 片付けてある）。
    pub(crate) fn commit(
        &self,
        ctx: &dyn UnitCx,
        written: Written,
        record: Record,
    ) -> Result<Committed> {
        let Written { p, staging } = written;
        let library = ctx.library();
        let db = library.db();
        let layout = library.layout();
        let job = p.job;
        ctx.progress(0.9, "保存しています");
        let gate = library.ingest_exclusive();
        // 6. 予告
        let pre = {
            let p = p.clone();
            db.write(move |tx| announce(tx, &p, now_ms()))?
        };
        let go = match pre {
            Tx1::Conflict(why) => {
                drop(gate);
                drop(staging);
                ctx.log(&format!("保存しません：{why}"));
                return Err(CoreError::Conflict(why));
            }
            Tx1::Existing {
                item_id,
                role,
                trashed,
            } => {
                drop(gate);
                drop(staging);
                let (role_name, version) = role_json(role);
                let message =
                    format!("同じ中身のアイテム（{item_id}）があるので、新しく作りません");
                ctx.log(&message);
                return Ok(Committed {
                    outcome: json!({
                        "outcome": "existing", "itemId": item_id, "role": role_name, "version": version,
                        "trashed": trashed,
                    }),
                    message,
                    saved: None,
                });
            }
            Tx1::Go(go) => go,
        };
        #[cfg(test)]
        if self.crash_at(Fault::AfterAnnounce) {
            staging.keep();
            return Err(crashed(Fault::AfterAnnounce));
        }
        // 7. 配置
        if let Err(err) = place_files(layout, &p, &go) {
            undo_placement(db, layout, job);
            return Err(err);
        }
        #[cfg(test)]
        if self.crash_at(Fault::AfterPlace) {
            staging.keep();
            return Err(crashed(Fault::AfterPlace));
        }
        #[cfg(test)]
        self.at(Stage::Placed);
        // 8. コミット
        let committed = {
            let (p, go) = (p.clone(), go.clone());
            db.write(move |tx| {
                let now = now_ms();
                match &p.act {
                    Act::Edit { doc, names, parts } => {
                        // DB にまだない文書（一括で継ぎ足した文書）は、版と同じトランザクションで部品を置く
                        if let Some(parts) = parts {
                            store::put_doc(tx, parts, now)?;
                        }
                        match names {
                            None => commit_overwrite(tx, &p, doc, &go, now, &record),
                            Some(names) => commit_save_as(tx, &p, doc, names, &go, now, &record),
                        }
                    }
                    Act::Revert(target) => revert::commit_revert(
                        tx,
                        &revert::Out::rebuilt(&p, target, &go)?,
                        now,
                        &record,
                    ),
                }
            })
        };
        let outcome = match committed {
            Ok(outcome) => outcome,
            Err(err) => {
                // 門の間にゴミ箱に入れられたなど：置いたファイルのうち台帳に載っていないものを消し、予告を消す
                undo_placement(db, layout, job);
                ctx.log(&format!("保存できませんでした：{err}"));
                return Err(err);
            }
        };
        #[cfg(test)]
        if self.crash_at(Fault::AfterCommit) {
            staging.keep();
            return Err(crashed(Fault::AfterCommit));
        }
        // 9. 後始末（後のジョブは呼び手が積む）
        drop(gate);
        drop(staging);
        let message = match (&p.act, go.unchanged) {
            (_, true) => "中身が同じなので、版だけ足しました".to_string(),
            (Act::Revert(t), false) => format!("版 {} に戻しました（作り直しました）", t.to),
            (Act::Edit { .. }, false) => "保存しました".to_string(),
        };
        ctx.log(&message);
        Ok(Committed {
            outcome,
            message,
            saved: Some(p.kind),
        })
    }
}

impl EditHandler {
    /// 保存のあとのジョブを積む（中身が変わったのでサムネイル、画像・音声は解析、動画・音声はプロキシ。GC の時計があれば、
    /// 候補があるときだけ GC も）。積めなくても保存は終わっている（開き直すときにも積む）ので、ログに残して続ける。
    fn queue_follow_ups(&self, ctx: &JobContext, kind: MediaKind) {
        let mut follow = vec![JobKind::Thumbnail];
        if matches!(kind, MediaKind::Image | MediaKind::Audio) {
            follow.push(JobKind::Analyze);
        }
        if kind.needs_proxy() {
            follow.push(JobKind::Proxy);
        }
        for kind in follow {
            if let Err(err) = ctx.ensure_queued(kind, "{}") {
                tracing::warn!(%err, kind = kind.as_str(), "保存のあとのジョブを積めませんでした");
            }
        }
        if let Some(clock) = &self.gc
            && let Err(err) = gc::queue_if_due(ctx.library(), clock, || {
                ctx.ensure_queued(JobKind::Gc, "{}")
            })
        {
            tracing::warn!(%err, "GC のジョブを積めませんでした");
        }
    }
}

fn role_json(role: OwnerRole) -> (&'static str, Option<i64>) {
    match role {
        OwnerRole::Current => ("current", None),
        OwnerRole::Original => ("original", Some(0)),
        OwnerRole::Past { version } => ("past", Some(version)),
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// バイト列をファイルに書いて同期する（`UnitCx::sync_file`。改名で `files/` に置く前に。電源断で中身のないファイルが台帳に
/// 載らないように）。
fn write_synced(ctx: &dyn UnitCx, path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    ctx.sync_file(&file)?;
    Ok(())
}

/// アンカーを `part` に書いて同期し（`UnitCx::sync_file`）、読み直して元の読み取りの記録と照らす（`anchor::write` →
/// `anchor::verify`）。
fn write_anchor(
    ctx: &dyn UnitCx,
    part: &Path,
    value: &Value,
    decoded: &DecodeStamp,
    cx: &AnchorContext,
    limits: &Limits,
    mon: &dyn Monitor,
) -> std::result::Result<(), EngineError> {
    let mut writer = BufWriter::new(File::create(part)?);
    anchor::write(value, decoded, cx, limits, &mut writer, mon)?;
    let file = writer
        .into_inner()
        .map_err(|e| EngineError::Io(e.into_error()))?;
    ctx.sync_file(&file)?;
    drop(file);
    anchor::verify(&mut BufReader::new(File::open(part)?), decoded, limits, mon)?;
    Ok(())
}

/// トランザクション 1（台帳の門を持って）：土台を確かめ直し、H の持ち主で決め、拡張子を決め、台帳にない出力とアンカーを
/// 予告する。
fn announce(tx: &Connection, p: &Prepared, now: i64) -> Result<Tx1> {
    let head = versions::head(tx, p.item_id)?
        .ok_or_else(|| CoreError::NotFound(format!("アイテム {}", p.item_id)))?;
    if let Some(why) = base_conflict(&head, &p.base) {
        return Ok(Tx1::Conflict(why));
    }
    // 読んだアンカーが、描いている間に片付けられていないか（ゴミ箱を空にすると、どの版の入力からも参照されなくなったアンカーを
    // 消す。門を持ったので、ここから後は消されない）
    if let Some(a) = p.anchor.existing()
        && !anchors::get(tx, a.id)?
            .is_some_and(|now| now.blob_id == a.blob_id && now.hash == a.hash)
    {
        return Ok(Tx1::Conflict(
            "保存する間に、元の読み取りの記録（アンカー）が片付けられました。もう一度保存してください"
                .into(),
        ));
    }
    let owner = find_owner(tx, &p.out_hash)?;
    let unchanged = match decide(owner.as_ref(), p.item_id, p.mode) {
        Decision::Conflict(why) => return Ok(Tx1::Conflict(why)),
        Decision::Existing {
            item_id,
            role,
            trashed,
        } => {
            return Ok(Tx1::Existing {
                item_id,
                role,
                trashed,
            });
        }
        Decision::Unchanged => true,
        Decision::Proceed => false,
    };
    let listed = ledger::ext_of(tx, &p.out_hash)?;
    let ext = output_ext(&p.out_format, &p.ext_hint, listed.as_deref()).ok_or_else(|| {
        CoreError::Unsupported(format!(
            "出力の形式 {} の拡張子を決められません",
            p.out_format
        ))
    })?;
    let mut announced = Vec::new();
    if listed.is_none() {
        announced.push(ledger::announce(
            tx,
            &p.out_hash,
            &ext,
            ledger::PendingKind::Place,
            Some(p.job),
            now,
        )?);
    }
    let anchor_ext = match &p.anchor {
        AnchorUse::New(a) => {
            let a_listed = if a.hash == p.out_hash {
                Some(ext.clone())
            } else {
                ledger::ext_of(tx, &a.hash)?
            };
            let a_ext = a_listed.clone().unwrap_or_else(|| a.ext.to_string());
            if a_listed.is_none() {
                announced.push(ledger::announce(
                    tx,
                    &a.hash,
                    &a_ext,
                    ledger::PendingKind::Place,
                    Some(p.job),
                    now,
                )?);
            }
            Some(a_ext)
        }
        AnchorUse::None | AnchorUse::Existing(_) | AnchorUse::Fallback(_) => None,
    };
    Ok(Tx1::Go(Go {
        ext,
        reused: listed.is_some(),
        unchanged,
        anchor_ext,
        announced,
    }))
}

/// 段 7：staging の出力（と今回作ったアンカー）を `files/` へ改名する（同じ中身がもうあれば staging を消す。台帳に行が
/// あるのにファイルがなければ、置いて直す）。
fn place_files(layout: &Layout, p: &Prepared, go: &Go) -> Result<()> {
    place(
        &Staged {
            path: p.out_part.clone(),
            hash: p.out_hash,
            size: p.out_size,
        },
        &layout.content_path(&p.out_hash, &go.ext),
    )?;
    if let (AnchorUse::New(a), Some(ext)) = (&p.anchor, &go.anchor_ext) {
        place(
            &Staged {
                path: a.part.clone(),
                hash: a.hash,
                size: a.size,
            },
            &layout.content_path(&a.hash, ext),
        )?;
    }
    Ok(())
}

/// 予告のあとで保存をやめるとき（台帳の門を持ったまま）：このジョブの予告のうち、同じ拡張子で台帳に載っていない中身の
/// ファイルを消し、予告を消す。消せないファイルがあれば予告を残す（次に開くときの照合がやり直す）。
fn undo_placement(db: &Db, layout: &Layout, job: i64) {
    let rows: Result<Vec<(Vec<u8>, String)>> = db.read(|c| {
        // 予告の表は小さいので走査する（docs/data-model.md §4 の例外）
        let mut stmt = c.prepare_cached(
            "SELECT p.hash, p.ext FROM ledger_pending p
              WHERE p.job_id = ?1
                AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash AND b.ext = p.ext)",
        )?;
        let rows = stmt.query_map([job], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(%err, job, "保存の予告を読めませんでした（次に開くときに照らします）");
            return;
        }
    };
    let mut removed_all = true;
    for (hash, ext) in rows {
        let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
            continue;
        };
        let path = layout.content_path(&hash, &ext);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                removed_all = false;
                tracing::warn!(%err, path = %path.display(), "置いたファイルを消せませんでした（次に開くときに消します）");
            }
        }
    }
    if removed_all && let Err(err) = db.write(move |tx| ledger::clear_announced(tx, job)) {
        tracing::warn!(%err, job, "保存の予告を消せませんでした（次に開くときに照らします）");
    }
}

/// 今回の版の結果（途中経過の `committed` とジョブの結果）。
fn saved_json(
    p: &Prepared,
    go: &Go,
    item_id: i64,
    version: i64,
    version_id: i64,
) -> serde_json::Value {
    json!({
        "outcome": if go.unchanged { "unchanged" } else { "saved" },
        "itemId": item_id,
        "version": version,
        "versionId": version_id,
        "hash": hex(&p.out_hash),
        "ext": go.ext,
        "reused": go.reused,
        "anchor": p.anchor.label(),
        "warnings": p.warnings,
    })
}

/// アンカーの行（今回作ったものは台帳に載せて足す）。
fn anchor_row(tx: &Connection, p: &Prepared, go: &Go, now: i64) -> Result<Option<i64>> {
    Ok(match &p.anchor {
        AnchorUse::None => None,
        AnchorUse::Existing(a) | AnchorUse::Fallback(a) => Some(a.id),
        AnchorUse::New(a) => Some(anchors::insert(
            tx,
            &NewAnchor {
                source: p.original.hash,
                options: &p.opts,
                decoded: &p.decoded,
                hash: a.hash,
                ext: go.anchor_ext.as_deref().unwrap_or(a.ext),
                size: a.size,
                now,
            },
        )?),
    })
}

/// 新しい頭の版（描いた版）を足し、入力の記録を書く。版の行の ID を返す。
#[allow(clippy::too_many_arguments)]
fn add_head(
    tx: &Connection,
    p: &Prepared,
    go: &Go,
    item_id: i64,
    version: i64,
    base_version: i64,
    doc_id: i64,
    anchor_id: Option<i64>,
    now: i64,
) -> Result<i64> {
    let blob = ledger::id_of(tx, &p.out_hash)?
        .ok_or_else(|| CoreError::NotFound("保存した中身が台帳にありません".into()))?;
    let size = i64::try_from(p.out_size)
        .map_err(|_| CoreError::TooLarge(format!("出力が大きすぎます: {}", p.out_size)))?;
    let record = VersionRecord {
        encoded: Some(p.encoded.clone()),
        media: Some(MediaRecord {
            mime: mime_for(&go.ext).map(str::to_string),
            codec_info: None,
        }),
        warnings: p.warnings.clone(),
        batch: p.batch.clone(),
        ..VersionRecord::new()
    };
    let vid = versions::insert_version(
        tx,
        &NewVersion {
            item_id,
            version,
            kind: VersionKind::Edit,
            doc_id: Some(doc_id),
            base_version: Some(base_version),
            revert_to: None,
            output_hash: p.out_hash,
            ext: &go.ext,
            size_bytes: size,
            format: Some(&p.out_format),
            value_digest: Some(p.value_digest),
            width: p.media.width,
            height: p.media.height,
            duration_ms: p.media.duration_ms,
            record: &record,
            file_blob_id: Some(blob),
            file_state: FileState::Hold,
            replaced_at: None,
            job_id: p.version_job,
            now,
        },
    )?;
    let rec = RecordedInput {
        source: Digest(p.original.hash),
        format: p.source_format.clone(),
        options: p.opts.clone(),
        decoded: p.decoded.clone(),
        anchor: p.anchor.file_hash().map(Digest),
    };
    let recorded = String::from_utf8(
        canonical(&rec).map_err(|e| from_engine("入力の記録を正規の形にできません", e))?,
    )
    .map_err(|e| CoreError::Invalid(format!("入力の記録が UTF-8 ではありません: {e}")))?;
    versions::insert_inputs(
        tx,
        vid,
        &[NewInput {
            input: 0,
            role: InputRole::Primary,
            source_hash: p.original.hash,
            value_digest: p.decoded.value_digest,
            recorded: &recorded,
            anchor_id,
        }],
    )?;
    Ok(vid)
}

/// トランザクション 2（上書き）：土台を確かめ直し、版 0（最初の保存）・アンカー・アイテムの置き換え・前の頭・新しい頭・入力を
/// 書き、自分の予告を消して途中経過に結果を書く。title・original_name・取り込み日時・再生の記録・素材の設定・thumb_rev は
/// 変えない（M12 の決定 10・11）。
fn commit_overwrite(
    tx: &Connection,
    p: &Prepared,
    doc: &Digest,
    go: &Go,
    now: i64,
    record: &Record,
) -> Result<serde_json::Value> {
    let id = p.item_id;
    // (a) 土台
    let head =
        versions::head(tx, id)?.ok_or_else(|| CoreError::NotFound(format!("アイテム {id}")))?;
    if let Some(why) = base_conflict(&head, &p.base) {
        return Err(CoreError::Conflict(why));
    }
    // (b) 別のアイテム（ゴミ箱を含む）の今と同じ中身（S7）
    let other: Option<i64> = tx
        .prepare_cached("SELECT id FROM items WHERE hash = ?1 AND id <> ?2")?
        .query_row(params![&p.out_hash[..], id], |r| r.get(0))
        .optional()?;
    if let Some(other) = other {
        return Err(CoreError::Conflict(format!(
            "保存した結果が、別のアイテム（{other}）の今と同じ中身になります"
        )));
    }
    // (c) 最初の保存なら版 0（元＝今）
    if head.version == 0 {
        versions::insert_original(tx, id, &p.original, &p.decoded, now)?;
    }
    // (d) 文書（start_save で置き済みか、一括ならこのトランザクションで置いた）
    let doc_id = store::doc_id_by_digest(tx, doc)?
        .ok_or_else(|| CoreError::NotFound(format!("保存する文書 {doc}")))?;
    // (e) アンカー
    let anchor_id = anchor_row(tx, p, go, now)?;
    // (f) 置き換え（同じ中身なら日時だけ）
    if go.unchanged {
        tx.prepare_cached("UPDATE items SET updated_at = ?2 WHERE id = ?1")?
            .execute(params![id, now])?;
    } else {
        let size = i64::try_from(p.out_size)
            .map_err(|_| CoreError::TooLarge(format!("出力が大きすぎます: {}", p.out_size)))?;
        let n = tx
            .prepare_cached(
                "UPDATE items SET hash = ?2, ext = ?3, size_bytes = ?4, mime = ?5, width = ?6, height = ?7,
                        duration_ms = ?8, codec_info = NULL, thumb_state = 'pending',
                        proxy_state = CASE WHEN kind IN ('video', 'audio') THEN 'unknown' ELSE 'not_needed' END,
                        analysis_state = CASE WHEN kind IN ('image', 'audio') THEN 'pending' ELSE 'not_needed' END,
                        updated_at = ?9
                  WHERE id = ?1 AND hash = ?10 AND trashed_at IS NULL",
            )?
            .execute(params![
                id,
                &p.out_hash[..],
                go.ext,
                size,
                mime_for(&go.ext),
                p.media.width,
                p.media.height,
                p.media.duration_ms,
                now,
                &head.hash[..],
            ])?;
        if n != 1 {
            return Err(CoreError::Conflict(
                "保存する間に、アイテムが変わりました".into(),
            ));
        }
    }
    // (g) 前の頭
    if head.version >= 1 {
        versions::supersede_head(tx, id, head.version, &p.out_hash, &p.original.hash, now)?;
    }
    // (h)(i) 新しい頭と入力
    let version = head.version + 1;
    let vid = add_head(tx, p, go, id, version, head.version, doc_id, anchor_id, now)?;
    // (k) 下書き：保存した文書と同じなら消し、このジョブのあとに置いた下書きは土台を新しい頭に付け替える
    drafts::settle_after_save(
        tx,
        id,
        Some(doc_id),
        p.job,
        Base {
            hash: p.out_hash,
            version,
        },
    )?;
    // (j)(l) 予告を消し、結果を呼び手の行に（保存のジョブは途中経過）
    ledger::clear_pending(tx, &go.announced)?;
    let outcome = saved_json(p, go, id, version, vid);
    record(tx, &outcome)?;
    Ok(outcome)
}

/// トランザクション 2（別名で保存）：元のアイテム A の土台を確かめ直し、新しいアイテム B を入れ、B の版 0（A の元）・
/// アンカー・版 1・入力・写すもの（タグ・フォルダ・評価・メモ・出どころ。決定 4・9）・来歴を書き、予告を消して結果を書く。
/// A は変えない。ユニット・プレイリスト・再生の記録・素材の設定は写さない。
fn commit_save_as(
    tx: &Connection,
    p: &Prepared,
    doc: &Digest,
    names: &(String, String),
    go: &Go,
    now: i64,
    record: &Record,
) -> Result<serde_json::Value> {
    let a = p.item_id;
    let head =
        versions::head(tx, a)?.ok_or_else(|| CoreError::NotFound(format!("アイテム {a}")))?;
    if let Some(why) = base_conflict(&head, &p.base) {
        return Err(CoreError::Conflict(why));
    }
    if let Some(owner) = find_owner(tx, &p.out_hash)? {
        return Err(CoreError::Conflict(format!(
            "保存する間に、同じ中身のアイテム（{}）ができました",
            owner.id
        )));
    }
    let (title, stem) = names;
    let original_name = format!("{stem}.{}", go.ext);
    let info = MediaInfo {
        width: p.media.width,
        height: p.media.height,
        duration_ms: p.media.duration_ms,
        ..MediaInfo::default()
    };
    let b = insert_item(
        tx,
        &NewItem {
            hash: &p.out_hash,
            ext: &go.ext,
            kind: p.kind,
            mime: mime_for(&go.ext),
            size: p.out_size,
            title,
            original_name: &original_name,
            original_path: "",
            info: Some(&info),
            source_site: "",
            source_url: None,
            captured_at: None,
            now,
        },
    )?;
    // 評価・メモと出どころ（source_site・source_url・captured_at）を写す（決定 4・9）
    tx.prepare_cached(
        "UPDATE items SET rating = (SELECT rating FROM items WHERE id = ?2),
                          note = (SELECT note FROM items WHERE id = ?2),
                          source_site = (SELECT source_site FROM items WHERE id = ?2),
                          source_url = (SELECT source_url FROM items WHERE id = ?2),
                          captured_at = (SELECT captured_at FROM items WHERE id = ?2)
          WHERE id = ?1",
    )?
    .execute(params![b, a])?;
    // B の版 0 は A の元（ファイルを共有する。決定 1）
    versions::insert_original(tx, b, &p.original, &p.decoded, now)?;
    let doc_id = store::doc_id_by_digest(tx, doc)?
        .ok_or_else(|| CoreError::NotFound(format!("保存する文書 {doc}")))?;
    let anchor_id = anchor_row(tx, p, go, now)?;
    let vid = add_head(tx, p, go, b, 1, 0, doc_id, anchor_id, now)?;
    // A の下書き：保存した文書と同じなら消す（上書きと同じ規則。A の今は変わらないので、土台は A の頭のまま）
    drafts::settle_after_save(
        tx,
        a,
        Some(doc_id),
        p.job,
        Base {
            hash: head.hash,
            version: head.version,
        },
    )?;
    tx.prepare_cached(
        "INSERT INTO item_tags (tag_id, item_id, source) SELECT tag_id, ?2, source FROM item_tags WHERE item_id = ?1",
    )?
    .execute(params![a, b])?;
    tx.prepare_cached(
        "INSERT INTO item_folders (folder_id, item_id, pos) SELECT folder_id, ?2, pos FROM item_folders WHERE item_id = ?1",
    )?
    .execute(params![a, b])?;
    tx.prepare_cached(
        "INSERT INTO item_derivations (item_id, source_item_id, source_version, source_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![b, a, p.base.version, &p.base.hash[..], now])?;
    ledger::clear_pending(tx, &go.announced)?;
    let outcome = saved_json(p, go, b, 1, vid);
    record(tx, &outcome)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::value::PixelFormat;

    fn owner(id: i64, role: OwnerRole, trashed: bool) -> Owner {
        Owner {
            id,
            ext: "png".into(),
            trashed,
            role,
        }
    }

    /// 上書き：持ち主なし・元・過去の版は通す（台帳の行を共有）、自分の今は版だけ、ほかの今（ゴミ箱を含む）は Conflict。
    /// 別名：持ち主がいれば（元のアイテム自身の今を含む）そのアイテムを示す（決定 3・12）。
    #[test]
    fn decide_follows_the_owner_and_the_mode() {
        use Decision::*;
        assert_eq!(decide(None, 1, Mode::Overwrite), Proceed);
        assert_eq!(decide(None, 1, Mode::SaveAs), Proceed);
        let me_now = owner(1, OwnerRole::Current, false);
        assert_eq!(decide(Some(&me_now), 1, Mode::Overwrite), Unchanged);
        for role in [OwnerRole::Original, OwnerRole::Past { version: 2 }] {
            assert_eq!(
                decide(Some(&owner(1, role, false)), 1, Mode::Overwrite),
                Proceed
            );
            assert_eq!(
                decide(Some(&owner(2, role, true)), 1, Mode::Overwrite),
                Proceed
            );
        }
        for trashed in [false, true] {
            assert!(matches!(
                decide(
                    Some(&owner(2, OwnerRole::Current, trashed)),
                    1,
                    Mode::Overwrite
                ),
                Conflict(_)
            ));
        }
        for (o, role) in [
            (1, OwnerRole::Current),
            (2, OwnerRole::Current),
            (1, OwnerRole::Original),
            (3, OwnerRole::Past { version: 4 }),
        ] {
            assert_eq!(
                decide(Some(&owner(o, role, o == 3)), 1, Mode::SaveAs),
                Existing {
                    item_id: o,
                    role,
                    trashed: o == 3
                }
            );
        }
    }

    /// 拡張子：台帳の拡張子 → 今と同じ形式なら今の拡張子 → 形式の代表の拡張子。
    #[test]
    fn output_ext_prefers_the_ledger_then_the_current_ext() {
        assert_eq!(output_ext("png", "png", None).as_deref(), Some("png"));
        assert_eq!(output_ext("png", "apng", None).as_deref(), Some("apng"));
        assert_eq!(output_ext("webp", "png", None).as_deref(), Some("webp"));
        assert_eq!(output_ext("png", "aseprite", None).as_deref(), Some("png"));
        assert_eq!(output_ext("jpeg", "png", None).as_deref(), Some("jpg"));
        assert_eq!(
            output_ext("png", "png", Some("apng")).as_deref(),
            Some("apng")
        );
        assert_eq!(
            output_ext("webp", "png", Some("png")).as_deref(),
            Some("png")
        );
        assert_eq!(output_ext("nope", "png", None), None);
    }

    /// メディア情報：画像の幅・高さ、フレームの列のキャンバス、音声の長さ（四捨五入。大きな値でもあふれない）。
    #[test]
    fn media_of_reads_the_shape() {
        let image = Shape::Image {
            w: Dim::Exact(32),
            h: Dim::Exact(24),
            fmt: PixelFormat::Rgba8,
        };
        assert_eq!(
            media_of(&image),
            Media {
                width: Some(32),
                height: Some(24),
                duration_ms: None
            }
        );
        let frames = Shape::Frames {
            count: Dim::Exact(3),
            w: Dim::Exact(40),
            h: Dim::Exact(30),
            fmt: PixelFormat::Rgba8,
            full: kura_engine::shape::Fullness::Full,
        };
        assert_eq!(media_of(&frames).width, Some(40));
        assert_eq!(media_of(&frames).duration_ms, None);
        let audio = |rate, frames| Shape::Audio {
            rate,
            channels: 2,
            frames: Dim::Exact(frames),
        };
        assert_eq!(media_of(&audio(44_100, 44_100)).duration_ms, Some(1000));
        assert_eq!(media_of(&audio(44_100, 22)).duration_ms, Some(0));
        assert_eq!(
            media_of(&audio(44_100, 23)).duration_ms,
            Some(1),
            "四捨五入"
        );
        assert_eq!(media_of(&audio(48_000, 72_000)).duration_ms, Some(1500));
        assert_eq!(
            media_of(&audio(1, u64::MAX)).duration_ms,
            None,
            "i64 を超える長さは書かない"
        );
        assert_eq!(media_of(&audio(0, 10)).duration_ms, None);
    }

    /// 保存できる文書：入力が Primary 1 つ、出力が 1 つのファイル。
    #[test]
    fn check_doc_requires_one_primary_and_one_file() {
        let g = Graph::new_single();
        assert!(check_doc(&g).is_ok());
        let mut two = g.clone();
        two.outputs.push(kura_engine::doc::OutputSpec {
            id: OutputId(1),
            ..g.outputs[0].clone()
        });
        assert!(matches!(check_doc(&two), Err(CoreError::Invalid(_))));
        let mut per = g.clone();
        per.outputs[0].expand = Expand::PerElement {
            name: "{stem}_{index}".into(),
        };
        assert!(matches!(check_doc(&per), Err(CoreError::Invalid(_))));
        let mut fixed = g.clone();
        fixed.inputs[0].role = DocRole::Fixed {
            hash: Digest([1; 32]),
        };
        assert!(matches!(check_doc(&fixed), Err(CoreError::Invalid(_))));
    }

    /// コミットの問い合わせは索引で引く（items は hash の一意の索引と行の ID、写すものはアイテムの索引）。予告の表だけは
    /// 小さいので走査する（docs/data-model.md §4 の例外）。
    #[test]
    fn commit_queries_use_their_indexes() {
        use crate::db::{query_plan, test_conn};
        let conn = test_conn();
        let plan = query_plan(&conn, "SELECT id FROM items WHERE hash = x'01' AND id <> 1");
        assert!(plan.contains("sqlite_autoindex_items_1"), "{plan}");
        for sql in [
            "INSERT INTO item_tags (tag_id, item_id, source) SELECT tag_id, 2, source FROM item_tags WHERE item_id = 1",
            "INSERT INTO item_folders (folder_id, item_id, pos) SELECT folder_id, 2, pos FROM item_folders WHERE item_id = 1",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(
                plan.contains("idx_item_tags_item") || plan.contains("idx_item_folders_item"),
                "{plan}"
            );
            assert!(!plan.contains("SCAN"), "{plan}");
        }
        let plan = query_plan(
            &conn,
            "UPDATE items SET rating = (SELECT rating FROM items WHERE id = 2),
                              note = (SELECT note FROM items WHERE id = 2) WHERE id = 1",
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        let plan = query_plan(
            &conn,
            "UPDATE items SET hash = x'02', updated_at = 3 WHERE id = 1 AND hash = x'01' AND trashed_at IS NULL",
        );
        assert!(plan.contains("INTEGER PRIMARY KEY"), "{plan}");
        let plan = query_plan(
            &conn,
            "SELECT p.hash, p.ext FROM ledger_pending p WHERE p.job_id = 1
                AND NOT EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash AND b.ext = p.ext)",
        );
        assert!(plan.contains("SCAN p"), "{plan}");
        assert!(plan.contains("sqlite_autoindex_blobs_1"), "{plan}");
    }

    /// 終わっていない保存のジョブは、アイテムごとに見分ける（版に戻すも数える。終わったもの・ほかの種類・読めない入力は
    /// 数えない）。状態の索引で引く。
    #[test]
    fn pending_saves_are_found_per_item() {
        use crate::db::{query_plan, test_conn};
        let conn = test_conn();
        let job = |kind: &str, state: &str, input: String| -> i64 {
            conn.query_row(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES (?1, ?2, ?3, 1) RETURNING id",
                params![kind, state, input],
                |r| r.get(0),
            )
            .unwrap()
        };
        let input = |item: i64, action: serde_json::Value| {
            json!({ "v": 1, "itemId": item, "base": { "hash": "00".repeat(32), "version": 0 }, "action": action })
                .to_string()
        };
        let doc = json!({ "type": "overwrite", "doc": "00".repeat(32) });
        job("edit", "done", input(1, doc.clone()));
        job("edit", "failed", input(1, doc.clone()));
        job("gc", "queued", input(1, doc.clone()));
        job("edit", "queued", "{}".into());
        assert_eq!(pending_save(&conn, 1).unwrap(), None);
        let paused = job(
            "edit",
            "paused",
            input(2, json!({ "type": "revert", "to": 0 })),
        );
        assert_eq!(pending_save(&conn, 2).unwrap(), Some(paused));
        assert_eq!(pending_save(&conn, 1).unwrap(), None);
        let running = job("edit", "running", input(1, doc));
        assert_eq!(pending_save(&conn, 1).unwrap(), Some(running));
        let plan = query_plan(
            &conn,
            "SELECT id, input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit'",
        );
        assert!(plan.contains("idx_jobs_queue"), "{plan}");
        assert!(!plan.contains("SCAN jobs"), "{plan}");
    }

    /// 画面が読む終わっていない保存のジョブ：古い順に、アイテムの名前・向き・状態つき。終わったもの・ほかの種類・読めない入力は
    /// 数えない。アイテムの行がなくても（ジョブは中止できるよう）出す。
    #[test]
    fn pending_saves_lists_unfinished_jobs_with_item_titles() {
        use crate::db::test_conn;
        let conn = test_conn();
        crate::edit::testing::insert_item(&conn, 1, [1; 32]);
        conn.execute("UPDATE items SET title = 'long-60min' WHERE id = 1", [])
            .unwrap();
        let job = |kind: &str, state: &str, input: String| -> i64 {
            conn.query_row(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES (?1, ?2, ?3, 1) RETURNING id",
                params![kind, state, input],
                |r| r.get(0),
            )
            .unwrap()
        };
        let input = |item: i64, action: serde_json::Value| {
            json!({ "v": 1, "itemId": item, "base": { "hash": "00".repeat(32), "version": 0 }, "action": action })
                .to_string()
        };
        let doc = json!({ "type": "overwrite", "doc": "00".repeat(32) });
        job("edit", "done", input(1, doc.clone()));
        job("edit", "canceled", input(1, doc.clone()));
        job("gc", "queued", input(1, doc.clone()));
        job("edit", "paused", "{}".into());
        assert_eq!(pending_saves(&conn).unwrap(), Vec::new());
        let running = job("edit", "running", input(1, doc));
        conn.execute(
            "UPDATE jobs SET progress = 0.5, message = '描いています' WHERE id = ?1",
            [running],
        )
        .unwrap();
        let save_as = job(
            "edit",
            "paused",
            input(
                1,
                json!({ "type": "saveAs", "doc": "00".repeat(32), "title": "夕方", "stem": "yu" }),
            ),
        );
        let revert = job(
            "edit",
            "queued",
            input(9, json!({ "type": "revert", "to": 0 })),
        );
        assert_eq!(
            pending_saves(&conn).unwrap(),
            vec![
                PendingSave {
                    job: running,
                    item_id: 1,
                    item_title: Some("long-60min".into()),
                    mode: PendingSaveMode::Overwrite,
                    state: JobState::Running,
                    progress: Some(0.5),
                    message: Some("描いています".into()),
                },
                PendingSave {
                    job: save_as,
                    item_id: 1,
                    item_title: Some("long-60min".into()),
                    mode: PendingSaveMode::SaveAs {
                        title: "夕方".into()
                    },
                    state: JobState::Paused,
                    progress: None,
                    message: None,
                },
                PendingSave {
                    job: revert,
                    item_id: 9,
                    item_title: None,
                    mode: PendingSaveMode::Revert,
                    state: JobState::Queued,
                    progress: None,
                    message: None,
                },
            ]
        );
    }

    /// ジョブの入力の形（小さく、根のダイジェストだけ）。知らない項目は拒む。
    #[test]
    fn job_input_round_trips() {
        let input = JobInput {
            v: 1,
            item_id: 12,
            base: BaseInput {
                hash: Digest([0xab; 32]),
                version: 3,
            },
            action: ActionInput::SaveAs {
                doc: Digest([1; 32]),
                title: "夕方".into(),
                stem: "夕方-2".into(),
            },
        };
        let text = serde_json::to_string(&input).unwrap();
        assert!(text.contains(r#""type":"saveAs""#), "{text}");
        assert!(text.contains(r#""itemId":12"#), "{text}");
        assert_eq!(serde_json::from_str::<JobInput>(&text).unwrap(), input);
        let revert: JobInput = serde_json::from_str(&format!(
            r#"{{"v":1,"itemId":1,"base":{{"hash":"{}","version":2}},"action":{{"type":"revert","to":0}}}}"#,
            "00".repeat(32)
        ))
        .unwrap();
        assert_eq!(revert.action, ActionInput::Revert { to: 0 });
        assert!(
            serde_json::from_str::<JobInput>(&text.replace("\"v\":1", "\"v\":1,\"x\":2")).is_err()
        );
    }
}
