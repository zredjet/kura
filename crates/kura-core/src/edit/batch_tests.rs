//! 一括加工のジョブの試験（M14 段 4。docs/editing.md §14「一括加工の受け入れテスト」の B1〜）。固定の段のコーデック（と偽の
//! Platform のデコーダ）と偽のエンジンの部品で、Linux でも回る。耐久は数える偽物（ファイルの同期は `sync_all`）で、区切りの
//! 同期の回数を見る。どの試験も終わりに整合性・予告・staging・版の不変条件を確かめる。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use kura_engine::codec::{CodecEnv, Decoder, ReadSeek, Tier};
use kura_engine::doc::{DecodeOptions, FormatChoice, InputId, NodeId};
use kura_engine::recipe::{Binding, Facts, SettingKey, Source, TimeAnchor, bound_json};
use kura_engine::testgen::InputGen;
use kura_engine::testing::platform::FakePlatformDecoder;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, EngineError, Graph, Limits, Monitor, NoMonitor, Shape};
use rusqlite::params;
use serde_json::{Value as Json, json};

use super::batch::{BatchHandler, BatchMode, BatchRequest, pending_batch, start_batch};
use super::recipes::{self, EditorPick};
use super::save::{EditHandler, Fault};
use super::save_stream_tests::{add_audio, audio_doc, noise_file};
use super::save_tests::{
    WAIT, add_item, assert_consistent, count, doc, flip, hash_of, head, heic_codecs, heic_decoder,
    render_png, sink, sprite, versions_of,
};
use super::services::fake::FakeEngineFactory;
use super::services::{Durability, EngineFactory};
use super::{CodecSet, ENGINE_WORK_DIR, EngineServices};
use crate::edit::runtime::RuntimeConfig;
use crate::jobs::{
    JobContext, JobEnd, JobEvent, JobHandler, JobKind, JobManager, JobRecord, JobState,
};
use crate::library::{Library, LibrarySession, Services};
use crate::media::fake::FakeBackend;

/// コミットの直前の割り込み（ライブラリ・エンジンの部品・アイテムの ID）。
type CommitHook = Arc<dyn Fn(&Library, &Arc<EngineServices>, i64) + Send + Sync>;

/// コミットの直前の割り込み（ジョブの管理・ライブラリ・アイテムの ID。動いている一括を止める・保存を積む）。
type JobsHook = Arc<dyn Fn(&JobManager, &Library, i64) + Send + Sync>;

/// 準備の途中（飛ばす決まりを見たあと）の割り込み（ライブラリ・アイテムの ID）。
type PrepareHook = Arc<dyn Fn(&Library, i64) + Send + Sync>;

/// 試験の割り込みと差し替え。
#[derive(Default)]
struct Hooks {
    commit: Option<CommitHook>,
    jobs: Option<JobsHook>,
    prepare: Option<PrepareHook>,
    /// 保存のジョブの処理の差し替え（譲る試験）。
    edit: Option<Arc<dyn JobHandler>>,
}

/// 対象の行（item, state, code, version_id, out_item_id）。
type Row = (i64, String, Option<String>, Option<i64>, Option<i64>);

/// 数える耐久（ファイルの同期は本物の `sync_all`、区切りは数えるだけ）。
#[derive(Default)]
struct Counting {
    files: AtomicUsize,
    barriers: AtomicUsize,
    /// コミットするたびの、それまでの区切りの同期の回数。
    at_commit: Mutex<Vec<usize>>,
}

impl Durability for Counting {
    fn sync_file(&self, file: &std::fs::File) -> std::io::Result<()> {
        self.files.fetch_add(1, Ordering::SeqCst);
        file.sync_all()
    }

    fn barrier(&self, _file: &std::fs::File) -> std::io::Result<()> {
        self.barriers.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// 偽のエンジンの部品に、数える耐久を付けたもの。
struct Factory {
    inner: FakeEngineFactory,
    durability: Arc<Counting>,
}

impl EngineFactory for Factory {
    fn runtime_config(&self) -> RuntimeConfig {
        self.inner.runtime_config()
    }

    fn codecs(&self, work_dir: &Path) -> CodecSet {
        self.inner.codecs(work_dir)
    }

    fn durability(&self) -> Arc<dyn Durability> {
        self.durability.clone()
    }
}

/// 一括と保存のジョブだけを動かすライブラリ。
struct BatchEnv {
    dir: tempfile::TempDir,
    lib: Arc<Library>,
    engine: Arc<EngineServices>,
    jobs: Arc<JobManager>,
    durability: Arc<Counting>,
    codecs: Option<Arc<Mutex<CodecSet>>>,
}

impl BatchEnv {
    fn new() -> Self {
        Self::with(None, None, true)
    }

    /// `codecs`（なければ固定の段）、`crash`（n 件コミットしたあとの次のコミットを落とす）、`start`（ジョブを動かすか）。
    fn with(
        codecs: Option<Arc<Mutex<CodecSet>>>,
        crash: Option<(usize, Fault)>,
        start: bool,
    ) -> Self {
        Self::build(codecs, crash, start, Hooks::default())
    }

    /// コミットの直前の割り込み `hook`（ライブラリ・エンジンの部品・アイテムの ID を受け取る）つき。
    fn with_hook(hook: CommitHook) -> Self {
        Self::build(
            None,
            None,
            true,
            Hooks {
                commit: Some(hook),
                ..Hooks::default()
            },
        )
    }

    /// コミットの直前の、ジョブの管理を受け取る割り込みつき。
    fn with_jobs_hook(hook: JobsHook) -> Self {
        Self::build(
            None,
            None,
            true,
            Hooks {
                jobs: Some(hook),
                ..Hooks::default()
            },
        )
    }

    fn build(
        codecs: Option<Arc<Mutex<CodecSet>>>,
        crash: Option<(usize, Fault)>,
        start: bool,
        hooks: Hooks,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let durability = Arc::new(Counting::default());
        let factory = Factory {
            inner: match &codecs {
                Some(set) => FakeEngineFactory::with_codecs(set.clone()),
                None => FakeEngineFactory::default(),
            },
            durability: durability.clone(),
        };
        let engine = Arc::new(EngineServices::new(
            Arc::new(factory),
            lib.layout().staging_dir().join(ENGINE_WORK_DIR),
        ));
        let mut batch = BatchHandler::new(engine.clone());
        batch.crash = crash;
        let slot: Arc<OnceLock<Weak<JobManager>>> = Arc::default();
        {
            let (lib, engine, durability, slot) = (
                lib.clone(),
                engine.clone(),
                durability.clone(),
                slot.clone(),
            );
            let (commit, on_jobs) = (hooks.commit, hooks.jobs);
            batch.before_commit = Some(Arc::new(move |item| {
                let barriers = durability.barriers.load(Ordering::SeqCst);
                durability.at_commit.lock().unwrap().push(barriers);
                if let Some(hook) = &commit {
                    hook(&lib, &engine, item);
                }
                if let Some(hook) = &on_jobs
                    && let Some(jobs) = slot.get().and_then(Weak::upgrade)
                {
                    hook(&jobs, &lib, item);
                }
            }));
        }
        if let Some(hook) = hooks.prepare {
            let lib = lib.clone();
            batch.before_prepare = Some(Arc::new(move |item| hook(&lib, item)));
        }
        let edit: Arc<dyn JobHandler> = match hooks.edit {
            Some(h) => h,
            None => Arc::new(EditHandler::new(engine.clone())),
        };
        let events: Arc<Mutex<Vec<JobEvent>>> = Arc::default();
        let jobs = Arc::new(
            JobManager::new(
                lib.clone(),
                vec![(JobKind::Edit, edit), (JobKind::Batch, Arc::new(batch))],
                sink(&events),
            )
            .unwrap(),
        );
        let _ = slot.set(Arc::downgrade(&jobs));
        if start {
            jobs.start().unwrap();
        }
        Self {
            dir,
            lib,
            engine,
            jobs,
            durability,
            codecs,
        }
    }

    fn png(&self, seed: u64) -> (i64, Vec<u8>) {
        let bytes = sprite(seed);
        let (id, _) = add_item(&self.lib, &bytes, "png", &format!("絵{seed}"));
        (id, bytes)
    }

    /// 文書 `g` のすべてのノードを含めたレシピ（結び付け `bindings`）。
    fn recipe(&self, name: &str, g: &Graph, bindings: Vec<Binding>) -> i64 {
        let keep: Vec<NodeId> = g.nodes.iter().map(|n| n.id).collect();
        recipes::create_from_editor(
            self.lib.db(),
            self.engine.runtime().unwrap(),
            name,
            "",
            EditorPick {
                graph: g,
                keep: &keep,
                with_format: true,
                bindings,
            },
        )
        .unwrap()
    }

    fn start(&self, recipe: i64, items: &[i64], mode: BatchMode, include_drafts: bool) -> i64 {
        start_batch(
            self.lib.db(),
            &self.jobs,
            &self.engine,
            &BatchRequest {
                recipe_id: recipe,
                items: items.to_vec(),
                mode,
                include_drafts,
            },
        )
        .unwrap()
    }

    fn run(&self, recipe: i64, items: &[i64], mode: BatchMode) -> JobRecord {
        let job = self.start(recipe, items, mode, false);
        assert!(self.jobs.wait_idle(WAIT));
        self.jobs.get(job).unwrap()
    }

    /// 対象の行（順に (item, state, code, version_id, out_item_id)）。
    fn rows(&self, job: i64) -> Vec<Row> {
        rows_of(&self.lib, job)
    }
}

fn rows_of(lib: &Library, job: i64) -> Vec<Row> {
    lib.db()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT item_id, state, code, version_id, out_item_id FROM batch_items WHERE job_id = ?1 ORDER BY ord",
            )?;
            let rows = stmt.query_map([job], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .unwrap()
}

fn result_of(record: &JobRecord) -> Json {
    serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap()
}

fn assert_done(record: &JobRecord) {
    assert_eq!(
        record.state,
        JobState::Done,
        "{:?} {:?}",
        record.message,
        record.error_code
    );
}

/// 一括で足した版（job_id が空で、行の version_id が指すもの）の数。
fn batch_versions(lib: &Library, item: i64) -> usize {
    versions_of(lib, item)
        .iter()
        .filter(|v| v.version > 0 && v.job_id.is_none())
        .count()
}

/// B1：上書きの一括は、1 件ずつの保存と同じバイト列になり（版 1・job_id は空・行の version_id が版を指す）、版の記録に
/// 一括の出どころ（ジョブ・レシピ）が残る。後のジョブ（サムネイル）を積む。区切りの同期は件数より少ない回数で、出力の
/// ファイルは 1 件ずつ同期する。
#[test]
fn b1_overwrite_matches_the_single_save() {
    let env = BatchEnv::new();
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let items: Vec<(i64, Vec<u8>)> = (1..=40).map(|k| env.png(k)).collect();
    let ids: Vec<i64> = items.iter().map(|(id, _)| *id).collect();
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    assert_eq!(result_of(&record)["counts"]["saved"], 40);
    let rows = env.rows(record.id);
    for ((id, png), row) in items.iter().zip(&rows) {
        assert_eq!((row.0, row.1.as_str()), (*id, "saved"));
        let vs = versions_of(&env.lib, *id);
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[1].job_id, None);
        assert_eq!(row.3, Some(vs[1].id));
        assert_eq!(head(&env.lib, *id).hash, hash_of(&render_png(png, &g)));
        let rec = vs[1].record().unwrap();
        let batch = rec.batch.unwrap();
        assert_eq!(batch["job"], record.id);
        assert_eq!(batch["recipe"]["name"], "反転");
    }
    let barriers = env.durability.barriers.load(Ordering::SeqCst);
    assert!((2..40).contains(&barriers), "{barriers}");
    assert_eq!(env.durability.files.load(Ordering::SeqCst), 40);
    // サムネイルの pending は後のジョブが拾う（この場はサムネイルのジョブを登録していないので積まない）
    assert_eq!(
        count(
            &env.lib,
            "SELECT count(*) FROM items WHERE thumb_state = 'pending'"
        ),
        40
    );
    assert_consistent(&env.lib);
}

/// B2：別名の一括は、名前の型「{名前} のコピー」で新しいアイテムを作り（表示名とファイル名の語幹は同じ）、行の out_item_id が
/// 指す。元のアイテムは変えない。
#[test]
fn b2_save_as_makes_named_copies() {
    let env = BatchEnv::new();
    let g = doc(&[flip("horizontal")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let items: Vec<(i64, Vec<u8>)> = (1..=3).map(|k| env.png(k)).collect();
    let ids: Vec<i64> = items.iter().map(|(id, _)| *id).collect();
    let record = env.run(
        recipe,
        &ids,
        BatchMode::SaveAs {
            name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
        },
    );
    assert_done(&record);
    for ((id, png), row) in items.iter().zip(env.rows(record.id)) {
        assert_eq!(row.1, "saved");
        let copy = row.4.unwrap();
        assert_ne!(copy, *id);
        let (title, name): (String, String) = env
            .lib
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT title, original_name FROM items WHERE id = ?1",
                    [copy],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        let k = id;
        assert_eq!(title, format!("絵{k} のコピー"));
        assert_eq!(name, format!("絵{k} のコピー.png"));
        assert_eq!(head(&env.lib, copy).hash, hash_of(&render_png(png, &g)));
        assert!(versions_of(&env.lib, *id).is_empty(), "元は変えない");
    }
    assert_consistent(&env.lib);
}

/// B3：n 件コミットしたあとに強制終了しても（予告のあと・置いたあと・コミットのあと）、開き直すと pending の行から続け、
/// どのアイテムも一括の版はちょうど 1 つ、行はすべて saved、食い違い 0。
#[test]
fn b3_a_crash_redoes_only_unfinished_items() {
    for at in [Fault::AfterAnnounce, Fault::AfterPlace, Fault::AfterCommit] {
        let env = BatchEnv::with(None, Some((2, at)), true);
        let g = doc(&[flip("vertical")], None);
        let recipe = env.recipe("反転", &g, Vec::new());
        let ids: Vec<i64> = (1..=6).map(|k| env.png(k).0).collect();
        let record = env.run(recipe, &ids, BatchMode::Overwrite);
        assert_eq!(record.state, JobState::Failed, "{at:?}");
        let job = record.id;
        let (dir, session) = reopen(env, job);
        let lib = session.library();
        let record = session.jobs().get(job).unwrap();
        assert_done(&record);
        for row in rows_of(lib, job) {
            assert_eq!(row.1, "saved", "{at:?}");
            assert_eq!(batch_versions(lib, row.0), 1, "{at:?} アイテム {}", row.0);
        }
        assert_consistent(lib);
        drop(session);
        drop(dir);
    }
}

/// 強制終了のあと（ジョブを実行中のまま）、同じ組で開き直してジョブが動き終えるのを待つ。
fn reopen(env: BatchEnv, job: i64) -> (tempfile::TempDir, LibrarySession) {
    env.lib
        .db()
        .write(move |tx| {
            tx.execute("UPDATE jobs SET state = 'running' WHERE id = ?1", [job])?;
            Ok(())
        })
        .unwrap();
    let BatchEnv {
        dir,
        lib,
        jobs,
        codecs,
        ..
    } = env;
    drop(jobs);
    let root = lib.layout().root().to_path_buf();
    lib.close();
    drop(lib);
    let factory = match codecs {
        Some(set) => FakeEngineFactory::with_codecs(set),
        None => FakeEngineFactory::default(),
    };
    let session = LibrarySession::open(
        &root,
        Arc::new(|_| {}),
        Services::new(Arc::new(FakeBackend)).with_engine(Arc::new(factory)),
    )
    .unwrap();
    session.wait_ready();
    assert!(session.jobs().wait_idle(WAIT));
    (dir, session)
}

/// B4・B5（待ちのまま）：動かす前に一時停止すると行はすべて pending のまま、再開すると処理する。待ちのまま中止すると残りの行は
/// canceled。
#[test]
fn b4_b5_idle_pause_and_cancel() {
    let env = BatchEnv::with(None, None, false);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=8).map(|k| env.png(k).0).collect();
    let job = env.start(recipe, &ids, BatchMode::Overwrite, false);
    env.jobs.pause(job).unwrap();
    env.jobs.start().unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Paused);
    assert!(env.rows(job).iter().all(|r| r.1 == "pending"));
    env.jobs.resume(job).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
    for row in env.rows(job) {
        assert_eq!(row.1, "saved");
        assert_eq!(batch_versions(&env.lib, row.0), 1);
    }
    assert_eq!(
        env.lib.db().read(|c| pending_batch(c, ids[0])).unwrap(),
        None
    );
    let env2 = BatchEnv::with(None, None, false);
    let recipe2 = env2.recipe("反転", &g, Vec::new());
    let ids2: Vec<i64> = (30..=33).map(|k| env2.png(k).0).collect();
    let job2 = env2.start(recipe2, &ids2, BatchMode::Overwrite, false);
    env2.jobs.cancel(job2).unwrap();
    assert_eq!(env2.jobs.get(job2).unwrap().state, JobState::Canceled);
    assert!(env2.rows(job2).iter().all(|r| r.1 == "canceled"));
    assert_consistent(&env.lib);
    assert_consistent(&env2.lib);
}

/// アイテム `item` を待っている一括のジョブ。
fn job_of(lib: &Library, item: i64) -> i64 {
    lib.db().read(|c| pending_batch(c, item)).unwrap().unwrap()
}

/// `n` 回目（0 から）のコミットの直前に `act` するジョブの割り込み。
fn at_commit(
    n: usize,
    act: impl Fn(&JobManager, &Library, i64) + Send + Sync + 'static,
) -> JobsHook {
    let calls = AtomicUsize::new(0);
    Arc::new(move |jobs, lib, item| {
        if calls.fetch_add(1, Ordering::SeqCst) == n {
            act(jobs, lib, item);
        }
    })
}

/// B4・B12：動いている一括を一時停止すると、書き終えた分をコミットしてから止まり（その行は saved、版はちょうど 1 つ）、残りは
/// pending のまま（版はない）。一時停止の間、待っているアイテムはエディタで開かず（SavePending）、保存も積まない（Busy）。
/// 再開すると残りだけを処理する（二重にしない）。
#[test]
fn b4_b12_pausing_a_running_batch_keeps_the_rest_pending() {
    let env = BatchEnv::with_jobs_hook(at_commit(2, |jobs, lib, item| {
        jobs.pause(job_of(lib, item)).unwrap();
    }));
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=40).map(|k| env.png(k).0).collect();
    let job = env.start(recipe, &ids, BatchMode::Overwrite, false);
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Paused);
    let rows = env.rows(job);
    let saved: Vec<i64> = rows
        .iter()
        .filter(|r| r.1 == "saved")
        .map(|r| r.0)
        .collect();
    let waiting: Vec<i64> = rows
        .iter()
        .filter(|r| r.1 == "pending")
        .map(|r| r.0)
        .collect();
    assert!(saved.len() >= 3, "{rows:?}");
    assert!(!waiting.is_empty(), "{rows:?}");
    assert_eq!(saved.len() + waiting.len(), ids.len(), "{rows:?}");
    for id in &saved {
        assert_eq!(batch_versions(&env.lib, *id), 1);
    }
    for id in &waiting {
        assert_eq!(batch_versions(&env.lib, *id), 0);
    }
    // B12：待っているアイテムは開かない・保存を積まない
    let w = waiting[0];
    assert_eq!(
        env.lib
            .db()
            .read(|c| super::save::pending_save(c, w))
            .unwrap(),
        Some(job)
    );
    let opened = super::editor::open(
        &super::editor::EditorEnv {
            library: &env.lib,
            jobs: &env.jobs,
            engine: &env.engine,
        },
        &super::editor::OpenRequest {
            item_id: w,
            viewport: super::editor::Viewport::whole(),
            from_draft: false,
        },
    );
    assert!(
        matches!(opened, Err(super::editor::EditorError::SavePending { job: j }) if j == job),
        "{:?}",
        opened.err()
    );
    assert!(env.engine.current_editor().is_none());
    let save = super::save::start_save(
        env.lib.db(),
        &env.jobs,
        &super::save::SaveRequest {
            item_id: w,
            base: super::save::Base {
                hash: head(&env.lib, w).hash,
                version: 0,
            },
            action: super::save::SaveAction::Overwrite {
                graph: doc(&[flip("horizontal")], None),
            },
        },
    );
    assert!(matches!(save, Err(crate::CoreError::Busy(_))), "{save:?}");
    // 再開すると残りだけ
    env.jobs.resume(job).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
    for row in env.rows(job) {
        assert_eq!(row.1, "saved");
        assert_eq!(batch_versions(&env.lib, row.0), 1, "アイテム {}", row.0);
    }
    assert_consistent(&env.lib);
}

/// B5：動いている一括を中止すると、中止の前にコミットした行は saved のまま残り（版はちょうど 1 つ）、まとまりの残りと書き終えた
/// 分は落として、残りの行はすべて canceled（版はない）。staging に書きかけを残さない。
#[test]
fn b5_canceling_a_running_batch_keeps_the_finished_rows() {
    let env = BatchEnv::with_jobs_hook(at_commit(2, |jobs, lib, item| {
        jobs.cancel(job_of(lib, item)).unwrap();
    }));
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=40).map(|k| env.png(k).0).collect();
    let job = env.start(recipe, &ids, BatchMode::Overwrite, false);
    assert!(env.jobs.wait_idle(WAIT));
    let record = env.jobs.get(job).unwrap();
    assert_eq!(record.state, JobState::Canceled);
    let counts = &result_of(&record)["counts"];
    assert_eq!(
        (counts["saved"].clone(), counts["canceled"].clone()),
        (json!(3), json!(37)),
        "{counts}"
    );
    for row in env.rows(job) {
        let want = usize::from(row.1 == "saved");
        assert!(row.1 == "saved" || row.1 == "canceled", "{row:?}");
        assert_eq!(batch_versions(&env.lib, row.0), want, "{row:?}");
    }
    assert_consistent(&env.lib);
}

/// 中止を書いたあとにアプリが終わる（行は canceled、ジョブは実行中のまま）と、開き直した一括は残りを当てずに中止で終える。
#[test]
fn a_batch_canceled_before_a_crash_stays_canceled() {
    let env = BatchEnv::with(None, None, false);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=4).map(|k| env.png(k).0).collect();
    let job = env.start(recipe, &ids, BatchMode::Overwrite, false);
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE batch_items SET state = 'canceled', finished_at = 1 WHERE job_id = ?1 AND ord >= 2",
                [job],
            )?;
            Ok(())
        })
        .unwrap();
    let (dir, session) = reopen(env, job);
    let lib = session.library();
    assert_eq!(session.jobs().get(job).unwrap().state, JobState::Canceled);
    let states: Vec<String> = rows_of(lib, job).into_iter().map(|r| r.1).collect();
    assert_eq!(states, ["canceled"; 4]);
    for id in ids {
        assert_eq!(batch_versions(lib, id), 0);
    }
    assert_consistent(lib);
    drop(session);
    drop(dir);
}

/// 保存のジョブの代わり：一括が実行中でなくなる（譲って待ちに戻る・止まる）まで動き続け、見えたら終わる。
struct WaitsForTheBatch {
    /// 一括が待ちに戻ったのを見た。
    saw_queued: Arc<AtomicBool>,
}

impl JobHandler for WaitsForTheBatch {
    fn run(&self, ctx: &JobContext) -> crate::Result<JobEnd> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let state: Option<String> = ctx.library().db().read(|c| {
                Ok(c.query_row(
                    "SELECT state FROM jobs WHERE kind = 'batch' ORDER BY id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )?)
            })?;
            match state.as_deref() {
                Some("queued") => {
                    self.saw_queued.store(true, Ordering::SeqCst);
                    break;
                }
                Some("running") => {}
                _ => break,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(JobEnd::Done {
            result_json: "{}".into(),
            message: None,
        })
    }
}

/// B13：保存のジョブが積まれると、一括はまとまりの区切りで譲り（書き終えた分をコミットし、準備の途中のものは落として行は pending の
/// まま）、待ちに戻る。保存が終わると残りの pending の行から続ける（どの行もちょうど 1 回）。
#[test]
fn b13_a_batch_yields_to_a_save_and_resumes() {
    let saw_queued = Arc::new(AtomicBool::new(false));
    let env = BatchEnv::build(
        None,
        None,
        true,
        Hooks {
            jobs: Some(at_commit(0, |jobs, _, _| {
                jobs.enqueue(JobKind::Edit, "{}".into()).unwrap();
            })),
            edit: Some(Arc::new(WaitsForTheBatch {
                saw_queued: saw_queued.clone(),
            })),
            ..Hooks::default()
        },
    );
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=40).map(|k| env.png(k).0).collect();
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    assert!(saw_queued.load(Ordering::SeqCst), "一括が譲らなかった");
    for row in env.rows(record.id) {
        assert_eq!(row.1, "saved");
        assert_eq!(batch_versions(&env.lib, row.0), 1, "アイテム {}", row.0);
    }
    let log = std::fs::read_to_string(env.lib.layout().job_log_path(record.id)).unwrap();
    assert!(log.contains("ほかのジョブに譲るため中断"), "{log}");
    assert_eq!(log.matches("開始").count(), 2, "{log}");
    assert_consistent(&env.lib);
}

/// B13（譲ると決めたあとの中止）：保存のジョブが積まれたのと同じときに中止を受けると、中止が勝つ（保存のあとに再開しない。
/// 残りの行は canceled）。
#[test]
fn b13_a_cancel_wins_over_yielding() {
    let env = BatchEnv::build(
        None,
        None,
        true,
        Hooks {
            jobs: Some(at_commit(0, |jobs, lib, item| {
                jobs.enqueue(JobKind::Edit, "{}".into()).unwrap();
                jobs.cancel(job_of(lib, item)).unwrap();
            })),
            edit: Some(Arc::new(WaitsForTheBatch {
                saw_queued: Arc::default(),
            })),
            ..Hooks::default()
        },
    );
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=40).map(|k| env.png(k).0).collect();
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_eq!(record.state, JobState::Canceled);
    let rows = env.rows(record.id);
    assert_eq!(
        rows.iter().filter(|r| r.1 == "saved").count(),
        1,
        "{rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|r| r.1 == "canceled").count(),
        39,
        "{rows:?}"
    );
    assert_consistent(&env.lib);
}

/// 働き手の 1 件が panic しても、その行を failed（Internal）にして先へ進み、ジョブは終わる（配った数が戻らず止まらない）。
#[test]
fn a_panicking_item_fails_alone() {
    let bad = Arc::new(AtomicI64::new(-1));
    let target = bad.clone();
    let env = BatchEnv::build(
        None,
        None,
        true,
        Hooks {
            prepare: Some(Arc::new(move |_, item| {
                if item == target.load(Ordering::SeqCst) {
                    panic!("試験の panic");
                }
            })),
            ..Hooks::default()
        },
    );
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=6).map(|k| env.png(k).0).collect();
    bad.store(ids[2], Ordering::SeqCst);
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    for row in env.rows(record.id) {
        if row.0 == ids[2] {
            assert_eq!(
                (row.1.as_str(), row.2.as_deref()),
                ("failed", Some("Internal"))
            );
        } else {
            assert_eq!(row.1, "saved", "{row:?}");
        }
    }
    assert_consistent(&env.lib);
}

/// 準備の途中（頭を読んだあと）でゴミ箱に入れられたアイテムは、頭を読み直してやり直し、skipped（Trashed）になる（failed にしない）。
#[test]
fn an_item_trashed_while_preparing_is_skipped_as_trashed() {
    let once = Arc::new(AtomicBool::new(false));
    let env = BatchEnv::build(
        None,
        None,
        true,
        Hooks {
            prepare: Some(Arc::new(move |lib, item| {
                if !once.swap(true, Ordering::SeqCst) {
                    lib.db()
                        .write(move |tx| {
                            tx.execute("UPDATE items SET trashed_at = 1 WHERE id = ?1", [item])?;
                            Ok(())
                        })
                        .unwrap();
                }
            })),
            ..Hooks::default()
        },
    );
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let (a, _) = env.png(1);
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    let row = &env.rows(record.id)[0];
    assert_eq!(
        (row.1.as_str(), row.2.as_deref()),
        ("skipped", Some("Trashed")),
        "{row:?}"
    );
    assert_eq!(batch_versions(&env.lib, a), 0);
    assert_consistent(&env.lib);
}

/// B10：区切りの同期は、まとまりのコミットの前に取る（どのコミットの前にも 1 回以上取っていて、1 回の区切りでコミットするのは
/// 32 件まで）。件数の 32 で区切るので、70 件なら 3 回以上。
#[test]
fn b10_a_barrier_precedes_each_group() {
    let env = BatchEnv::new();
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=70).map(|k| env.png(k).0).collect();
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    let at = env.durability.at_commit.lock().unwrap().clone();
    assert_eq!(at.len(), 70);
    assert!(at[0] >= 1, "{at:?}");
    assert!(at.windows(2).all(|w| w[0] <= w[1]), "{at:?}");
    let mut per: std::collections::BTreeMap<usize, usize> = Default::default();
    for b in &at {
        *per.entry(*b).or_default() += 1;
    }
    assert!(
        per.values().all(|n| *n <= super::batch::GROUP_ITEMS),
        "{per:?}"
    );
    assert!(env.durability.barriers.load(Ordering::SeqCst) >= 3);
    assert_consistent(&env.lib);
}

/// B3（別名）：別名の一括を n 件コミットしたあとに強制終了しても、開き直すとどの元にもコピーはちょうど 1 つ（行の out_item_id が
/// 指す）。
#[test]
fn b3_a_crash_in_a_save_as_batch_makes_each_copy_once() {
    for at in [Fault::AfterAnnounce, Fault::AfterPlace, Fault::AfterCommit] {
        let env = BatchEnv::with(None, Some((2, at)), true);
        let g = doc(&[flip("vertical")], None);
        let recipe = env.recipe("反転", &g, Vec::new());
        let ids: Vec<i64> = (1..=6).map(|k| env.png(k).0).collect();
        let record = env.run(
            recipe,
            &ids,
            BatchMode::SaveAs {
                name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
            },
        );
        assert_eq!(record.state, JobState::Failed, "{at:?}");
        let job = record.id;
        let (dir, session) = reopen(env, job);
        let lib = session.library();
        assert_done(&session.jobs().get(job).unwrap());
        let rows = rows_of(lib, job);
        let mut copies: Vec<i64> = rows.iter().map(|r| r.4.unwrap()).collect();
        assert!(rows.iter().all(|r| r.1 == "saved"), "{at:?} {rows:?}");
        copies.sort();
        copies.dedup();
        assert_eq!(copies.len(), ids.len(), "{at:?}");
        assert_eq!(
            count(
                lib,
                "SELECT count(*) FROM items WHERE title LIKE '% のコピー'"
            ),
            ids.len() as i64,
            "{at:?}"
        );
        for id in &ids {
            assert!(versions_of(lib, *id).is_empty(), "元は変えない");
        }
        assert_consistent(lib);
        drop(session);
        drop(dir);
    }
}

/// B6：飛ばす決まり。ゴミ箱・前の保存が終わっていない・上書きで下書きがある（含めると当てる）・上書きで書けない形式（偽の
/// HEIC。別名なら PNG で書く）。
#[test]
fn b6_skip_rules() {
    let decoder = heic_decoder(false);
    let set = heic_codecs(&decoder);
    let env = BatchEnv::with(Some(set), None, true);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let (trashed, _) = env.png(1);
    let (pending, _) = env.png(2);
    let (drafted, _) = env.png(3);
    let (heic, _) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    env.lib
        .db()
        .write(move |tx| {
            tx.execute("UPDATE items SET trashed_at = 1 WHERE id = ?1", [trashed])?;
            // 前の保存（一時停止のまま）
            tx.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('edit', 'paused', ?1, 1)",
                [format!(
                    r#"{{"v":1,"itemId":{pending},"base":{{"hash":"{}","version":0}},"action":{{"type":"revert","to":0}}}}"#,
                    "00".repeat(32)
                )],
            )?;
            Ok(())
        })
        .unwrap();
    super::drafts::save(
        env.lib.db(),
        drafted,
        &doc(&[flip("horizontal")], None),
        super::save::Base {
            hash: head(&env.lib, drafted).hash,
            version: 0,
        },
        None,
    )
    .unwrap();
    let ids = [trashed, pending, drafted, heic];
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    let codes: Vec<(String, Option<String>)> = env
        .rows(record.id)
        .into_iter()
        .map(|r| (r.1, r.2))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("skipped".into(), Some("Trashed".into())),
            ("skipped".into(), Some("SavePending".into())),
            ("skipped".into(), Some("Draft".into())),
            ("skipped".into(), Some("FormatUnavailable".into())),
        ]
    );
    // 下書きを含める・別名なら書けない形式を PNG で
    let job = env.start(recipe, &[drafted], BatchMode::Overwrite, true);
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.rows(job)[0].1, "saved");
    let record = env.run(
        recipe,
        &[heic],
        BatchMode::SaveAs {
            name: "{名前} の PNG".into(),
        },
    );
    let row = &env.rows(record.id)[0];
    assert_eq!(row.1, "saved", "{row:?}");
    assert_eq!(head(&env.lib, row.4.unwrap()).ext, "png");
    // 前の保存のジョブは動かさずに消しておく（整合性の確かめのため）
    env.lib
        .db()
        .write(|tx| {
            tx.execute("UPDATE jobs SET state = 'canceled' WHERE kind = 'edit'", [])?;
            Ok(())
        })
        .unwrap();
    assert_consistent(&env.lib);
}

/// B8：編集済みのアイテムには頭の文書に継ぎ足す（版 2 の文書は頭の操作とレシピの操作。値は両方を当てたもの）。
#[test]
fn b8_edited_items_compose_onto_the_head() {
    let env = BatchEnv::new();
    let (a, png) = env.png(5);
    let save = super::save::start_save(
        env.lib.db(),
        &env.jobs,
        &super::save::SaveRequest {
            item_id: a,
            base: super::save::Base {
                hash: head(&env.lib, a).hash,
                version: 0,
            },
            action: super::save::SaveAction::Overwrite {
                graph: doc(&[flip("horizontal")], None),
            },
        },
    )
    .unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(save).unwrap());
    let recipe = env.recipe("縦", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    let both = doc(&[flip("horizontal"), flip("vertical")], None);
    assert_eq!(head(&env.lib, a).hash, hash_of(&render_png(&png, &both)));
    let v2 = versions_of(&env.lib, a)[2].clone();
    let g = env
        .engine
        .runtime()
        .unwrap()
        .preview(|e| env.lib.db().read(|c| super::versions::doc_of(c, e, a, 2)))
        .unwrap();
    let ops: Vec<&str> = g.nodes.iter().map(|n| n.op.as_str()).collect();
    assert_eq!(ops, ["image.flip", "image.flip"]);
    assert_eq!(v2.base_version, Some(1));
    assert_consistent(&env.lib);
}

/// B9：結び付けは素材の設定から（格子で切って並べ直す）。設定のないアイテムは当てられない（skipped）。
#[test]
fn b9_bindings_read_the_asset_settings() {
    let env = BatchEnv::new();
    let mut g = Graph::new_single();
    g.push_linear(
        "sprite.split_grid",
        1,
        json!({ "grid": { "cellW": 8, "cellH": 8, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 },
                "durationMs": 100, "skipEmpty": false }),
        0,
    )
    .unwrap();
    g.push_linear(
        "sprite.pack_grid",
        1,
        json!({ "columns": 1, "gapX": 0, "gapY": 0, "fill": null }),
        0,
    )
    .unwrap();
    let bind = Binding {
        node: NodeId(1),
        param: "grid".into(),
        part: None,
        from: Source::Setting {
            key: SettingKey::Grid,
        },
    };
    let recipe = env.recipe("並べ直す", &g, vec![bind]);
    let (with, _) = env.png(7);
    let (without, _) = env.png(8);
    env.lib
        .db()
        .write(move |tx| {
            crate::asset::set_settings(
                tx,
                &[with],
                &crate::asset::SettingsPatch {
                    grid: Some(crate::asset::Change::Set(crate::asset::GridSetting {
                        cell_w: 4,
                        cell_h: 4,
                        offset_x: 0,
                        offset_y: 0,
                        gap_x: 0,
                        gap_y: 0,
                        count: None,
                    })),
                    ..Default::default()
                },
                1,
            )?;
            Ok(())
        })
        .unwrap();
    let record = env.run(recipe, &[with, without], BatchMode::Overwrite);
    assert_done(&record);
    let rows = env.rows(record.id);
    assert_eq!(rows[0].1, "saved", "{rows:?}");
    assert_eq!(
        (rows[1].1.as_str(), rows[1].2.as_deref()),
        ("skipped", Some("NotApplicable"))
    );
    let rec = versions_of(&env.lib, with)[1].record().unwrap();
    let bound = &rec.batch.unwrap()["bound"];
    assert!(bound.to_string().contains("grid"), "{bound}");
    assert_consistent(&env.lib);
}

/// B11：前回 OS のデコーダで読んでいる途中で落ちた行（decoding = 1）は、開き直すと failed（Crashed）にして先へ進む。
#[test]
fn b11_a_row_that_crashed_while_decoding_fails_once() {
    let env = BatchEnv::with(None, None, false);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=3).map(|k| env.png(k).0).collect();
    let job = env.start(recipe, &ids, BatchMode::Overwrite, false);
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE batch_items SET decoding = 1 WHERE job_id = ?1 AND ord = 1",
                [job],
            )?;
            Ok(())
        })
        .unwrap();
    env.jobs.start().unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let states: Vec<(String, Option<String>)> =
        env.rows(job).into_iter().map(|r| (r.1, r.2)).collect();
    assert_eq!(
        states,
        vec![
            ("saved".into(), None),
            ("failed".into(), Some("Crashed".into())),
            ("saved".into(), None),
        ]
    );
    assert_consistent(&env.lib);
}

/// B14：動いている一括のレシピの文書は、レシピを消しても GC の部品の片付けが残す（ジョブの入力の写し）。
#[test]
fn b14_gc_keeps_the_recipe_of_a_pending_batch() {
    let env = BatchEnv::with(None, None, false);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let (a, _) = env.png(1);
    let doc_root = env.lib.db().read(|c| recipes::doc_root(c, recipe)).unwrap();
    let job = env.start(recipe, &[a], BatchMode::Overwrite, false);
    env.lib
        .db()
        .write(move |tx| recipes::delete(tx, recipe))
        .unwrap();
    let roots = env.lib.db().read(|c| super::save::gc_roots(c, 0)).unwrap();
    assert!(roots.contains(&doc_root));
    env.lib
        .db()
        .write(move |tx| super::gc::sweep_parts(tx, super::gc::Scope::All, &roots))
        .unwrap();
    // レシピを消しても、一括は写しで動く
    env.jobs.start().unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
    assert_eq!(env.rows(job)[0].1, "saved");
    assert_consistent(&env.lib);
}

/// B15：音声は流して書く（WAV → WAV）。一括の版は job_id が空で、行の version_id が指す。
#[test]
fn b15_audio_items_stream() {
    let env = BatchEnv::new();
    let pinned = CodecSet::pinned();
    let ids: Vec<i64> = (0..3)
        .map(|k| {
            let bytes = noise_file(&pinned, "wav", 2, 9_000, 70 + k);
            add_audio(&env.lib, &bytes, "wav", &format!("音{k}")).0
        })
        .collect();
    let recipe = env.recipe("下げる", &audio_doc(-3.0, None), Vec::new());
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    for row in env.rows(record.id) {
        assert_eq!(row.1, "saved", "{row:?}");
        assert_eq!(batch_versions(&env.lib, row.0), 1);
    }
    let _ = NoMonitor;
    let _ = params![1];
    assert_consistent(&env.lib);
}

/// 一括の外で 1 件を上書きする場（土台の競合を起こす割り込みが使う）。
struct Other<'a>(&'a Library);

impl super::save::UnitCx for Other<'_> {
    fn library(&self) -> &Library {
        self.0
    }

    fn control(&self) -> crate::jobs::Control {
        crate::jobs::Control::Continue
    }

    fn progress(&self, _: f64, _: &str) {}

    fn log(&self, _: &str) {}

    fn mark(&self, _: &str, _: f64, _: &str) -> crate::Result<()> {
        Ok(())
    }

    fn job(&self) -> i64 {
        9999
    }

    fn version_job(&self) -> Option<i64> {
        None
    }

    fn tag(&self) -> String {
        "other".into()
    }
}

/// `lib` のアイテム `item` を、一括の外で反転して上書きする（頭の版が 1 つ進む）。
fn overwrite_outside(lib: &Library, engine: &Arc<EngineServices>, item: i64) {
    let saver = super::save::Saver::new(engine.clone());
    let unit = Other(lib);
    let g = doc(&[flip("horizontal")], None);
    let parts = kura_engine::doc_parts::split(&g).unwrap();
    let root = parts.root;
    lib.db()
        .write(move |tx| super::store::put_doc(tx, &parts, 1))
        .unwrap();
    let h = head(lib, item);
    let written = match saver
        .prepare(
            &unit,
            super::save::Mode::Overwrite,
            item,
            super::save::Base {
                hash: h.hash,
                version: h.version,
            },
            root,
            None,
        )
        .unwrap()
    {
        super::save::Flow::Go(w) => w,
        super::save::Flow::Stop(_) => panic!("止まりました"),
    };
    saver
        .commit(&unit, written, Arc::new(|_, _| Ok(())))
        .unwrap();
}

/// B7：準備してからコミットするまでに頭が変われば（一括の外の保存）、1 回だけ頭を読み直してやり直し、新しい頭に継ぎ足す。
/// コミットのたびに変わり続ければ、2 回目で skipped（Conflict）にして先へ進む。
#[test]
fn b7_a_conflict_is_retried_once() {
    // 1 回だけ変わる
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let env = BatchEnv::with_hook(Arc::new(move |lib, engine, item| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            overwrite_outside(lib, engine, item);
        }
    }));
    let (a, png) = env.png(9);
    let recipe = env.recipe("縦", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    assert_eq!(env.rows(record.id)[0].1, "saved");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "やり直して 2 回コミットした"
    );
    let both = doc(&[flip("horizontal"), flip("vertical")], None);
    assert_eq!(head(&env.lib, a).hash, hash_of(&render_png(&png, &both)));
    let vs = versions_of(&env.lib, a);
    assert_eq!(vs.len(), 3);
    assert_eq!((vs[2].base_version, vs[2].job_id), (Some(1), None));
    assert_consistent(&env.lib);
    // 変わり続ける
    let env = BatchEnv::with_hook(Arc::new(overwrite_outside));
    let (b, _) = env.png(10);
    let recipe = env.recipe("縦", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(recipe, &[b], BatchMode::Overwrite);
    assert_done(&record);
    let row = &env.rows(record.id)[0];
    assert_eq!(
        (row.1.as_str(), row.2.as_deref()),
        ("skipped", Some("Conflict"))
    );
    // 一括の版は足さない（行は版を指さず、版は元と一括の外の 2 回だけ）
    assert_eq!(row.3, None);
    assert_eq!(versions_of(&env.lib, b).len(), 3);
    assert_consistent(&env.lib);
}

/// kura-engine のフィクスチャ（他のツールで作ったファイル）。
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kura-engine/fixtures")
            .join(name),
    )
    .unwrap()
}

/// 格子で切って縦に並べ直すレシピの文書（格子は `grid`）。
fn regrid(grid: Json) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear(
        "sprite.split_grid",
        1,
        json!({ "grid": grid, "durationMs": 100, "skipEmpty": false }),
        0,
    )
    .unwrap();
    g.push_linear(
        "sprite.pack_grid",
        1,
        json!({ "columns": 1, "gapX": 0, "gapY": 0, "fill": null }),
        0,
    )
    .unwrap();
    g
}

/// 格子の設定を素材に置く。
fn set_grid(lib: &Library, item: i64, g: crate::asset::GridSetting) {
    lib.db()
        .write(move |tx| {
            crate::asset::set_settings(
                tx,
                &[item],
                &crate::asset::SettingsPatch {
                    grid: Some(crate::asset::Change::Set(g)),
                    ..Default::default()
                },
                1,
            )?;
            Ok(())
        })
        .unwrap();
}

fn grid_binding() -> Binding {
    Binding {
        node: NodeId(1),
        param: "grid".into(),
        part: None,
        from: Source::Setting {
            key: SettingKey::Grid,
        },
    }
}

/// B9（値）：結び付けた格子の値（幅と高さ・ずらしと間が違う）が出力に効く。素材の設定の値をそのまま書いた文書で描いたものと
/// 同じバイト列。
#[test]
fn b9_bound_values_reach_the_output() {
    let env = BatchEnv::new();
    let recipe = env.recipe(
        "並べ直す",
        &regrid(
            json!({ "cellW": 8, "cellH": 8, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 }),
        ),
        vec![grid_binding()],
    );
    let (a, png) = env.png(7);
    set_grid(
        &env.lib,
        a,
        crate::asset::GridSetting {
            cell_w: 6,
            cell_h: 5,
            offset_x: 1,
            offset_y: 0,
            gap_x: 0,
            gap_y: 1,
            count: None,
        },
    );
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    assert_eq!(env.rows(record.id)[0].1, "saved");
    let want =
        regrid(json!({ "cellW": 6, "cellH": 5, "offsetX": 1, "offsetY": 0, "gapX": 0, "gapY": 1 }));
    assert_eq!(head(&env.lib, a).hash, hash_of(&render_png(&png, &want)));
    assert_consistent(&env.lib);
}

/// B9（終わりからの秒）：見出しの長さが上限しか分からない MP3 でも、終わりからの秒を結び付けられる（形を読めた段で流して数えた
/// 長さで。エディタがデコードした値の長さで結び付けるのと同じ値）。
#[test]
fn b9_an_end_anchored_time_binds_an_mp3_by_its_decoded_length() {
    let env = BatchEnv::new();
    let bytes = fixture("mp3-lame.mp3");
    let (a, _) = add_audio(&env.lib, &bytes, "mp3", "曲");
    let mut g = Graph::new_single();
    g.push_linear("audio.trim", 1, json!({ "start": 0, "end": null }), 0)
        .unwrap();
    g.outputs[0].format = FormatChoice::To {
        format: "wav".into(),
        options: json!({}),
    };
    let bind = Binding {
        node: NodeId(1),
        param: "end".into(),
        part: None,
        from: Source::Time {
            ms: 250,
            anchor: TimeAnchor::End,
        },
    };
    let recipe = env.recipe("終わりを切る", &g, vec![bind]);
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    let row = &env.rows(record.id)[0];
    assert_eq!(row.1, "saved", "{row:?}");
    // 結び付けた値は、元をデコードした長さで当てたものと同じ
    let runtime = env.engine.runtime().unwrap().clone();
    let limits = runtime.bulk(|e| e.limits().clone());
    let (value, _) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(&bytes),
            "mp3",
            &Default::default(),
            &limits,
            &NoMonitor,
        )
        .unwrap();
    let stored = recipes::get(env.lib.db(), &runtime, recipe).unwrap().recipe;
    let shapes = std::collections::BTreeMap::from([(InputId(0), value.shape())]);
    let want = runtime
        .bulk(|e| e.instantiate(&Graph::new_single(), &stored, &shapes, &Facts::default()))
        .unwrap();
    let want: Json = serde_json::from_slice(&bound_json(&want.bound).unwrap()).unwrap();
    let rec = versions_of(&env.lib, a)[1].record().unwrap();
    assert_eq!(rec.batch.unwrap()["bound"], want);
    assert_consistent(&env.lib);
}

/// B6（書けない・読めない・合わない）：上書きで書き手のない形式（SVG）は skipped（FormatUnavailable）で、別名なら PNG で書く。
/// 元をどの段も読めない（MP3 の中身が Layer II）なら、上書きでも別名でも failed（書けない形式とは数えず、代わりの形式で読み
/// 直さない）。画像のレシピを音声に当てると skipped（NotApplicable）。
#[test]
fn b6_unwritable_unreadable_and_unfit_items() {
    let env = BatchEnv::new();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6"><rect width="8" height="6" fill="red"/></svg>"#;
    let (vector, _) = add_item(&env.lib, svg, "svg", "図");
    let (layer2, _) = add_audio(&env.lib, &fixture("mp2-layer2.mp3"), "mp3", "古い曲");
    let pinned = CodecSet::pinned();
    let (sound, _) = add_audio(
        &env.lib,
        &noise_file(&pinned, "wav", 2, 4_000, 3),
        "wav",
        "音",
    );
    let image = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let audio = env.recipe("下げる", &audio_doc(-3.0, None), Vec::new());
    let record = env.run(image, &[vector, sound], BatchMode::Overwrite);
    assert_done(&record);
    let codes: Vec<(String, Option<String>)> = env
        .rows(record.id)
        .into_iter()
        .map(|r| (r.1, r.2))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("skipped".into(), Some("FormatUnavailable".into())),
            ("skipped".into(), Some("NotApplicable".into())),
        ]
    );
    let record = env.run(
        image,
        &[vector],
        BatchMode::SaveAs {
            name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
        },
    );
    let row = &env.rows(record.id)[0];
    assert_eq!(row.1, "saved", "{row:?}");
    assert_eq!(head(&env.lib, row.4.unwrap()).ext, "png");
    for mode in [
        BatchMode::Overwrite,
        BatchMode::SaveAs {
            name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
        },
    ] {
        let record = env.run(audio, &[layer2], mode.clone());
        let row = &env.rows(record.id)[0];
        assert_eq!(row.1, "failed", "{mode:?} {row:?}");
        assert_ne!(row.2.as_deref(), Some("FormatUnavailable"), "{mode:?}");
    }
    assert_consistent(&env.lib);
}

/// 上書きの出力が別のアイテムの今と同じ中身になる（見た目が同じ別のファイル）なら、土台の競合としてやり直さず existing にする
/// （版を足さない）。
#[test]
fn an_overwrite_matching_another_item_is_existing() {
    let env = BatchEnv::new();
    let value = InputGen::PixelSprite {
        w: 24,
        h: 20,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let (a, png) = env.png(1);
    let fast = CodecSet::pinned()
        .encode_bytes(&value, "png", &json!({ "compression": "fast" }), &NoMonitor)
        .unwrap()
        .0;
    assert_ne!(fast, png);
    let (b, _) = add_item(&env.lib, &fast, "png", "絵の写し");
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(recipe, &[a, b], BatchMode::Overwrite);
    assert_done(&record);
    let rows = env.rows(record.id);
    let mut states: Vec<(String, Option<String>)> =
        rows.iter().map(|r| (r.1.clone(), r.2.clone())).collect();
    states.sort();
    assert_eq!(
        states,
        vec![
            ("existing".into(), Some("Existing".into())),
            ("saved".into(), None),
        ],
        "{rows:?}"
    );
    assert_eq!(batch_versions(&env.lib, a) + batch_versions(&env.lib, b), 1);
    let log = std::fs::read_to_string(env.lib.layout().job_log_path(record.id)).unwrap();
    assert!(!log.contains("読み直してやり直します"), "{log}");
    assert_consistent(&env.lib);
}

/// 偽の HEIC の読み手を包み、形・値を読むたびに、印（decoding）の立っている一括の行の数を数える。
struct Watching {
    inner: Arc<FakePlatformDecoder>,
    lib: OnceLock<Arc<Library>>,
    seen: Mutex<Vec<i64>>,
}

impl Watching {
    fn look(&self) {
        if let Some(lib) = self.lib.get() {
            let n = count(lib, "SELECT count(*) FROM batch_items WHERE decoding = 1");
            self.seen.lock().unwrap().push(n);
        }
    }
}

impl Decoder for Watching {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn tier(&self) -> Tier {
        self.inner.tier()
    }

    fn reproducible(&self) -> bool {
        self.inner.reproducible()
    }

    fn env(&self) -> Option<CodecEnv> {
        self.inner.env()
    }

    fn can_decode(&self, format: &str) -> bool {
        self.inner.can_decode(format)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.look();
        self.inner.shape(src, format, opts, limits)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<kura_engine::Value, EngineError> {
        self.look();
        self.inner.decode(src, format, opts, limits, mon)
    }
}

/// B11（印を書く）：OS のデコーダで読む間は、一括の行の decoding が立っている（結び付けの形・保存の形・値のどの読みでも）。
/// 読み終えると外れる。
#[test]
fn b11_the_row_is_marked_while_the_os_decoder_reads() {
    let watching = Arc::new(Watching {
        inner: heic_decoder(false),
        lib: OnceLock::new(),
        seen: Mutex::new(Vec::new()),
    });
    let mut set = CodecSet::pinned();
    set.add_decoder(watching.clone()).unwrap();
    let env = BatchEnv::with(Some(Arc::new(Mutex::new(set))), None, true);
    let _ = watching.lib.set(env.lib.clone());
    let (a, _) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    set_grid(
        &env.lib,
        a,
        crate::asset::GridSetting {
            cell_w: 4,
            cell_h: 4,
            offset_x: 0,
            offset_y: 0,
            gap_x: 0,
            gap_y: 0,
            count: None,
        },
    );
    let mut g =
        regrid(json!({ "cellW": 8, "cellH": 8, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 }));
    g.outputs[0].format = FormatChoice::To {
        format: "png".into(),
        options: json!({}),
    };
    let recipe = env.recipe("並べ直す", &g, vec![grid_binding()]);
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    assert_eq!(env.rows(record.id)[0].1, "saved");
    let seen = watching.seen.lock().unwrap().clone();
    assert!(seen.len() >= 3, "{seen:?}");
    assert!(seen.iter().all(|n| *n == 1), "{seen:?}");
    assert_eq!(
        count(
            &env.lib,
            "SELECT count(*) FROM batch_items WHERE decoding = 1"
        ),
        0
    );
    assert_consistent(&env.lib);
}

/// 積むトランザクションの中で照らすレシピの変わり目：名前を変える・中身を差し替える・消すと、読んだときのものと違う。
#[test]
fn a_recipe_changed_after_reading_is_noticed() {
    let env = BatchEnv::with(None, None, false);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let runtime = env.engine.runtime().unwrap().clone();
    let read = || {
        let stored = recipes::get(env.lib.db(), &runtime, recipe).unwrap();
        let root = env.lib.db().read(|c| recipes::doc_root(c, recipe)).unwrap();
        (stored, root)
    };
    let unchanged = |stored: &recipes::StoredRecipe, root: &Digest| {
        env.lib
            .db()
            .read(|c| recipes::unchanged(c, recipe, stored, root))
    };
    let (stored, root) = read();
    assert!(unchanged(&stored, &root).unwrap());
    recipes::rename(env.lib.db(), recipe, "反転 2").unwrap();
    assert!(!unchanged(&stored, &root).unwrap());
    let (stored, root) = read();
    let other = doc(&[flip("horizontal")], None);
    let keep: Vec<NodeId> = other.nodes.iter().map(|n| n.id).collect();
    recipes::replace_from_editor(
        env.lib.db(),
        &runtime,
        recipe,
        EditorPick {
            graph: &other,
            keep: &keep,
            with_format: true,
            bindings: Vec::new(),
        },
    )
    .unwrap();
    assert!(!unchanged(&stored, &root).unwrap());
    let (stored, root) = read();
    env.lib
        .db()
        .write(move |tx| recipes::delete(tx, recipe))
        .unwrap();
    assert!(matches!(
        unchanged(&stored, &root),
        Err(crate::CoreError::NotFound(_))
    ));
}
