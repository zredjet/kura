//! 一括加工のジョブの試験（M14 段 4。docs/editing.md §14「一括加工の受け入れテスト」の B1〜）。固定の段のコーデック（と偽の
//! Platform のデコーダ）と偽のエンジンの部品で、Linux でも回る。耐久は数える偽物（ファイルの同期は `sync_all`）で、区切りの
//! 同期の回数を見る。どの試験も終わりに整合性・予告・staging・版の不変条件を確かめる。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kura_engine::doc::NodeId;
use kura_engine::recipe::{Binding, SettingKey, Source};
use kura_engine::{Graph, NoMonitor};
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
use crate::jobs::{JobEvent, JobKind, JobManager, JobRecord, JobState};
use crate::library::{Library, LibrarySession, Services};
use crate::media::fake::FakeBackend;

/// コミットの直前の割り込み（ライブラリ・エンジンの部品・アイテムの ID）。
type CommitHook = Arc<dyn Fn(&Library, &Arc<EngineServices>, i64) + Send + Sync>;

/// 対象の行（item, state, code, version_id, out_item_id）。
type Row = (i64, String, Option<String>, Option<i64>, Option<i64>);

/// 数える耐久（ファイルの同期は本物の `sync_all`、区切りは数えるだけ）。
#[derive(Default)]
struct Counting {
    files: AtomicUsize,
    barriers: AtomicUsize,
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
    jobs: JobManager,
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
        Self::build(codecs, crash, start, None)
    }

    /// コミットの直前の割り込み `hook`（ライブラリ・エンジンの部品・アイテムの ID を受け取る）つき。
    fn with_hook(hook: CommitHook) -> Self {
        Self::build(None, None, true, Some(hook))
    }

    fn build(
        codecs: Option<Arc<Mutex<CodecSet>>>,
        crash: Option<(usize, Fault)>,
        start: bool,
        hook: Option<CommitHook>,
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
        if let Some(hook) = hook {
            let (lib, engine) = (lib.clone(), engine.clone());
            batch.before_commit = Some(Arc::new(move |item| hook(&lib, &engine, item)));
        }
        let events: Arc<Mutex<Vec<JobEvent>>> = Arc::default();
        let jobs = JobManager::new(
            lib.clone(),
            vec![
                (JobKind::Edit, Arc::new(EditHandler::new(engine.clone()))),
                (JobKind::Batch, Arc::new(batch)),
            ],
            sink(&events),
        )
        .unwrap();
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

/// B4・B5：一時停止すると pending の行が残り、再開すると残りだけを処理する（二重にしない）。中止すると済んだ行は残り、
/// 残りは canceled。
#[test]
fn b4_b5_pause_resume_and_cancel() {
    let env = BatchEnv::with(None, None, false);
    let g = doc(&[flip("vertical")], None);
    let recipe = env.recipe("反転", &g, Vec::new());
    let ids: Vec<i64> = (1..=8).map(|k| env.png(k).0).collect();
    // 動かす前に一時停止すると、行はすべて pending のまま
    let job = env.start(recipe, &ids, BatchMode::Overwrite, false);
    env.jobs.pause(job).unwrap();
    env.jobs.start().unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_eq!(env.jobs.get(job).unwrap().state, JobState::Paused);
    assert!(env.rows(job).iter().all(|r| r.1 == "pending"));
    // 一時停止の間は、そのアイテムを開かない・保存を積まない（一括で待っている）
    assert_eq!(
        env.lib.db().read(|c| pending_batch(c, ids[0])).unwrap(),
        Some(job)
    );
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
    // 中止：待ちのまま中止すると残りの行は canceled
    let more: Vec<i64> = (20..=23).map(|k| env.png(k).0).collect();
    let env2 = BatchEnv::with(None, None, false);
    let _ = more;
    let recipe2 = env2.recipe("反転", &g, Vec::new());
    let ids2: Vec<i64> = (30..=33).map(|k| env2.png(k).0).collect();
    let job2 = env2.start(recipe2, &ids2, BatchMode::Overwrite, false);
    env2.jobs.cancel(job2).unwrap();
    assert_eq!(env2.jobs.get(job2).unwrap().state, JobState::Canceled);
    assert!(env2.rows(job2).iter().all(|r| r.1 == "canceled"));
    assert_consistent(&env.lib);
    assert_consistent(&env2.lib);
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
