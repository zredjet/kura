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
use super::{EngineServices, drafts, from_engine, store};
use crate::db::Db;
use crate::import::place::{Staged, place};
use crate::import::store::{NewItem, Owner, OwnerRole, find_owner, insert_item};
use crate::import::{MediaKind, format_id, hash_file, mime_for};
use crate::jobs::{
    Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager, save_partial,
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

/// アイテム `item_id` の終わっていない（待ち・実行中・一時停止の）保存のジョブ（あればその 1 つ）。状態の索引
/// `idx_jobs_queue` で引き、入力を読んでアイテムを比べる（読めない入力は飛ばす。そのジョブは動くときに失敗する）。
pub fn pending_save(conn: &Connection, item_id: i64) -> Result<Option<JobId>> {
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

/// 文書を内容アドレスの部品に分ける。
fn split_graph(graph: &Graph) -> Result<kura_engine::doc_parts::DocParts> {
    split(graph).map_err(|e| from_engine("文書を部品に分けられません", e))
}

/// 終わっていない（待ち・実行中・一時停止の）保存のジョブの入力が指す文書の根（版の不変条件で、部品があることを確かめる）。
/// 版に戻すジョブは文書を持たない。読めない入力は飛ばす（そのジョブは動くときに失敗する）。
pub fn live_roots(conn: &Connection) -> Result<Vec<Digest>> {
    let mut stmt = conn.prepare_cached(
        "SELECT input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    roots_of(rows)
}

/// GC の部品の片付けで残す保存のジョブの根（docs/data-model.md §4「保存と版（M12）」）：終わっていないジョブと、`since` 以後に
/// 失敗・中止したジョブ（利用者がやり直せるように。前回 OS のデコーダで読んでいる途中で落ちた保存は、失敗にしてやり直しで
/// 読み直す。docs/editing.md §14 の段 0）。どちらも `idx_jobs_queue`（状態）で引く。
pub fn gc_roots(conn: &Connection, since: i64) -> Result<Vec<Digest>> {
    let mut stmt = conn.prepare_cached(
        "SELECT input_json FROM jobs WHERE state IN ('queued', 'running', 'paused') AND kind = 'edit'
         UNION ALL
         SELECT input_json FROM jobs
          WHERE state IN ('failed', 'canceled') AND kind = 'edit' AND finished_at >= ?1",
    )?;
    let rows = stmt.query_map([since], |r| r.get::<_, String>(0))?;
    roots_of(rows)
}

/// ジョブの入力の列から、文書の根のダイジェスト（重なりなし）。
fn roots_of(rows: impl Iterator<Item = rusqlite::Result<String>>) -> Result<Vec<Digest>> {
    let mut out = Vec::new();
    for row in rows {
        match serde_json::from_str::<JobInput>(&row?).map(|i| i.action) {
            Ok(ActionInput::Overwrite { doc } | ActionInput::SaveAs { doc, .. }) => out.push(doc),
            Ok(ActionInput::Revert { .. }) | Err(_) => {}
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
}

/// テストの割り込み（段ごとに呼ばれる。止めて待つ・DB を書き換えるなど）。
#[cfg(test)]
pub(crate) type Hook = Arc<dyn Fn(Stage) + Send + Sync>;

/// 保存のジョブの処理。エンジンの部品（実行の場・コーデックの組）は、このライブラリのもの。
pub struct EditHandler {
    engine: Arc<EngineServices>,
    /// GC の時計（あれば、コミットのあとに候補があれば GC を積む。`LibrarySession` が GC と同じものを渡す）。
    gc: Option<GcClock>,
    #[cfg(test)]
    pub(crate) fault: Option<Fault>,
    #[cfg(test)]
    pub(crate) hook: Option<Hook>,
}

impl EditHandler {
    /// このライブラリのエンジンの部品で保存する。
    pub fn new(engine: Arc<EngineServices>) -> Self {
        Self {
            engine,
            gc: None,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            hook: None,
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
        self.fault = Some(fault);
        self
    }

    /// 段ごとに `hook` を呼ぶ（テストの割り込み）。
    #[cfg(test)]
    pub(crate) fn with_hook(mut self, hook: Hook) -> Self {
        self.hook = Some(hook);
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

#[cfg(test)]
fn crashed(at: Fault) -> CoreError {
    CoreError::Invalid(format!("テスト用の強制終了（{at:?}）"))
}

/// ジョブの指示（一時停止・中止・終了）をエンジンの中止に写す。
struct JobMonitor<'a>(&'a JobContext);

impl Monitor for JobMonitor<'_> {
    fn canceled(&self) -> bool {
        self.0.control() != Control::Continue
    }
}

/// 段の結果：続けるか、止めるか（一時停止・中止）。
pub(crate) enum Flow<T> {
    Go(T),
    Stop(JobEnd),
}

/// 止める指示があれば、その止め方。
fn stop_end(ctx: &JobContext) -> Option<JobEnd> {
    match ctx.control() {
        Control::Continue => None,
        Control::Pause => Some(JobEnd::Paused),
        Control::Cancel => Some(JobEnd::Canceled { result_json: None }),
    }
}

/// エンジンの結果：中止（止める指示があるとき）は止め方に、ほかの誤りは CoreError に。
fn engine_flow<T>(
    ctx: &JobContext,
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

// 段 1・2 の読みと、版に戻す・GC の照合（`go!` を使うので、マクロの定義のあとに置く）
mod read;
mod revert;

pub use revert::Verdict;
pub(crate) use revert::verify_version;

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
    /// 文書を描いた版（上書き・別名で保存）。`names` は別名で保存の (表示名, 語幹)。
    Edit {
        doc: Digest,
        names: Option<(String, String)>,
    },
    /// 版 k に戻す版（版 k の文書から作り直した）。
    Revert(Box<revert::Target>),
}

/// 描いて書き終えたもの（段 5 まで）。
#[derive(Debug, Clone)]
struct Prepared {
    job: i64,
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
    /// しうるので、中止と同じトランザクションで GC の部品の印を置く（版に戻すは文書を持たない）。
    fn canceled_while_idle(&self, tx: &Connection, input_json: &str) -> Result<()> {
        let with_doc = serde_json::from_str::<JobInput>(input_json)
            .is_ok_and(|i| !matches!(i.action, ActionInput::Revert { .. }));
        if with_doc {
            gc::mark_parts(tx)?;
        }
        Ok(())
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
        let (mode, doc, names) = match input.action.clone() {
            ActionInput::Overwrite { doc } => (Mode::Overwrite, doc, None),
            ActionInput::SaveAs { doc, title, stem } => (Mode::SaveAs, doc, Some((title, stem))),
            ActionInput::Revert { to } => return self.revert(ctx, input.item_id, base, to),
        };
        let (prepared, staging) = match self.prepare(ctx, mode, input.item_id, base, doc, names)? {
            Flow::Go(p) => p,
            Flow::Stop(end) => return Ok(end),
        };
        self.commit(ctx, prepared, staging)
    }

    /// 書き終えたあと（段 5 の終わり）：止める指示があれば止める（呼び手が staging を落として消す）。テストの強制終了なら
    /// staging を残して失敗する。続けるなら None。
    fn after_written<T>(
        &self,
        ctx: &JobContext,
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

    /// 段 0〜5：文書を読み、元を読み（アンカーを作り）、描いて書く。
    fn prepare(
        &self,
        ctx: &JobContext,
        mode: Mode,
        item_id: i64,
        base: Base,
        doc: Digest,
        names: Option<(String, String)>,
    ) -> Result<Flow<(Prepared, Staging)>> {
        let library = ctx.library();
        let db = library.db();
        let layout = library.layout();
        let job = ctx.id();
        let doc_id = db
            .read(|c| store::doc_id_by_digest(c, &doc))?
            .ok_or_else(|| CoreError::NotFound(format!("保存する文書 {doc}")))?;
        let bytes = db.read(|c| store::load_doc_bytes(c, doc_id))?;
        let runtime = self.engine.runtime()?.clone();
        let codecs = self.engine.codecs();
        let graph = runtime
            .bulk(|e| e.load(&bytes))
            .map_err(|e| from_engine("文書を読めません", e))?;
        drop(bytes);
        check_doc(&graph)?;
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
        let mon = JobMonitor(ctx);
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
                .bulk(|e| e.compile(&graph, &shapes))
                .map_err(|issues| CoreError::Invalid(format!("文書を描けません：{issues}")))?;
            let out_shape = plan
                .output_shapes()
                .first()
                .map(|(_, s)| *s)
                .ok_or_else(|| CoreError::Invalid("文書に出力がありません".into()))?;
            if codecs.encoder(&out_format, &out_shape).is_none() {
                return Err(CoreError::Unsupported(format!(
                    "この結果は {out_format} に書けません"
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
                    .join(format!("edit-{job}-anchor.{ext}.part"));
                staging.add(part.clone());
                let acx = AnchorContext {
                    source: Some(Digest(original.hash)),
                    format: Some(source_format.to_string()),
                    options: opts.clone(),
                };
                let written =
                    runtime.bulk(|_| write_anchor(&part, &value, &decoded, &acx, &limits, &mon));
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
            .join(format!("edit-{job}-out.{ext0}.part"));
        staging.add(out_part.clone());
        write_synced(&out_part, &bytes)?;
        drop(bytes);
        if let Some(stop) = self.after_written(ctx, &mut staging) {
            return stop;
        }
        Ok(Flow::Go((
            Prepared {
                job,
                mode,
                item_id,
                base,
                act: Act::Edit { doc, names },
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
            },
            staging,
        )))
    }

    /// 段 6〜9：台帳の門を持って予告 → 配置 → コミットし、後始末をする。
    fn commit(&self, ctx: &JobContext, p: Prepared, staging: Staging) -> Result<JobEnd> {
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
                return Ok(JobEnd::Done {
                    result_json: json!({
                        "outcome": "existing", "itemId": item_id, "role": role_name, "version": version,
                        "trashed": trashed,
                    })
                    .to_string(),
                    message: Some(message),
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
                    Act::Edit { doc, names } => match names {
                        None => commit_overwrite(tx, &p, doc, &go, now),
                        Some(names) => commit_save_as(tx, &p, doc, names, &go, now),
                    },
                    Act::Revert(target) => {
                        revert::commit_revert(tx, &revert::Out::rebuilt(&p, target, &go)?, now)
                    }
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
        // 9. 後始末
        drop(gate);
        drop(staging);
        self.queue_follow_ups(ctx, p.kind);
        let message = match (&p.act, go.unchanged) {
            (_, true) => "中身が同じなので、版だけ足しました".to_string(),
            (Act::Revert(t), false) => format!("版 {} に戻しました（作り直しました）", t.to),
            (Act::Edit { .. }, false) => "保存しました".to_string(),
        };
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: outcome.to_string(),
            message: Some(message),
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

/// バイト列をファイルに書いて `sync_all` する（改名で `files/` に置く前に。電源断で中身のないファイルが台帳に載らないように）。
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// アンカーを `part` に書いて `sync_all` し、読み直して元の読み取りの記録と照らす（`anchor::write` → `anchor::verify`）。
fn write_anchor(
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
    file.sync_all()?;
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
    if listed.is_none() {
        ledger::announce(
            tx,
            &p.out_hash,
            &ext,
            ledger::PendingKind::Place,
            Some(p.job),
            now,
        )?;
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
                ledger::announce(
                    tx,
                    &a.hash,
                    &a_ext,
                    ledger::PendingKind::Place,
                    Some(p.job),
                    now,
                )?;
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
            job_id: Some(p.job),
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
    // (d) 文書（start_save で置き済み）
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
    // (j)(l) 予告を消し、結果を途中経過に
    ledger::clear_announced(tx, p.job)?;
    let outcome = saved_json(p, go, id, version, vid);
    save_partial(
        tx,
        p.job,
        &json!({ "v": 1, "committed": outcome }).to_string(),
        1.0,
        "保存しました",
    )?;
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
    ledger::clear_announced(tx, p.job)?;
    let outcome = saved_json(p, go, b, 1, vid);
    save_partial(
        tx,
        p.job,
        &json!({ "v": 1, "committed": outcome }).to_string(),
        1.0,
        "保存しました",
    )?;
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
