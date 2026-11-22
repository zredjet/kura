//! 一括加工のジョブの試験（M14 段 4。docs/editing.md §14「一括加工の受け入れテスト」の B1〜）。固定の段のコーデック（と偽の
//! Platform のデコーダ）と偽のエンジンの部品で、Linux でも回る。耐久は数える偽物（ファイルの同期は `sync_all`）で、区切りの
//! 同期の回数を見る。どの試験も終わりに整合性・予告・staging・版の不変条件を確かめる。

use std::collections::BTreeMap;
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

use super::GcHandler;
use super::batch::precheck::{Loss, OutputGroup, ShapeInfo};
use super::batch::{
    BatchAction, BatchHandler, BatchMode, BatchRequest, Precheck, PrecheckCx, RecipeSource,
    pending_batch, precheck, start_batch, start_undo,
};
use super::recipes::{self, EditorPick};
use super::save::{EditHandler, Fault};
use super::save_stream_tests::{add_audio, audio_doc, noise_file};
use super::save_tests::{
    DAY, TestClock, WAIT, add_item, assert_consistent, count, doc, flip, hash_of, head,
    heic_codecs, heic_decoder, render_png, sink, sprite, versions_of,
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
    /// 一括の 1 件ずつの保存の段ごとの割り込み（作り直しの途中で止める）。
    save: Option<super::save::Hook>,
    /// 強制終了（`crash`）を、構えたあとだけ効かせる。
    crash_armed: Option<Arc<AtomicBool>>,
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
    /// GC の時計（[`BatchEnv::gc`] が 8 日進める）。
    clock: TestClock,
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
        if let Some(hook) = hooks.save {
            batch = batch.with_save_hook(hook);
        }
        batch.crash = crash;
        batch.crash_armed = hooks.crash_armed;
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
        let clock = TestClock::default();
        let jobs = Arc::new(
            JobManager::new(
                lib.clone(),
                vec![
                    (JobKind::Edit, edit),
                    (JobKind::Batch, Arc::new(batch)),
                    (
                        JobKind::Gc,
                        Arc::new(GcHandler::new(engine.clone(), clock.gc())),
                    ),
                ],
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
            clock,
        }
    }

    /// 8 日たったことにして GC を動かす。
    fn gc(&self) {
        self.clock.set(Some(crate::time::now_ms() + 8 * DAY));
        let id = self.jobs.enqueue(JobKind::Gc, "{}".into()).unwrap();
        assert!(self.jobs.wait_idle(WAIT));
        self.clock.set(None);
        assert_done(&self.jobs.get(id).unwrap());
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
                source: RecipeSource::Recipe(recipe),
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

// ---- 段 5：事前チェック・結果・元に戻す（C1〜C5・U1〜U4・R1。docs/editing.md §14.2）----

/// 事前チェック（中止しない。進みは 1 件ごとに呼ばれ、最後は (total, total)）。
fn precheck_of(
    env: &BatchEnv,
    source: RecipeSource,
    items: &[i64],
    mode: BatchMode,
    include_drafts: bool,
) -> Precheck {
    let canceled = AtomicBool::new(false);
    let calls = Mutex::new(Vec::new());
    let progress = |n: u64, total: u64| calls.lock().unwrap().push((n, total));
    let p = precheck(
        &env.lib,
        &env.engine,
        &BatchRequest {
            source,
            items: items.to_vec(),
            mode,
            include_drafts,
        },
        &PrecheckCx {
            canceled: &canceled,
            progress: &progress,
        },
    )
    .unwrap();
    let calls = calls.into_inner().unwrap();
    assert_eq!(calls.len() as u64, p.total);
    if p.total > 0 {
        assert!(calls.contains(&(p.total, p.total)), "{calls:?}");
    }
    p
}

/// 事前チェックの見込み（アイテム → (状態, code)）。当てる見込みは「当てる」。
fn predicted(p: &Precheck) -> BTreeMap<i64, (String, Option<String>)> {
    let mut out = BTreeMap::new();
    for item in &p.apply {
        out.insert(*item, ("当てる".to_string(), None));
    }
    for g in &p.skips {
        for item in &g.items {
            out.insert(*item, (g.state.clone(), Some(g.code.clone())));
        }
    }
    out
}

/// 一括の行（アイテム → (状態, code)）。版を足した・同じ中身は「当てる」にまとめる。
fn outcomes(env: &BatchEnv, job: i64) -> BTreeMap<i64, (String, Option<String>)> {
    env.rows(job)
        .into_iter()
        .map(|r| {
            let state = match r.1.as_str() {
                "saved" | "unchanged" | "existing" => ("当てる".to_string(), None),
                _ => (r.1, r.2),
            };
            (r.0, state)
        })
        .collect()
}

/// アイテムに格子の設定（セル `cell` 四方）を置く。
fn grid_of(env: &BatchEnv, item: i64, cell: u32) {
    env.lib
        .db()
        .write(move |tx| {
            crate::asset::set_settings(
                tx,
                &[item],
                &crate::asset::SettingsPatch {
                    grid: Some(crate::asset::Change::Set(crate::asset::GridSetting {
                        cell_w: cell,
                        cell_h: cell,
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
}

/// 上書きの保存（ジョブを待つ）。
fn overwrite(env: &BatchEnv, item: i64, g: Graph) {
    let h = head(&env.lib, item);
    let job = super::save::start_save(
        env.lib.db(),
        &env.jobs,
        &super::save::SaveRequest {
            item_id: item,
            base: super::save::Base {
                hash: h.hash,
                version: h.version,
            },
            action: super::save::SaveAction::Overwrite { graph: g },
        },
    )
    .unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
}

/// 下書きを置く（頭を土台に）。
fn put_draft(env: &BatchEnv, item: i64) {
    let h = head(&env.lib, item);
    super::drafts::save(
        env.lib.db(),
        item,
        &doc(&[flip("horizontal")], None),
        super::save::Base {
            hash: h.hash,
            version: h.version,
        },
        None,
    )
    .unwrap();
}

fn trash(env: &BatchEnv, item: i64) {
    env.lib
        .db()
        .write(move |tx| {
            crate::items::trash(tx, &[item], 1)?;
            Ok(())
        })
        .unwrap();
}

fn undo_and_wait(env: &BatchEnv, of: i64) -> JobRecord {
    let job = start_undo(env.lib.db(), &env.jobs, &env.engine, of).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    env.jobs.get(job).unwrap()
}

/// C1：事前チェックの見込みは、同じ頼みで一括を当てた行と同じになる（状態・code・言葉。当てる・ゴミ箱・下書き・上書きで書けない
/// 形式（SVG）・別名なら代わりの形式（PNG）・画像のレシピを音声に（NotApplicable）・元を読めない（MP3 の中身が Layer II）・
/// 音声のレシピを画像に・種類を変える形式（元を読む前に NotApplicable））。ライブラリにない ID は別に返す。
#[test]
fn c1_the_precheck_predicts_the_rows() {
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
    let (plain, _) = env.png(1);
    let (trashed, _) = env.png(2);
    let (drafted, _) = env.png(3);
    trash(&env, trashed);
    put_draft(&env, drafted);
    let image = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let mirror = env.recipe("左右", &doc(&[flip("horizontal")], None), Vec::new());
    let audio = env.recipe("下げる", &audio_doc(-3.0, None), Vec::new());
    let webp = env.recipe(
        "WebP に",
        &doc(&[flip("vertical")], Some("webp")),
        Vec::new(),
    );
    // ライブラリにない ID は確かめず（一括も行を入れない）、別に返す
    let missing = 9_999_999;
    let ids = [plain, trashed, drafted, vector, sound, layer2, missing];
    let save_as = BatchMode::SaveAs {
        name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
    };
    for (recipe, mode) in [
        (image, BatchMode::Overwrite),
        (mirror, save_as),
        (audio, BatchMode::Overwrite),
        // 種類を変える形式は、元を読む前に NotApplicable（読めない MP3 でも）
        (webp, BatchMode::Overwrite),
    ] {
        let p = precheck_of(
            &env,
            RecipeSource::Recipe(recipe),
            &ids,
            mode.clone(),
            false,
        );
        assert_eq!(p.total, ids.len() as u64 - 1);
        assert_eq!(p.missing, vec![missing]);
        let want = predicted(&p);
        let record = env.run(recipe, &ids, mode.clone());
        assert_done(&record);
        assert_eq!(outcomes(&env, record.id), want, "{mode:?} {p:#?}");
        // 理由の言葉も一括の行と同じ（群は言葉ごと）
        for g in &p.skips {
            let rows = super::batch::rows(
                env.lib.db(),
                record.id,
                Some(&g.state),
                Some(&g.code),
                0,
                100,
            )
            .unwrap();
            for row in rows.iter().filter(|r| g.items.contains(&r.item_id)) {
                assert_eq!(
                    row.message.as_deref(),
                    Some(g.message.as_str()),
                    "{mode:?} {}",
                    g.code
                );
            }
        }
        if matches!(mode, BatchMode::SaveAs { .. }) {
            // 別名は書けない形式を PNG で作る
            let fallback: Vec<&OutputGroup> = p.outputs.iter().filter(|g| g.fallback).collect();
            assert_eq!(fallback.len(), 1, "{p:#?}");
            assert_eq!(
                (fallback[0].format.as_str(), fallback[0].items.clone()),
                ("png", vec![vector])
            );
        }
    }
    assert_consistent(&env.lib);
}

/// C2：出力の形（元 24×20 → 4 倍の 96×80、例と変わる件数）、失うもの（JPEG は非可逆・透明。上書きで大きさが変わり格子の
/// 設定があれば素材の設定。別名は設定を写さないので出さない）、編集済みに継ぎ足すもの、ディスクの目安（元の大きさ × 値の比 16、
/// 上書きは版 1 以上の今のファイルが置き換わる）、メモリの見積もり。透明のある絵は JPEG に書けずに失敗する（描かないので
/// 「透明」は書けないかもしれないという見込み）。PNG なら当てると見込みの大きさになる。
#[test]
fn c2_the_precheck_reports_shapes_losses_and_disk() {
    let env = BatchEnv::new();
    let (a, png_a) = env.png(1);
    let (b, png_b) = env.png(2);
    overwrite(&env, b, doc(&[flip("horizontal")], None));
    grid_of(&env, a, 4);
    let g = doc(
        &[(
            "image.resize",
            json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        )],
        Some("jpeg"),
    );
    let recipe = env.recipe("4 倍の JPEG", &g, Vec::new());
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(recipe),
        &[a, b],
        BatchMode::Overwrite,
        false,
    );
    assert_eq!(p.apply, vec![a, b]);
    assert!(p.skips.is_empty() && p.unchecked.is_empty(), "{p:#?}");
    assert_eq!(p.composed, vec![b]);
    assert_eq!(p.outputs.len(), 1);
    let out = &p.outputs[0];
    assert_eq!(
        (
            out.format.as_str(),
            out.fallback,
            out.items.clone(),
            out.resized
        ),
        ("jpeg", false, vec![a, b], 2)
    );
    assert_eq!(
        (out.examples[0].from, out.examples[0].to),
        (
            ShapeInfo::Image {
                w: Some(24),
                h: Some(20),
                frames: None
            },
            ShapeInfo::Image {
                w: Some(96),
                h: Some(80),
                frames: None
            }
        )
    );
    let losses: Vec<(Loss, String, Vec<i64>)> = p
        .losses
        .iter()
        .map(|l| (l.loss, l.format.clone(), l.items.clone()))
        .collect();
    assert_eq!(
        losses,
        vec![
            (Loss::Lossy, "jpeg".into(), vec![a, b]),
            (Loss::Alpha, "jpeg".into(), vec![a, b]),
            (Loss::Settings, String::new(), vec![a]),
        ]
    );
    assert_eq!(
        p.disk.new_bytes,
        (png_a.len() + png_b.len()) as u64 * 16,
        "{p:#?}"
    );
    let current_b = versions_of(&env.lib, b)[1].size_bytes as u64;
    assert_eq!(p.disk.replaced_bytes, current_b);
    assert!(p.memory_peak > 0 && p.memory_peak <= p.memory_budget);
    // 別名：素材の設定は写さない・置き換えない
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(recipe),
        &[a, b],
        BatchMode::SaveAs {
            name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
        },
        false,
    );
    assert!(p.losses.iter().all(|l| l.loss != Loss::Settings), "{p:#?}");
    assert_eq!(p.disk.replaced_bytes, 0);
    // 透明のあるドット絵は JPEG に書けずに失敗する（失うものの「透明」）。PNG なら当てると見込みの大きさになる
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_eq!(
        outcomes(&env, record.id)[&a],
        ("failed".into(), Some("Unsupported".into()))
    );
    let mut g = g;
    g.outputs[0].format = FormatChoice::Keep;
    let png = env.recipe("4 倍", &g, Vec::new());
    let record = env.run(png, &[a], BatchMode::Overwrite);
    assert_eq!(env.rows(record.id)[0].1, "saved");
    let size: (i64, i64) = env
        .lib
        .db()
        .read(|c| {
            Ok(
                c.query_row("SELECT width, height FROM items WHERE id = ?1", [a], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .unwrap();
    assert_eq!(size, (96, 80));
    // パレットの透明（Indexed8。0 番の色が透明）も「透明」として出し、JPEG に書けずに失敗する
    let indexed = InputGen::PixelSprite {
        w: 24,
        h: 20,
        fmt: PixelFormat::Indexed8,
        seed: 3,
    }
    .generate()
    .unwrap();
    let pal_png = CodecSet::pinned()
        .encode_bytes(&indexed, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0;
    let (pal, _) = add_item(&env.lib, &pal_png, "png", "パレットの絵");
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(recipe),
        &[pal],
        BatchMode::Overwrite,
        false,
    );
    assert!(
        p.losses
            .iter()
            .any(|l| l.loss == Loss::Alpha && l.items == vec![pal]),
        "{p:#?}"
    );
    let record = env.run(recipe, &[pal], BatchMode::Overwrite);
    assert_eq!(
        outcomes(&env, record.id)[&pal],
        ("failed".into(), Some("Unsupported".into()))
    );
    assert_consistent(&env.lib);
}

/// C3：結び付け。値のないアイテムは一括と同じ理由で飛ばす見込みにし、読む値の例を出す。終わりからの秒で長さが見出しで決まらない
/// MP3 は「確かめられなかった」（NoLength）にして当てる側に数える（当てると版ができる）。
#[test]
fn c3_bindings_in_the_precheck() {
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
    grid_of(&env, with, 4);
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(recipe),
        &[with, without],
        BatchMode::Overwrite,
        false,
    );
    assert_eq!(p.apply, vec![with]);
    assert_eq!(p.skips.len(), 1, "{p:#?}");
    assert_eq!(
        (
            p.skips[0].state.as_str(),
            p.skips[0].code.as_str(),
            p.skips[0].items.clone()
        ),
        ("skipped", "NotApplicable", vec![without])
    );
    assert_eq!(p.bound.len(), 1);
    assert_eq!(p.bound[0].item_id, with);
    assert!(
        p.bound[0].bound.to_string().contains("grid"),
        "{:?}",
        p.bound
    );
    let message = p.skips[0].message.clone();
    let record = env.run(recipe, &[with, without], BatchMode::Overwrite);
    let rows = super::batch::rows(env.lib.db(), record.id, Some("skipped"), None, 0, 10).unwrap();
    assert_eq!(rows[0].message.as_deref(), Some(message.as_str()));
    // 終わりからの秒（MP3）
    let (song, _) = add_audio(&env.lib, &fixture("mp3-lame.mp3"), "mp3", "曲");
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
    let trim = env.recipe("終わりを切る", &g, vec![bind]);
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(trim),
        &[song],
        BatchMode::Overwrite,
        false,
    );
    assert_eq!(p.apply, vec![song]);
    assert_eq!(p.unchecked.len(), 1);
    assert_eq!(
        (p.unchecked[0].code.as_str(), p.unchecked[0].items.clone()),
        ("NoLength", vec![song])
    );
    let record = env.run(trim, &[song], BatchMode::Overwrite);
    assert_eq!(env.rows(record.id)[0].1, "saved");
    // 出力が「元と同じ」（MP3）で、MP3 の書き手がない（ffmpeg がない）：終わりからの秒でも書き手は見出しの上限の長さで確かめ、
    // 上書きは FormatUnavailable、別名は代わりの形式（Ogg Vorbis）で当てる見込み。どちらも一括の行と同じ
    let (song2, _) = add_audio(&env.lib, &fixture("mp3-lame.mp3"), "mp3", "曲 2");
    let mut keep = g.clone();
    keep.outputs[0].format = FormatChoice::Keep;
    let bind = Binding {
        node: NodeId(1),
        param: "end".into(),
        part: None,
        from: Source::Time {
            ms: 250,
            anchor: TimeAnchor::End,
        },
    };
    let trim_keep = env.recipe("終わりを切る（元と同じ）", &keep, vec![bind]);
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(trim_keep),
        &[song2],
        BatchMode::Overwrite,
        false,
    );
    assert!(p.apply.is_empty(), "{p:#?}");
    assert_eq!(p.skips[0].code, "FormatUnavailable");
    let record = env.run(trim_keep, &[song2], BatchMode::Overwrite);
    assert_eq!(outcomes(&env, record.id), predicted(&p));
    let save_as = BatchMode::SaveAs {
        name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
    };
    let p = precheck_of(
        &env,
        RecipeSource::Recipe(trim_keep),
        &[song2],
        save_as.clone(),
        false,
    );
    assert_eq!(p.apply, vec![song2]);
    assert_eq!(p.unchecked[0].code, "NoLength");
    assert_eq!(
        (p.outputs[0].format.as_str(), p.outputs[0].fallback),
        ("ogg", true)
    );
    let record = env.run(trim_keep, &[song2], save_as);
    assert_eq!(outcomes(&env, record.id), predicted(&p));
    assert_consistent(&env.lib);
}

/// C4：事前チェックは中止できる（始める前・途中）。別名の名前の型が空・元に戻した一括の写しは Invalid。
#[test]
fn c4_the_precheck_can_be_canceled() {
    let env = BatchEnv::new();
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let ids: Vec<i64> = (1..=30).map(|k| env.png(k).0).collect();
    let req = |mode: BatchMode| BatchRequest {
        source: RecipeSource::Recipe(recipe),
        items: ids.clone(),
        mode,
        include_drafts: false,
    };
    let canceled = AtomicBool::new(true);
    let progress = |_: u64, _: u64| {};
    let got = precheck(
        &env.lib,
        &env.engine,
        &req(BatchMode::Overwrite),
        &PrecheckCx {
            canceled: &canceled,
            progress: &progress,
        },
    );
    assert!(matches!(got, Err(crate::CoreError::Canceled)), "{got:?}");
    let canceled = AtomicBool::new(false);
    let calls = AtomicUsize::new(0);
    let progress = |n: u64, _: u64| {
        calls.fetch_add(1, Ordering::SeqCst);
        if n >= 2 {
            canceled.store(true, Ordering::SeqCst);
        }
    };
    let got = precheck(
        &env.lib,
        &env.engine,
        &req(BatchMode::Overwrite),
        &PrecheckCx {
            canceled: &canceled,
            progress: &progress,
        },
    );
    assert!(matches!(got, Err(crate::CoreError::Canceled)), "{got:?}");
    // 途中で止まる（働き手が持っていた分だけ確かめ終えて、残りは見ない）
    let k = env.engine.runtime().unwrap().bulk_threads();
    let checked = calls.load(Ordering::SeqCst);
    assert!(
        checked <= 2 + k && checked < ids.len(),
        "{checked} 件確かめた"
    );
    let canceled = AtomicBool::new(false);
    let progress = |_: u64, _: u64| {};
    let got = precheck(
        &env.lib,
        &env.engine,
        &req(BatchMode::SaveAs { name: "  ".into() }),
        &PrecheckCx {
            canceled: &canceled,
            progress: &progress,
        },
    );
    assert!(matches!(got, Err(crate::CoreError::Invalid(_))), "{got:?}");
    let record = env.run(recipe, &ids[..1], BatchMode::Overwrite);
    let undo = undo_and_wait(&env, record.id);
    assert_done(&undo);
    let got = precheck(
        &env.lib,
        &env.engine,
        &BatchRequest {
            source: RecipeSource::Job(undo.id),
            ..req(BatchMode::Overwrite)
        },
        &PrecheckCx {
            canceled: &canceled,
            progress: &progress,
        },
    );
    assert!(matches!(got, Err(crate::CoreError::Invalid(_))), "{got:?}");
    assert_consistent(&env.lib);
}

/// C5：［やり直す］は一括の写しで当てる（レシピを消したあとも、その一括と同じ文書で当たる）。元に戻した一括の写しは Invalid。
#[test]
fn c5_retrying_uses_the_copy_of_the_batch() {
    let env = BatchEnv::new();
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let (a, _) = env.png(1);
    let (b, png_b) = env.png(2);
    trash(&env, b);
    let record = env.run(recipe, &[a, b], BatchMode::Overwrite);
    assert_eq!(
        outcomes(&env, record.id)[&b],
        ("skipped".into(), Some("Trashed".into()))
    );
    env.lib
        .db()
        .write(move |tx| {
            recipes::delete(tx, recipe)?;
            crate::items::restore(tx, &[b], 2)?;
            Ok(())
        })
        .unwrap();
    let p = precheck_of(
        &env,
        RecipeSource::Job(record.id),
        &[b],
        BatchMode::Overwrite,
        false,
    );
    assert_eq!(p.apply, vec![b]);
    let retry = start_batch(
        env.lib.db(),
        &env.jobs,
        &env.engine,
        &BatchRequest {
            source: RecipeSource::Job(record.id),
            items: vec![b],
            mode: BatchMode::Overwrite,
            include_drafts: false,
        },
    )
    .unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(retry).unwrap());
    assert_eq!(head(&env.lib, b).hash, hash_of(&render_png(&png_b, &g)));
    let rec = versions_of(&env.lib, b)[1].record().unwrap();
    assert_eq!(rec.batch.unwrap()["recipe"]["name"], "反転");
    // 元に戻した一括の写しはやり直せない
    let undo = undo_and_wait(&env, retry);
    let got = start_batch(
        env.lib.db(),
        &env.jobs,
        &env.engine,
        &BatchRequest {
            source: RecipeSource::Job(undo.id),
            items: vec![a],
            mode: BatchMode::Overwrite,
            include_drafts: false,
        },
    );
    assert!(matches!(got, Err(crate::CoreError::Invalid(_))), "{got:?}");
    assert_consistent(&env.lib);
}

/// U1：上書きの一括を元に戻すと、一括の前の版（版 0 と、編集済みなら版 1）の中身に戻る（版 kind = revert・revert_to・行の
/// version_id が戻した版）。一括のあとに保存したアイテムは Changed、下書きのあるアイテムは Draft で飛ばす。結果に戻した一括が
/// 載り、戻す一括そのものは戻せず、戻し終えたものは次の［元に戻す…］に入らない。
#[test]
fn u1_undoing_an_overwrite_batch_restores_the_previous_versions() {
    let env = BatchEnv::new();
    let (a, png_a) = env.png(1);
    let (b, _) = env.png(2);
    let (c, _) = env.png(3);
    let (d, png_d) = env.png(4);
    let h1 = doc(&[flip("horizontal")], None);
    overwrite(&env, d, h1.clone());
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(recipe, &[a, b, c, d], BatchMode::Overwrite);
    assert_done(&record);
    overwrite(&env, b, doc(&[flip("horizontal")], None));
    put_draft(&env, c);
    let undo = undo_and_wait(&env, record.id);
    assert_done(&undo);
    let got: Vec<(i64, String, Option<String>)> = env
        .rows(undo.id)
        .into_iter()
        .map(|r| (r.0, r.1, r.2))
        .collect();
    assert_eq!(
        got,
        vec![
            (a, "saved".into(), None),
            (b, "skipped".into(), Some("Changed".into())),
            (c, "skipped".into(), Some("Draft".into())),
            (d, "saved".into(), None),
        ]
    );
    assert_eq!(head(&env.lib, a).hash, hash_of(&png_a));
    assert_eq!(head(&env.lib, d).hash, hash_of(&render_png(&png_d, &h1)));
    let va = versions_of(&env.lib, a);
    assert_eq!(
        (va[2].kind.as_str(), va[2].revert_to, va[2].job_id),
        ("revert", Some(0), None)
    );
    assert_eq!(env.rows(undo.id)[0].3, Some(va[2].id));
    let vd = versions_of(&env.lib, d);
    assert_eq!((vd[3].kind.as_str(), vd[3].revert_to), ("revert", Some(1)));
    // 結果：戻した一括と、まだ戻していない件数（b・c）
    let result = super::batch::result(env.lib.db(), record.id).unwrap();
    assert_eq!(result.undo.by, vec![undo.id]);
    assert_eq!(result.undo.available, 2);
    let back = super::batch::result(env.lib.db(), undo.id).unwrap();
    assert_eq!(
        back.action,
        BatchAction::Undo {
            of: record.id,
            save_as: false,
            merge: false
        }
    );
    assert_eq!(back.undo.available, 0);
    // 戻す一括は戻せない。もう一度戻すと、戻し終えた a・d は入らない
    assert!(matches!(
        start_undo(env.lib.db(), &env.jobs, &env.engine, undo.id),
        Err(crate::CoreError::Invalid(_))
    ));
    super::drafts::discard(env.lib.db(), c).unwrap();
    let again = undo_and_wait(&env, record.id);
    let items: Vec<i64> = env.rows(again.id).into_iter().map(|r| r.0).collect();
    assert_eq!(items, vec![b, c]);
    assert_eq!(env.rows(again.id)[1].1, "saved");
    assert_consistent(&env.lib);
}

/// U2：別名の一括を元に戻すと、作ったアイテムのうち未編集のものをゴミ箱へ入れる（元は変えない）。作ったあとに保存したものは
/// Changed。
#[test]
fn u2_undoing_a_save_as_batch_trashes_the_copies() {
    let env = BatchEnv::new();
    let (a, _) = env.png(1);
    let (b, _) = env.png(2);
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(
        recipe,
        &[a, b],
        BatchMode::SaveAs {
            name: super::batch::DEFAULT_SAVE_AS_NAME.into(),
        },
    );
    assert_done(&record);
    let result = super::batch::result(env.lib.db(), record.id).unwrap();
    assert_eq!(result.created.len(), 2);
    let (ca, cb) = (result.created[0], result.created[1]);
    overwrite(&env, cb, doc(&[flip("horizontal")], None));
    let undo = undo_and_wait(&env, record.id);
    assert_done(&undo);
    let got: Vec<(i64, String, Option<String>)> = env
        .rows(undo.id)
        .into_iter()
        .map(|r| (r.0, r.1, r.2))
        .collect();
    assert_eq!(
        got,
        vec![
            (ca, "saved".into(), None),
            (cb, "skipped".into(), Some("Changed".into())),
        ]
    );
    assert!(head(&env.lib, ca).trashed);
    assert!(!head(&env.lib, cb).trashed);
    for item in [a, b] {
        assert!(!head(&env.lib, item).trashed);
        assert!(versions_of(&env.lib, item).is_empty(), "元は変えない");
    }
    assert_consistent(&env.lib);
}

/// U3：元に戻すのは一括が終わってから（待ちのままなら Busy）。ないジョブは NotFound。
#[test]
fn u3_undo_waits_for_the_batch_to_finish() {
    let env = BatchEnv::with(None, None, false);
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let (a, _) = env.png(1);
    let job = env.start(recipe, &[a], BatchMode::Overwrite, false);
    assert!(matches!(
        start_undo(env.lib.db(), &env.jobs, &env.engine, job),
        Err(crate::CoreError::Busy(_))
    ));
    assert!(matches!(
        start_undo(env.lib.db(), &env.jobs, &env.engine, job + 100),
        Err(crate::CoreError::NotFound(_))
    ));
    env.jobs.start().unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let undo = undo_and_wait(&env, job);
    assert_done(&undo);
    assert_eq!(env.rows(undo.id)[0].1, "saved");
    assert_consistent(&env.lib);
}

/// U4：戻す先の版のファイルが GC で消えていても、文書と入力の記録から作り直して戻す（同じバイト列）。
#[test]
fn u4_undo_rebuilds_a_collected_version() {
    let env = BatchEnv::new();
    let (a, png) = env.png(5);
    let h1 = doc(&[flip("horizontal")], None);
    overwrite(&env, a, h1.clone());
    let v1 = versions_of(&env.lib, a)[1].clone();
    let path1 = env.lib.layout().content_path(&v1.output_hash, "png");
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let record = env.run(recipe, &[a], BatchMode::Overwrite);
    assert_done(&record);
    env.gc();
    assert!(!path1.exists(), "版 1 のファイルは片付けた");
    let undo = undo_and_wait(&env, record.id);
    assert_done(&undo);
    assert_eq!(env.rows(undo.id)[0].1, "saved");
    assert_eq!(head(&env.lib, a).hash, v1.output_hash);
    assert_eq!(std::fs::read(&path1).unwrap(), render_png(&png, &h1));
    let v3 = versions_of(&env.lib, a)[3].clone();
    assert_eq!((v3.kind.as_str(), v3.revert_to), ("revert", Some(1)));
    assert_eq!(v3.record().unwrap().revert.map(|r| r.rebuilt), Some(true));
    assert_consistent(&env.lib);
}

/// R1：結果は状態と理由ごとの群（状態の並び、アイテムは行の順）と、1 件ずつの行（状態・理由で絞り、ずらして取る）。
#[test]
fn r1_results_group_rows_by_state_and_code() {
    let env = BatchEnv::new();
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let (a, _) = env.png(1);
    let (t, _) = env.png(2);
    let (b, _) = env.png(3);
    trash(&env, t);
    // 飛ばした行を先に置いても、群は状態の並び（当てた → 飛ばした）
    let record = env.run(recipe, &[t, a, b], BatchMode::Overwrite);
    let result = super::batch::result(env.lib.db(), record.id).unwrap();
    assert_eq!(result.state, JobState::Done);
    assert_eq!(result.action, BatchAction::Overwrite);
    assert_eq!(result.recipe.as_deref(), Some("反転"));
    assert_eq!(result.total, 3);
    let groups: Vec<(String, Option<String>, Vec<i64>)> = result
        .groups
        .iter()
        .map(|g| (g.state.clone(), g.code.clone(), g.items.clone()))
        .collect();
    assert_eq!(
        groups,
        vec![
            ("saved".into(), None, vec![a, b]),
            ("skipped".into(), Some("Trashed".into()), vec![t]),
        ]
    );
    assert!(result.created.is_empty());
    assert_eq!(result.undo.available, 2);
    let skipped = super::batch::rows(
        env.lib.db(),
        record.id,
        Some("skipped"),
        Some("Trashed"),
        0,
        10,
    )
    .unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].item_id, t);
    assert!(skipped[0].message.is_some());
    let second = super::batch::rows(env.lib.db(), record.id, None, None, 1, 1).unwrap();
    assert_eq!((second.len(), second[0].ord, second[0].item_id), (1, 1, a));
    assert!(matches!(
        super::batch::result(env.lib.db(), record.id + 100),
        Err(crate::CoreError::NotFound(_))
    ));
}

/// 動いている戻す一括を、アイテム `target` の直前（飛ばす決まりを見たあと）で止める割り込み（一時停止か中止）。
type StopSlot = Arc<Mutex<Option<(Weak<JobManager>, i64, bool)>>>;

fn stop_env(slot: &StopSlot) -> BatchEnv {
    let slot = slot.clone();
    BatchEnv::build(
        None,
        None,
        true,
        Hooks {
            prepare: Some(Arc::new(move |lib: &Library, item| {
                let hit = {
                    let mut s = slot.lock().unwrap();
                    match s.as_ref() {
                        Some((_, target, _)) if *target == item => s.take(),
                        _ => None,
                    }
                };
                if let Some((jobs, _, cancel)) = hit
                    && let Some(jobs) = jobs.upgrade()
                {
                    let job: i64 = lib
                        .db()
                        .read(|c| {
                            Ok(c.query_row(
                                "SELECT id FROM jobs WHERE kind = 'batch' AND state = 'running'",
                                [],
                                |r| r.get(0),
                            )?)
                        })
                        .unwrap();
                    if cancel {
                        jobs.cancel(job).unwrap();
                    } else {
                        jobs.pause(job).unwrap();
                    }
                }
            })),
            ..Hooks::default()
        },
    )
}

/// 戻した版（kind = revert）の数。
fn reverts(lib: &Library, item: i64) -> usize {
    versions_of(lib, item)
        .iter()
        .filter(|v| v.kind.as_str() == "revert")
        .count()
}

/// U5：動いている戻す一括（働き手がそのままコミットする道）を一時停止すると、済んだ行は戻した版がちょうど 1 つ、残りは pending で
/// 版はなく、再開すると残りだけを戻す。中止すると、済んだ行は残り、残りは canceled で版はない。
#[test]
fn u5_pausing_and_canceling_a_running_undo_keeps_each_row_once() {
    for cancel in [false, true] {
        let slot: StopSlot = Arc::default();
        let env = stop_env(&slot);
        let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
        let ids: Vec<i64> = (1..=8).map(|k| env.png(k).0).collect();
        let record = env.run(recipe, &ids, BatchMode::Overwrite);
        assert_done(&record);
        *slot.lock().unwrap() = Some((Arc::downgrade(&env.jobs), ids[3], cancel));
        let undo = start_undo(env.lib.db(), &env.jobs, &env.engine, record.id).unwrap();
        assert!(env.jobs.wait_idle(WAIT));
        let state = env.jobs.get(undo).unwrap().state;
        let rows = env.rows(undo);
        let stopped = if cancel { "canceled" } else { "pending" };
        assert_eq!(
            state,
            if cancel {
                JobState::Canceled
            } else {
                JobState::Paused
            }
        );
        assert!(rows.iter().any(|r| r.1 == stopped), "{rows:?}");
        for r in &rows {
            match r.1.as_str() {
                "saved" => assert_eq!(reverts(&env.lib, r.0), 1, "{r:?}"),
                s if s == stopped => assert_eq!(reverts(&env.lib, r.0), 0, "{r:?}"),
                other => panic!("{other} {r:?}"),
            }
        }
        if !cancel {
            env.jobs.resume(undo).unwrap();
            assert!(env.jobs.wait_idle(WAIT));
            assert_done(&env.jobs.get(undo).unwrap());
            for r in env.rows(undo) {
                assert_eq!(r.1, "saved");
                assert_eq!(reverts(&env.lib, r.0), 1, "{r:?}");
            }
        }
        assert_consistent(&env.lib);
    }
}

/// U6：戻す一括の作り直し（戻す先の版のファイルを GC で片付けた音声。元は直接読めない FLAC なので作業用の PCM で読む）を
/// 一時停止すると作業用の PCM を残し、一時停止のまま中止すると捨てる（一括・保存のジョブと同じ）。
#[test]
fn u6_a_paused_undo_rebuild_keeps_its_work_pcm_until_canceled() {
    use super::save::Stage;
    use std::sync::mpsc;
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let armed = Arc::new(AtomicBool::new(false));
    let hook: super::save::Hook = {
        let armed = armed.clone();
        let (reached, go) = (Mutex::new(reached_tx), Mutex::new(go_rx));
        Arc::new(move |stage| {
            if stage == Stage::Written && armed.swap(false, Ordering::SeqCst) {
                reached.lock().unwrap().send(()).unwrap();
                let _ = go.lock().unwrap().recv();
            }
        })
    };
    let env = BatchEnv::build(
        None,
        None,
        true,
        Hooks {
            save: Some(hook),
            ..Hooks::default()
        },
    );
    let flac = noise_file(&CodecSet::pinned(), "flac", 2, 9_000, 7);
    let (item, _) = add_audio(&env.lib, &flac, "flac", "音");
    overwrite(&env, item, audio_doc(-3.0, Some("wav")));
    let recipe = env.recipe("下げる", &audio_doc(-1.0, None), Vec::new());
    let record = env.run(recipe, &[item], BatchMode::Overwrite);
    assert_done(&record);
    assert_eq!(env.rows(record.id)[0].1, "saved");
    env.gc();
    assert_eq!(
        versions_of(&env.lib, item)[1].file_state,
        super::versions::FileState::Released
    );
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    armed.store(true, Ordering::SeqCst);
    let undo = start_undo(env.lib.db(), &env.jobs, &env.engine, record.id).unwrap();
    reached_rx
        .recv_timeout(4 * WAIT)
        .expect("作り直しの門に来ない");
    env.jobs.pause(undo).unwrap();
    go_tx.send(()).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(undo).unwrap().state, JobState::Paused);
    assert_eq!(env.rows(undo)[0].1, "pending");
    assert_eq!(env.engine.work_pcm().usage().0, 1, "一時停止は残す");
    env.jobs.cancel(undo).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(undo).unwrap().state, JobState::Canceled);
    assert_eq!(
        env.engine.work_pcm().usage(),
        (0, 0),
        "中止したら作業用の PCM を捨てる"
    );
    assert_eq!(reverts(&env.lib, item), 0);
    assert_consistent(&env.lib);
}

/// U7：戻す一括の作り直しの道（戻す先の版のファイルを GC で片付けた）で、n 件コミットしたあとに強制終了しても（予告のあと・
/// 置いたあと・コミットのあと）、開き直すと残りから続け、どのアイテムも戻した版はちょうど 1 つで、一括の前の中身に戻る。
#[test]
fn u7_a_crash_while_undoing_redoes_only_unfinished_rows() {
    for at in [Fault::AfterAnnounce, Fault::AfterPlace, Fault::AfterCommit] {
        let armed = Arc::new(AtomicBool::new(false));
        let env = BatchEnv::build(
            None,
            Some((1, at)),
            true,
            Hooks {
                crash_armed: Some(armed.clone()),
                ..Hooks::default()
            },
        );
        let h1 = doc(&[flip("horizontal")], None);
        let items: Vec<(i64, Vec<u8>)> = (1..=4).map(|k| env.png(k)).collect();
        for (id, _) in &items {
            overwrite(&env, *id, h1.clone());
        }
        let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
        let ids: Vec<i64> = items.iter().map(|(id, _)| *id).collect();
        let record = env.run(recipe, &ids, BatchMode::Overwrite);
        assert_done(&record);
        env.gc();
        armed.store(true, Ordering::SeqCst);
        let undo = start_undo(env.lib.db(), &env.jobs, &env.engine, record.id).unwrap();
        assert!(env.jobs.wait_idle(WAIT));
        assert_eq!(
            env.jobs.get(undo).unwrap().state,
            JobState::Failed,
            "{at:?}"
        );
        let (dir, session) = reopen(env, undo);
        let lib = session.library();
        assert_done(&session.jobs().get(undo).unwrap());
        for row in rows_of(lib, undo) {
            assert_eq!(row.1, "saved", "{at:?}");
            assert_eq!(reverts(lib, row.0), 1, "{at:?} アイテム {}", row.0);
        }
        for (id, png) in &items {
            assert_eq!(
                head(lib, *id).hash,
                hash_of(&render_png(png, &h1)),
                "{at:?}"
            );
        }
        assert_consistent(lib);
        drop(session);
        drop(dir);
    }
}

/// U8：同じ一括を戻す一括が終わっていなければ、新しく戻す・戻す一括のやり直し・元の一括のやり直しはどれも Busy で、やり直しを
/// 断ったら状態を戻さない。戻す一括が終われば、やり直せる。
#[test]
fn u8_undo_and_retry_do_not_overlap() {
    let env = BatchEnv::new();
    let recipe = env.recipe("反転", &doc(&[flip("vertical")], None), Vec::new());
    let ids: Vec<i64> = (1..=2).map(|k| env.png(k).0).collect();
    let record = env.run(recipe, &ids, BatchMode::Overwrite);
    assert_done(&record);
    let u1 = undo_and_wait(&env, record.id);
    assert_done(&u1);
    let of = record.id;
    let other = env
        .lib
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('batch', 'paused', ?1, 1)",
                [format!(
                    r#"{{"v":1,"mode":{{"type":"undo","of":{of},"saveAs":false}},"includeDrafts":false}}"#
                )],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap();
    assert!(matches!(
        start_undo(env.lib.db(), &env.jobs, &env.engine, record.id),
        Err(crate::CoreError::Busy(_))
    ));
    let (u1_id, batch_id) = (u1.id, record.id);
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE jobs SET state = 'failed' WHERE id IN (?1, ?2)",
                params![u1_id, batch_id],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        env.jobs.retry(u1.id),
        Err(crate::CoreError::Busy(_))
    ));
    assert!(matches!(
        env.jobs.retry(record.id),
        Err(crate::CoreError::Busy(_))
    ));
    assert_eq!(env.jobs.get(u1.id).unwrap().state, JobState::Failed);
    assert_eq!(env.jobs.get(record.id).unwrap().state, JobState::Failed);
    env.lib
        .db()
        .write(move |tx| {
            tx.execute("UPDATE jobs SET state = 'canceled' WHERE id = ?1", [other])?;
            Ok(())
        })
        .unwrap();
    env.jobs.retry(u1.id).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(u1.id).unwrap());
    assert_consistent(&env.lib);
}

// ---- 段 6：N → 1（スプライトシート。M1〜M4。docs/editing.md §14.2「N → 1」）----

fn merge_req(
    items: &[i64],
    sheet: kura_engine::selection::SheetParams,
) -> super::batch::MergeRequest {
    super::batch::MergeRequest {
        items: items.to_vec(),
        name: super::batch::DEFAULT_SHEET_NAME.into(),
        sheet,
    }
}

fn run_merge(env: &BatchEnv, req: &super::batch::MergeRequest) -> JobRecord {
    let job = super::batch::start_merge(&env.jobs, req).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    env.jobs.get(job).unwrap()
}

/// 単色の PNG を足す。
fn solid_png(env: &BatchEnv, w: u32, h: u32, rgba: [u8; 4], title: &str) -> i64 {
    let px = kura_engine::value::Raster::filled(w, h, PixelFormat::Rgba8, None, &rgba).unwrap();
    let value = kura_engine::Value::Image(Arc::new(kura_engine::value::Image {
        px,
        meta: Default::default(),
    }));
    let bytes = CodecSet::pinned()
        .encode_bytes(&value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0;
    add_item(&env.lib, &bytes, "png", title).0
}

/// 2 フレームのアニメの GIF を足す。
fn anim_gif(env: &BatchEnv) -> i64 {
    use kura_engine::value::{Frames, Raster};
    let a = Raster::filled(24, 20, PixelFormat::Rgba8, None, &[200, 10, 10, 255]).unwrap();
    let b = Raster::filled(24, 20, PixelFormat::Rgba8, None, &[10, 10, 200, 255]).unwrap();
    let fr = Frames::full(vec![(a, 50), (b, 70)], Default::default(), 0).unwrap();
    let bytes = CodecSet::pinned()
        .encode_bytes(
            &kura_engine::Value::Frames(Arc::new(fr)),
            "gif",
            &json!({}),
            &NoMonitor,
        )
        .unwrap()
        .0;
    add_item(&env.lib, &bytes, "gif", "アニメ").0
}

/// エンジンで直に描いたシートのハッシュ（今の中身を読み、選択の値にして、シートの文書を描いて PNG に書く）。
fn expected_sheet(
    env: &BatchEnv,
    items: &[i64],
    sheet: &kura_engine::selection::SheetParams,
) -> [u8; 32] {
    use kura_engine::render::InputValue;
    use kura_engine::selection;
    let pinned = CodecSet::pinned();
    let limits = env.engine.runtime().unwrap().bulk(|e| e.limits().clone());
    let mut values = Vec::new();
    let mut hashes = Vec::new();
    for id in items {
        let h = head(&env.lib, *id);
        let path = env.lib.layout().content_path(&h.hash, &h.ext);
        let format = crate::import::format_id(&h.ext).unwrap();
        let (v, _) = pinned
            .decode_path(
                &path,
                format,
                &DecodeOptions::default(),
                &limits,
                &NoMonitor,
            )
            .unwrap();
        values.push(v);
        hashes.push(h.hash);
    }
    let frames = selection::frames(&values, sheet, &NoMonitor)
        .unwrap()
        .frames;
    let count = frames.frames().len() as u64;
    let g = selection::sheet_graph(sheet, count).unwrap();
    let engine = kura_engine::Engine::new().unwrap();
    let v = kura_engine::Value::Frames(Arc::new(frames));
    let plan = engine
        .compile(&g, &BTreeMap::from([(InputId(0), v.shape())]))
        .unwrap();
    let out = engine
        .render(
            &plan,
            &BTreeMap::from([(
                InputId(0),
                InputValue {
                    value: v,
                    source: selection::source(&hashes),
                },
            )]),
            &NoMonitor,
        )
        .unwrap();
    let bytes = pinned
        .encode_bytes(&out.outputs[0].value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0;
    hash_of(&bytes)
}

/// 来歴（作ったアイテムの元を選んだ順に：(元, 版, 中身, フレームの数)）。
fn sources_of(lib: &Library, item: i64) -> Vec<(Option<i64>, i64, Vec<u8>, i64)> {
    lib.db()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT source_item_id, source_version, source_hash, frames FROM item_sources WHERE item_id = ?1 ORDER BY ord",
            )?;
            let rows = stmt.query_map([item], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .unwrap()
}

/// M1：選んだ順にシートを作る（大きさの違う絵は中央に揃え、アニメは全フレーム、音声・ゴミ箱は飛ばす）。できたアイテムは PNG で、
/// 元は描いたファイル（版の行はない）、名前は「{最初の名前} のシート」、来歴は元・版・中身・フレームの数、素材の設定に格子
/// （セル＝最大の大きさ、間隔、コマの数）。中身はエンジンで直に描いたものと同じバイト列。事前チェックの見込みは行と同じで、
/// シートの大きさも同じ。結果の作ったアイテムは 1 つ。
#[test]
fn m1_a_sheet_is_made_from_the_selection_in_order() {
    let env = BatchEnv::new();
    let (a, _) = env.png(1);
    let small = solid_png(&env, 10, 8, [0, 200, 0, 255], "小さい絵");
    let anim = anim_gif(&env);
    let pinned = CodecSet::pinned();
    let (sound, _) = add_audio(
        &env.lib,
        &noise_file(&pinned, "wav", 2, 4_000, 3),
        "wav",
        "音",
    );
    let (gone, _) = env.png(2);
    trash(&env, gone);
    let sheet = kura_engine::selection::SheetParams {
        gap_x: 1,
        gap_y: 2,
        ..Default::default()
    };
    let ids = [a, sound, small, gone, anim];
    let req = merge_req(&ids, sheet);
    let canceled = AtomicBool::new(false);
    let progress = |_: u64, _: u64| {};
    let p = super::batch::precheck_merge(
        &env.lib,
        &env.engine,
        &req,
        &PrecheckCx {
            canceled: &canceled,
            progress: &progress,
        },
    )
    .unwrap();
    let record = run_merge(&env, &req);
    assert_done(&record);
    assert_eq!(outcomes(&env, record.id), predicted(&p), "{p:#?}");
    let result = super::batch::result(env.lib.db(), record.id).unwrap();
    assert!(matches!(result.action, BatchAction::Merge { .. }));
    assert_eq!(result.created.len(), 1);
    let made = result.created[0];
    let codes: Vec<(i64, String, Option<String>)> = env
        .rows(record.id)
        .into_iter()
        .map(|r| (r.0, r.1, r.2))
        .collect();
    assert_eq!(
        codes,
        vec![
            (a, "saved".into(), None),
            (sound, "skipped".into(), Some("Kind".into())),
            (small, "saved".into(), None),
            (gone, "skipped".into(), Some("Trashed".into())),
            (anim, "saved".into(), None),
        ]
    );
    let h = head(&env.lib, made);
    assert_eq!((h.ext.as_str(), h.version), ("png", 0));
    assert_eq!(h.hash, expected_sheet(&env, &[a, small, anim], &sheet));
    assert!(versions_of(&env.lib, made).is_empty(), "元はできたファイル");
    let title: String = env
        .lib
        .db()
        .read(|c| {
            Ok(
                c.query_row("SELECT title FROM items WHERE id = ?1", [made], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(title, "絵1 のシート");
    let src = sources_of(&env.lib, made);
    assert_eq!(
        src.iter().map(|s| (s.0, s.3)).collect::<Vec<_>>(),
        vec![(Some(a), 1), (Some(small), 1), (Some(anim), 2)]
    );
    assert_eq!(src[0].2, head(&env.lib, a).hash.to_vec());
    let grid = env
        .lib
        .db()
        .read(|c| crate::asset::get(c, made))
        .unwrap()
        .settings
        .grid
        .unwrap();
    assert_eq!(
        (grid.cell_w, grid.cell_h, grid.gap_x, grid.gap_y, grid.count),
        (24, 20, 1, 2, Some(4))
    );
    // シートの見込みは、できたシートの大きさと同じ（4 フレーム → 2 列 × 2 行、間隔 1・2）
    let info = p.sheet.unwrap();
    let (w, hh): (i64, i64) = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT width, height FROM items WHERE id = ?1",
                [made],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((info.width as i64, info.height as i64), (w, hh));
    assert_eq!((w, hh), (24 * 2 + 1, 20 * 2 + 2));
    assert_eq!((info.frames, info.columns, info.approximate), (4, 2, false));
    assert_consistent(&env.lib);
}

/// M2：作る途中に強制終了しても（予告のあと・置いたあと・コミットのあと）、開き直すと残りから作り、シートはちょうど 1 枚で、
/// どの行も同じアイテムを指す。
#[test]
fn m2_a_crash_while_merging_makes_exactly_one_sheet() {
    for at in [Fault::AfterAnnounce, Fault::AfterPlace, Fault::AfterCommit] {
        let env = BatchEnv::with(None, Some((0, at)), true);
        let ids: Vec<i64> = (1..=3).map(|k| env.png(k).0).collect();
        let record = run_merge(&env, &merge_req(&ids, Default::default()));
        assert_eq!(record.state, JobState::Failed, "{at:?}");
        let job = record.id;
        let (dir, session) = reopen(env, job);
        let lib = session.library();
        assert_done(&session.jobs().get(job).unwrap());
        let rows = rows_of(lib, job);
        let made = rows[0].4.unwrap();
        for row in &rows {
            assert_eq!((row.1.as_str(), row.4), ("saved", Some(made)), "{at:?}");
        }
        let sheets: i64 = lib
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(DISTINCT item_id) FROM item_sources",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(sheets, 1, "{at:?}");
        // 結果は行から読む（コミットのあとに落ちて開き直しても「作りました」）
        let record = session.jobs().get(job).unwrap();
        assert_eq!(result_of(&record)["made"], made, "{at:?}");
        assert!(
            record
                .message
                .as_deref()
                .unwrap_or("")
                .contains("作りました"),
            "{at:?} {:?}",
            record.message
        );
        assert_consistent(lib);
        drop(session);
        drop(dir);
    }
}

/// M3：シートの一括を元に戻すと、作ったシートをゴミ箱へ入れる（元は変えない）。作ったあとに保存したシートは Changed。
#[test]
fn m3_undoing_a_sheet_trashes_it() {
    let env = BatchEnv::new();
    let ids: Vec<i64> = (1..=2).map(|k| env.png(k).0).collect();
    let record = run_merge(&env, &merge_req(&ids, Default::default()));
    let made = super::batch::result(env.lib.db(), record.id)
        .unwrap()
        .created[0];
    assert_eq!(
        super::batch::result(env.lib.db(), record.id)
            .unwrap()
            .undo
            .available,
        1
    );
    let undo = undo_and_wait(&env, record.id);
    assert_done(&undo);
    let rows = env.rows(undo.id);
    assert_eq!(
        (rows.len(), rows[0].0, rows[0].1.as_str()),
        (1, made, "saved")
    );
    assert!(head(&env.lib, made).trashed);
    for id in &ids {
        assert!(!head(&env.lib, *id).trashed);
    }
    // 作ったあとに保存したシートは戻さない
    let ids2: Vec<i64> = (3..=4).map(|k| env.png(k).0).collect();
    let record = run_merge(&env, &merge_req(&ids2, Default::default()));
    let made2 = super::batch::result(env.lib.db(), record.id)
        .unwrap()
        .created[0];
    overwrite(&env, made2, doc(&[flip("vertical")], None));
    let undo = undo_and_wait(&env, record.id);
    assert_eq!(
        (
            env.rows(undo.id)[0].1.as_str(),
            env.rows(undo.id)[0].2.as_deref()
        ),
        ("skipped", Some("Changed"))
    );
    assert!(!head(&env.lib, made2).trashed);
    assert_consistent(&env.lib);
}

/// M4：同じ選択でもう一度作ると、同じ中身のアイテムがあるので作らずに existing（行は前のシートを指す）。待ちのまま一時停止すると
/// 行は pending のまま、再開すると作る。待ちのまま中止すると canceled で作らない。名前が空・どれもないなら Invalid。
#[test]
fn m4_existing_pause_cancel_and_refusals() {
    let env = BatchEnv::with(None, None, false);
    let ids: Vec<i64> = (1..=2).map(|k| env.png(k).0).collect();
    let req = merge_req(&ids, Default::default());
    let job = super::batch::start_merge(&env.jobs, &req).unwrap();
    env.jobs.pause(job).unwrap();
    env.jobs.start().unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Paused);
    assert!(env.rows(job).iter().all(|r| r.1 == "pending"));
    env.jobs.resume(job).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
    let made = env.rows(job)[0].4.unwrap();
    let again = run_merge(&env, &req);
    assert_done(&again);
    assert_eq!(result_of(&again)["existing"], made);
    assert!(result_of(&again)["made"].is_null());
    assert!(
        again
            .message
            .as_deref()
            .unwrap_or("")
            .contains("新しく作りませんでした"),
        "{:?}",
        again.message
    );
    for row in env.rows(again.id) {
        assert_eq!(
            (row.1.as_str(), row.2.as_deref(), row.4),
            ("existing", Some("Existing"), Some(made))
        );
    }
    let canceled = super::batch::start_merge(&env.jobs, &req).unwrap();
    env.jobs.pause(canceled).unwrap();
    env.jobs.cancel(canceled).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(canceled).unwrap().state, JobState::Canceled);
    assert!(env.rows(canceled).iter().all(|r| r.1 == "canceled"));
    let mut empty = req.clone();
    empty.name = "  ".into();
    assert!(matches!(
        super::batch::start_merge(&env.jobs, &empty),
        Err(crate::CoreError::Invalid(_))
    ));
    assert!(matches!(
        super::batch::start_merge(&env.jobs, &merge_req(&[9_999_999], Default::default())),
        Err(crate::CoreError::Invalid(_))
    ));
    let items: i64 = count(
        &env.lib,
        "SELECT count(*) FROM items WHERE id IN (SELECT item_id FROM item_sources)",
    );
    assert_eq!(items, 1);
    assert_consistent(&env.lib);
}

/// RGB の PNG を足す（アルファなし。ICC があれば付ける）。
fn rgb_png(env: &BatchEnv, rgb: [u8; 3], icc: Option<Vec<u8>>, title: &str) -> i64 {
    let px = kura_engine::value::Raster::filled(6, 4, PixelFormat::Rgb8, None, &rgb).unwrap();
    let value = kura_engine::Value::Image(Arc::new(kura_engine::value::Image {
        px,
        meta: kura_engine::value::ImageMeta {
            icc: icc.map(Arc::new),
            ..Default::default()
        },
    }));
    let bytes = CodecSet::pinned()
        .encode_bytes(&value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0;
    add_item(&env.lib, &bytes, "png", title).0
}

/// シートを読み直した値。
fn sheet_value(env: &BatchEnv, item: i64) -> kura_engine::Value {
    let h = head(&env.lib, item);
    let limits = env.engine.runtime().unwrap().bulk(|e| e.limits().clone());
    CodecSet::pinned()
        .decode_path(
            &env.lib.layout().content_path(&h.hash, &h.ext),
            "png",
            &DecodeOptions::default(),
            &limits,
            &NoMonitor,
        )
        .unwrap()
        .0
}

/// M5：アルファのない絵（RGB）だけでも、透明の fill を指定すればシートを作れる（形式を広げる）。すべての元の ICC が同じなら
/// シートに運び、違えば持たずに警告を結果に残す。
#[test]
fn m5_fill_widens_and_the_icc_is_carried_when_shared() {
    let env = BatchEnv::new();
    let icc = vec![9u8; 200];
    let a = rgb_png(&env, [10, 20, 30], Some(icc.clone()), "赤");
    let b = rgb_png(&env, [40, 50, 60], Some(icc.clone()), "青");
    let sheet = kura_engine::selection::SheetParams {
        fill: Some([0, 0, 0, 0]),
        gap_x: 2,
        ..Default::default()
    };
    let record = run_merge(&env, &merge_req(&[a, b], sheet));
    assert_done(&record);
    let made = result_of(&record)["made"].as_i64().unwrap();
    let kura_engine::Value::Image(img) = sheet_value(&env, made) else {
        panic!()
    };
    assert_eq!(img.px.format(), PixelFormat::Rgba8);
    assert_eq!(img.px.pixel(6, 0), Some(&[0, 0, 0, 0][..]), "間隔は透明");
    assert_eq!(img.meta.icc.as_deref(), Some(&icc));
    assert!(
        super::batch::result(env.lib.db(), record.id)
            .unwrap()
            .warnings
            .is_empty()
    );
    // ICC が違えば持たず、警告を残す
    let c = rgb_png(&env, [70, 80, 90], None, "緑");
    let record = run_merge(&env, &merge_req(&[a, c], Default::default()));
    assert_done(&record);
    let made = result_of(&record)["made"].as_i64().unwrap();
    let kura_engine::Value::Image(img) = sheet_value(&env, made) else {
        panic!()
    };
    assert_eq!(img.meta.icc, None);
    let warnings = super::batch::result(env.lib.db(), record.id)
        .unwrap()
        .warnings;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("sRGB"), "{warnings:?}");
    assert_consistent(&env.lib);
}

/// M6：N → 1 が待っている間も、並べる元は開ける・保存できる（元を読むだけ。読むのは処理するときの今で、来歴に残る）。
#[test]
fn m6_a_pending_merge_does_not_block_its_sources() {
    let env = BatchEnv::with(None, None, false);
    let ids: Vec<i64> = (1..=2).map(|k| env.png(k).0).collect();
    let job = super::batch::start_merge(&env.jobs, &merge_req(&ids, Default::default())).unwrap();
    for id in &ids {
        let id = *id;
        assert_eq!(
            env.lib
                .db()
                .read(move |c| super::save::pending_save(c, id))
                .unwrap(),
            None
        );
    }
    env.jobs.start().unwrap();
    overwrite(&env, ids[0], doc(&[flip("vertical")], None));
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
    let made = env.rows(job)[0].4.unwrap();
    // 来歴は読んだときの頭（保存のあとなら版 1、前なら版 0）
    let src = sources_of(&env.lib, made);
    assert_eq!(src.len(), 2);
    assert!(matches!(src[0].1, 0 | 1), "{src:?}");
    assert_eq!(src[1].1, 0);
    assert_consistent(&env.lib);
}

/// M7：N → 1 は保存のジョブに譲る（読む途中に保存が積まれたら止めて待ちに戻り、保存のあとに読み直して 1 枚だけ作る）。
#[test]
fn m7_a_merge_yields_to_a_save() {
    let saw_queued = Arc::new(AtomicBool::new(false));
    let slot: Arc<Mutex<Option<Weak<JobManager>>>> = Arc::default();
    let fired = Arc::new(AtomicBool::new(false));
    let env = {
        let (slot, fired) = (slot.clone(), fired.clone());
        BatchEnv::build(
            None,
            None,
            true,
            Hooks {
                prepare: Some(Arc::new(move |_lib: &Library, _item| {
                    if !fired.swap(true, Ordering::SeqCst)
                        && let Some(jobs) = slot.lock().unwrap().as_ref().and_then(Weak::upgrade)
                    {
                        jobs.enqueue(JobKind::Edit, "{}".into()).unwrap();
                        // 見張りが譲る指示を読むまで待つ（読む途中で止まるように）
                        std::thread::sleep(Duration::from_millis(200));
                    }
                })),
                edit: Some(Arc::new(WaitsForTheBatch {
                    saw_queued: saw_queued.clone(),
                })),
                ..Hooks::default()
            },
        )
    };
    *slot.lock().unwrap() = Some(Arc::downgrade(&env.jobs));
    let ids: Vec<i64> = (1..=4).map(|k| env.png(k).0).collect();
    let record = run_merge(&env, &merge_req(&ids, Default::default()));
    assert_done(&record);
    assert!(saw_queued.load(Ordering::SeqCst), "N → 1 が譲らなかった");
    let log = std::fs::read_to_string(env.lib.layout().job_log_path(record.id)).unwrap();
    assert!(log.contains("ほかのジョブに譲るため中断"), "{log}");
    let sheets = count(&env.lib, "SELECT count(DISTINCT item_id) FROM item_sources");
    assert_eq!(sheets, 1);
    assert!(env.rows(record.id).iter().all(|r| r.1 == "saved"));
    assert_consistent(&env.lib);
}

/// M8：シートの引数（間隔・1 行の数）は積む前と事前チェックで確かめる（Invalid）。画像でも音声でもない元は N → 1 の言葉で飛ばす。
/// 事前チェックの 1 行の数は実際に並ぶ数。
#[test]
fn m8_sheet_params_are_checked_before_queueing() {
    let env = BatchEnv::new();
    let (a, _) = env.png(1);
    let (video, _) = add_item(&env.lib, b"fake movie bytes", "mp4", "動画");
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE items SET kind = 'video', proxy_state = 'pending', analysis_state = 'not_needed' WHERE id = ?1",
                [video],
            )?;
            Ok(())
        })
        .unwrap();
    let bad = kura_engine::selection::SheetParams {
        gap_x: kura_engine::selection::MAX_GAP + 1,
        ..Default::default()
    };
    assert!(matches!(
        super::batch::start_merge(&env.jobs, &merge_req(&[a], bad)),
        Err(crate::CoreError::Invalid(_))
    ));
    let canceled = AtomicBool::new(false);
    let progress = |_: u64, _: u64| {};
    let cx = PrecheckCx {
        canceled: &canceled,
        progress: &progress,
    };
    assert!(matches!(
        super::batch::precheck_merge(&env.lib, &env.engine, &merge_req(&[a], bad), &cx),
        Err(crate::CoreError::Invalid(_))
    ));
    let p = super::batch::precheck_merge(
        &env.lib,
        &env.engine,
        &merge_req(&[a, video], Default::default()),
        &cx,
    )
    .unwrap();
    assert_eq!(p.skips.len(), 1);
    assert!(
        p.skips[0].message.contains("スプライトシートにできません"),
        "{:?}",
        p.skips[0]
    );
    let p = super::batch::precheck_merge(
        &env.lib,
        &env.engine,
        &merge_req(
            &[a],
            kura_engine::selection::SheetParams {
                columns: Some(8),
                ..Default::default()
            },
        ),
        &cx,
    )
    .unwrap();
    assert_eq!(p.sheet.unwrap().columns, 1);
    assert_consistent(&env.lib);
}
