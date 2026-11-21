//! 一括加工のジョブ（`JobKind::Batch`。M14 段 4。docs/editing.md §4.3・§14「一括加工」、docs/data-model.md §4「一括加工（M14）」、
//! ADR-190・192・193）。
//!
//! 1 本のジョブで、選んだアイテムに 1 つのレシピを 1 件ずつ当てる。対象と結果は `batch_items` の行（積むのと同じトランザクションで
//! pending の行を入れる）。1 件の流れ：
//! 1. 飛ばす（働き手に渡す前に。行は `skipped` とその理由）：ゴミ箱にある、エディタで開いている、前の保存が終わっていない、上書きで
//!    保存していない下書きがある（`includeDrafts` が偽のとき）、画像・音声でない。前回 OS のデコーダで読んでいる途中で落ちた行
//!    （`decoding = 1`）は、同じファイルで落ち続けないよう `failed`（`Crashed`）にする。
//! 2. 継ぎ足し：頭の文書（版がなければ空の文書）にレシピをつなぎ、素材の設定（格子・ピボット・試聴のループ・コマの時間）と秒で
//!    結び付ける（`Engine::instantiate`。一括のプールで。結び付けがあるときだけ、保存と同じ読み方で入力の形を読む。MP3・AAC の
//!    終わりからの秒は流して数えた長さで）。値がない・当てられない手は `skipped`。土台は処理するときの頭（ADR-190 (9)）。
//! 3. 準備（k 本の働き手。k は一括のプールのスレッドの数）：`Saver::prepare_graph`（文書は DB に置かず、部品はトランザクション 2 で
//!    置く）。1 件の場 [`BatchUnit`]：止める指示はジョブの、読む前の印は行の `decoding`、版の持ち主は空（`batch_items.version_id`
//!    で引く。ADR-192 (1)）、書きかけの印は `{ジョブ}-{順}`、ファイルの同期は fsync（`Durability::sync_file`）。書き手のない形式
//!    （SVG・aseprite・ffmpeg のない MP3 など）は上書きなら `skipped`、別名では代わりの形式（画像は PNG、音声は Ogg Vorbis か WAV）
//!    で書く（元を読めないときは替えずに `failed`）。準備の途中の土台の競合は 1 回だけ読み直してやり直す。
//! 4. コミット（ジョブのスレッドで直列、まとまりごと）：書き終えた [`GROUP_ITEMS`] 件か [`GROUP_TIME`] がたまったら、区切りの同期
//!    （`Durability::barrier`。macOS では F_FULLFSYNC）を 1 回取ってから、1 件ずつ `Saver::commit` する。記録（`Record`）が同じ
//!    トランザクションで行を `saved`・`unchanged` にし、版・別名で作ったアイテム・土台・時刻を書く（版と行が同じトランザクション
//!    なので、落ちても二重にも抜けにもならない）。同じ中身のアイテムがあれば `existing`（上書きで別のアイテムの今と同じも）。
//!    土台の競合は 1 回だけ頭を読み直して
//!    やり直し、だめなら `skipped`（Conflict）。ほかの誤りは `failed`。まとまりごとに、後のジョブ（サムネイル・解析・プロキシ）を積む。
//!
//! 止め方：一時停止・譲るは新しく渡さず、書き終えた分をコミットしてから止まる（準備中のものは落とし、行は pending のまま）。中止は
//! 見たときにすぐ残りの pending の行を `canceled` にし、書き終えた分も落とす（済んだ分は残す。開き直して canceled の行があれば
//! 中止で終える）。指示は回ごとに読み直し、中止＞一時停止＞譲る（ADR-194）。強制終了は、開く順の staging の掃除と予告の
//! 照合のあと、pending の行から続ける。区切りの同期をまとまりのコミットの前に取るので、電源断で DB の最後のコミットを失っても
//! 「DB にないファイル」が残るだけで、DB が永続になっていない中身を指すことはない（失った行は pending に戻ってやり直す）。

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kura_engine::audio::stream::StreamEnd;
use kura_engine::budget::Class;
use kura_engine::codec::stream::{DecodeInfo, DecodeSink};
use kura_engine::codec::{Tier, anchor};
use kura_engine::doc::{DecodeOptions, FormatChoice, Graph, InputId};
use kura_engine::ops::audio::set_loop::FrameRange;
use kura_engine::ops::util::GridParam;
use kura_engine::recipe::{BindError, Binding, Facts, PivotFact, Recipe, RecipeKind, bound_json};
use kura_engine::{Digest, Dim, EngineError, Monitor, Shape};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::save::{
    Base, Committed, Flow, GraphExtra, Mode, Record, Saver, UnitCx, UnitMonitor, Written,
    pending_edit_job, work_keys,
};
use super::services::Durability;
use super::work_pcm::WorkKey;
use super::{
    EngineRuntime, EngineServices, anchors, drafts, from_engine, gc, recipes, store, versions,
};
use crate::db::Db;
use crate::import::{MediaKind, format_id};
use crate::jobs::{Control, JobContext, JobEnd, JobHandler, JobId, JobKind, JobManager};
use crate::library::Library;
use crate::text::nfc;
use crate::time::now_ms;
use crate::{CoreError, Result};

pub mod merge;
pub mod precheck;
pub mod results;
pub mod undo;

pub use merge::{DEFAULT_SHEET_NAME, MergeRequest, start_merge};
pub use precheck::{Precheck, PrecheckCx, precheck, precheck_merge};
pub use results::{BatchAction, BatchResult, BatchRow, result, rows};
pub use undo::start_undo;

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

/// 当てるレシピの出どころ（[`BatchRequest::source`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeSource {
    /// レシピの表（`edit_recipes`）のレシピ。
    Recipe(i64),
    /// 一括のジョブを積んだときのレシピの写し（結果のシートの［やり直す］。レシピをあとで直した・消しても、その一括と同じ
    /// 文書と結び付けで当てる。M14 段 5）。
    Job(JobId),
}

/// 一括の頼み（[`start_batch`]・[`precheck::precheck`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRequest {
    pub source: RecipeSource,
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
    /// 当てるレシピの写し（元に戻す一括は持たない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipe: Option<RecipeInput>,
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
    SaveAs {
        name: String,
    },
    /// 一括の［元に戻す…］（M14 段 5。[`undo`]）：一括 `of` の前の版へ戻す（上書き）、作ったアイテムをゴミ箱へ（別名・N → 1）。
    Undo {
        of: JobId,
        save_as: bool,
        /// 元の一括が N → 1（作ったアイテムは版を持たない。M14 段 6）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        merge: bool,
    },
    /// N → 1（M14 段 6。[`merge`]）：選んだアイテムから 1 枚のスプライトシートを作る（`name` は名前の型）。
    Merge {
        name: String,
        sheet: kura_engine::selection::SheetParams,
    },
}

/// 出どころから読んだレシピ（積む・事前チェックが使う）。
pub(crate) struct Resolved {
    pub(crate) recipe: Recipe,
    /// 文書の根。
    pub(crate) doc: Digest,
    /// 今の版でない操作のノードの印（docs/editing.md §8.4）。
    pub(crate) notes: Vec<kura_engine::recipe::RecipeNote>,
    /// 積むトランザクションの中で照らすもの。
    recheck: Recheck,
}

/// 積むトランザクションの中で照らすもの。
enum Recheck {
    /// レシピが読んだときから変わっていない（[`recipes::unchanged`]）。
    Recipe {
        id: i64,
        stored: Box<recipes::StoredRecipe>,
    },
    /// 写しの文書の根がまだある（終わった一括の写しは GC の根でないので、レシピを消したあとは片付けられうる）。
    Copy,
}

/// レシピの出どころを読む：レシピの表なら確かめて写し、一括の写しならその文書と結び付けを読んで確かめる。当てられない理由が
/// ある・N → 1 のレシピ・元に戻す一括は Invalid、ないものは NotFound。
pub(crate) fn resolve(db: &Db, runtime: &EngineRuntime, source: RecipeSource) -> Result<Resolved> {
    match source {
        RecipeSource::Recipe(id) => {
            let stored = recipes::get(db, runtime, id)?;
            if let Some(why) = &stored.problem {
                return Err(CoreError::Invalid(format!(
                    "このレシピは当てられません：{why}"
                )));
            }
            if stored.recipe.kind != RecipeKind::Each {
                return Err(CoreError::Invalid(
                    "まとめて 1 つにするレシピは、1 件ずつの一括では当てられません".into(),
                ));
            }
            let doc = db.read(|c| recipes::doc_root(c, id))?;
            Ok(Resolved {
                recipe: stored.recipe.clone(),
                doc,
                notes: stored.notes.clone(),
                recheck: Recheck::Recipe {
                    id,
                    stored: Box::new(stored),
                },
            })
        }
        RecipeSource::Job(of) => {
            let input = input_of(db, of)?;
            let Some(copy) = input.recipe else {
                return Err(CoreError::Invalid(
                    "元に戻した一括は、やり直せません".into(),
                ));
            };
            let recipe = load_copy(db, &copy)?;
            let notes = runtime
                .bulk(|e| e.check_recipe(&recipe))
                .map_err(|e| from_engine("このレシピは当てられません", e))?;
            Ok(Resolved {
                recipe,
                doc: copy.doc,
                notes,
                recheck: Recheck::Copy,
            })
        }
    }
}

/// 一括のジョブ `job` の入力（ないジョブは NotFound、一括でなければ Invalid）。
fn input_of(db: &Db, job: JobId) -> Result<BatchInput> {
    let (kind, input_json): (String, String) = db
        .read(|c| {
            Ok(
                c.prepare_cached("SELECT kind, input_json FROM jobs WHERE id = ?1")?
                    .query_row([job], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?,
            )
        })?
        .ok_or_else(|| CoreError::NotFound(format!("ジョブ {job}")))?;
    if kind != JobKind::Batch.as_str() {
        return Err(CoreError::Invalid(format!(
            "ジョブ {job} は一括加工ではありません"
        )));
    }
    let input: BatchInput = serde_json::from_str(&input_json)?;
    if input.v != INPUT_V {
        return Err(CoreError::Invalid(format!(
            "一括のジョブの入力の版 {} を知りません",
            input.v
        )));
    }
    Ok(input)
}

/// 一括の入力のレシピの写しを読む（文書の根から文書を読み、名前と結び付けを付ける）。文書の根がもうなければ NotFound。
fn load_copy(db: &Db, copy: &RecipeInput) -> Result<Recipe> {
    let doc = copy.doc;
    let doc_id = db
        .read(|c| store::doc_id_by_digest(c, &doc))?
        .ok_or_else(|| CoreError::NotFound(format!("レシピの文書 {doc}")))?;
    let bytes = db.read(|c| store::load_doc_bytes(c, doc_id))?;
    let graph =
        kura_engine::doc::load(&bytes).map_err(|e| from_engine("レシピの文書を読めません", e))?;
    Ok(Recipe {
        name: copy.name.clone(),
        description: String::new(),
        kind: RecipeKind::Each,
        graph,
        bindings: copy.bindings.clone(),
    })
}

/// 頼みの対象（選んだ順。重なりは前のものだけ残す）。
pub(crate) fn dedup(items: &[i64]) -> Vec<i64> {
    let mut seen = std::collections::BTreeSet::new();
    items
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .collect()
}

/// 一括のジョブを積む。レシピ（`edit_recipes`。［やり直す］は一括のジョブの写し）を確かめて、文書の根と結び付けを入力に写し、
/// 同じトランザクションで対象の行を選んだ順に入れる（ないアイテムは入れない）。エディタで開いているアイテムは、積んだ時点で
/// `skipped`（Open）にする（開いている間に保存できるように。あとから開こうとするアイテムは、一括が済むまで開かない：
/// [`super::save::pending_save`]）。レシピに当てられない理由がある・N → 1 のレシピ・元に戻した一括の写し・対象がない・別名の
/// 名前の型が空なら Invalid。読んでから積むまでにレシピが差し替え・名前を変える・消すで変わったら Conflict・NotFound（積む
/// トランザクションの中で照らす。入力の写しが古い結び付けと新しい文書の組にならず、文書の根が片付けられる前に根になる）。写しの
/// 文書の根が片付けられていれば NotFound。開いているアイテムは、エディタの置き場の錠を持ったまま読んで行を入れる（開く途中の
/// エディタと重ならない。エディタは置いたあとに確かめ直す。M14 段 4 の検証の直し）。
pub fn start_batch(
    db: &Db,
    jobs: &JobManager,
    engine: &EngineServices,
    req: &BatchRequest,
) -> Result<JobId> {
    let runtime = engine.runtime()?.clone();
    let Resolved {
        recipe,
        doc,
        recheck,
        ..
    } = resolve(db, &runtime, req.source)?;
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
    let items = dedup(&req.items);
    if items.is_empty() {
        return Err(CoreError::Invalid("当てるアイテムがありません".into()));
    }
    let input = BatchInput {
        v: INPUT_V,
        recipe: Some(RecipeInput {
            doc,
            name: recipe.name,
            bindings: recipe.bindings,
        }),
        mode,
        include_drafts: req.include_drafts,
    };
    let input_json = serde_json::to_string(&input)?;
    let now = now_ms();
    let id = engine.with_open_item(|open| {
        jobs.enqueue_then(JobKind::Batch, input_json, move |tx, job| {
            match &recheck {
                Recheck::Recipe { id, stored } => {
                    if !recipes::unchanged(tx, *id, stored, &doc)? {
                        return Err(CoreError::Conflict(
                            "レシピが変わりました。もう一度当ててください".into(),
                        ));
                    }
                }
                Recheck::Copy => {
                    if store::doc_id_by_digest(tx, &doc)?.is_none() {
                        return Err(CoreError::NotFound(
                            "元の一括のレシピの文書がもうありません（レシピを消したあと片付けられました）"
                                .into(),
                        ));
                    }
                }
            }
            if insert_rows(tx, job, &items, open, now)? == 0 {
                return Err(CoreError::Invalid(
                    "当てるアイテムがありません（どれもライブラリにありません）".into(),
                ));
            }
            Ok(())
        })
    })?;
    Ok(id)
}

/// 対象の行を選んだ順に pending で入れる（ないアイテムは入れない。エディタで開いているアイテム `open` は skipped（Open））。
/// 入れた行の数を返す。
pub(crate) fn insert_rows(
    tx: &Connection,
    job: JobId,
    targets: &[i64],
    open: Option<i64>,
    now: i64,
) -> Result<i64> {
    let mut exists = tx.prepare_cached("SELECT 1 FROM items WHERE id = ?1")?;
    let mut insert = tx.prepare_cached(
        "INSERT INTO batch_items (job_id, ord, item_id, state, code, message, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let mut ord = 0i64;
    for item in targets {
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
    Ok(ord)
}

/// アイテム `item_id` を待っている（pending の行のある、終わっていない）一括のジョブ（`idx_batch_items_item`）。N → 1 は元を読むだけ
/// なので数えない（並べている元も開ける・保存できる。読むのはそのときの今の中身で、来歴に読んだ版と中身が残る。M14 段 6 の
/// 検証のあとの利用者の決定。ADR-198）。
pub fn pending_batch(conn: &Connection, item_id: i64) -> Result<Option<JobId>> {
    Ok(conn
        .prepare_cached(
            "SELECT b.job_id FROM batch_items b JOIN jobs j ON j.id = b.job_id
              WHERE b.item_id = ?1 AND b.state = 'pending' AND j.state IN ('queued', 'running', 'paused')
                AND json_extract(j.input_json, '$.mode.type') IS NOT 'merge'
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
        .and_then(|i| i.recipe)
        .map(|r| r.doc)
}

/// 一括のジョブの処理。
pub struct BatchHandler {
    saver: Saver,
    engine: Arc<EngineServices>,
    /// GC の時計（あれば、終わったあとに候補があれば GC を積む）。
    gc: Option<gc::GcClock>,
    /// 一時停止・譲るで止めた 1 件が残しうる作業用の PCM の鍵（ジョブごと）。待ち・一時停止のまま中止したら捨てる（保存の
    /// ジョブの M13 段 3 の直しと同じ。開き直したあとは空：残したものは期限・上限・閉じるときに消える。M14 段 4 の検証の直し）。
    kept: Mutex<BTreeMap<JobId, Vec<WorkKey>>>,
    /// テストの強制終了：`(n, fault)` なら n 件コミットしたあとの次のコミットを `fault` で落とし、後始末をせずに失敗で戻る。
    #[cfg(test)]
    pub(crate) crash: Option<(usize, super::save::Fault)>,
    /// テストの強制終了を効かせるか（None ならいつも。あとから構える試験が使う：前の一括は落とさず、戻す一括だけを落とす）。
    #[cfg(test)]
    pub(crate) crash_armed: Option<Arc<AtomicBool>>,
    /// テストの割り込み：1 件をコミットする直前に、そのアイテムの ID で呼ぶ（土台の競合・動いている間の一時停止・中止・譲る）。
    #[cfg(test)]
    pub(crate) before_commit: Option<Arc<dyn Fn(i64) + Send + Sync>>,
    /// テストの割り込み：働き手が飛ばす決まりを見たあと、継ぎ足す前に、そのアイテムの ID で呼ぶ（panic する 1 件・準備の途中で
    /// ゴミ箱に入れる）。
    #[cfg(test)]
    pub(crate) before_prepare: Option<Arc<dyn Fn(i64) + Send + Sync>>,
}

impl BatchHandler {
    /// このライブラリのエンジンの部品で当てる。
    pub fn new(engine: Arc<EngineServices>) -> Self {
        Self {
            saver: Saver::new(engine.clone()),
            engine,
            gc: None,
            kept: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            crash: None,
            #[cfg(test)]
            crash_armed: None,
            #[cfg(test)]
            before_commit: None,
            #[cfg(test)]
            before_prepare: None,
        }
    }

    /// 1 件ずつの保存の段ごとに `hook` を呼ぶ（テストの割り込み：作り直しの途中で止める）。
    #[cfg(test)]
    pub(crate) fn with_save_hook(mut self, hook: super::save::Hook) -> Self {
        self.saver.hook = Some(hook);
        self
    }

    /// テストの強制終了を効かせるか（[`Self::crash`]）。
    #[cfg(test)]
    fn crash_now(&self, commits: usize) -> Option<super::save::Fault> {
        match self.crash {
            Some((n, fault))
                if commits == n
                    && self
                        .crash_armed
                        .as_ref()
                        .is_none_or(|a| a.load(Ordering::SeqCst)) =>
            {
                Some(fault)
            }
            _ => None,
        }
    }

    /// 終わったあとに、GC の候補（`clock` で数える）があれば GC のジョブを積む。
    pub fn with_gc(mut self, clock: gc::GcClock) -> Self {
        self.gc = Some(clock);
        self
    }

    /// ジョブ `job` の前の回が残しうる作業用の PCM の鍵を取り出す。
    fn take_kept(&self, job: JobId) -> Vec<WorkKey> {
        self.kept
            .lock()
            .map(|mut kept| kept.remove(&job).unwrap_or_default())
            .unwrap_or_default()
    }
}

/// 1 件の場（一括の 1 件）。
struct BatchUnit<'a> {
    ctx: &'a JobContext,
    ord: i64,
    durability: &'a dyn Durability,
    /// 譲ると決めた（準備の途中のものも一時停止と同じに止める。行は pending のまま。docs/editing.md §14.2 の止め方）。
    yielding: &'a AtomicBool,
}

impl UnitCx for BatchUnit<'_> {
    fn library(&self) -> &Library {
        self.ctx.library()
    }

    fn control(&self) -> Control {
        match self.ctx.control() {
            Control::Continue if self.yielding.load(Ordering::Acquire) => Control::Pause,
            other => other,
        }
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

/// 読む前後の印（`BatchUnit::mark` が `decoding` を読む）。
const DECODING: &str = r#"{"v":1,"decoding":true}"#;
const DECODED: &str = r#"{"v":1,"decoding":false}"#;

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
    /// 働き手がそのままコミットした（元に戻す一括の、ファイルを書かない戻し・ゴミ箱へ。行は同じトランザクションで書いた）。
    /// 中身が変わったならその種類（後のジョブを積む）。
    Done { saved: Option<MediaKind> },
    /// 飛ばした・失敗した（行の state と理由）。
    Finished {
        task: Task,
        state: &'static str,
        code: String,
        message: String,
    },
    /// 準備の途中で土台が変わった（ゴミ箱に入れられたのを含む）：頭を読み直して 1 回だけやり直す。
    Retry(Task),
    /// 止める指示で止まった（行は pending のまま。一時停止・譲るで残しうる作業用の PCM の鍵）。
    Stopped(Vec<WorkKey>),
}

/// 積んだレシピと向き（働き手が読むだけ）。
struct Plan {
    /// 当てるレシピ（元に戻す一括は None）。
    recipe: Option<Applied>,
    mode: ModeInput,
    include_drafts: bool,
}

impl Plan {
    /// 保存していない下書きのあるアイテムを飛ばすか：上書きは含める指定がなければ（ADR-190 (8)）、元に戻す一括はいつも（下書きの
    /// 土台を黙って古くしない）。別名は元を変えないので飛ばさない。
    fn skips_drafts(&self) -> bool {
        match self.mode {
            ModeInput::Overwrite => !self.include_drafts,
            ModeInput::SaveAs { .. } | ModeInput::Merge { .. } => false,
            ModeInput::Undo { .. } => true,
        }
    }
}

/// 当てるレシピ（積んだときの写し）と文書の根。
struct Applied {
    recipe: Recipe,
    digest: Digest,
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
        // 前の回が残した作業用の PCM は、この回で使い回すか、期限で消える
        drop(self.take_kept(job));
        // 中止を書いたあとに落ちた（行は canceled、ジョブは running のまま開き直した）なら、残りを当てずに中止で終える
        let canceled: bool = db.read(|c| {
            Ok(c.prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM batch_items WHERE job_id = ?1 AND state = 'canceled')",
            )?
            .query_row([job], |r| r.get(0))?)
        })?;
        if canceled {
            ctx.log("前回、中止を受けたあとにアプリが終わったので、中止で終えます");
            return finish_canceled(db, job);
        }
        // レシピ（積んだときの写し。元に戻す一括は持たない）
        let recipe = match &input.recipe {
            Some(copy) => {
                let runtime = self.engine.runtime()?.clone();
                let recipe = load_copy(db, copy)?;
                runtime
                    .bulk(|e| e.check_recipe(&recipe))
                    .map_err(|e| from_engine("このレシピは当てられません", e))?;
                Some(Applied {
                    recipe,
                    digest: copy.doc,
                })
            }
            None if matches!(input.mode, ModeInput::Undo { .. } | ModeInput::Merge { .. }) => None,
            None => {
                return Err(CoreError::Invalid(
                    "一括のジョブの入力にレシピがありません".into(),
                ));
            }
        };
        let plan = Plan {
            recipe,
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
        // N → 1 は 1 本の単位で作る（働き手に配らない）
        if let ModeInput::Merge { name, sheet } = &plan.mode {
            return self.run_merge(ctx, name, sheet);
        }
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
        match self.process(ctx, &plan, pending, total, &mut done)? {
            Stop::Paused => return Ok(JobEnd::Paused),
            Stop::Yield => return Ok(JobEnd::Yield),
            Stop::Canceled => return finish_canceled(db, job),
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

    /// もう一度動かす前の確かめ（[`undo::check_retry`]：同じ一括を戻す一括と重ならない）。
    fn check_retry(&self, tx: &Connection, job: JobId, input_json: &str) -> Result<()> {
        undo::check_retry(tx, job, input_json)
    }

    /// 待ち・一時停止のまま中止された（譲ったあとに中止を受けたときも、ジョブの管理が同じトランザクションで呼ぶ）：残りの pending
    /// の行を canceled にし、一時停止・譲るで残した作業用の PCM を捨てる。
    fn canceled_while_idle(&self, tx: &Connection, job: JobId, _input_json: &str) -> Result<()> {
        cancel_rows(tx, job, now_ms())?;
        for key in self.take_kept(job) {
            self.engine.work_pcm().discard(&key);
        }
        Ok(())
    }
}

/// 中止で終える（残りの pending の行を canceled にする。中止を見たときに書いてあるが、そのあとの行もまとめる）。
fn finish_canceled(db: &Db, job: i64) -> Result<JobEnd> {
    let now = now_ms();
    db.write(move |tx| cancel_rows(tx, job, now))?;
    let result = summary(db, job)?;
    Ok(JobEnd::Canceled {
        result_json: Some(result.to_string()),
    })
}

/// 止め方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Finished,
    Paused,
    Yield,
    Canceled,
}

/// 止める指示を読む（ループの回ごと）。中止は一時停止・譲るに、一時停止は譲るに勝つ（譲ると決めたあとも読み直す。譲る途中に
/// 受けた中止・一時停止を落とさない。M14 段 4 の検証の直し）。譲るかは、まだ止めていないときだけ見る。
fn observe(ctx: &JobContext, stop: Option<Stop>) -> Option<Stop> {
    match ctx.control() {
        Control::Cancel => Some(Stop::Canceled),
        _ if stop == Some(Stop::Canceled) => stop,
        Control::Pause => Some(Stop::Paused),
        Control::Continue => match stop {
            Some(s) => Some(s),
            None if ctx.should_yield() => Some(Stop::Yield),
            None => None,
        },
    }
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
        let yielding = AtomicBool::new(false);
        let mut commits = 0usize;
        let mut kept: Vec<WorkKey> = Vec::new();
        let end = std::thread::scope(|s| -> Result<Stop> {
            for _ in 0..k {
                let (task_rx, out_tx, durability, yielding) =
                    (&task_rx, out_tx.clone(), durability.as_ref(), &yielding);
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
                            yielding,
                        };
                        // 1 件の panic は、その行の失敗にする（結果を返さずに終わると、配った数が戻らずジョブが止まる）
                        let outcome =
                            catch_unwind(AssertUnwindSafe(|| self.prepare_one(&unit, plan, task)))
                                .unwrap_or_else(|_| {
                                    tracing::error!(
                                        job = ctx.id(),
                                        item = task.item_id,
                                        "一括の 1 件の準備が panic しました"
                                    );
                                    Outcome::Finished {
                                        task,
                                        state: "failed",
                                        code: "Internal".into(),
                                        message: "準備の途中で内部の誤りが起きました".into(),
                                    }
                                });
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
            // 働き手がそのままコミットした分の後のジョブ（まとまりごとに積む）
            let mut follow: Vec<MediaKind> = Vec::new();
            let result = loop {
                let next = observe(ctx, stop);
                if next == Some(Stop::Canceled) && stop != Some(Stop::Canceled) {
                    // 中止を見たらすぐ残りの行を canceled にする（このあと落ちても、開き直した一括が残りを当てない）。書き
                    // 終えた分は落とす
                    ready.clear();
                    group_since = None;
                    let now = now_ms();
                    if let Err(e) = db.write(move |tx| cancel_rows(tx, job, now)) {
                        break Err(e);
                    }
                }
                if next == Some(Stop::Yield) {
                    yielding.store(true, Ordering::Release);
                }
                stop = next;
                let canceled = stop == Some(Stop::Canceled);
                // 配る（止める指示がなければ）
                while stop.is_none() && in_flight < k {
                    let Some(task) = queue.pop_front() else { break };
                    if task_tx.send(task).is_err() {
                        break;
                    }
                    in_flight += 1;
                }
                let drained = in_flight == 0 && (queue.is_empty() || stop.is_some());
                // まとまりのコミット（一時停止・譲るは書き終えた分をコミットしてから止まる）
                let due = !canceled
                    && !ready.is_empty()
                    && (ready.len() >= GROUP_ITEMS
                        || group_since.is_some_and(|t| t.elapsed() >= GROUP_TIME)
                        || drained
                        || matches!(stop, Some(Stop::Paused | Stop::Yield)));
                if due {
                    let group = std::mem::take(&mut ready);
                    group_since = None;
                    match self.commit_group(
                        ctx,
                        plan,
                        group,
                        durability.as_ref(),
                        &yielding,
                        done,
                        &mut commits,
                    ) {
                        Ok(retry) => {
                            report(*done);
                            for task in retry.into_iter().rev() {
                                queue.push_front(task);
                            }
                        }
                        Err(e) => break Err(e),
                    }
                    continue;
                }
                if drained && ready.is_empty() {
                    break Ok(());
                }
                match out_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(outcome) => {
                        in_flight = in_flight.saturating_sub(1);
                        match outcome {
                            Outcome::Ready(task, written) => {
                                if canceled {
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
                                if !canceled {
                                    if let Err(e) =
                                        finish_row(db, job, task.ord, state, &code, &message)
                                    {
                                        break Err(e);
                                    }
                                    *done += 1;
                                    report(*done);
                                }
                            }
                            Outcome::Done { saved } => {
                                // 行は働き手が同じトランザクションで書いた（中止のあとなら行は canceled でコミットされない）
                                *done += 1;
                                report(*done);
                                if let Some(kind) = saved {
                                    follow.push(kind);
                                }
                                if follow.len() >= GROUP_ITEMS {
                                    queue_follow_ups(ctx, &std::mem::take(&mut follow));
                                }
                            }
                            Outcome::Retry(task) => queue.push_front(Task { attempt: 1, ..task }),
                            Outcome::Stopped(keys) => kept.extend(keys),
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        break Err(CoreError::Invalid("一括の働き手が止まりました".into()));
                    }
                }
            };
            if !follow.is_empty() {
                queue_follow_ups(ctx, &follow);
            }
            // 誤りで抜けたときも、準備の途中の働き手をすぐ止める（書き終えたものは受けずに落とす）
            yielding.store(true, Ordering::Release);
            drop(task_tx);
            result.map(|()| stop.unwrap_or(Stop::Finished))
        });
        if matches!(end, Ok(Stop::Paused | Stop::Yield))
            && !kept.is_empty()
            && let Ok(mut map) = self.kept.lock()
        {
            map.insert(job, kept);
        }
        end
    }

    /// まとまり 1 つ：区切りの同期を 1 回取り、1 件ずつコミットする。土台の競合で 1 回目のものは、やり直す仕事として返す。
    /// 中止を受けたら、残りはコミットせずに落とす。
    #[allow(clippy::too_many_arguments)]
    fn commit_group(
        &self,
        ctx: &JobContext,
        plan: &Plan,
        group: Vec<(Task, Box<Written>)>,
        durability: &dyn Durability,
        yielding: &AtomicBool,
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
            if ctx.control() == Control::Cancel {
                break;
            }
            let unit = BatchUnit {
                ctx,
                ord: task.ord,
                durability,
                yielding,
            };
            let base = written.base();
            #[cfg(test)]
            let crash_saver;
            #[cfg(test)]
            let saver = match self.crash_now(*commits) {
                Some(fault) => {
                    crash_saver = Saver::new(self.engine.clone()).with_fault(fault);
                    &crash_saver
                }
                None => &self.saver,
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
                    message,
                    ..
                }) => {
                    // 同じ中身のアイテムがある（版を足さない・作らない）
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
                    if self.crash_now(*commits).is_some() {
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
        let head = skip_rules(&self.engine, db, item, plan.skips_drafts())?;
        #[cfg(test)]
        if let Some(hook) = &self.before_prepare {
            hook(item);
        }
        if let ModeInput::Undo { of, save_as, merge } = plan.mode {
            return self.prepare_undo(unit, task, head, of, save_as, merge);
        }
        let Some(applied) = &plan.recipe else {
            return Err(failed(CoreError::Invalid(
                "一括のジョブにレシピがありません".into(),
            )));
        };
        let overwrite = matches!(plan.mode, ModeInput::Overwrite);
        // 2. 継ぎ足して結び付ける（土台は今の頭）
        let base = Base {
            hash: head.hash,
            version: head.version,
        };
        let runtime = self.engine.runtime().map_err(failed)?.clone();
        let head_doc = head_doc(db, &runtime, item, head.version).map_err(failed)?;
        let stopped = || Outcome::Stopped(kept_keys(db, item, &head_doc));
        let facts = db.read(|c| facts_of(c, item)).map_err(failed)?;
        let instance = match self.bind(unit, &runtime, &applied.recipe, item, &head_doc, &facts)? {
            Flow::Go(instance) => instance,
            Flow::Stop(_) => return Ok(stopped()),
        };
        let mut graph = instance.graph;
        let bound: serde_json::Value = bound_json(&instance.bound)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(serde_json::Value::Null);
        let record = json!({
            "job": ctx.id(),
            "recipe": { "name": applied.recipe.name, "digest": applied.digest },
            "bound": bound,
        });
        // 3. 準備
        let names = match &plan.mode {
            ModeInput::Overwrite | ModeInput::Undo { .. } | ModeInput::Merge { .. } => None,
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
            Err(CoreError::Unwritable(why)) if !overwrite => {
                // 別名は代わりの形式で（ADR-190 (6)。書き手がないときだけ。元を読めないときは替えない）
                let fallbacks: &[&str] = match head.kind {
                    MediaKind::Audio => &["ogg", "wav"],
                    _ => &["png"],
                };
                let mut last = Err(CoreError::Unwritable(why));
                for format in fallbacks {
                    graph.outputs[0].format = FormatChoice::To {
                        format: (*format).to_string(),
                        options: json!({}),
                    };
                    graph.outputs[0].quantize = None;
                    last = attempt(&graph);
                    if !matches!(last, Err(CoreError::Unwritable(_))) {
                        break;
                    }
                }
                last
            }
            other => other,
        };
        match prepared {
            Ok(Flow::Go(w)) => Ok(Outcome::Ready(task, Box::new(w))),
            Ok(Flow::Stop(_)) => Ok(stopped()),
            // 書けない形式（上書き。別名は代わりの形式でも書けない）
            Err(CoreError::Unwritable(why)) => Err(Skip::skipped("FormatUnavailable", why)),
            // 準備の途中で土台が変わった（ゴミ箱に入れられたのを含む）：頭を読み直して 1 回だけやり直す（ゴミ箱なら Trashed に
            // なる）。やり直しても変われば skipped（Conflict）
            Err(CoreError::Conflict(_)) if task.attempt == 0 => Ok(Outcome::Retry(task)),
            Err(e @ CoreError::Conflict(_)) => Err(Skip::skipped("Conflict", e.to_string())),
            // このアイテムに当てられない（レシピの手が種類・形に合わず描けない、アイテムの種類を変える出力の形式。元が壊れて
            // 読めないときの Invalid は failed のまま）
            Err(e @ CoreError::Unfit(_)) => Err(Skip::skipped("NotApplicable", e.to_string())),
            Err(e) => Err(failed(e)),
        }
    }

    /// 継ぎ足して結び付ける（`Engine::instantiate`。一括のプールで）。結び付けがあれば、保存が読むのと同じ入力の形を読む
    /// （[`Self::input_shape`]）。終わりからの秒で、見出しの長さが上限しか分からない（MP3・AAC）なら、形を読めた段で流して
    /// 数えた長さで結び付け直す（エディタがデコードした値の長さで結び付けるのと同じ値にする）。
    fn bind(
        &self,
        unit: &BatchUnit<'_>,
        runtime: &EngineRuntime,
        recipe: &Recipe,
        item: i64,
        head_doc: &Graph,
        facts: &Facts,
    ) -> std::result::Result<Flow<kura_engine::recipe::Instance>, Skip> {
        let instantiate = |shapes: &BTreeMap<InputId, Shape>| {
            runtime
                .bulk(|e| e.instantiate(head_doc, recipe, shapes, facts))
                .map_err(bind_skip)
        };
        if recipe.bindings.is_empty() {
            return instantiate(&BTreeMap::new()).map(Flow::Go);
        }
        let read = match self.input_shape(unit, item, head_doc) {
            Ok(Flow::Go(read)) => read,
            Ok(Flow::Stop(end)) => return Ok(Flow::Stop(end)),
            Err(e) => return Err(Skip::failed(&e)),
        };
        let shapes = BTreeMap::from([(InputId(0), read.shape)]);
        match runtime.bulk(|e| e.instantiate(head_doc, recipe, &shapes, facts)) {
            Err(BindError::NoLength { .. }) if read.stage.is_some() => {
                let exact = match self.count_frames(unit, &read) {
                    Ok(Flow::Go(shape)) => shape,
                    Ok(Flow::Stop(end)) => return Ok(Flow::Stop(end)),
                    Err(e) => return Err(Skip::failed(&e)),
                };
                instantiate(&BTreeMap::from([(InputId(0), exact)])).map(Flow::Go)
            }
            other => other.map(Flow::Go).map_err(bind_skip),
        }
    }

    /// 結び付けのための入力 0 の形（保存が読むのと同じもの。[`read_input_shape`]）：段ごとに一括の枠で予算を待ち、OS の段なら
    /// 読む前後に行の印を置く。
    fn input_shape(
        &self,
        unit: &BatchUnit<'_>,
        item: i64,
        head_doc: &Graph,
    ) -> Result<Flow<InputRead>> {
        let runtime = self.engine.runtime()?.clone();
        let hooks = JobShape {
            unit,
            mon: UnitMonitor(unit),
            runtime: &runtime,
        };
        read_input_shape(&self.engine, unit.ctx.library(), item, head_doc, &hooks)
    }

    /// 見出しの長さが上限しか分からない音声（MP3・AAC）の長さを、形を読めた段で流して数える（予算はその段の流して読む見積もり。
    /// OS の段なら読む前後に行の印を置く）。
    fn count_frames(&self, unit: &BatchUnit<'_>, read: &InputRead) -> Result<Flow<Shape>> {
        let (Some(stage), Shape::Audio { rate, channels, .. }) = (&read.stage, read.shape) else {
            return Ok(Flow::Go(read.shape));
        };
        let runtime = self.engine.runtime()?.clone();
        let codecs = self.engine.codecs();
        let limits = runtime.bulk(|e| e.limits().clone());
        let mon = UnitMonitor(unit);
        let len = std::fs::metadata(&read.path).map_or(0, |m| m.len());
        let need = codecs.stage_stream_work_bytes(stage, read.format, &read.shape, len);
        let _lease = match runtime.budget().acquire(need, Class::Bulk, &mon) {
            Ok(lease) => lease,
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                return Ok(Flow::Stop(JobEnd::Paused));
            }
            Err(e) => return Err(from_engine("元の長さを数えるメモリを確保できません", e)),
        };
        if read.platform {
            unit.mark(DECODING, 0.0, "元を読んでいます")?;
        }
        let mut sink = FrameCount(None);
        let counted = runtime.bulk(|_| {
            codecs.decode_stream_with_path(
                stage.id,
                &read.path,
                read.format,
                &read.opts,
                &limits,
                &mut sink,
                &mon,
            )
        });
        if read.platform {
            unit.mark(DECODED, 0.0, "元を読みました")?;
        }
        match (counted, sink.0) {
            (Ok(_), Some(frames)) => Ok(Flow::Go(Shape::Audio {
                rate,
                channels,
                frames: Dim::Exact(frames),
            })),
            (Ok(_), None) => Err(CoreError::Invalid(
                "元の長さを数えられません（流れが終わりませんでした）".into(),
            )),
            (Err(e), _) if matches!(e.root(), EngineError::Canceled) => {
                Ok(Flow::Stop(JobEnd::Paused))
            }
            (Err(e), _) => Err(from_engine("元の長さを数えられません", e)),
        }
    }
}

/// 入力の形を読むときの、呼び手ごとの振る舞い（一括の 1 件と事前チェック）。
trait ShapeHooks {
    /// 止める指示（中止・一時停止・事前チェックの中止）。
    fn monitor(&self) -> &dyn Monitor;
    /// 段の形を読む分を予算から確保する（一括の 1 件は一括の枠で待つ、事前チェックは対話の枠で待たない）。
    fn acquire(&self, bytes: u64) -> std::result::Result<kura_engine::budget::Lease, EngineError>;
    /// OS のデコーダで読む前後の印（一括の 1 件は行の `decoding`。事前チェックは置かない）。
    fn mark(&self, decoding: bool) -> Result<()>;
    fn log(&self, line: &str);
}

/// 一括の 1 件の読み方。
struct JobShape<'a, 'b> {
    unit: &'a BatchUnit<'b>,
    mon: UnitMonitor<'a>,
    runtime: &'a EngineRuntime,
}

impl ShapeHooks for JobShape<'_, '_> {
    fn monitor(&self) -> &dyn Monitor {
        &self.mon
    }

    fn acquire(&self, bytes: u64) -> std::result::Result<kura_engine::budget::Lease, EngineError> {
        self.runtime.budget().acquire(bytes, Class::Bulk, &self.mon)
    }

    fn mark(&self, decoding: bool) -> Result<()> {
        if decoding {
            self.unit.mark(DECODING, 0.0, "元を読んでいます")
        } else {
            self.unit.mark(DECODED, 0.0, "元を読みました")
        }
    }

    fn log(&self, line: &str) {
        self.unit.log(line);
    }
}

/// 入力 0 の形（保存が読むのと同じもの）：アンカーがあればアンカーの見出し（記録の値の形。OS のデコーダを通らない。読めなければ
/// 元から）、なければ元を段の順に読む（保存と同じ段。段ごとに、その段で形を読む分を `hooks` で確保し、OS の段なら読む前後に
/// 印を置く）。止める指示・中止の確保は `Flow::Stop`。
fn read_input_shape(
    engine: &EngineServices,
    library: &Library,
    item: i64,
    head_doc: &Graph,
    hooks: &dyn ShapeHooks,
) -> Result<Flow<InputRead>> {
    let db = library.db();
    let original = db
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
    let runtime = engine.runtime()?.clone();
    let limits = runtime.bulk(|e| e.limits().clone());
    let path = library.layout().content_path(&original.hash, &original.ext);
    let options_digest = anchors::options_digest(&opts)?;
    if let Some(a) = db.read(|c| anchors::find(c, &original.hash, &options_digest))? {
        let anchor_path = library.layout().content_path(&a.hash, &a.ext);
        match File::open(&anchor_path)
            .map_err(EngineError::from)
            .and_then(|f| anchor::shape(&mut BufReader::new(f), &limits))
        {
            Ok(shape) => {
                return Ok(Flow::Go(InputRead {
                    shape,
                    anchor: Some(anchor_path),
                    stage: None,
                    path,
                    format,
                    opts,
                    platform: false,
                }));
            }
            // 保存も、アンカーが使えなければ元を読み直す
            Err(e) => hooks.log(&format!(
                "アイテム {item} のアンカーの見出しを読めないので、元から読みます：{e}"
            )),
        }
    }
    staged_shape(engine, path, format, opts, hooks)
}

/// ファイル `path`（形式 `format`・読み方 `opts`）の形を段の順に読む（保存と同じ段。段ごとに、その段で形を読む分を `hooks` で確保し、
/// OS の段なら読む前後に印を置く）。止める指示・中止の確保は `Flow::Stop`。
fn staged_shape(
    engine: &EngineServices,
    path: std::path::PathBuf,
    format: &'static str,
    opts: DecodeOptions,
    hooks: &dyn ShapeHooks,
) -> Result<Flow<InputRead>> {
    let runtime = engine.runtime()?.clone();
    let limits = runtime.bulk(|e| e.limits().clone());
    let codecs = engine.codecs();
    let mon = hooks.monitor();
    let is_platform = |id: &str| {
        codecs
            .decoders_for(format)
            .iter()
            .any(|d| d.id() == id && d.tier() == Tier::Platform)
    };
    let mut lease = None;
    let mut stopped: Option<Result<()>> = None;
    let mut marked = false;
    let read = codecs.shape_path_staged(&path, format, &opts, &limits, mon, None, &mut |stage| {
        lease = None;
        match hooks.acquire(stage.shape_bytes) {
            Ok(l) => lease = Some(l),
            Err(e) => {
                stopped = Some(if matches!(e.root(), EngineError::Canceled) {
                    Ok(())
                } else {
                    Err(from_engine("元の大きさを読むメモリを確保できません", e))
                });
                return Err(EngineError::Canceled);
            }
        }
        if !marked && is_platform(stage.id) {
            if let Err(e) = hooks.mark(true) {
                stopped = Some(Err(e));
                return Err(EngineError::Canceled);
            }
            marked = true;
        }
        Ok(())
    });
    drop(lease);
    if marked {
        hooks.mark(false)?;
    }
    match stopped {
        Some(Ok(())) => return Ok(Flow::Stop(JobEnd::Paused)),
        Some(Err(e)) => return Err(e),
        None => {}
    }
    match read {
        Ok((shape, stage)) => {
            let platform = is_platform(stage.id);
            Ok(Flow::Go(InputRead {
                shape,
                anchor: None,
                stage: Some(stage),
                path,
                format,
                opts,
                platform,
            }))
        }
        Err(e) if matches!(e.root(), EngineError::Canceled) => Ok(Flow::Stop(JobEnd::Paused)),
        Err(e) => Err(from_engine("元の大きさを読めません", e)),
    }
}

/// 結び付けのために読んだ入力 0 の形と、読み方（長さを数え直すときに使う）。
struct InputRead {
    shape: Shape,
    /// アンカーの見出しから読んだなら、そのファイル（見積もりの読み手と長さ）。
    anchor: Option<std::path::PathBuf>,
    /// 元を読んだ段（アンカーの見出しから読んだなら None：長さは決まっている）。
    stage: Option<kura_engine::codec::PickedStage>,
    /// 元のファイル。
    path: std::path::PathBuf,
    format: &'static str,
    opts: DecodeOptions,
    /// 読んだ段が OS のデコーダか。
    platform: bool,
}

/// 流れの長さだけを受ける（標本は捨てる）。
struct FrameCount(Option<u64>);

impl DecodeSink for FrameCount {
    fn begin(&mut self, _info: &DecodeInfo) -> std::result::Result<(), EngineError> {
        self.0 = None;
        Ok(())
    }

    fn feed(
        &mut self,
        _start: u64,
        _planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> std::result::Result<(), EngineError> {
        Ok(())
    }

    fn end(&mut self, end: &StreamEnd) -> std::result::Result<(), EngineError> {
        self.0 = Some(end.frames);
        Ok(())
    }
}

/// 結び付けの誤りの分け方：レシピの側（規則の版・大きすぎる・ノードの ID を使い切る）は CoreError の code で失敗、N → 1 は
/// Invalid、ほか（値がない・前の手が座標を変える・長さが決まらない・音声でない・範囲の外・前の部分をこのアイテムで推論できない）
/// はこのアイテムに当てられない手として skipped（NotApplicable）。
fn bind_skip(e: BindError) -> Skip {
    match e {
        BindError::Recipe(err) => Skip::failed(&from_engine("レシピを当てられません", err)),
        BindError::Kind => Skip::failed(&CoreError::Invalid(e.to_string())),
        other => Skip::skipped("NotApplicable", other.to_string()),
    }
}

/// 頭の文書（版 `version`。版 0 と、版 0 に戻した版は空の文書）。DB から読んだバイト列を一括のプールで読み込む（エディタの
/// プレビューのプールを使わない。M14 段 4 の検証の直し）。
pub(crate) fn head_doc(db: &Db, runtime: &EngineRuntime, item: i64, version: i64) -> Result<Graph> {
    let bytes = db.read(|c| match versions::get(c, item, version)? {
        Some(versions::Version {
            doc_id: Some(id), ..
        }) => store::load_doc_bytes(c, id).map(Some),
        Some(_) => Ok(None),
        None if version == 0 => Ok(None),
        None => Err(CoreError::NotFound(format!(
            "アイテム {item} の版 {version}"
        ))),
    })?;
    match bytes {
        Some(bytes) => runtime
            .bulk(|e| e.load(&bytes))
            .map_err(|e| from_engine("文書を読めません", e)),
        None => Ok(Graph::new_single()),
    }
}

/// 止めた 1 件が一時停止で残しうる作業用の PCM の鍵（求められなければ空。残したものは期限・上限・閉じるときに消える）。
fn kept_keys(db: &Db, item: i64, head_doc: &Graph) -> Vec<WorkKey> {
    let opts = head_doc
        .inputs
        .first()
        .map(|slot| slot.decode.clone())
        .unwrap_or_default();
    db.read(|c| work_keys(c, item, opts)).unwrap_or_else(|err| {
        tracing::warn!(%err, item, "止めた 1 件の作業用の PCM の鍵を求められません");
        Vec::new()
    })
}

/// 飛ばす決まり（ジョブの段 1・事前チェック・元に戻す一括で同じ）：ないアイテム（Gone）・ゴミ箱にある（Trashed）・画像と音声で
/// ない（Kind）・エディタで開いている（Open）・前の保存が終わっていない（SavePending）・保存していない下書きがある（Draft。
/// `drafts` が真のとき）。当てられるなら頭を返す。
fn skip_rules(
    engine: &EngineServices,
    db: &Db,
    item: i64,
    drafts: bool,
) -> std::result::Result<versions::Head, Skip> {
    skip_rules_with(engine, db, item, drafts, true)
}

/// [`skip_rules`]。`open` が偽ならエディタで開いているアイテムも飛ばさない（N → 1 は元を読むだけ。今の中身で作る）。
fn skip_rules_with(
    engine: &EngineServices,
    db: &Db,
    item: i64,
    drafts: bool,
    open: bool,
) -> std::result::Result<versions::Head, Skip> {
    let failed = |e: CoreError| Skip::failed(&e);
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
    if open && engine.current_editor().is_some_and(|e| e.item_id() == item) {
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
    if drafts && db.read(|c| drafts::get(c, item)).map_err(failed)?.is_some() {
        return Err(Skip::skipped(
            "Draft",
            "保存していない下書きがあるので飛ばしました".into(),
        ));
    }
    Ok(head)
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
