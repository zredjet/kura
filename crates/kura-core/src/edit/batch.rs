//! 一括加工のジョブ（`JobKind::Batch`。M14 段 4。docs/editing.md §4.3・§14「一括加工」、docs/data-model.md §4「一括加工（M14）」、
//! ADR-190・192・193）。
//!
//! 1 本のジョブで、選んだアイテムに 1 つのレシピを 1 件ずつ当てる。対象と結果は `batch_items` の行（積むのと同じトランザクションで
//! pending の行を入れる）。1 件の流れ：
//! 1. 飛ばす（働き手に渡す前に。行は `skipped` とその理由）：ゴミ箱にある、エディタで開いている、前の保存が終わっていない、上書きで
//!    保存していない下書きがある（`includeDrafts` が偽のとき）、画像・音声でない。前回 OS のデコーダで読んでいる途中で落ちた行
//!    （`decoding = 1`）は、同じファイルで落ち続けないよう `failed`（`Crashed`）にする。
//! 2. 継ぎ足し：頭の文書（版がなければ空の文書）にレシピをつなぎ、素材の設定（格子・ピボット・試聴のループ・コマの時間）と秒で
//!    結び付ける（`Engine::instantiate`。結び付けがあるときだけ元の見出しから形を読む）。値がない・当てられない手は `skipped`。
//!    土台は処理するときの頭（ADR-190 (9)）。
//! 3. 準備（k 本の働き手。k は一括のプールのスレッドの数）：`Saver::prepare_graph`（文書は DB に置かず、部品はトランザクション 2 で
//!    置く）。1 件の場 [`BatchUnit`]：止める指示はジョブの、読む前の印は行の `decoding`、版の持ち主は空（`batch_items.version_id`
//!    で引く。ADR-192 (1)）、書きかけの印は `{ジョブ}-{順}`、ファイルの同期は fsync（`Durability::sync_file`）。上書きで書けない形式
//!    （SVG・aseprite・ffmpeg のない MP3 など）は `skipped`、別名では代わりの形式（画像は PNG、音声は Ogg Vorbis か WAV）で書く。
//! 4. コミット（ジョブのスレッドで直列、まとまりごと）：書き終えた [`GROUP_ITEMS`] 件か [`GROUP_TIME`] がたまったら、区切りの同期
//!    （`Durability::barrier`。macOS では F_FULLFSYNC）を 1 回取ってから、1 件ずつ `Saver::commit` する。記録（`Record`）が同じ
//!    トランザクションで行を `saved`・`unchanged` にし、版・別名で作ったアイテム・土台・時刻を書く（版と行が同じトランザクション
//!    なので、落ちても二重にも抜けにもならない）。同じ中身のアイテムがあれば `existing`。土台の競合は 1 回だけ頭を読み直して
//!    やり直し、だめなら `skipped`（Conflict）。ほかの誤りは `failed`。まとまりごとに、後のジョブ（サムネイル・解析・プロキシ）を積む。
//!
//! 止め方：一時停止・譲るは新しく渡さず、書き終えた分をコミットしてから止まる（準備中のものは落とし、行は pending のまま）。中止は
//! 書き終えた分も落とし、残りの pending の行を `canceled` にする（済んだ分は残す）。強制終了は、開く順の staging の掃除と予告の
//! 照合のあと、pending の行から続ける。区切りの同期をまとまりのコミットの前に取るので、電源断で DB の最後のコミットを失っても
//! 「DB にないファイル」が残るだけで、DB が永続になっていない中身を指すことはない（失った行は pending に戻ってやり直す）。

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kura_engine::doc::{FormatChoice, Graph, InputId};
use kura_engine::ops::audio::set_loop::FrameRange;
use kura_engine::ops::util::GridParam;
use kura_engine::recipe::{BindError, Binding, Facts, PivotFact, Recipe, RecipeKind, bound_json};
use kura_engine::{Digest, Shape};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::save::{
    Base, Committed, Flow, GraphExtra, Mode, Record, Saver, UnitCx, UnitMonitor, Written,
    pending_edit_job,
};
use super::services::Durability;
use super::{EngineServices, drafts, from_engine, gc, recipes, store, versions};
use crate::db::Db;
use crate::import::{MediaKind, format_id};
use crate::jobs::{Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager};
use crate::library::Library;
use crate::text::nfc;
use crate::time::now_ms;
use crate::{CoreError, Result};

/// ジョブの入力の形の版。
const INPUT_V: u32 = 1;
/// 区切りの同期を取るまとまりの件数（ADR-190 (2) の「数十件ごと」。ADR-193）。
pub(crate) const GROUP_ITEMS: usize = 32;
/// 区切りの同期を取るまとまりの時間（件数に届かなくても、書き終えた分をこれだけ待たせない）。
pub(crate) const GROUP_TIME: Duration = Duration::from_secs(1);
/// 別名の名前の型の、アイテムの表示名が入る所。
pub const NAME_PLACEHOLDER: &str = "{名前}";
/// 別名の名前の型の既定（ADR-190 (9)）。
pub const DEFAULT_SAVE_AS_NAME: &str = "{名前} のコピー";

/// 一括の向き。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchMode {
    /// 上書き（アイテム ID を保ったまま版を足す）。
    Overwrite,
    /// 別名で保存（`name` は名前の型。[`NAME_PLACEHOLDER`] にアイテムの表示名が入る）。
    SaveAs { name: String },
}

/// 一括の頼み（[`start_batch`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRequest {
    pub recipe_id: i64,
    /// 当てるアイテム（選んだ順。重なりは前のものだけ残す）。
    pub items: Vec<i64>,
    pub mode: BatchMode,
    /// 上書きで、保存していない下書きのあるアイテムも含める（初めは外す。ADR-190 (8)）。
    pub include_drafts: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BatchInput {
    v: u32,
    recipe: RecipeInput,
    mode: ModeInput,
    #[serde(default)]
    include_drafts: bool,
}

/// 積んだときのレシピの写し（文書の根と結び付け。レシピを消す・直しても、動いている一括はこれを使う）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecipeInput {
    doc: Digest,
    name: String,
    #[serde(default)]
    bindings: Vec<Binding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type",
    deny_unknown_fields
)]
enum ModeInput {
    Overwrite,
    SaveAs { name: String },
}

/// 一括のジョブを積む。レシピ（`edit_recipes`）を確かめて、文書の根と結び付けを入力に写し、同じトランザクションで対象の行を
/// 選んだ順に入れる（ないアイテムは入れない）。エディタで開いているアイテムは、積んだ時点で `skipped`（Open）にする（開いている
/// 間に保存できるように。あとから開こうとするアイテムは、一括が済むまで開かない：[`super::save::pending_save`]）。レシピに
/// 当てられない理由がある・N → 1 のレシピ・対象がない・別名の名前の型が空なら Invalid。
pub fn start_batch(
    db: &Db,
    jobs: &JobManager,
    engine: &EngineServices,
    req: &BatchRequest,
) -> Result<JobId> {
    let runtime = engine.runtime()?.clone();
    let stored = recipes::get(db, &runtime, req.recipe_id)?;
    if let Some(why) = stored.problem {
        return Err(CoreError::Invalid(format!(
            "このレシピは当てられません：{why}"
        )));
    }
    if stored.recipe.kind != RecipeKind::Each {
        return Err(CoreError::Invalid(
            "まとめて 1 つにするレシピは、1 件ずつの一括では当てられません".into(),
        ));
    }
    let mode = match &req.mode {
        BatchMode::Overwrite => ModeInput::Overwrite,
        BatchMode::SaveAs { name } => {
            let name = nfc(name.trim());
            if name.is_empty() {
                return Err(CoreError::Invalid("別名の名前の型が空です".into()));
            }
            ModeInput::SaveAs { name }
        }
    };
    let mut items: Vec<i64> = Vec::with_capacity(req.items.len());
    for id in &req.items {
        if !items.contains(id) {
            items.push(*id);
        }
    }
    if items.is_empty() {
        return Err(CoreError::Invalid("当てるアイテムがありません".into()));
    }
    let doc = db.read(|c| recipes::doc_root(c, req.recipe_id))?;
    let input = BatchInput {
        v: INPUT_V,
        recipe: RecipeInput {
            doc,
            name: stored.recipe.name.clone(),
            bindings: stored.recipe.bindings.clone(),
        },
        mode,
        include_drafts: req.include_drafts,
    };
    let open = engine.current_editor().map(|e| e.item_id());
    let now = now_ms();
    let id = jobs.enqueue_then(
        JobKind::Batch,
        serde_json::to_string(&input)?,
        move |tx, job| {
            let mut exists = tx.prepare_cached("SELECT 1 FROM items WHERE id = ?1")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO batch_items (job_id, ord, item_id, state, code, message, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut ord = 0i64;
            for item in &items {
                if exists.query_row([item], |_| Ok(())).optional()?.is_none() {
                    continue;
                }
                if open == Some(*item) {
                    insert.execute(params![
                        job,
                        ord,
                        item,
                        "skipped",
                        "Open",
                        "エディタで開いているので飛ばしました",
                        now
                    ])?;
                } else {
                    insert.execute(params![
                        job,
                        ord,
                        item,
                        "pending",
                        None::<String>,
                        None::<String>,
                        None::<i64>
                    ])?;
                }
                ord += 1;
            }
            if ord == 0 {
                return Err(CoreError::Invalid(
                    "当てるアイテムがありません（どれもライブラリにありません）".into(),
                ));
            }
            Ok(())
        },
    )?;
    Ok(id)
}

/// アイテム `item_id` を待っている（pending の行のある、終わっていない）一括のジョブ（`idx_batch_items_item`）。
pub fn pending_batch(conn: &Connection, item_id: i64) -> Result<Option<JobId>> {
    Ok(conn
        .prepare_cached(
            "SELECT b.job_id FROM batch_items b JOIN jobs j ON j.id = b.job_id
              WHERE b.item_id = ?1 AND b.state = 'pending' AND j.state IN ('queued', 'running', 'paused')
              ORDER BY b.job_id LIMIT 1",
        )?
        .query_row([item_id], |r| r.get(0))
        .optional()?)
}

/// 終わっていない（と `since` 以後に失敗・中止した）一括のジョブの入力が指すレシピの文書の根（GC の部品の片付けが残す。
/// `save::gc_roots` が足す）。
pub(crate) fn roots_of_input(input_json: &str) -> Option<Digest> {
    serde_json::from_str::<BatchInput>(input_json)
        .ok()
        .map(|i| i.recipe.doc)
}

/// 一括のジョブの処理。
pub struct BatchHandler {
    saver: Saver,
    engine: Arc<EngineServices>,
    /// GC の時計（あれば、終わったあとに候補があれば GC を積む）。
    gc: Option<gc::GcClock>,
    /// テストの強制終了：`(n, fault)` なら n 件コミットしたあとの次のコミットを `fault` で落とし、後始末をせずに失敗で戻る。
    #[cfg(test)]
    pub(crate) crash: Option<(usize, super::save::Fault)>,
    /// テストの割り込み：1 件をコミットする直前に、そのアイテムの ID で呼ぶ（土台の競合を起こす）。
    #[cfg(test)]
    pub(crate) before_commit: Option<Arc<dyn Fn(i64) + Send + Sync>>,
}

impl BatchHandler {
    /// このライブラリのエンジンの部品で当てる。
    pub fn new(engine: Arc<EngineServices>) -> Self {
        Self {
            saver: Saver::new(engine.clone()),
            engine,
            gc: None,
            #[cfg(test)]
            crash: None,
            #[cfg(test)]
            before_commit: None,
        }
    }

    /// 終わったあとに、GC の候補（`clock` で数える）があれば GC のジョブを積む。
    pub fn with_gc(mut self, clock: gc::GcClock) -> Self {
        self.gc = Some(clock);
        self
    }
}

/// 1 件の場（一括の 1 件）。
struct BatchUnit<'a> {
    ctx: &'a JobContext,
    ord: i64,
    durability: &'a dyn Durability,
}

impl UnitCx for BatchUnit<'_> {
    fn library(&self) -> &Library {
        self.ctx.library()
    }

    fn control(&self) -> Control {
        self.ctx.control()
    }

    fn progress(&self, _progress: f64, _message: &str) {
        // 1 件の中の進みは送らない（ジョブの進みは済んだ件数で送る）
    }

    fn log(&self, line: &str) {
        self.ctx.log(line);
    }

    fn mark(&self, result_json: &str, _progress: f64, _message: &str) -> Result<()> {
        let decoding = serde_json::from_str::<serde_json::Value>(result_json)
            .ok()
            .and_then(|v| v.get("decoding").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        let (job, ord) = (self.ctx.id(), self.ord);
        self.ctx.library().db().write(move |tx| {
            tx.prepare_cached(
                "UPDATE batch_items SET decoding = ?3 WHERE job_id = ?1 AND ord = ?2",
            )?
            .execute(params![job, ord, i64::from(decoding)])?;
            Ok(())
        })
    }

    fn job(&self) -> i64 {
        self.ctx.id()
    }

    fn version_job(&self) -> Option<i64> {
        None
    }

    fn tag(&self) -> String {
        format!("{}-{}", self.ctx.id(), self.ord)
    }

    fn sync_file(&self, file: &File) -> std::io::Result<()> {
        self.durability.sync_file(file)
    }
}

/// 1 件の仕事。
#[derive(Debug, Clone, Copy)]
struct Task {
    ord: i64,
    item_id: i64,
    /// 土台の競合でやり直した回数。
    attempt: u8,
}

/// 働き手の結果。
enum Outcome {
    /// 書き終えた（コミットを待つ）。
    Ready(Task, Box<Written>),
    /// 飛ばした（行の state と理由）。
    Finished {
        task: Task,
        state: &'static str,
        code: String,
        message: String,
    },
    /// 止める指示で止まった（行は pending のまま）。
    Stopped,
}

/// 積んだレシピと向き（働き手が読むだけ）。
struct Plan {
    recipe: Recipe,
    digest: Digest,
    mode: ModeInput,
    include_drafts: bool,
}

impl JobHandler for BatchHandler {
    fn run(&self, ctx: &JobContext) -> Result<JobEnd> {
        let input: BatchInput = serde_json::from_str(ctx.input())?;
        if input.v != INPUT_V {
            return Err(CoreError::Invalid(format!(
                "一括のジョブの入力の版 {} を知りません",
                input.v
            )));
        }
        let db = ctx.library().db();
        let job = ctx.id();
        let runtime = self.engine.runtime()?.clone();
        // レシピ（積んだときの写し）
        let graph = {
            let doc = input.recipe.doc;
            let doc_id = db
                .read(|c| store::doc_id_by_digest(c, &doc))?
                .ok_or_else(|| CoreError::NotFound(format!("レシピの文書 {doc}")))?;
            let bytes = db.read(|c| store::load_doc_bytes(c, doc_id))?;
            kura_engine::doc::load(&bytes)
                .map_err(|e| from_engine("レシピの文書を読めません", e))?
        };
        let recipe = Recipe {
            name: input.recipe.name.clone(),
            description: String::new(),
            kind: RecipeKind::Each,
            graph,
            bindings: input.recipe.bindings.clone(),
        };
        runtime
            .preview(|e| e.check_recipe(&recipe))
            .map_err(|e| from_engine("このレシピは当てられません", e))?;
        let plan = Plan {
            recipe,
            digest: input.recipe.doc,
            mode: input.mode.clone(),
            include_drafts: input.include_drafts,
        };
        // 前回 OS のデコーダで読んでいる途中で落ちた行
        let now = now_ms();
        db.write(move |tx| {
            tx.prepare_cached(
                "UPDATE batch_items SET state = 'failed', code = 'Crashed', decoding = 0, finished_at = ?2,
                        message = '前回、元を読んでいる途中でアプリが終わったので飛ばしました'
                  WHERE job_id = ?1 AND state = 'pending' AND decoding = 1",
            )?
            .execute(params![job, now])?;
            Ok(())
        })?;
        let pending: Vec<Task> = db.read(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT ord, item_id FROM batch_items WHERE job_id = ?1 AND state = 'pending' ORDER BY ord",
            )?;
            let rows = stmt.query_map([job], |r| {
                Ok(Task {
                    ord: r.get(0)?,
                    item_id: r.get(1)?,
                    attempt: 0,
                })
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let total: i64 = db.read(|c| {
            Ok(
                c.prepare_cached("SELECT count(*) FROM batch_items WHERE job_id = ?1")?
                    .query_row([job], |r| r.get(0))?,
            )
        })?;
        let mut done = total - pending.len() as i64;
        let end = self.process(ctx, &plan, pending, total, &mut done)?;
        match end {
            Stop::Paused => return Ok(JobEnd::Paused),
            Stop::Yield => return Ok(JobEnd::Yield),
            Stop::Canceled => {
                let now = now_ms();
                db.write(move |tx| cancel_rows(tx, job, now))?;
                let result = summary(db, job)?;
                return Ok(JobEnd::Canceled {
                    result_json: Some(result.to_string()),
                });
            }
            Stop::Finished => {}
        }
        if let Some(clock) = &self.gc
            && let Err(err) = gc::queue_if_due(ctx.library(), clock, || {
                ctx.ensure_queued(JobKind::Gc, "{}")
            })
        {
            tracing::warn!(%err, "GC のジョブを積めませんでした");
        }
        let result = summary(db, job)?;
        let applied = count_of(&result, "saved") + count_of(&result, "unchanged");
        let message = format!("{applied} 件に当てました");
        ctx.log(&message);
        Ok(JobEnd::Done {
            result_json: result.to_string(),
            message: Some(message),
        })
    }

    fn canceled_while_idle(&self, tx: &Connection, job: JobId, _input_json: &str) -> Result<()> {
        cancel_rows(tx, job, now_ms())
    }
}

/// 止め方。
enum Stop {
    Finished,
    Paused,
    Yield,
    Canceled,
}

/// 残りの pending の行を canceled にする。
fn cancel_rows(tx: &Connection, job: i64, now: i64) -> Result<()> {
    tx.prepare_cached(
        "UPDATE batch_items SET state = 'canceled', decoding = 0, finished_at = ?2
          WHERE job_id = ?1 AND state = 'pending'",
    )?
    .execute(params![job, now])?;
    Ok(())
}

/// 結果（状態ごとの件数）。
fn summary(db: &Db, job: i64) -> Result<serde_json::Value> {
    let rows: Vec<(String, i64)> = db.read(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT state, count(*) FROM batch_items WHERE job_id = ?1 GROUP BY state",
        )?;
        let rows = stmt.query_map([job], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    })?;
    let mut counts = serde_json::Map::new();
    for state in [
        "saved",
        "unchanged",
        "existing",
        "skipped",
        "failed",
        "canceled",
        "pending",
    ] {
        let n = rows.iter().find(|(s, _)| s == state).map_or(0, |(_, n)| *n);
        counts.insert(state.to_string(), json!(n));
    }
    Ok(json!({ "v": 1, "counts": counts }))
}

fn count_of(result: &serde_json::Value, state: &str) -> i64 {
    result["counts"][state].as_i64().unwrap_or(0)
}

/// 行を終わりの状態にする（飛ばした・失敗した・同じ中身。pending のときだけ）。
fn finish_row(db: &Db, job: i64, ord: i64, state: &str, code: &str, message: &str) -> Result<()> {
    let (state, code, message, now) = (
        state.to_string(),
        code.to_string(),
        message.to_string(),
        now_ms(),
    );
    db.write(move |tx| {
        tx.prepare_cached(
            "UPDATE batch_items SET state = ?3, code = ?4, message = ?5, decoding = 0, finished_at = ?6
              WHERE job_id = ?1 AND ord = ?2 AND state = 'pending'",
        )?
        .execute(params![job, ord, state, code, message, now])?;
        Ok(())
    })
}

/// コミットの記録：版と同じトランザクションで行を saved・unchanged にし、版・別名で作ったアイテム・土台・時刻を書く（行が
/// pending でなければ Conflict で戻す：対象のアイテムが消えて行がない、など）。
fn row_record(job: i64, ord: i64, base: Base, save_as: bool) -> Record {
    Arc::new(move |tx, outcome| {
        let state = match outcome["outcome"].as_str() {
            Some("unchanged") => "unchanged",
            _ => "saved",
        };
        let version = outcome["versionId"].as_i64();
        let out_item = if save_as {
            outcome["itemId"].as_i64()
        } else {
            None
        };
        let n = tx
            .prepare_cached(
                "UPDATE batch_items SET state = ?3, base_hash = ?4, base_version = ?5, version_id = ?6,
                        out_item_id = ?7, decoding = 0, code = NULL, message = NULL, finished_at = ?8
                  WHERE job_id = ?1 AND ord = ?2 AND state = 'pending'",
            )?
            .execute(params![
                job,
                ord,
                state,
                &base.hash[..],
                base.version,
                version,
                out_item,
                now_ms()
            ])?;
        if n != 1 {
            return Err(CoreError::Conflict(
                "一括の対象の行がありません（アイテムが消えました）".into(),
            ));
        }
        Ok(())
    })
}

impl BatchHandler {
    /// 働き手に配り、書き終えたものをまとまりごとにコミットする。
    fn process(
        &self,
        ctx: &JobContext,
        plan: &Plan,
        pending: Vec<Task>,
        total: i64,
        done: &mut i64,
    ) -> Result<Stop> {
        let db = ctx.library().db();
        let job = ctx.id();
        let durability = self.engine.durability();
        let k = self.engine.runtime()?.bulk_threads().max(1);
        let mut queue: VecDeque<Task> = pending.into();
        let report = |done: i64| {
            let frac = if total > 0 {
                done as f64 / total as f64
            } else {
                1.0
            };
            ctx.progress(frac, &format!("当てています（{done} / {total}）"));
        };
        report(*done);
        let (task_tx, task_rx) = mpsc::channel::<Task>();
        let task_rx = Mutex::new(task_rx);
        let (out_tx, out_rx) = mpsc::channel::<Outcome>();
        let mut commits = 0usize;
        std::thread::scope(|s| -> Result<Stop> {
            for _ in 0..k {
                let (task_rx, out_tx, durability) = (&task_rx, out_tx.clone(), durability.as_ref());
                s.spawn(move || {
                    loop {
                        let task = match task_rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => return,
                        };
                        let Ok(task) = task else { return };
                        let unit = BatchUnit {
                            ctx,
                            ord: task.ord,
                            durability,
                        };
                        let outcome = self.prepare_one(&unit, plan, task);
                        if out_tx.send(outcome).is_err() {
                            return;
                        }
                    }
                });
            }
            drop(out_tx);
            let mut in_flight = 0usize;
            let mut ready: Vec<(Task, Box<Written>)> = Vec::new();
            let mut group_since: Option<Instant> = None;
            let mut stop: Option<Stop> = None;
            loop {
                // 配る（止める指示がなければ）
                while stop.is_none() && in_flight < k {
                    let Some(task) = queue.pop_front() else { break };
                    if task_tx.send(task).is_err() {
                        break;
                    }
                    in_flight += 1;
                }
                let drained = in_flight == 0 && (queue.is_empty() || stop.is_some());
                // まとまりのコミット
                let due = !ready.is_empty()
                    && (ready.len() >= GROUP_ITEMS
                        || group_since.is_some_and(|t| t.elapsed() >= GROUP_TIME)
                        || drained
                        || matches!(stop, Some(Stop::Paused | Stop::Yield)));
                if due && !matches!(stop, Some(Stop::Canceled)) {
                    let group = std::mem::take(&mut ready);
                    group_since = None;
                    let retry = self.commit_group(
                        ctx,
                        plan,
                        group,
                        durability.as_ref(),
                        done,
                        &mut commits,
                    )?;
                    report(*done);
                    for task in retry {
                        queue.push_front(task);
                    }
                    continue;
                }
                if drained && ready.is_empty() {
                    break;
                }
                if drained && matches!(stop, Some(Stop::Canceled)) {
                    // 書き終えた分も落とす（staging を消す）
                    ready.clear();
                    break;
                }
                match out_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(outcome) => {
                        in_flight = in_flight.saturating_sub(1);
                        match outcome {
                            Outcome::Ready(task, written) => {
                                if matches!(stop, Some(Stop::Canceled)) {
                                    drop(written);
                                } else {
                                    group_since.get_or_insert_with(Instant::now);
                                    ready.push((task, written));
                                }
                            }
                            Outcome::Finished {
                                task,
                                state,
                                code,
                                message,
                            } => {
                                finish_row(db, job, task.ord, state, &code, &message)?;
                                *done += 1;
                                report(*done);
                            }
                            Outcome::Stopped => {}
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if stop.is_none() {
                    stop = match ctx.control() {
                        Control::Pause => Some(Stop::Paused),
                        Control::Cancel => Some(Stop::Canceled),
                        Control::Continue if ctx.should_yield() => Some(Stop::Yield),
                        Control::Continue => None,
                    };
                }
            }
            drop(task_tx);
            Ok(stop.unwrap_or(Stop::Finished))
        })
    }

    /// まとまり 1 つ：区切りの同期を 1 回取り、1 件ずつコミットする。土台の競合で 1 回目のものは、やり直す仕事として返す。
    fn commit_group(
        &self,
        ctx: &JobContext,
        plan: &Plan,
        group: Vec<(Task, Box<Written>)>,
        durability: &dyn Durability,
        done: &mut i64,
        commits: &mut usize,
    ) -> Result<Vec<Task>> {
        let db = ctx.library().db();
        let job = ctx.id();
        // 区切りの同期（書き終えた中身をドライブのキャッシュから流してから、それを指す行をコミットする）
        if let Some((_, w)) = group.first() {
            File::open(w.part())
                .and_then(|f| durability.barrier(&f))
                .map_err(|e| {
                    CoreError::Io(std::io::Error::other(format!(
                        "区切りの同期に失敗しました：{e}"
                    )))
                })?;
        }
        let save_as = matches!(plan.mode, ModeInput::SaveAs { .. });
        let mut retry = Vec::new();
        let mut saved: Vec<MediaKind> = Vec::new();
        for (task, written) in group {
            let unit = BatchUnit {
                ctx,
                ord: task.ord,
                durability,
            };
            let base = written.base();
            #[cfg(test)]
            let crash_saver;
            #[cfg(test)]
            let saver = match self.crash {
                Some((n, fault)) if *commits == n => {
                    crash_saver = Saver::new(self.engine.clone()).with_fault(fault);
                    &crash_saver
                }
                _ => &self.saver,
            };
            #[cfg(not(test))]
            let saver = &self.saver;
            #[cfg(test)]
            if let Some(hook) = &self.before_commit {
                hook(task.item_id);
            }
            match saver.commit(&unit, *written, row_record(job, task.ord, base, save_as)) {
                Ok(Committed {
                    saved: Some(kind), ..
                }) => {
                    saved.push(kind);
                    *commits += 1;
                    *done += 1;
                }
                Ok(Committed {
                    saved: None,
                    outcome,
                    message,
                }) => {
                    // 同じ中身のアイテムがある（版を足さない）
                    let _ = outcome;
                    finish_row(db, job, task.ord, "existing", "Existing", &message)?;
                    *done += 1;
                }
                Err(CoreError::Conflict(why)) if task.attempt == 0 => {
                    ctx.log(&format!(
                        "アイテム {} は土台が変わったので、読み直してやり直します：{why}",
                        task.item_id
                    ));
                    retry.push(Task { attempt: 1, ..task });
                }
                Err(e) => {
                    #[cfg(test)]
                    if self.crash.is_some_and(|(n, _)| *commits == n) {
                        // テストの強制終了：後始末をせずに戻る
                        return Err(e);
                    }
                    let state = if matches!(e, CoreError::Conflict(_)) {
                        "skipped"
                    } else {
                        "failed"
                    };
                    finish_row(db, job, task.ord, state, e.code(), &e.to_string())?;
                    *done += 1;
                }
            }
        }
        if !saved.is_empty() {
            queue_follow_ups(ctx, &saved);
        }
        Ok(retry)
    }

    /// 働き手の 1 件：飛ばす決まりを見て、継ぎ足して結び付け、準備する。
    fn prepare_one(&self, unit: &BatchUnit<'_>, plan: &Plan, task: Task) -> Outcome {
        match self.try_prepare(unit, plan, task) {
            Ok(outcome) => outcome,
            Err(Skip {
                state,
                code,
                message,
            }) => Outcome::Finished {
                task,
                state,
                code,
                message,
            },
        }
    }

    fn try_prepare(
        &self,
        unit: &BatchUnit<'_>,
        plan: &Plan,
        task: Task,
    ) -> std::result::Result<Outcome, Skip> {
        let ctx = unit.ctx;
        let db = ctx.library().db();
        let item = task.item_id;
        let failed = |e: CoreError| Skip::failed(&e);
        // 1. 飛ばす
        let head = db
            .read(|c| versions::head(c, item))
            .map_err(failed)?
            .ok_or_else(|| Skip::skipped("Gone", "アイテムがありません".into()))?;
        if head.trashed {
            return Err(Skip::skipped(
                "Trashed",
                "ゴミ箱にあるので飛ばしました".into(),
            ));
        }
        if !matches!(head.kind, MediaKind::Image | MediaKind::Audio) {
            return Err(Skip::skipped(
                "Kind",
                format!(
                    "{} には当てられません（画像と音声だけ）",
                    head.kind.as_str()
                ),
            ));
        }
        if self
            .engine
            .current_editor()
            .is_some_and(|e| e.item_id() == item)
        {
            return Err(Skip::skipped(
                "Open",
                "エディタで開いているので飛ばしました".into(),
            ));
        }
        if db
            .read(|c| pending_edit_job(c, item))
            .map_err(failed)?
            .is_some()
        {
            return Err(Skip::skipped(
                "SavePending",
                "前の保存が終わっていないので飛ばしました".into(),
            ));
        }
        let overwrite = matches!(plan.mode, ModeInput::Overwrite);
        if overwrite
            && !plan.include_drafts
            && db.read(|c| drafts::get(c, item)).map_err(failed)?.is_some()
        {
            return Err(Skip::skipped(
                "Draft",
                "保存していない下書きがあるので飛ばしました".into(),
            ));
        }
        // 2. 継ぎ足して結び付ける（土台は今の頭）
        let base = Base {
            hash: head.hash,
            version: head.version,
        };
        let runtime = self.engine.runtime().map_err(failed)?.clone();
        let head_doc = runtime
            .preview(|e| db.read(|c| versions::doc_of(c, e, item, head.version)))
            .map_err(failed)?;
        let shapes = if plan.recipe.bindings.is_empty() {
            BTreeMap::new()
        } else {
            match self.input_shapes(unit, item, &head_doc) {
                Ok(Flow::Go(s)) => s,
                Ok(Flow::Stop(_)) => return Ok(Outcome::Stopped),
                Err(e) => return Err(failed(e)),
            }
        };
        let facts = db.read(|c| facts_of(c, item)).map_err(failed)?;
        let instance = runtime
            .preview(|e| e.instantiate(&head_doc, &plan.recipe, &shapes, &facts))
            .map_err(|e| match e {
                BindError::Recipe(_) | BindError::Invalid(_) => Skip {
                    state: "failed",
                    code: "Invalid".into(),
                    message: e.to_string(),
                },
                other => Skip::skipped("NotApplicable", other.to_string()),
            })?;
        let mut graph = instance.graph;
        let bound: serde_json::Value = bound_json(&instance.bound)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(serde_json::Value::Null);
        let record = json!({
            "job": ctx.id(),
            "recipe": { "name": plan.recipe.name, "digest": plan.digest },
            "bound": bound,
        });
        // 3. 準備
        let names = match &plan.mode {
            ModeInput::Overwrite => None,
            ModeInput::SaveAs { name } => {
                let title: String = db
                    .read(|c| {
                        Ok(c.prepare_cached("SELECT title FROM items WHERE id = ?1")?
                            .query_row([item], |r| r.get(0))?)
                    })
                    .map_err(failed)?;
                Some(save_as_names(name, &title))
            }
        };
        let mode = if overwrite {
            Mode::Overwrite
        } else {
            Mode::SaveAs
        };
        let attempt = |graph: &Graph| {
            self.saver.prepare_graph(
                unit,
                mode,
                item,
                base,
                graph,
                GraphExtra {
                    names: names.clone(),
                    batch: Some(record.clone()),
                },
            )
        };
        let prepared = match attempt(&graph) {
            Err(CoreError::Unsupported(why)) if !overwrite => {
                // 別名は代わりの形式で（ADR-190 (6)）
                let fallbacks: &[&str] = match head.kind {
                    MediaKind::Audio => &["ogg", "wav"],
                    _ => &["png"],
                };
                let mut last = Err(CoreError::Unsupported(why));
                for format in fallbacks {
                    graph.outputs[0].format = FormatChoice::To {
                        format: (*format).to_string(),
                        options: json!({}),
                    };
                    graph.outputs[0].quantize = None;
                    last = attempt(&graph);
                    if !matches!(last, Err(CoreError::Unsupported(_))) {
                        break;
                    }
                }
                last
            }
            other => other,
        };
        match prepared {
            Ok(Flow::Go(w)) => Ok(Outcome::Ready(task, Box::new(w))),
            Ok(Flow::Stop(_)) => Ok(Outcome::Stopped),
            Err(CoreError::Unsupported(why)) => Err(Skip::skipped("FormatUnavailable", why)),
            Err(e) => Err(failed(e)),
        }
    }

    /// 結び付けのための入力の形（頭の文書の入力 0 ＝ 元の見出しから。OS のデコーダを通りうるなら、読む前後に行の印を置く）。
    fn input_shapes(
        &self,
        unit: &BatchUnit<'_>,
        item: i64,
        head_doc: &Graph,
    ) -> Result<Flow<BTreeMap<InputId, Shape>>> {
        let library = unit.ctx.library();
        let original = library
            .db()
            .read(|c| versions::original(c, item))?
            .ok_or_else(|| CoreError::NotFound(format!("アイテム {item}")))?;
        let format = format_id(&original.ext).ok_or_else(|| {
            CoreError::Unsupported(format!("元の形式（{}）を読めません", original.ext))
        })?;
        let opts = head_doc
            .inputs
            .first()
            .map(|slot| slot.decode.clone())
            .unwrap_or_default();
        let path = library.layout().content_path(&original.hash, &original.ext);
        let codecs = self.engine.codecs();
        let runtime = self.engine.runtime()?.clone();
        let limits = runtime.bulk(|e| e.limits().clone());
        let platform = codecs
            .decoders_for(format)
            .iter()
            .any(|d| !d.reproducible());
        let mon = UnitMonitor(unit);
        if platform {
            unit.mark(r#"{"v":1,"decoding":true}"#, 0.0, "元を読んでいます")?;
        }
        let shape = codecs.shape_path_first(&path, format, &opts, &limits, &mon);
        if platform {
            unit.mark(r#"{"v":1,"decoding":false}"#, 0.0, "元を読みました")?;
        }
        match shape {
            Ok((shape, _)) => Ok(Flow::Go(BTreeMap::from([(InputId(0), shape)]))),
            Err(e) if matches!(e.root(), kura_engine::EngineError::Canceled) => {
                Ok(Flow::Stop(JobEnd::Paused))
            }
            Err(e) => Err(from_engine("元の大きさを読めません", e)),
        }
    }
}

/// 飛ばす・失敗の理由。
struct Skip {
    state: &'static str,
    code: String,
    message: String,
}

impl Skip {
    fn skipped(code: &str, message: String) -> Self {
        Self {
            state: "skipped",
            code: code.to_string(),
            message,
        }
    }

    fn failed(e: &CoreError) -> Self {
        Self {
            state: "failed",
            code: e.code().to_string(),
            message: e.to_string(),
        }
    }
}

/// 素材の設定から結び付けの値（格子は `count` を除く。ピボット・試聴のループ・コマの時間）。
fn facts_of(conn: &Connection, item: i64) -> Result<Facts> {
    let settings = crate::asset::get(conn, item)?.settings;
    Ok(Facts {
        grid: settings.grid.map(|g| GridParam {
            cell_w: g.cell_w,
            cell_h: g.cell_h,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            gap_x: g.gap_x,
            gap_y: g.gap_y,
        }),
        pivot: settings.pivot.map(|p| PivotFact { x: p.x, y: p.y }),
        loop_range: settings
            .loop_range
            .map(|(start, end)| FrameRange { start, end }),
        frame_ms: settings.frame_ms,
    })
}

/// 別名の (表示名, 語幹)：名前の型の [`NAME_PLACEHOLDER`] にアイテムの表示名を入れる（語幹は同じ名前の「/」を「_」に。空なら
/// 「コピー」）。エディタの別名で保存と同じく、表示名とファイル名の語幹を同じにする。
pub fn save_as_names(template: &str, title: &str) -> (String, String) {
    let name = nfc(template.replace(NAME_PLACEHOLDER, title).trim());
    let name = if name.is_empty() {
        "コピー".to_string()
    } else {
        name
    };
    let stem = name.replace(['/', '\0'], "_");
    (name, stem)
}

/// まとまりのコミットのあとに、後のジョブを積む（中身が変わったのでサムネイル、画像・音声は解析、音声はプロキシ）。積めなくても
/// 一括は続ける（開き直すときにも積む）。
fn queue_follow_ups(ctx: &JobContext, kinds: &[MediaKind]) {
    let mut follow = vec![JobKind::Thumbnail];
    if kinds
        .iter()
        .any(|k| matches!(k, MediaKind::Image | MediaKind::Audio))
    {
        follow.push(JobKind::Analyze);
    }
    if kinds.iter().any(|k| k.needs_proxy()) {
        follow.push(JobKind::Proxy);
    }
    for kind in follow {
        if let Err(err) = ctx.ensure_queued(kind, "{}") {
            tracing::warn!(%err, kind = kind.as_str(), "一括のあとのジョブを積めませんでした");
        }
    }
}
