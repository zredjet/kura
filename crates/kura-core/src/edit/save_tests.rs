//! 保存のジョブの受け入れテスト（docs/editing.md §14.1 の S1〜S9・S14・S15・S17 と、M12 の段 2 の決定 9〜12、版に戻す・
//! 下書き・中止・門の間の Conflict）。固定の段のコーデック（と偽の Platform のデコーダ）と偽のエンジンの部品で、Linux でも
//! 回る。どのテストも終わりに [`assert_consistent`]（整合性・予告・staging・版の不変条件・ジョブ）で締める。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kura_engine::codec::{CodecEnv, CodecSet, Decoder, ReadSeek, Tier};
use kura_engine::doc::{DecodeOptions, FormatChoice, InputId};
use kura_engine::render::InputValue;
use kura_engine::testgen::InputGen;
use kura_engine::testing::platform::FakePlatformDecoder;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, Engine, EngineError, Graph, Limits, Monitor, NoMonitor, Shape};
use rusqlite::params;
use serde_json::{Value as Json, json};

use super::gc::{GcClock, GcFault, GcHandler, GcHook};
use super::save::{Base, EditHandler, Fault, Hook, SaveAction, SaveRequest, Stage, start_save};
use super::services::fake::FakeEngineFactory;
use super::versions::{self, FileState, Head, Version, VersionKind};
use super::{ENGINE_WORK_DIR, EngineServices, drafts};
use crate::CoreError;
use crate::import::{ImportInput, ImportOptions, ImportResult, hash_file, start_import};
use crate::jobs::{EventSink, JobEvent, JobKind, JobManager, JobRecord, JobState};
use crate::library::{
    IntegrityDepth, Library, LibrarySession, Services, check_files, check_integrity, hex, ledger,
};
use crate::media::fake::FakeBackend;

pub(super) const WAIT: Duration = Duration::from_secs(60);

// ---- 道具 ----

/// ドット絵の PNG（固定の段で書いたもの。読み直して書くと同じバイト列になる）。
pub(super) fn sprite(seed: u64) -> Vec<u8> {
    let value = InputGen::PixelSprite {
        w: 24,
        h: 20,
        fmt: PixelFormat::Rgba8,
        seed,
    }
    .generate()
    .unwrap();
    CodecSet::pinned()
        .encode_bytes(&value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

pub(super) fn hash_of(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

pub(super) fn flip(axis: &str) -> (&'static str, Json) {
    ("image.flip", json!({ "axis": axis }))
}

/// 元に対する累積の文書（`to` があれば出力の形式）。
pub(super) fn doc(ops: &[(&'static str, Json)], to: Option<&str>) -> Graph {
    let mut g = Graph::new_single();
    for (op, params) in ops {
        g.push_linear(op, 1, params.clone(), 0).unwrap();
    }
    if let Some(format) = to {
        g.outputs[0].format = FormatChoice::To {
            format: format.into(),
            options: json!({}),
        };
    }
    g
}

/// 保存のジョブの外で、PNG に文書を当てて PNG に書いたバイト列（保存の出力と同じになるはずのもの）。
pub(super) fn render_png(png: &[u8], g: &Graph) -> Vec<u8> {
    let (value, _) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(png),
            "png",
            &Default::default(),
            Engine::new().unwrap().limits(),
            &NoMonitor,
        )
        .unwrap();
    render_value_png(value, g, hash_of(png))
}

/// 保存のジョブの外で、元（中身 `source`）を読んだ値 `value` に文書を当てて PNG に書いたバイト列。
pub(super) fn render_value_png(value: kura_engine::Value, g: &Graph, source: [u8; 32]) -> Vec<u8> {
    let engine = Engine::new().unwrap();
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), value.shape())]))
        .unwrap();
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value,
            source: Digest(source),
        },
    )]);
    let out = engine.render(&plan, &inputs, &NoMonitor).unwrap();
    CodecSet::pinned()
        .encode_bytes(&out.outputs[0].value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

pub(super) fn sink(events: &Arc<Mutex<Vec<JobEvent>>>) -> EventSink {
    let events = events.clone();
    Arc::new(move |e| events.lock().unwrap().push(e))
}

pub(super) fn count(lib: &Library, sql: &str) -> i64 {
    lib.db()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

pub(super) fn one<T: rusqlite::types::FromSql>(lib: &Library, sql: &str, id: i64) -> T {
    lib.db()
        .read(|c| Ok(c.query_row(sql, [id], |r| r.get(0))?))
        .unwrap()
}

/// アイテムのファイルを置いて行を入れる（サムネイル・解析は済んだものとして）。
pub(super) fn add_item(lib: &Library, bytes: &[u8], ext: &str, title: &str) -> (i64, [u8; 32]) {
    let hash = hash_of(bytes);
    let path = lib.layout().content_path(&hash, ext);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let (ext, title, size) = (ext.to_string(), title.to_string(), bytes.len() as i64);
    let id = lib
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, width, height, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?2, 'image', ?3, ?4, 24, 20, ?5, ?5 || '.' || ?2, '/src/' || ?5, 'local', 1, 1, 'ready',
                         'not_needed', 'ready')",
                params![&hash[..], ext, crate::import::mime_for(&ext), size, title],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap();
    (id, hash)
}

pub(super) fn head(lib: &Library, id: i64) -> Head {
    lib.db().read(|c| versions::head(c, id)).unwrap().unwrap()
}

pub(super) fn base_of(lib: &Library, id: i64) -> Base {
    let h = head(lib, id);
    Base {
        hash: h.hash,
        version: h.version,
    }
}

pub(super) fn versions_of(lib: &Library, id: i64) -> Vec<Version> {
    lib.db().read(|c| versions::list(c, id)).unwrap()
}

pub(super) fn states(lib: &Library, id: i64) -> Vec<(i64, FileState)> {
    versions_of(lib, id)
        .iter()
        .map(|v| (v.version, v.file_state))
        .collect()
}

pub(super) fn result_of(record: &JobRecord) -> Json {
    serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap()
}

/// `files/` のファイル（相対パス）の一覧。
pub(super) fn files(lib: &Library) -> Vec<String> {
    let root = lib.layout().files_dir();
    let mut out: Vec<String> = walkdir::WalkDir::new(&root)
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            e.path()
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    out.sort();
    out
}

/// staging のファイル（エンジンの置き場を含む）。
pub(super) fn staged(lib: &Library) -> Vec<PathBuf> {
    walkdir::WalkDir::new(lib.layout().staging_dir())
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// 台帳・版・ファイルのスナップショット（Conflict で何も変わらないことを照らす）。
/// (files/ のファイル, 台帳の (hash, ext), 版の (アイテム, 版, 状態), アイテムの数)。
pub(super) type Snapshot = (
    Vec<String>,
    Vec<(Vec<u8>, String)>,
    Vec<(i64, i64, String)>,
    i64,
);

pub(super) fn snapshot(lib: &Library) -> Snapshot {
    let blobs = lib
        .db()
        .read(|c| {
            let mut stmt = c.prepare("SELECT hash, ext FROM blobs ORDER BY hash")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    let vs = lib
        .db()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT item_id, version, file_state FROM item_versions ORDER BY item_id, version",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    (
        files(lib),
        blobs,
        vs,
        count(lib, "SELECT count(*) FROM items"),
    )
}

/// 食い違い 0：整合性（DB にないファイル・ファイルのない台帳の行・参照されない台帳の行）が 0、台帳のどの行もファイルの
/// 中身が hash と同じ、予告が空、staging が空、版の不変条件が空、保存のジョブが動いていない。
pub(super) fn assert_consistent(lib: &Library) {
    let report = check_integrity(lib.db(), lib.layout(), 0, IntegrityDepth::Startup).unwrap();
    assert_eq!(
        (
            report.missing_files,
            report.orphan_files,
            report.unreferenced_blobs
        ),
        (0, 0, 0),
        "{report:?}"
    );
    let blobs: Vec<(Vec<u8>, String)> = lib
        .db()
        .read(|c| {
            let mut stmt = c.prepare("SELECT hash, ext FROM blobs")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    for (hash, ext) in blobs {
        let hash: [u8; 32] = hash.try_into().unwrap();
        let path = lib.layout().content_path(&hash, &ext);
        assert_eq!(hash_file(&path).unwrap(), hash, "{}", path.display());
    }
    assert_eq!(count(lib, "SELECT count(*) FROM ledger_pending"), 0);
    assert_eq!(staged(lib), Vec::<PathBuf>::new(), "staging に残骸がある");
    let broken = lib.db().read(versions::check_invariants).unwrap();
    assert!(broken.is_empty(), "{broken:?}");
    assert_eq!(
        count(
            lib,
            "SELECT count(*) FROM jobs WHERE kind IN ('edit', 'gc') AND state NOT IN ('done', 'failed', 'canceled')"
        ),
        0
    );
}

/// GC の時計（テストが決める）：決めた時刻（なければ本物の時計）と、残っているバックアップの作成時刻。
#[derive(Clone, Default)]
pub(super) struct TestClock {
    fixed: Arc<Mutex<Option<i64>>>,
    backups: Arc<Mutex<Vec<i64>>>,
}

impl TestClock {
    /// GC の時計。
    pub(super) fn gc(&self) -> GcClock {
        let (fixed, backups) = (self.fixed.clone(), self.backups.clone());
        GcClock::new(
            Arc::new(move || fixed.lock().unwrap().unwrap_or_else(crate::time::now_ms)),
            Arc::new(move |_| Ok(backups.lock().unwrap().clone())),
        )
    }

    /// 時刻を決める（None で本物の時計に戻す）。
    pub(super) fn set(&self, now: Option<i64>) {
        *self.fixed.lock().unwrap() = now;
    }

    /// 残っているバックアップの作成時刻を決める。
    pub(super) fn set_backups(&self, backups: &[i64]) {
        *self.backups.lock().unwrap() = backups.to_vec();
    }
}

/// 1 日（ミリ秒）。
pub(super) const DAY: i64 = 24 * 60 * 60 * 1000;

/// Env の組み立て（保存の強制終了・割り込み、GC の割り込み・強制終了）。
#[derive(Default)]
pub(super) struct Setup {
    pub(super) factory: FakeEngineFactory,
    pub(super) fault: Option<Fault>,
    pub(super) gc_hook: Option<GcHook>,
    pub(super) gc_fault: Option<GcFault>,
}

/// 保存・GC・取り込みのジョブだけを動かすライブラリ（サムネイル・解析のジョブは登録しないので、置き換えたあとの pending が
/// 見える）。GC の時計はテストが決め（既定は本物の時計・バックアップなし）、保存のコミットのあとも同じ時計で GC の候補を数える。
pub(super) struct Env {
    pub(super) dir: tempfile::TempDir,
    pub(super) lib: Arc<Library>,
    pub(super) jobs: JobManager,
    pub(super) events: Arc<Mutex<Vec<JobEvent>>>,
    pub(super) engine: Arc<EngineServices>,
    pub(super) clock: TestClock,
}

impl Env {
    pub(super) fn new() -> Self {
        Self::with(FakeEngineFactory::default(), None)
    }

    pub(super) fn with(factory: FakeEngineFactory, fault: Option<Fault>) -> Self {
        Self::setup(
            Setup {
                factory,
                fault,
                ..Setup::default()
            },
            |_| None,
        )
    }

    /// 段ごとの割り込み（`hook` はライブラリを受け取って作る）つきのもの。
    fn with_hook(hook: impl FnOnce(&Arc<Library>) -> Hook) -> Self {
        Self::setup(Setup::default(), |lib| Some(hook(lib)))
    }

    /// 組み立てを渡して作る。
    pub(super) fn setup(setup: Setup, hook: impl FnOnce(&Arc<Library>) -> Option<Hook>) -> Self {
        let Setup {
            factory,
            fault,
            gc_hook,
            gc_fault,
        } = setup;
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let engine = Arc::new(EngineServices::new(
            Arc::new(factory),
            lib.layout().staging_dir().join(ENGINE_WORK_DIR),
        ));
        let clock = TestClock::default();
        let mut handler = EditHandler::new(engine.clone()).with_gc(clock.gc());
        if let Some(fault) = fault {
            handler = handler.with_fault(fault);
        }
        if let Some(hook) = hook(&lib) {
            handler = handler.with_hook(hook);
        }
        let mut gc = GcHandler::new(engine.clone(), clock.gc());
        if let Some(hook) = gc_hook {
            gc = gc.with_hook(hook);
        }
        if let Some(fault) = gc_fault {
            gc = gc.with_fault(fault);
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let jobs = JobManager::new(
            lib.clone(),
            vec![
                (JobKind::Edit, Arc::new(handler)),
                (JobKind::Gc, Arc::new(gc)),
                (
                    JobKind::Import,
                    Arc::new(crate::import::ImportHandler::new(Arc::new(FakeBackend))),
                ),
            ],
            sink(&events),
        )
        .unwrap();
        jobs.start().unwrap();
        Self {
            dir,
            lib,
            jobs,
            events,
            engine,
            clock,
        }
    }

    /// 時刻を `now`（None なら置き換えから 8 日あと）にして GC のジョブを積み、動き終えるのを待つ（時計は本物に戻す）。
    pub(super) fn gc_at(&self, now: Option<i64>) -> JobRecord {
        self.clock
            .set(Some(now.unwrap_or_else(|| crate::time::now_ms() + 8 * DAY)));
        let id = self.jobs.enqueue(JobKind::Gc, "{}".into()).unwrap();
        assert!(self.jobs.wait_idle(WAIT));
        self.clock.set(None);
        self.jobs.get(id).unwrap()
    }

    /// 8 日たったことにして GC を動かす（バックアップは今の決めたもの）。ジョブは done。
    pub(super) fn gc(&self) -> Json {
        let record = self.gc_at(None);
        assert_done(&record);
        result_of(&record)
    }

    pub(super) fn revert(&self, item_id: i64, to: i64) -> JobRecord {
        self.save(
            item_id,
            base_of(&self.lib, item_id),
            SaveAction::Revert { to },
        )
    }

    pub(super) fn png(&self, seed: u64) -> (i64, [u8; 32], Vec<u8>) {
        let bytes = sprite(seed);
        let (id, hash) = add_item(&self.lib, &bytes, "png", &format!("絵{seed}"));
        (id, hash, bytes)
    }

    pub(super) fn start(&self, item_id: i64, base: Base, action: SaveAction) -> i64 {
        start_save(
            self.lib.db(),
            &self.jobs,
            &SaveRequest {
                item_id,
                base,
                action,
            },
        )
        .unwrap()
    }

    pub(super) fn save(&self, item_id: i64, base: Base, action: SaveAction) -> JobRecord {
        let id = self.start(item_id, base, action);
        assert!(self.jobs.wait_idle(WAIT));
        self.jobs.get(id).unwrap()
    }

    pub(super) fn overwrite(&self, item_id: i64, graph: Graph) -> JobRecord {
        self.save(
            item_id,
            base_of(&self.lib, item_id),
            SaveAction::Overwrite { graph },
        )
    }

    pub(super) fn save_as(&self, item_id: i64, graph: Graph, title: &str, stem: &str) -> JobRecord {
        self.save(
            item_id,
            base_of(&self.lib, item_id),
            SaveAction::SaveAs {
                graph,
                title: title.into(),
                stem: stem.into(),
            },
        )
    }

    pub(super) fn count(&self, sql: &str) -> i64 {
        count(&self.lib, sql)
    }
}

pub(super) fn assert_done(record: &JobRecord) {
    assert_eq!(
        record.state,
        JobState::Done,
        "{:?} {:?}",
        record.message,
        record.error_code
    );
}

pub(super) fn assert_conflict(record: &JobRecord) {
    assert_eq!(record.state, JobState::Failed, "{:?}", record.message);
    assert_eq!(
        record.error_code.as_deref(),
        Some("Conflict"),
        "{:?}",
        record.message
    );
}

/// アイテムに所属・記録をひととおり付ける（タグ・フォルダ・ユニット・プレイリスト・再生の記録・評価・メモ・出どころ・
/// 素材の設定）。
fn decorate(lib: &Library, id: i64) {
    lib.db()
        .write(move |tx| {
            tx.execute_batch(
                "INSERT OR IGNORE INTO tags (id, namespace, name, created_at) VALUES (1, '', '海', 1), (2, '作者', 'A', 1);
                 INSERT OR IGNORE INTO folders (id, parent_id, name, pos, created_at) VALUES (1, NULL, '写真', 1, 1);
                 INSERT OR IGNORE INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'set', '束', 'manual', 1, 1);
                 INSERT OR IGNORE INTO playlists (id, name, kind, pos, created_at, updated_at) VALUES (1, '夜', 'manual', 1, 1, 1);",
            )?;
            tx.execute(
                "INSERT INTO item_tags (tag_id, item_id, source) VALUES (1, ?1, 'user'), (2, ?1, 'import')",
                [id],
            )?;
            tx.execute("INSERT INTO item_folders (folder_id, item_id, pos) VALUES (1, ?1, 7)", [id])?;
            tx.execute("INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, ?1, 1)", [id])?;
            tx.execute(
                "INSERT INTO playlist_entries (playlist_id, pos, entry_type, item_id) VALUES (1, 1, 'item', ?1)",
                [id],
            )?;
            tx.execute(
                "INSERT INTO play_events (item_id, played_at, played_ms, completed) VALUES (?1, 5, 1000, 1)",
                [id],
            )?;
            tx.execute(
                "UPDATE items SET rating = 4, note = 'メモ', play_count = 3, source_site = 'pixiv',
                        source_url = 'https://example.com/a', captured_at = 123 WHERE id = ?1",
                [id],
            )?;
            tx.execute(
                "INSERT INTO asset_settings (item_id, pixel_art, pivot_x, pivot_y, updated_at) VALUES (?1, 1, 0.5, 1.0, 1)",
                [id],
            )?;
            Ok(())
        })
        .unwrap();
}

/// 所属と記録の数（タグ・フォルダ・ユニット・プレイリスト・再生の記録・素材の設定）。
fn memberships(lib: &Library, id: i64) -> [i64; 6] {
    [
        one(lib, "SELECT count(*) FROM item_tags WHERE item_id = ?1", id),
        one(
            lib,
            "SELECT count(*) FROM item_folders WHERE item_id = ?1",
            id,
        ),
        one(
            lib,
            "SELECT count(*) FROM unit_items WHERE item_id = ?1",
            id,
        ),
        one(
            lib,
            "SELECT count(*) FROM playlist_entries WHERE item_id = ?1",
            id,
        ),
        one(
            lib,
            "SELECT count(*) FROM play_events WHERE item_id = ?1",
            id,
        ),
        one(
            lib,
            "SELECT count(*) FROM asset_settings WHERE item_id = ?1",
            id,
        ),
    ]
}

// ---- S1・S2：上書き ----

/// S1：上書きしても ID・タグ・フォルダ・ユニット・プレイリストのエントリ・再生の記録・素材の設定・表示名は同じ。hash が
/// 変わり、サムネイル・解析は作り直しを待つ。版 0（元）と版 1 ができ、元のファイルは版 0 が持つ（決定 11）。
#[test]
fn s1_overwrite_keeps_identity_and_adds_versions_0_and_1() {
    let env = Env::new();
    let (a, h0, png) = env.png(1);
    decorate(&env.lib, a);
    let before = memberships(&env.lib, a);
    let record = env.overwrite(a, doc(&[flip("vertical")], None));
    assert_done(&record);
    let result = result_of(&record);
    let want = hash_of(&render_png(&png, &doc(&[flip("vertical")], None)));
    assert_ne!(want, h0, "左右対称でないドット絵");
    assert_eq!(result["outcome"], "saved");
    assert_eq!(result["itemId"], a);
    assert_eq!(result["version"], 1);
    assert_eq!(result["hash"], hex(&want));
    assert_eq!(result["anchor"], Json::Null);

    assert_eq!(env.count("SELECT count(*) FROM items"), 1);
    assert_eq!(memberships(&env.lib, a), before);
    let h = head(&env.lib, a);
    assert_eq!((h.hash, h.ext.as_str(), h.version), (want, "png", 1));
    let (title, name, thumb, analysis, play_count, mime): (String, String, String, String, i64, String) = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT title, original_name, thumb_state, analysis_state, play_count, mime FROM items WHERE id = ?1",
                [a],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (
            title.as_str(),
            name.as_str(),
            thumb.as_str(),
            analysis.as_str(),
            play_count,
            mime.as_str()
        ),
        ("絵1", "絵1.png", "pending", "pending", 3, "image/png")
    );
    let pivot: f64 = one(
        &env.lib,
        "SELECT pivot_x FROM asset_settings WHERE item_id = ?1",
        a,
    );
    assert_eq!(pivot, 0.5, "素材の設定は残す（決定 11）");

    let vs = versions_of(&env.lib, a);
    assert_eq!(vs.len(), 2);
    assert_eq!(
        (
            vs[0].kind,
            vs[0].output_hash,
            vs[0].file_state,
            vs[0].doc_id,
            vs[0].job_id
        ),
        (VersionKind::Original, h0, FileState::Hold, None, None)
    );
    assert_eq!(
        (
            vs[1].kind,
            vs[1].output_hash,
            vs[1].file_state,
            vs[1].base_version,
            vs[1].job_id
        ),
        (
            VersionKind::Edit,
            want,
            FileState::Hold,
            Some(0),
            Some(record.id)
        )
    );
    assert_eq!((vs[1].width, vs[1].height), (Some(24), Some(20)));
    let r0 = vs[0].record().unwrap();
    assert_eq!(r0.media.unwrap().mime.as_deref(), Some("image/png"));
    assert_eq!(vs[0].value_digest, Some(r0.decoded.unwrap().value_digest));
    assert_eq!(
        vs[1].record().unwrap().encoded.unwrap().codec,
        "png-rs-enc/1"
    );
    let inputs = env
        .lib
        .db()
        .read(|c| versions::inputs(c, vs[1].id))
        .unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!((inputs[0].source_hash, inputs[0].anchor_id), (h0, None));
    assert!(
        env.lib.layout().content_path(&h0, "png").exists(),
        "元は版 0 が持つ"
    );
    assert!(env.lib.layout().content_path(&want, "png").exists());
    assert_consistent(&env.lib);
}

/// S2：2 回上書きすると、版 1 は消し待ち（置き換えの時刻つき）、版 0（元）と版 2（今）は保持。
#[test]
fn s2_second_overwrite_waits_version_1_and_holds_original() {
    let env = Env::new();
    let (a, h0, _) = env.png(2);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let h1 = head(&env.lib, a).hash;
    assert_done(&env.overwrite(a, doc(&[flip("vertical"), flip("horizontal")], None)));
    let h2 = head(&env.lib, a).hash;
    assert!(h0 != h1 && h1 != h2 && h0 != h2);
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Waiting),
            (2, FileState::Hold)
        ]
    );
    let vs = versions_of(&env.lib, a);
    assert!(vs[1].replaced_at.is_some() && vs[1].file_blob_id.is_some());
    assert_eq!((vs[0].replaced_at, vs[2].replaced_at), (None, None));
    assert_eq!(vs[2].base_version, Some(1));
    for h in [h0, h1, h2] {
        assert!(env.lib.layout().content_path(&h, "png").exists());
    }
    assert_consistent(&env.lib);
}

// ---- S5：別名で保存 ----

/// S5：別名で保存すると新しいアイテムと来歴ができ、元のアイテムは変わらない（版の行も足さない）。新しいアイテムの元は元の
/// アイテムの元（今ではない）。タグ・フォルダ・評価・メモ・出どころ（source_site・source_url・captured_at）を写し、ユニット・
/// プレイリスト・再生の記録・素材の設定は写さない。original_path は空（決定 1・4・9）。
#[test]
fn s5_save_as_inherits_original_and_copies_metadata() {
    let env = Env::new();
    let (a, h0, _) = env.png(3);
    decorate(&env.lib, a);
    let a_before = memberships(&env.lib, a);
    let record = env.save_as(a, doc(&[flip("vertical")], None), "コピー", "copy");
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(result["outcome"], "saved");
    let b = result["itemId"].as_i64().unwrap();
    assert_ne!(b, a);
    assert!(
        versions_of(&env.lib, a).is_empty(),
        "元のアイテムに版の行を足さない"
    );
    assert_eq!(head(&env.lib, a).hash, h0);
    assert_eq!(memberships(&env.lib, a), a_before);

    type Copied = (
        String,
        String,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        i64,
    );
    let (title, name, path, rating, note, site, url, captured, play_count): Copied = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT title, original_name, original_path, rating, note, source_site, source_url, captured_at, play_count
                   FROM items WHERE id = ?1",
                [b],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )?)
        })
        .unwrap();
    assert_eq!(
        (title.as_str(), name.as_str(), path, rating, note.as_deref()),
        ("コピー", "copy.png", None, 4, Some("メモ"))
    );
    assert_eq!(
        (site.as_deref(), url.as_deref(), captured, play_count),
        (Some("pixiv"), Some("https://example.com/a"), Some(123), 0)
    );
    // タグ 2・フォルダ 1 は写し、ユニット・プレイリスト・再生の記録・素材の設定は写さない
    assert_eq!(memberships(&env.lib, b), [2, 1, 0, 0, 0, 0]);
    let tag_sources: String = one(
        &env.lib,
        "SELECT group_concat(source, ',') FROM (SELECT source FROM item_tags WHERE item_id = ?1 ORDER BY tag_id)",
        b,
    );
    assert_eq!(tag_sources, "user,import");
    let pos: f64 = one(
        &env.lib,
        "SELECT pos FROM item_folders WHERE item_id = ?1",
        b,
    );
    assert_eq!(pos, 7.0);

    let vs = versions_of(&env.lib, b);
    assert_eq!(vs.len(), 2);
    assert_eq!((vs[0].output_hash, vs[0].file_state), (h0, FileState::Hold));
    let a_blob = env.lib.db().read(|c| ledger::id_of(c, &h0)).unwrap();
    assert_eq!(
        vs[0].file_blob_id, a_blob,
        "B の元は A の元の台帳の行を共有する"
    );
    assert_eq!(vs[1].job_id, Some(record.id));
    let (src, sv, sh): (Option<i64>, i64, Vec<u8>) = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT source_item_id, source_version, source_hash FROM item_derivations WHERE item_id = ?1",
                [b],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!((src, sv, sh), (Some(a), 0, h0.to_vec()));
    assert_consistent(&env.lib);

    // A を上書きしてから別名で保存すると、新しいアイテムの元は A の今ではなく A の元
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    let h1 = head(&env.lib, a).hash;
    let record = env.save_as(
        a,
        doc(&[flip("vertical"), flip("horizontal")], None),
        "二つ目",
        "second",
    );
    assert_done(&record);
    let c = result_of(&record)["itemId"].as_i64().unwrap();
    let vs = versions_of(&env.lib, c);
    assert_eq!(vs[0].output_hash, h0);
    let (sv, sh): (i64, Vec<u8>) = env
        .lib
        .db()
        .read(|conn| {
            Ok(conn.query_row(
                "SELECT source_version, source_hash FROM item_derivations WHERE item_id = ?1",
                [c],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((sv, sh), (1, h1.to_vec()));
    assert_eq!(
        states(&env.lib, a),
        vec![(0, FileState::Hold), (1, FileState::Hold)]
    );
    assert_consistent(&env.lib);
}

/// S5：別名で保存の結果が元のアイテムの元と同じ中身なら、新しいアイテムを作らずそのアイテムを示す（決定 3）。何も置かない。
#[test]
fn s5_save_as_matching_original_points_to_it() {
    let env = Env::new();
    let (a, _, _) = env.png(4);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let before = snapshot(&env.lib);
    // 文書は元に対する累積なので、空の文書は元をそのまま書き直す
    let record = env.save_as(a, Graph::new_single(), "元", "orig");
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(result["outcome"], "existing");
    assert_eq!(result["itemId"], a);
    assert_eq!(result["role"], "original");
    assert_eq!(snapshot(&env.lib), before);
    assert_consistent(&env.lib);
}

/// 決定 12：別名で保存の結果が元のアイテム自身の今と同じなら、Conflict にせずそのアイテムを示す（existing）。
#[test]
fn save_as_matching_its_own_current_is_existing() {
    let env = Env::new();
    let (a, _, _) = env.png(5);
    let before = snapshot(&env.lib);
    let record = env.save_as(a, Graph::new_single(), "同じ", "same");
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(
        (result["outcome"].as_str(), result["role"].as_str()),
        (Some("existing"), Some("current"))
    );
    assert_eq!(result["itemId"], a);
    assert_eq!(snapshot(&env.lib), before);
    assert_consistent(&env.lib);
}

// ---- S6：土台が変わった ----

/// S6：同じ土台の保存 2 つ。2 つ目は Conflict で、版・files・台帳が変わらない。
#[test]
fn s6_conflict_when_head_changed() {
    let env = Env::new();
    let (a, h0, _) = env.png(6);
    let opened = Base {
        hash: h0,
        version: 0,
    };
    assert_done(&env.save(
        a,
        opened,
        SaveAction::Overwrite {
            graph: doc(&[flip("vertical")], None),
        },
    ));
    let before = snapshot(&env.lib);
    let record = env.save(
        a,
        opened,
        SaveAction::Overwrite {
            graph: doc(&[flip("horizontal")], None),
        },
    );
    assert_conflict(&record);
    assert_eq!(snapshot(&env.lib), before);
    assert_consistent(&env.lib);
}

/// S6：出力が今と同じ保存は版だけ足す（ファイルは変えない）。その版を知らない土台の保存は、今が同じでも Conflict。
#[test]
fn s6_conflict_when_version_added_with_same_hash() {
    let env = Env::new();
    let (a, h0, _) = env.png(7);
    let opened = Base {
        hash: h0,
        version: 0,
    };
    let record = env.save(
        a,
        opened,
        SaveAction::Overwrite {
            graph: Graph::new_single(),
        },
    );
    assert_done(&record);
    assert_eq!(result_of(&record)["outcome"], "unchanged");
    let h = head(&env.lib, a);
    assert_eq!((h.hash, h.version), (h0, 1));
    assert_eq!(
        states(&env.lib, a),
        vec![(0, FileState::Hold), (1, FileState::Hold)]
    );
    assert_eq!(env.count("SELECT count(*) FROM blobs"), 1);
    // もう一度同じ中身：版 1 は新しい頭と同じ中身なので released
    assert_done(&env.overwrite(a, Graph::new_single()));
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Released),
            (2, FileState::Hold)
        ]
    );
    let before = snapshot(&env.lib);
    let record = env.save(
        a,
        opened,
        SaveAction::Overwrite {
            graph: doc(&[flip("vertical")], None),
        },
    );
    assert_conflict(&record);
    assert_eq!(snapshot(&env.lib), before);
    assert_consistent(&env.lib);
}

/// 編集の途中でゴミ箱に入れられたアイテムの保存は Conflict（推奨で採った決定）。
#[test]
fn saving_a_trashed_item_is_a_conflict() {
    let env = Env::new();
    let (a, _, _) = env.png(8);
    let opened = base_of(&env.lib, a);
    env.lib
        .db()
        .write(move |tx| crate::items::trash(tx, &[a], 5).map(|_| ()))
        .unwrap();
    let before = snapshot(&env.lib);
    let record = env.save(
        a,
        opened,
        SaveAction::Overwrite {
            graph: doc(&[flip("vertical")], None),
        },
    );
    assert_conflict(&record);
    assert_eq!(snapshot(&env.lib), before);
    assert_consistent(&env.lib);
}

/// 版に戻す先は積む前に確かめる：まだ保存していないアイテム・頭の版・負の版は Invalid、ないアイテムは NotFound（積まない）。
#[test]
fn revert_targets_are_checked_before_queueing() {
    let env = Env::new();
    let (a, h0, _) = env.png(9);
    let start = |item_id: i64, to: i64, version: i64| {
        start_save(
            env.lib.db(),
            &env.jobs,
            &SaveRequest {
                item_id,
                base: Base { hash: h0, version },
                action: SaveAction::Revert { to },
            },
        )
    };
    let err = start(a, 0, 0).unwrap_err();
    assert!(matches!(err, CoreError::Invalid(_)), "{err}");
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    for to in [1, 2, -1] {
        let err = start(a, to, 1).unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)), "{to}: {err}");
    }
    let err = start(999, 0, 1).unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
    let err = start_save(
        env.lib.db(),
        &env.jobs,
        &SaveRequest {
            item_id: 999,
            base: Base {
                hash: h0,
                version: 0,
            },
            action: SaveAction::Overwrite {
                graph: Graph::new_single(),
            },
        },
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
    assert_eq!(
        env.count("SELECT count(*) FROM jobs"),
        1,
        "上書きの 1 本だけ"
    );
    assert_consistent(&env.lib);
}

// ---- S7：出力が別のアイテムと同じ中身 ----

/// S7：上書きの結果が別のアイテム（ゴミ箱のものを含む）の今と同じなら Conflict。files/ に孤立したファイルがなく、staging も空。
#[test]
fn s7_conflict_with_other_current_including_trash() {
    for trashed in [true, false] {
        let env = Env::new();
        let (a, _, png) = env.png(10);
        let graph = doc(&[flip("vertical")], None);
        let (b, _) = add_item(&env.lib, &render_png(&png, &graph), "png", "先に同じ中身");
        if trashed {
            env.lib
                .db()
                .write(move |tx| crate::items::trash(tx, &[b], 5).map(|_| ()))
                .unwrap();
        }
        let before = snapshot(&env.lib);
        let record = env.overwrite(a, graph);
        assert_conflict(&record);
        assert_eq!(snapshot(&env.lib), before);
        assert!(versions_of(&env.lib, a).is_empty());
        assert_eq!(check_files(env.lib.db(), env.lib.layout()).unwrap(), (0, 0));
        assert_consistent(&env.lib);
    }
}

/// S7：上書きの結果が別のアイテムの元と同じ中身なら通し、台帳の行を共有する（決定 3）。
#[test]
fn s7_overwrite_matching_other_original_shares_ledger_row() {
    let env = Env::new();
    let (a, p0, png) = env.png(11);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    // B は A の元を左右に返したもの。B を左右に返すと A の元と同じ中身になる
    let (b, _) = add_item(
        &env.lib,
        &render_png(&png, &doc(&[flip("horizontal")], None)),
        "png",
        "返した",
    );
    let files_before = files(&env.lib).len();
    let record = env.overwrite(b, doc(&[flip("horizontal")], None));
    assert_done(&record);
    assert_eq!(result_of(&record)["reused"], true);
    assert_eq!(head(&env.lib, b).hash, p0);
    let a0 = versions_of(&env.lib, a)[0].file_blob_id;
    let b1 = versions_of(&env.lib, b)[1].file_blob_id;
    assert_eq!(a0, b1, "同じ台帳の行");
    assert_eq!(
        env.count("SELECT count(*) FROM blobs"),
        3,
        "A の元・A の今・B の元"
    );
    assert_eq!(files(&env.lib).len(), files_before, "ファイルは増えない");
    assert_consistent(&env.lib);
}

// ---- S8：形式が変わる上書き ----

/// S8：PNG → WebP の上書き（今が版 1 以降のとき）。ext・mime・台帳の拡張子が新しくなり、古い今（版 1 の PNG）は消し待ち。
/// 表示名・元のファイル名・素材の設定は変えない（決定 10・11）。
#[test]
fn s8_png_to_webp_changes_ext_mime_and_ledger() {
    let env = Env::new();
    let (a, _, _) = env.png(12);
    decorate(&env.lib, a);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let record = env.overwrite(a, doc(&[flip("vertical")], Some("webp")));
    assert_done(&record);
    assert_eq!(result_of(&record)["ext"], "webp");
    let h = head(&env.lib, a);
    assert_eq!(h.ext, "webp");
    let (mime, title, name): (String, String, String) = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT mime, title, original_name FROM items WHERE id = ?1",
                [a],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(
        (mime.as_str(), title.as_str(), name.as_str()),
        ("image/webp", "絵12", "絵12.png")
    );
    assert_eq!(
        env.lib
            .db()
            .read(|c| ledger::ext_of(c, &h.hash))
            .unwrap()
            .as_deref(),
        Some("webp")
    );
    let vs = versions_of(&env.lib, a);
    assert_eq!(
        vs.iter()
            .map(|v| (v.ext.as_str(), v.file_state))
            .collect::<Vec<_>>(),
        vec![
            ("png", FileState::Hold),
            ("png", FileState::Waiting),
            ("webp", FileState::Hold)
        ]
    );
    assert_eq!(vs[2].format.as_deref(), Some("webp"));
    assert_eq!(memberships(&env.lib, a)[5], 1, "素材の設定は残す");
    assert!(env.lib.layout().content_path(&h.hash, "webp").exists());
    assert_consistent(&env.lib);
}

// ---- S9：強制終了 ----

/// 段ごとの、強制終了した直後の状態を確かめる（`anchored` ならアンカーも作る保存で、予告・staging・孤立したファイルは
/// 出力とアンカーの 2 つずつ）。
fn assert_crashed_state(env: &Env, job: i64, at: Fault, out: [u8; 32], anchored: bool) {
    let lib = &env.lib;
    let n: u64 = if anchored { 2 } else { 1 };
    let pending = count(lib, "SELECT count(*) FROM ledger_pending") as u64;
    let placed = lib.layout().content_path(&out, "png").exists();
    let (_, orphans) = check_files(lib.db(), lib.layout()).unwrap();
    let committed = lib
        .db()
        .read(|c| versions::by_job(c, job))
        .unwrap()
        .is_some();
    let parts = staged(lib).len() as u64;
    match at {
        Fault::AfterRender => assert_eq!((pending, placed, committed, parts), (0, false, false, n)),
        Fault::AfterAnnounce => {
            assert_eq!((pending, placed, committed, parts), (n, false, false, n))
        }
        Fault::AfterPlace => {
            assert_eq!((pending, placed, committed, orphans), (n, true, false, n));
        }
        Fault::AfterCommit => {
            assert_eq!((pending, placed, committed, orphans), (0, true, true, 0));
        }
    }
}

/// 偽の HEIC（読むたびに値が変わる）を読めるコーデックの組。
pub(super) fn heic_codecs(decoder: &Arc<FakePlatformDecoder>) -> Arc<Mutex<CodecSet>> {
    let mut codecs = CodecSet::pinned();
    codecs.add_decoder(decoder.clone()).unwrap();
    Arc::new(Mutex::new(codecs))
}

/// S9 の 1 回分：`at` で強制終了する保存（`save_as` なら別名、`heic` なら偽の HEIC の最初の保存でアンカーも作る）のあと、
/// 同じ組で開き直すと、予告の照合・staging の掃除・ジョブの再開で食い違い 0、版はちょうど 1 つ（別名は新しいアイテムが 1 つ）、
/// ジョブは done、アンカーは HEIC なら 1 行。
fn crash_and_reopen(heic: bool, save_as: bool, at: Fault) {
    let decoder = heic_decoder(true);
    let set = heic_codecs(&decoder);
    let factory = || {
        if heic {
            FakeEngineFactory::with_codecs(set.clone())
        } else {
            FakeEngineFactory::default()
        }
    };
    let env = Env::with(factory(), Some(at));
    let (a, graph, out) = if heic {
        let (a, source) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
        let graph = doc(&[flip("vertical")], Some("png"));
        // 最初の読み取り（0 回目）の値で描いたもの
        let out = hash_of(&render_value_png(
            decoder.value_for(0).unwrap(),
            &graph,
            source,
        ));
        (a, graph, out)
    } else {
        let (a, _, png) = env.png(13);
        let graph = doc(&[flip("vertical")], None);
        let out = hash_of(&render_png(&png, &graph));
        (a, graph, out)
    };
    let record = if save_as {
        env.save_as(a, graph, "別名", "alt")
    } else {
        env.overwrite(a, graph)
    };
    let case = format!("heic={heic} save_as={save_as} {at:?}");
    assert_eq!(record.state, JobState::Failed, "{case}");
    assert_crashed_state(&env, record.id, at, out, heic);
    let job = record.id;
    let (dir, session) = reopen_after_crash(env, job, factory());
    let record = session.jobs().get(job).unwrap();
    assert_done(&record);
    let lib = session.library();
    let item = if save_as {
        assert!(versions_of(lib, a).is_empty(), "{case}");
        assert_eq!(count(lib, "SELECT count(*) FROM items"), 2, "{case}");
        result_of(&record)["itemId"].as_i64().unwrap()
    } else {
        assert_eq!(count(lib, "SELECT count(*) FROM items"), 1, "{case}");
        a
    };
    assert_eq!(
        states(lib, item),
        vec![(0, FileState::Hold), (1, FileState::Hold)],
        "{case}"
    );
    // 読むたびに値の変わる HEIC は、初めからやり直すと別の出力になる（コミットのあとに落ちたものだけ同じ）
    if !heic || at == Fault::AfterCommit {
        assert_eq!(head(lib, item).hash, out, "{case}");
    }
    assert_eq!(head(lib, item).ext, "png", "{case}");
    assert_eq!(
        count(
            lib,
            "SELECT count(*) FROM item_versions WHERE job_id IS NOT NULL"
        ),
        1,
        "{case}"
    );
    let anchors = count(lib, "SELECT count(*) FROM edit_anchors");
    assert_eq!(anchors, i64::from(heic), "{case}");
    if heic {
        let v1 = versions_of(lib, item)[1].clone();
        let used: i64 = count(lib, "SELECT id FROM edit_anchors");
        assert_eq!(inputs_of(lib, v1.id)[0].anchor_id, Some(used), "{case}");
    }
    // 保存のあとのサムネイル・解析も終わっている
    assert_eq!(
        count(
            lib,
            "SELECT count(*) FROM items WHERE thumb_state = 'pending' OR analysis_state = 'pending'"
        ),
        0,
        "{case}"
    );
    assert_consistent(lib);
    drop(session);
    drop(dir);
}

/// S9：{上書き, 別名} × {描いたあと, 予告のあと, 置いたあと, コミットのあと} で強制終了したライブラリを開き直すと、
/// 予告の照合・staging の掃除・ジョブの再開で食い違い 0、版はちょうど 1 つ（別名は新しいアイテムが 1 つ）、ジョブは done。
#[test]
fn s9_crash_at_each_stage_recovers() {
    for save_as in [false, true] {
        for at in [
            Fault::AfterRender,
            Fault::AfterAnnounce,
            Fault::AfterPlace,
            Fault::AfterCommit,
        ] {
            crash_and_reopen(false, save_as, at);
        }
    }
}

/// S9 のアンカーを作る保存：偽の HEIC の最初の保存（出力とアンカーを予告し、2 つ置き、台帳に 2 行載せる）を {上書き, 別名} ×
/// 4 段で強制終了しても、落ちた直後は予告・孤立したファイルが 2 つずつで、開き直すと食い違い 0、版は 1 つ、アンカーは 1 行。
#[test]
fn s9_crash_while_making_an_anchor_recovers() {
    for save_as in [false, true] {
        for at in [
            Fault::AfterRender,
            Fault::AfterAnnounce,
            Fault::AfterPlace,
            Fault::AfterCommit,
        ] {
            crash_and_reopen(true, save_as, at);
        }
    }
}

// ---- S14：保存した中身の取り込み ----

/// 本物の GC（8 日たったことにする）で、アイテム `item` の版 `v` のファイルを片付ける（照合して消し、released になる）。
pub(super) fn gc_version(env: &Env, item: i64, v: i64) {
    let result = env.gc();
    let state = versions_of(&env.lib, item)
        .into_iter()
        .find(|x| x.version == v)
        .map(|x| x.file_state);
    assert_eq!(state, Some(FileState::Released), "{result}");
}

/// S14：本当の保存のあと、元・GC で消えた過去の版・今と同じ中身を取り込むと、どれもそのアイテムの重複になり（アイテムは
/// 増えない）、孤立したファイルが残らない。GC は 8 日進めた時計で、2 回目の保存のコミットのあとに積まれて版 1 を消す。
#[test]
fn s14_importing_saved_contents_is_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let session = LibrarySession::create(
        &dir.path().join("L"),
        Arc::new(|_| {}),
        Services::new(Arc::new(FakeBackend))
            .with_engine(Arc::new(FakeEngineFactory::default()))
            .with_gc_clock(GcClock::shifted(8 * DAY)),
    )
    .unwrap();
    session.wait_ready();
    let lib = session.library().clone();
    let png = sprite(14);
    let (a, h0) = add_item(&lib, &png, "png", "取り込み");
    let save = |graph: Graph| {
        let id = start_save(
            lib.db(),
            session.jobs(),
            &SaveRequest {
                item_id: a,
                base: base_of(&lib, a),
                action: SaveAction::Overwrite { graph },
            },
        )
        .unwrap();
        assert!(session.jobs().wait_idle(WAIT));
        assert_done(&session.jobs().get(id).unwrap());
    };
    save(doc(&[flip("vertical")], None));
    let h1 = head(&lib, a).hash;
    let v1 = std::fs::read(lib.layout().content_path(&h1, "png")).unwrap();
    save(doc(&[flip("horizontal")], None));
    let h2 = head(&lib, a).hash;
    let now = std::fs::read(lib.layout().content_path(&h2, "png")).unwrap();
    // 保存のコミットのあとに積まれた GC が、照合して版 1 を消した
    assert_eq!(states(&lib, a)[1], (1, FileState::Released));
    assert!(!lib.layout().content_path(&h1, "png").exists());

    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("orig.png"), &png).unwrap();
    std::fs::write(src.join("past.png"), &v1).unwrap();
    std::fs::write(src.join("now.png"), &now).unwrap();
    std::fs::write(src.join("orig.apng"), &png).unwrap();
    let id = start_import(
        session.jobs(),
        &ImportInput {
            paths: vec![src],
            options: ImportOptions::default(),
        },
    )
    .unwrap();
    assert!(session.jobs().wait_idle(WAIT));
    let record = session.jobs().get(id).unwrap();
    let result: ImportResult =
        serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
    assert_eq!((result.added, result.duplicates, result.failed), (0, 4, 0));
    assert_eq!(count(&lib, "SELECT count(*) FROM items"), 1);
    assert!(
        !lib.layout().content_path(&h1, "png").exists(),
        "消えた版のコピーが残った"
    );
    assert!(
        !lib.layout().content_path(&h0, "apng").exists(),
        "拡張子違いのコピーが残った"
    );
    assert_eq!(check_files(lib.db(), lib.layout()).unwrap(), (0, 0));
    assert_consistent(&lib);
}

// ---- S15：OS のデコーダで読む元 ----

/// S15 の前半：再現を約束しないデコーダ（偽の HEIC、読むたびに値が変わる）で読む元の最初の保存で、アンカーの行・台帳の行・
/// ファイルができ、版の入力がアンカーを指す。読む前に OS のデコーダの印（`decoding`）を付けて外す。2 回目の保存はアンカーから
/// 読み、デコーダを呼ばない（印も付けない）。
#[test]
fn s15_heic_first_save_makes_anchor_and_reads_it_afterwards() {
    let decoder = Arc::new(
        FakePlatformDecoder::new(
            "fake-heic-dec/1",
            &["heic"],
            InputGen::PixelSprite {
                w: 24,
                h: 20,
                fmt: PixelFormat::Rgba8,
                seed: 15,
            },
        )
        .drifting(),
    );
    let mut codecs = CodecSet::pinned();
    codecs.add_decoder(decoder.clone()).unwrap();
    let env = Env::with(
        FakeEngineFactory::with_codecs(Arc::new(Mutex::new(codecs))),
        None,
    );
    let (a, heic) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    let record = env.overwrite(a, doc(&[flip("vertical")], Some("png")));
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(result["anchor"], "created");
    assert_eq!(decoder.reads(), 1);
    let marks: Vec<String> = env
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.id == record.id)
        .filter_map(|e| e.result_json.clone())
        .collect();
    assert!(
        marks.iter().any(|m| m.contains(r#""decoding":true"#)),
        "{marks:?}"
    );
    assert!(
        marks.iter().any(|m| m.contains(r#""decoding":false"#)),
        "{marks:?}"
    );

    let (anchor_id, source, blob_hash, blob_ext): (i64, Vec<u8>, Vec<u8>, String) = env
        .lib
        .db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT a.id, a.source_hash, b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!((source, blob_ext.as_str()), (heic.to_vec(), "png"));
    let anchor_hash: [u8; 32] = blob_hash.try_into().unwrap();
    assert!(env.lib.layout().content_path(&anchor_hash, "png").exists());
    let vs = versions_of(&env.lib, a);
    assert_eq!((vs[0].ext.as_str(), vs[0].output_hash), ("heic", heic));
    assert_eq!(head(&env.lib, a).ext, "png");
    let inputs = env
        .lib
        .db()
        .read(|c| versions::inputs(c, vs[1].id))
        .unwrap();
    assert_eq!(inputs[0].anchor_id, Some(anchor_id));
    let recorded: Json = serde_json::from_str(&inputs[0].recorded).unwrap();
    assert_eq!(recorded["anchor"], hex(&anchor_hash));
    assert_eq!(recorded["decoded"]["codec"], "fake-heic-dec/1");
    assert_eq!(recorded["decoded"]["reproducible"], false);
    assert_consistent(&env.lib);

    // 2 回目：アンカーから読む（デコーダを呼ばない・印を付けない）
    let record = env.overwrite(a, doc(&[flip("horizontal")], Some("png")));
    assert_done(&record);
    assert_eq!(result_of(&record)["anchor"], "used");
    assert_eq!(decoder.reads(), 1, "アンカーから読む");
    let marked = env
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.id == record.id)
        .filter_map(|e| e.result_json.clone())
        .any(|m| m.contains("decoding"));
    assert!(!marked);
    let vs = versions_of(&env.lib, a);
    let inputs = env
        .lib
        .db()
        .read(|c| versions::inputs(c, vs[2].id))
        .unwrap();
    assert_eq!(inputs[0].anchor_id, Some(anchor_id));
    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 1);
    assert_consistent(&env.lib);
}

/// 前回 OS のデコーダで元を読んでいる途中で落ちた保存（途中経過の `decoding` が真のまま）は、読まずに失敗にし、印を消す
/// （同じファイルで落ち続けない。やり直せば読み直す）。
#[test]
fn a_save_that_crashed_while_decoding_fails_once() {
    let env = Env::new();
    let (a, h0, _) = env.png(16);
    let opened = Base {
        hash: h0,
        version: 0,
    };
    // 積んだジョブが動く前に、前回の途中経過を書いておく（一時停止して書き、戻す）
    let id = env.start(
        a,
        opened,
        SaveAction::Overwrite {
            graph: doc(&[flip("vertical")], None),
        },
    );
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(id).unwrap());
    // 同じ形の 2 本目を、途中経過つきで直接積む
    let input = env.jobs.get(id).unwrap().input_json;
    let input = input
        .replace(&format!("\"version\":{}", 0), "\"version\":1")
        .replace(&hex(&h0), &hex(&head(&env.lib, a).hash));
    let job = env
        .lib
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO jobs (kind, state, input_json, result_json, created_at) VALUES ('edit', 'paused', ?1, ?2, 1)",
                params![input, r#"{"v":1,"decoding":true}"#],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap();
    env.jobs.resume(job).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    let record = env.jobs.get(job).unwrap();
    assert_eq!(record.state, JobState::Failed);
    assert_eq!(
        record.error_code.as_deref(),
        Some("ToolFailed"),
        "{:?}",
        record.message
    );
    assert_eq!(
        record.result_json.as_deref(),
        Some(r#"{"v":1,"crashed":true}"#)
    );
    assert_eq!(states(&env.lib, a).len(), 2, "版を足さない");
    // やり直すと読み直して保存する
    env.jobs.retry(job).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    assert_done(&env.jobs.get(job).unwrap());
    assert_eq!(states(&env.lib, a).len(), 3);
    assert_consistent(&env.lib);
}

/// 見出し（形）を読むときに、動いている保存のジョブの途中経過を写し取る偽の HEIC のデコーダ（`fail` が真なら見出しで
/// 失敗する）。OS のデコーダの印が、見出しを読む前にコミットされているかを確かめる。
struct HeaderProbe {
    inner: FakePlatformDecoder,
    lib: Mutex<Option<Arc<Library>>>,
    seen: Mutex<Vec<Option<String>>>,
    shapes: AtomicU64,
    fail: AtomicBool,
}

impl Decoder for HeaderProbe {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn tier(&self) -> Tier {
        self.inner.tier()
    }

    fn reproducible(&self) -> bool {
        false
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
    ) -> std::result::Result<Shape, EngineError> {
        self.shapes.fetch_add(1, Ordering::SeqCst);
        if let Some(lib) = self.lib.lock().unwrap().as_ref() {
            let partial: Option<String> = lib
                .db()
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT result_json FROM jobs WHERE kind = 'edit' AND state = 'running'",
                        [],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            self.seen.lock().unwrap().push(partial);
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(EngineError::Decode {
                codec: self.id().into(),
                message: "偽の見出しの失敗".into(),
            });
        }
        self.inner.shape(src, format, opts, limits)
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> std::result::Result<kura_engine::Value, EngineError> {
        self.inner.decode(src, format, opts, limits, mon)
    }
}

/// 段 1：OS のデコーダで元の見出しを読む前に `decoding` の印をコミットし、見出しでつまずいて（落ちずに）失敗したら外す。
/// 見出しを読んでいる途中で落ちたことにした（その時の印が真のまま running の）ジョブは、開き直すと読まずに 1 回だけ失敗し、
/// やり直すと読み直して保存する（ADR-158 (9) と同じ決まり）。
#[test]
fn a_save_that_crashed_while_reading_the_header_fails_once() {
    let probe = Arc::new(HeaderProbe {
        inner: FakePlatformDecoder::new(
            "fake-heic-dec/1",
            &["heic"],
            InputGen::PixelSprite {
                w: 24,
                h: 20,
                fmt: PixelFormat::Rgba8,
                seed: 15,
            },
        ),
        lib: Mutex::new(None),
        seen: Mutex::new(Vec::new()),
        shapes: AtomicU64::new(0),
        fail: AtomicBool::new(true),
    });
    let mut codecs = CodecSet::pinned();
    codecs.add_decoder(probe.clone()).unwrap();
    let set = Arc::new(Mutex::new(codecs));
    let env = Env::with(FakeEngineFactory::with_codecs(set.clone()), None);
    *probe.lib.lock().unwrap() = Some(env.lib.clone());
    let (a, _) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    let graph = doc(&[flip("vertical")], Some("png"));
    let record = env.overwrite(a, graph);
    assert_eq!(record.state, JobState::Failed, "{:?}", record.message);
    // 見出しを読んでいる間、印はコミットされていた。落ちずに失敗したので外れている
    let seen = probe.seen.lock().unwrap().clone();
    assert_eq!(seen, vec![Some(r#"{"v":1,"decoding":true}"#.to_string())]);
    assert_eq!(
        record.result_json.as_deref(),
        Some(r#"{"v":1,"decoding":false}"#)
    );
    assert_eq!(probe.inner.reads(), 0);

    // 見出しを読んでいる途中で落ちたことにする（その時の途中経過のまま running）
    let job = record.id;
    let mark = seen[0].clone();
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE jobs SET result_json = ?2 WHERE id = ?1",
                params![job, mark],
            )?;
            Ok(())
        })
        .unwrap();
    probe.fail.store(false, Ordering::SeqCst);
    *probe.lib.lock().unwrap() = None;
    let (dir, session) = reopen_after_crash(env, job, FakeEngineFactory::with_codecs(set.clone()));
    let record = session.jobs().get(job).unwrap();
    assert_eq!(record.state, JobState::Failed);
    assert_eq!(
        record.error_code.as_deref(),
        Some("ToolFailed"),
        "{:?}",
        record.message
    );
    assert_eq!(
        record.result_json.as_deref(),
        Some(r#"{"v":1,"crashed":true}"#)
    );
    assert_eq!(probe.shapes.load(Ordering::SeqCst), 1, "読まずに失敗した");
    let lib = session.library().clone();
    assert!(versions_of(&lib, a).is_empty(), "版を足さない");

    // やり直すと読み直して保存する（印を付けて読む）
    *probe.lib.lock().unwrap() = Some(lib.clone());
    session.jobs().retry(job).unwrap();
    assert!(session.jobs().wait_idle(WAIT));
    assert_done(&session.jobs().get(job).unwrap());
    *probe.lib.lock().unwrap() = None;
    assert_eq!(
        probe.seen.lock().unwrap().last().cloned(),
        Some(Some(r#"{"v":1,"decoding":true}"#.to_string()))
    );
    assert_eq!(probe.inner.reads(), 1);
    assert_eq!(
        states(&lib, a),
        vec![(0, FileState::Hold), (1, FileState::Hold)]
    );
    assert_eq!(count(&lib, "SELECT count(*) FROM edit_anchors"), 1);
    assert_consistent(&lib);
    drop(session);
    drop(dir);
}

// ---- S3・S4：版に戻す ----

pub(super) fn inputs_of(lib: &Library, version_id: i64) -> Vec<versions::VersionInput> {
    lib.db().read(|c| versions::inputs(c, version_id)).unwrap()
}

fn items_row(lib: &Library, id: i64) -> (String, Option<String>, i64, i64, String, String, String) {
    lib.db()
        .read(|c| {
            Ok(c.query_row(
                "SELECT mime, codec_info, width, height, title, original_name, thumb_state FROM items WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )?)
        })
        .unwrap()
}

/// S3：2 回上書きしてから元に戻すと、今が元とバイト単位で同じになる。版 3 は revert（戻した先 0・文書なし・値は版 0 と同じ・
/// 入力なし・ファイルは版 0 と同じ台帳の行）、前の今（版 2）は消し待ち、版 0・3 は保持。アイテムの列（mime・codec_info・幅・
/// 高さ）は版 0 のものに戻り、表示名・元のファイル名・所属・素材の設定は変わらない。元はいつも版 0 が持つので描かない。
#[test]
fn s3_revert_to_original_restores_bytes() {
    let env = Env::new();
    let (a, h0, png) = env.png(20);
    decorate(&env.lib, a);
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                r#"UPDATE items SET codec_info = '{"bitDepth":8}' WHERE id = ?1"#,
                [a],
            )?;
            Ok(())
        })
        .unwrap();
    let before = memberships(&env.lib, a);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    assert_eq!(items_row(&env.lib, a).1, None, "上書きで codec_info は空");
    assert_done(&env.overwrite(a, doc(&[flip("vertical"), flip("horizontal")], None)));
    let h2 = head(&env.lib, a).hash;
    let record = env.revert(a, 0);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(result["outcome"], "saved");
    assert_eq!(result["version"], 3);
    assert_eq!(
        result["revert"],
        json!({ "to": 0, "reused": true, "rebuilt": false })
    );
    let h = head(&env.lib, a);
    assert_eq!((h.hash, h.ext.as_str(), h.version), (h0, "png", 3));
    assert_eq!(
        std::fs::read(env.lib.layout().content_path(&h0, "png")).unwrap(),
        png
    );
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Waiting),
            (2, FileState::Waiting),
            (3, FileState::Hold)
        ]
    );
    let vs = versions_of(&env.lib, a);
    assert_eq!(
        (
            vs[3].kind,
            vs[3].revert_to,
            vs[3].doc_id,
            vs[3].base_version,
            vs[3].job_id
        ),
        (VersionKind::Revert, Some(0), None, Some(2), Some(record.id))
    );
    assert_eq!(vs[3].value_digest, vs[0].value_digest);
    assert_eq!(vs[3].file_blob_id, vs[0].file_blob_id);
    assert!(inputs_of(&env.lib, vs[3].id).is_empty());
    assert_eq!(
        vs[3].record().unwrap().revert,
        Some(versions::RevertRecord {
            to: 0,
            reused: true,
            rebuilt: false
        })
    );
    let (mime, codec_info, w, hgt, title, name, thumb) = items_row(&env.lib, a);
    assert_eq!(
        (
            mime.as_str(),
            codec_info.as_deref(),
            w,
            hgt,
            title.as_str(),
            name.as_str(),
            thumb.as_str()
        ),
        (
            "image/png",
            Some(r#"{"bitDepth":8}"#),
            24,
            20,
            "絵20",
            "絵20.png",
            "pending"
        )
    );
    assert_eq!(memberships(&env.lib, a), before);
    assert!(
        env.lib.layout().content_path(&h2, "png").exists(),
        "版 2 は猶予中"
    );
    assert_consistent(&env.lib);
}

/// S4：猶予中の版 1 に戻すと、そのファイルを使い直す（描かない・ファイルを足さない・版 1 の状態は変えない）。戻した版は
/// 版 1 の文書・値・入力・書き出しの記録を写す。もう一度版 1 に戻すと今と同じ中身なので版だけ足す（前の頭は released）。
#[test]
fn s4_revert_reuses_waiting_file() {
    let env = Env::new();
    let (a, _, _) = env.png(21);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let h1 = head(&env.lib, a).hash;
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    let files_before = files(&env.lib);
    let record = env.revert(a, 1);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(
        result["revert"],
        json!({ "to": 1, "reused": true, "rebuilt": false })
    );
    assert_eq!(head(&env.lib, a).hash, h1);
    assert_eq!(files(&env.lib), files_before, "ファイルを足さない");
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Waiting),
            (2, FileState::Waiting),
            (3, FileState::Hold)
        ]
    );
    let vs = versions_of(&env.lib, a);
    assert_eq!(
        vs[3].file_blob_id, vs[1].file_blob_id,
        "版 1 と同じ台帳の行"
    );
    assert_eq!(
        (vs[3].doc_id, vs[3].value_digest, vs[3].revert_to),
        (vs[1].doc_id, vs[1].value_digest, Some(1))
    );
    assert_eq!(
        inputs_of(&env.lib, vs[3].id),
        inputs_of(&env.lib, vs[1].id),
        "入力は版 1 の写し"
    );
    assert_eq!(
        vs[3].record().unwrap().encoded,
        vs[1].record().unwrap().encoded
    );
    let record = env.revert(a, 1);
    assert_done(&record);
    assert_eq!(result_of(&record)["outcome"], "unchanged");
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Waiting),
            (2, FileState::Waiting),
            (3, FileState::Released),
            (4, FileState::Hold)
        ]
    );
    assert_consistent(&env.lib);
}

/// S4：版 1 のファイルが GC で消えたあとに版 1 に戻すと、版 1 の文書と入力の記録から作り直し、値のダイジェストが記録と同じ
/// （記録のエンコーダで書くので、バイト列も版 1 と同じ）。版 1 は released のまま、戻した版が台帳の行を持つ。
#[test]
fn s4_revert_rebuilds_after_gc() {
    let env = Env::new();
    let (a, _, png) = env.png(22);
    let g1 = doc(&[flip("vertical")], None);
    assert_done(&env.overwrite(a, g1.clone()));
    let v1 = versions_of(&env.lib, a)[1].clone();
    let path1 = env.lib.layout().content_path(&v1.output_hash, "png");
    let bytes1 = std::fs::read(&path1).unwrap();
    assert_eq!(bytes1, render_png(&png, &g1));
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    gc_version(&env, a, 1);
    assert!(!path1.exists());
    let record = env.revert(a, 1);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(
        result["revert"],
        json!({ "to": 1, "reused": false, "rebuilt": true })
    );
    assert_eq!(result["hash"], hex(&v1.output_hash));
    assert_eq!(result["warnings"], json!([]));
    assert_eq!(std::fs::read(&path1).unwrap(), bytes1, "同じバイト列");
    let vs = versions_of(&env.lib, a);
    assert_eq!(
        (vs[3].output_hash, vs[3].value_digest, vs[3].doc_id),
        (v1.output_hash, v1.value_digest, v1.doc_id)
    );
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Released),
            (2, FileState::Waiting),
            (3, FileState::Hold)
        ]
    );
    let r3 = vs[3].record().unwrap();
    assert_eq!(r3.encoded.unwrap().codec, "png-rs-enc/1");
    assert_eq!(r3.revert.map(|r| r.rebuilt), Some(true));
    assert_consistent(&env.lib);
}

/// 作り直しで記録のエンコーダがこの環境にない（識別子を上げた・外した）ときは、今の組の同じ形式のエンコーダで書き、新しい
/// 書き出しの記録と知らせを残す（値は照らし済み）。書いたバイト列が版 k の記録と違えば（記録のエンコーダが別のバイト列を
/// 書いていた）その知らせも残し、台帳には作り直したファイルを載せ、アイテムの mime は拡張子から・codec_info は空にする。
#[test]
fn revert_rebuild_without_the_recorded_encoder_uses_the_current_one() {
    for bytes_differ in [false, true] {
        let env = Env::new();
        let (a, _, png) = env.png(28);
        let g1 = doc(&[flip("vertical")], None);
        assert_done(&env.overwrite(a, g1.clone()));
        let v1 = versions_of(&env.lib, a)[1].clone();
        assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
        gc_version(&env, a, 1);
        // 版 1 は、この組にないエンコーダで書いたことにする（違うバイト列を書いていたなら、そのハッシュも）
        let mut record: Json = serde_json::from_str(&v1.record).unwrap();
        record["encoded"]["codec"] = json!("gone-png-enc/9");
        record["media"]["codecInfo"] = json!(r#"{"probe":1}"#);
        let recorded_hash = if bytes_differ {
            [0x5a; 32]
        } else {
            v1.output_hash
        };
        let text = record.to_string();
        env.lib
            .db()
            .write(move |tx| {
                tx.execute(
                    "UPDATE item_versions SET record = ?1, output_hash = ?2 WHERE item_id = ?3 AND version = 1",
                    params![text, &recorded_hash[..], a],
                )?;
                Ok(())
            })
            .unwrap();
        let record = env.revert(a, 1);
        assert_done(&record);
        let result = result_of(&record);
        assert_eq!(result["revert"]["rebuilt"], true);
        let warnings: Vec<String> = serde_json::from_value(result["warnings"].clone()).unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("記録のエンコーダ gone-png-enc/9 がない")),
            "{warnings:?}"
        );
        assert_eq!(
            warnings
                .iter()
                .any(|w| w.contains("バイト列は、版 1 のものと違います")),
            bytes_differ,
            "{warnings:?}"
        );
        // 今の組のエンコーダは、もとの版 1 と同じバイト列を書く
        let rebuilt = hash_of(&render_png(&png, &g1));
        assert_eq!(rebuilt, v1.output_hash);
        assert_eq!(result["hash"], hex(&rebuilt));
        let vs = versions_of(&env.lib, a);
        let r3 = vs[3].record().unwrap();
        assert_eq!(
            r3.encoded.map(|e| e.codec),
            Some("png-rs-enc/1".to_string()),
            "新しい書き出しの記録"
        );
        assert_eq!(r3.warnings, warnings);
        assert_eq!(
            (vs[3].output_hash, vs[3].value_digest),
            (rebuilt, v1.value_digest)
        );
        assert_eq!(
            states(&env.lib, a),
            vec![
                (0, FileState::Hold),
                (1, FileState::Released),
                (2, FileState::Waiting),
                (3, FileState::Hold)
            ]
        );
        assert_eq!(head(&env.lib, a).hash, rebuilt);
        assert_eq!(
            one::<String>(
                &env.lib,
                "SELECT b.ext FROM blobs b JOIN items i ON i.hash = b.hash WHERE i.id = ?1",
                a
            ),
            "png"
        );
        let (mime, codec_info, ..) = items_row(&env.lib, a);
        assert_eq!(mime, "image/png");
        assert_eq!(
            codec_info.as_deref(),
            if bytes_differ {
                None
            } else {
                Some(r#"{"probe":1}"#)
            },
            "中身が版 1 の記録と同じときだけ、記録の codec_info に戻す"
        );
        assert_consistent(&env.lib);
    }
}

/// 作り直した値が版の記録と違えば戻さない（Mismatch）。版・ファイル・台帳は変わらず、staging も予告も空。
#[test]
fn revert_rebuild_that_does_not_match_the_record_is_refused() {
    let env = Env::new();
    let (a, _, _) = env.png(23);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    gc_version(&env, a, 1);
    // 版 1 の値の記録を、作り直しと合わない値にする（デコーダ・エンジンの変化で記録と違う値になった場合を写す）
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE item_versions SET value_digest = ?1 WHERE item_id = ?2 AND version = 1",
                params![&[7u8; 32][..], a],
            )?;
            Ok(())
        })
        .unwrap();
    let before = snapshot(&env.lib);
    let record = env.revert(a, 1);
    assert_eq!(record.state, JobState::Failed, "{:?}", record.message);
    assert_eq!(
        record.error_code.as_deref(),
        Some("Mismatch"),
        "{:?}",
        record.message
    );
    assert_eq!(snapshot(&env.lib), before);
    assert_eq!(head(&env.lib, a).version, 2);
    assert_consistent(&env.lib);
}

/// 戻す先の中身が別のアイテムの今と同じなら Conflict（上書きと同じ規則。items.hash は一意）。何も変わらない。
#[test]
fn revert_to_contents_another_item_holds_is_a_conflict() {
    let env = Env::new();
    let (a, _, png) = env.png(24);
    let g1 = doc(&[flip("vertical")], None);
    assert_done(&env.overwrite(a, g1.clone()));
    let h1 = head(&env.lib, a).hash;
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    let (_, hb) = add_item(&env.lib, &render_png(&png, &g1), "png", "写し");
    assert_eq!(hb, h1);
    let before = snapshot(&env.lib);
    let record = env.revert(a, 1);
    assert_conflict(&record);
    assert_eq!(snapshot(&env.lib), before);
    assert_consistent(&env.lib);
}

// ---- 門の間の Conflict・中止・一時停止 ----

/// 置いたあと・トランザクション 2 の前にゴミ箱に入れられたら（台帳の門はゴミ箱に入れる操作を止めない）、保存は Conflict に
/// なり、置いたファイル（台帳にないもの）と予告が片付く。
#[test]
fn trashed_between_placement_and_commit_cleans_up_placed_files() {
    let env = Env::with_hook(|lib| {
        let lib = lib.clone();
        Arc::new(move |stage| {
            if stage == Stage::Placed {
                lib.db()
                    .write(|tx| {
                        let ids: Vec<i64> = {
                            let mut stmt = tx.prepare("SELECT id FROM items")?;
                            let rows = stmt.query_map([], |r| r.get(0))?;
                            rows.collect::<rusqlite::Result<_>>()?
                        };
                        crate::items::trash(tx, &ids, 5).map(|_| ())
                    })
                    .unwrap();
            }
        })
    });
    let (a, _, png) = env.png(25);
    let graph = doc(&[flip("vertical")], None);
    let out = hash_of(&render_png(&png, &graph));
    let before = snapshot(&env.lib);
    let record = env.overwrite(a, graph);
    assert_conflict(&record);
    assert!(head(&env.lib, a).trashed);
    assert_eq!(snapshot(&env.lib), before);
    assert!(!env.lib.layout().content_path(&out, "png").exists());
    assert_eq!(check_files(env.lib.db(), env.lib.layout()).unwrap(), (0, 0));
    assert_consistent(&env.lib);
}

/// 段 5 の終わり（書いて staging に置いたところ）で一時停止・中止が来ると、staging を消して止まり、版・ファイル・台帳・予告に
/// 何も残らない。一時停止したものは、続けると初めからやり直して保存する。上書きと、版に戻す作り直しで確かめる。
#[test]
fn cancel_or_pause_after_writing_leaves_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    for (rebuild, pause) in [(false, false), (false, true), (true, false), (true, true)] {
        let (reached_tx, reached_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let armed = Arc::new(AtomicBool::new(false));
        let env = Env::with_hook({
            let armed = armed.clone();
            move |_| {
                let (reached, go) = (Mutex::new(reached_tx), Mutex::new(go_rx));
                Arc::new(move |stage| {
                    if stage == Stage::Written && armed.swap(false, Ordering::SeqCst) {
                        reached.lock().unwrap().send(()).unwrap();
                        let _ = go.lock().unwrap().recv();
                    }
                })
            }
        });
        let (a, _, _) = env.png(26);
        let action = if rebuild {
            assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
            assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
            gc_version(&env, a, 1);
            SaveAction::Revert { to: 1 }
        } else {
            SaveAction::Overwrite {
                graph: doc(&[flip("vertical")], None),
            }
        };
        let versions_before = versions_of(&env.lib, a).len();
        let before = snapshot(&env.lib);
        armed.store(true, Ordering::SeqCst);
        let id = env.start(a, base_of(&env.lib, a), action);
        reached_rx.recv_timeout(WAIT).unwrap();
        if pause {
            env.jobs.pause(id).unwrap();
        } else {
            env.jobs.cancel(id).unwrap();
        }
        go_tx.send(()).unwrap();
        assert!(env.jobs.wait_idle(WAIT));
        let record = env.jobs.get(id).unwrap();
        let want = if pause {
            JobState::Paused
        } else {
            JobState::Canceled
        };
        assert_eq!(record.state, want, "{rebuild} {pause} {:?}", record.message);
        assert_eq!(snapshot(&env.lib), before, "{rebuild} {pause}");
        assert_eq!(staged(&env.lib), Vec::<PathBuf>::new());
        assert_eq!(env.count("SELECT count(*) FROM ledger_pending"), 0);
        if pause {
            env.jobs.resume(id).unwrap();
            assert!(env.jobs.wait_idle(WAIT));
            assert_done(&env.jobs.get(id).unwrap());
            assert_eq!(versions_of(&env.lib, a).len(), versions_before.max(1) + 1);
        }
        assert_consistent(&env.lib);
    }
}

/// 強制終了したライブラリを、偽のエンジンの部品（`factory`）つきで開き直して（予告の照合・staging の掃除・ジョブの再開）、
/// 動き終えるのを待つ。ジョブ `job` は running のまま落ちたことにする。
pub(super) fn reopen_after_crash(
    env: Env,
    job: i64,
    factory: FakeEngineFactory,
) -> (tempfile::TempDir, LibrarySession) {
    env.lib
        .db()
        .write(move |tx| {
            tx.execute("UPDATE jobs SET state = 'running' WHERE id = ?1", [job])?;
            Ok(())
        })
        .unwrap();
    let Env { dir, lib, jobs, .. } = env;
    drop(jobs);
    let root = lib.layout().root().to_path_buf();
    lib.close();
    drop(lib);
    let session = LibrarySession::open(
        &root,
        Arc::new(|_| {}),
        Services::new(Arc::new(FakeBackend)).with_engine(Arc::new(factory)),
    )
    .unwrap();
    session.wait_ready();
    let report = session.integrity().unwrap();
    assert_eq!((report.orphan_files, report.missing_files), (0, 0));
    assert!(session.jobs().wait_idle(WAIT));
    (dir, session)
}

/// S9 の版に戻す：作り直しを 4 段（描いたあと・予告のあと・置いたあと・コミットのあと）のそれぞれで強制終了しても、開き直すと
/// 食い違い 0、このジョブの版はちょうど 1 つ、ジョブは done で、今は作り直した版 1 の中身。
#[test]
fn revert_rebuild_crash_at_each_stage_recovers() {
    for at in [
        Fault::AfterRender,
        Fault::AfterAnnounce,
        Fault::AfterPlace,
        Fault::AfterCommit,
    ] {
        let env = Env::new();
        let (a, _, _) = env.png(27);
        assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
        let h1 = head(&env.lib, a).hash;
        assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
        gc_version(&env, a, 1);
        // 同じライブラリで、強制終了する保存のジョブに入れ替える
        let Env {
            dir,
            lib,
            jobs,
            events,
            engine,
            clock,
        } = env;
        drop(jobs);
        let jobs = JobManager::new(
            lib.clone(),
            vec![(
                JobKind::Edit,
                Arc::new(EditHandler::new(engine.clone()).with_fault(at)),
            )],
            sink(&events),
        )
        .unwrap();
        jobs.start().unwrap();
        let env = Env {
            dir,
            lib,
            jobs,
            events,
            engine,
            clock,
        };
        let record = env.revert(a, 1);
        assert_eq!(record.state, JobState::Failed, "{at:?}");
        assert_crashed_state(&env, record.id, at, h1, false);
        let job = record.id;
        let (dir, session) = reopen_after_crash(env, job, FakeEngineFactory::default());
        assert_done(&session.jobs().get(job).unwrap());
        let lib = session.library();
        assert_eq!(head(lib, a).hash, h1, "{at:?}");
        assert_eq!(
            states(lib, a),
            vec![
                (0, FileState::Hold),
                (1, FileState::Released),
                (2, FileState::Waiting),
                (3, FileState::Hold)
            ],
            "{at:?}"
        );
        assert_eq!(
            one::<i64>(
                lib,
                "SELECT count(*) FROM item_versions WHERE job_id = ?1",
                job
            ),
            1
        );
        assert_consistent(lib);
        drop(session);
        drop(dir);
    }
}

// ---- S15 の後半：アンカーから作り直す ----

pub(super) fn heic_decoder(drifting: bool) -> Arc<FakePlatformDecoder> {
    let d = FakePlatformDecoder::new(
        "fake-heic-dec/1",
        &["heic"],
        InputGen::PixelSprite {
            w: 24,
            h: 20,
            fmt: PixelFormat::Rgba8,
            seed: 15,
        },
    );
    Arc::new(if drifting { d.drifting() } else { d })
}

/// S15 の後半：OS のデコーダを組から外し、版 1 のファイルを GC で消してから版 1 に戻すと、アンカーから作り直せて照合が合う
/// （デコーダは呼ばない。出力は版 1 と同じバイト列）。元に戻すと HEIC の元が今になる（版 0 が持つ）。
#[test]
fn s15_rebuilds_from_the_anchor_without_the_os_decoder() {
    let decoder = heic_decoder(true);
    let mut codecs = CodecSet::pinned();
    codecs.add_decoder(decoder.clone()).unwrap();
    let set = Arc::new(Mutex::new(codecs));
    let env = Env::with(FakeEngineFactory::with_codecs(set.clone()), None);
    let (a, heic) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], Some("png"))));
    let v1 = versions_of(&env.lib, a)[1].clone();
    let path1 = env.lib.layout().content_path(&v1.output_hash, "png");
    let bytes1 = std::fs::read(&path1).unwrap();
    let anchor1 = inputs_of(&env.lib, v1.id)[0].anchor_id;
    assert!(anchor1.is_some());
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], Some("png"))));
    assert_eq!(decoder.reads(), 1);
    // デコーダを外す（ツールの場所を変えたときと同じく、組を作り直させる）
    *set.lock().unwrap() = CodecSet::pinned();
    env.engine.reload_codecs();
    gc_version(&env, a, 1);
    assert!(!path1.exists());
    let record = env.revert(a, 1);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(result["revert"]["rebuilt"], true);
    assert_eq!(result["anchor"], "used");
    assert_eq!(std::fs::read(&path1).unwrap(), bytes1);
    assert_eq!(decoder.reads(), 1, "デコーダを呼ばない");
    let vs = versions_of(&env.lib, a);
    assert_eq!(vs[3].value_digest, v1.value_digest);
    assert_eq!(inputs_of(&env.lib, vs[3].id)[0].anchor_id, anchor1);
    assert_consistent(&env.lib);

    let record = env.revert(a, 0);
    assert_done(&record);
    let h = head(&env.lib, a);
    assert_eq!((h.hash, h.ext.as_str()), (heic, "heic"));
    assert_eq!(items_row(&env.lib, a).0, "image/heic");
    assert_consistent(&env.lib);
}

/// アンカーが壊れていれば、元を記録のデコーダで読めるときは元で読み直して作り直す（値が記録と同じなら戻し、知らせを残す）。
/// 読み直した値が記録と違えば（読むたびに値の変わるデコーダ）戻さない（Mismatch。何も変わらない）。
#[test]
fn revert_falls_back_to_the_original_when_the_anchor_is_broken() {
    for drifting in [false, true] {
        let decoder = heic_decoder(drifting);
        let mut codecs = CodecSet::pinned();
        codecs.add_decoder(decoder.clone()).unwrap();
        let env = Env::with(
            FakeEngineFactory::with_codecs(Arc::new(Mutex::new(codecs))),
            None,
        );
        let (a, _) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
        assert_done(&env.overwrite(a, doc(&[flip("vertical")], Some("png"))));
        let v1 = versions_of(&env.lib, a)[1].clone();
        let path1 = env.lib.layout().content_path(&v1.output_hash, "png");
        let bytes1 = std::fs::read(&path1).unwrap();
        assert_done(&env.overwrite(a, doc(&[flip("horizontal")], Some("png"))));
        gc_version(&env, a, 1);
        let (anchor_hash, anchor_ext): (Vec<u8>, String) = env
            .lib
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        let anchor_path = env
            .lib
            .layout()
            .content_path(&anchor_hash.try_into().unwrap(), &anchor_ext);
        let saved = std::fs::read(&anchor_path).unwrap();
        std::fs::write(&anchor_path, b"broken anchor").unwrap();
        let before = snapshot(&env.lib);
        let record = env.revert(a, 1);
        if drifting {
            assert_eq!(record.state, JobState::Failed, "{:?}", record.message);
            assert_eq!(record.error_code.as_deref(), Some("Mismatch"));
            assert_eq!(snapshot(&env.lib), before);
        } else {
            assert_done(&record);
            let result = result_of(&record);
            assert_eq!(result["anchor"], Json::Null, "元から読んだ");
            assert!(
                result["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w.as_str().unwrap().contains("アンカーが使えない")),
                "{result}"
            );
            assert_eq!(std::fs::read(&path1).unwrap(), bytes1);
        }
        assert_eq!(decoder.reads(), 2, "元を記録のデコーダで読み直した");
        std::fs::write(&anchor_path, saved).unwrap();
        assert_consistent(&env.lib);
    }
}

/// アンカーが壊れている・ファイルがないまま上書き・別名で保存すると、元を記録のデコーダ（アンカーの元の読み取りの記録の
/// codec）で読み直す（OS のデコーダなので、見出しを読む前から印を付ける）。値がアンカーの記録と同じなら保存し（入力の記録は
/// アンカーを指したまま。知らせを版の記録の警告に残す）、違えば（読むたびに値の変わるデコーダ）Mismatch で何も変わらない
/// （docs/editing.md §9.4）。
#[test]
fn saving_with_a_broken_or_missing_anchor_reads_the_original_again() {
    for missing in [false, true] {
        for save_as in [false, true] {
            for drifting in [false, true] {
                let case = format!("missing={missing} save_as={save_as} drifting={drifting}");
                let decoder = heic_decoder(drifting);
                let env = Env::with(FakeEngineFactory::with_codecs(heic_codecs(&decoder)), None);
                let (a, heic) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
                assert_done(&env.overwrite(a, doc(&[flip("vertical")], Some("png"))));
                let (anchor_id, anchor_hash, anchor_ext): (i64, Vec<u8>, String) = env
                    .lib
                    .db()
                    .read(|c| {
                        Ok(c.query_row(
                            "SELECT a.id, b.hash, b.ext FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id",
                            [],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )?)
                    })
                    .unwrap();
                let anchor_hash: [u8; 32] = anchor_hash.try_into().unwrap();
                let anchor_path = env.lib.layout().content_path(&anchor_hash, &anchor_ext);
                let saved = std::fs::read(&anchor_path).unwrap();
                if missing {
                    std::fs::remove_file(&anchor_path).unwrap();
                } else {
                    std::fs::write(&anchor_path, b"broken anchor").unwrap();
                }
                let before = snapshot(&env.lib);
                let graph = doc(&[flip("horizontal")], Some("png"));
                let record = if save_as {
                    env.save_as(a, graph.clone(), "別名", "alt")
                } else {
                    env.overwrite(a, graph.clone())
                };
                assert_eq!(decoder.reads(), 2, "{case}：元を記録のデコーダで読み直した");
                let marked = env
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| e.id == record.id)
                    .filter_map(|e| e.result_json.clone())
                    .any(|m| m.contains(r#""decoding":true"#));
                assert!(marked, "{case}");
                if drifting {
                    assert_eq!(
                        record.state,
                        JobState::Failed,
                        "{case} {:?}",
                        record.message
                    );
                    assert_eq!(record.error_code.as_deref(), Some("Mismatch"), "{case}");
                    assert!(
                        record
                            .message
                            .as_deref()
                            .is_some_and(|m| m.contains("保存できません")),
                        "{case} {:?}",
                        record.message
                    );
                    assert_eq!(snapshot(&env.lib), before, "{case}");
                } else {
                    assert_done(&record);
                    let result = result_of(&record);
                    assert_eq!(result["anchor"], Json::Null, "{case}：元から読んだ");
                    let warned =
                        |ws: &[String]| ws.iter().any(|w| w.contains("アンカーが使えない"));
                    let warnings: Vec<String> =
                        serde_json::from_value(result["warnings"].clone()).unwrap();
                    assert!(warned(&warnings), "{case} {result}");
                    let item = if save_as {
                        result["itemId"].as_i64().unwrap()
                    } else {
                        a
                    };
                    let saved_version = versions_of(&env.lib, item).last().unwrap().clone();
                    assert!(warned(&saved_version.record().unwrap().warnings), "{case}");
                    let inputs = inputs_of(&env.lib, saved_version.id);
                    assert_eq!(inputs[0].anchor_id, Some(anchor_id), "{case}");
                    let recorded: Json = serde_json::from_str(&inputs[0].recorded).unwrap();
                    assert_eq!(recorded["anchor"], hex(&anchor_hash), "{case}");
                    // 出力は、アンカーの値（最初の読み取りと同じ値）に横の反転を当てたもの
                    let want = hash_of(&render_value_png(
                        decoder.value_for(0).unwrap(),
                        &graph,
                        heic,
                    ));
                    assert_eq!(head(&env.lib, item).hash, want, "{case}");
                    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 1, "{case}");
                }
                std::fs::write(&anchor_path, saved).unwrap();
                assert_consistent(&env.lib);
            }
        }
    }
}

/// 描いている間にゴミ箱を空にして、読んでいたアンカーが片付けられた（それを使う唯一の版を持つ別名のアイテムをゴミ箱に
/// 入れた）保存は、予告の前に Conflict で終わり、何も置かない（外部キーの誤りにしない）。もう一度保存すると、元を読み直して
/// アンカーを作り直す。
#[test]
fn anchor_swept_while_saving_is_a_conflict() {
    let decoder = heic_decoder(false);
    let victim = Arc::new(AtomicI64::new(0));
    let env = Env::setup(
        Setup {
            factory: FakeEngineFactory::with_codecs(heic_codecs(&decoder)),
            ..Setup::default()
        },
        |lib| {
            let (lib, victim) = (lib.clone(), victim.clone());
            Some(Arc::new(move |stage| {
                if stage != Stage::Written {
                    return;
                }
                let b = victim.swap(0, Ordering::SeqCst);
                if b != 0 {
                    lib.db()
                        .write(move |tx| crate::items::trash(tx, &[b], 5).map(|_| ()))
                        .unwrap();
                    lib.empty_trash().unwrap();
                }
            }))
        },
    );
    let (a, heic) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    let record = env.save_as(a, doc(&[flip("vertical")], Some("png")), "別名", "alt");
    assert_done(&record);
    assert_eq!(result_of(&record)["anchor"], "created");
    let b = result_of(&record)["itemId"].as_i64().unwrap();
    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 1);

    victim.store(b, Ordering::SeqCst);
    let record = env.overwrite(a, doc(&[flip("horizontal")], Some("png")));
    assert_conflict(&record);
    assert!(
        record
            .message
            .as_deref()
            .is_some_and(|m| m.contains("アンカー")),
        "{:?}",
        record.message
    );
    assert_eq!(decoder.reads(), 1, "アンカーから読んでいた");
    assert_eq!(
        env.count("SELECT count(*) FROM items"),
        1,
        "別名のアイテムは消えた"
    );
    assert_eq!(
        env.count("SELECT count(*) FROM edit_anchors"),
        0,
        "アンカーは片付けられた"
    );
    assert!(versions_of(&env.lib, a).is_empty(), "A は変わらない");
    assert_eq!(head(&env.lib, a).hash, heic);
    assert_consistent(&env.lib);

    // もう一度保存すると、元を読み直してアンカーを作り直す
    let record = env.overwrite(a, doc(&[flip("horizontal")], Some("png")));
    assert_done(&record);
    assert_eq!(result_of(&record)["anchor"], "created");
    assert_eq!(decoder.reads(), 2);
    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 1);
    assert_eq!(
        states(&env.lib, a),
        vec![(0, FileState::Hold), (1, FileState::Hold)]
    );
    assert_consistent(&env.lib);
}

// ---- S17：下書き ----

fn root_of(g: &Graph) -> Digest {
    kura_engine::doc_parts::split(g).unwrap().root
}

/// S17：下書きを置いてアプリが落ちても、開き直すと同じ文書（同じ根）で fresh。その土台で保存すると下書きは消える。間に別の
/// 上書きが入ると fresh が偽になり、その土台での保存は Conflict（下書きは残る）。保存を積んだあとに置いた下書き（after_job）は、
/// 保存がコミットすると土台が新しい頭に付け替わり、保存した文書と同じなら消える。
#[test]
fn s17_draft_survives_crash_and_conflicts_when_base_changed() {
    let env = Env::new();
    let (a, _, _) = env.png(30);
    let d1 = doc(&[flip("vertical")], None);
    let root = drafts::save(env.lib.db(), a, &d1, base_of(&env.lib, a), None).unwrap();
    assert_eq!(root, root_of(&d1));
    let Env { dir, lib, jobs, .. } = env;
    drop(jobs);
    let path = lib.layout().root().to_path_buf();
    lib.close();
    drop(lib);

    let session = LibrarySession::open(
        &path,
        Arc::new(|_| {}),
        Services::new(Arc::new(FakeBackend)).with_engine(Arc::new(FakeEngineFactory::default())),
    )
    .unwrap();
    session.wait_ready();
    let lib = session.library().clone();
    let runtime = session.engine().unwrap().runtime().unwrap().clone();
    let opened = drafts::open(lib.db(), &runtime, a).unwrap().unwrap();
    assert_eq!(
        (root_of(&opened.graph), opened.doc, opened.fresh),
        (root, root, true)
    );
    let save = |base: Base, action: SaveAction| {
        let id = start_save(
            lib.db(),
            session.jobs(),
            &SaveRequest {
                item_id: a,
                base,
                action,
            },
        )
        .unwrap();
        assert!(session.jobs().wait_idle(WAIT));
        session.jobs().get(id).unwrap()
    };
    assert_done(&save(
        opened.base,
        SaveAction::Overwrite {
            graph: opened.graph.clone(),
        },
    ));
    assert!(lib.db().read(|c| drafts::get(c, a)).unwrap().is_none());

    // 下書きを置いたあとに別の上書きが入る：fresh が偽、その土台の保存は Conflict で下書きは残る
    let d2 = doc(&[flip("vertical"), flip("horizontal")], None);
    drafts::save(lib.db(), a, &d2, base_of(&lib, a), None).unwrap();
    assert_done(&save(
        base_of(&lib, a),
        SaveAction::Overwrite {
            graph: doc(&[flip("horizontal")], None),
        },
    ));
    let stale = drafts::open(lib.db(), &runtime, a).unwrap().unwrap();
    assert_eq!(root_of(&stale.graph), root_of(&d2));
    assert!(!stale.fresh);
    assert_conflict(&save(
        stale.base,
        SaveAction::Overwrite {
            graph: stale.graph.clone(),
        },
    ));
    assert!(lib.db().read(|c| drafts::get(c, a)).unwrap().is_some());

    // 保存を積んでから置いた下書き：保存がコミットすると土台が新しい頭に付け替わる（同じ文書なら消える）
    for same in [false, true] {
        let saved = doc(&[flip(if same { "vertical" } else { "horizontal" })], None);
        let editing = if same {
            saved.clone()
        } else {
            doc(
                &[flip("horizontal"), flip("vertical"), flip("horizontal")],
                None,
            )
        };
        let opened_base = base_of(&lib, a);
        let id = start_save(
            lib.db(),
            session.jobs(),
            &SaveRequest {
                item_id: a,
                base: opened_base,
                action: SaveAction::Overwrite { graph: saved },
            },
        )
        .unwrap();
        drafts::save(lib.db(), a, &editing, opened_base, Some(id)).unwrap();
        assert!(session.jobs().wait_idle(WAIT));
        assert_done(&session.jobs().get(id).unwrap());
        let draft = lib.db().read(|c| drafts::get(c, a)).unwrap();
        if same {
            assert!(draft.is_none(), "保存した文書と同じ下書きは消える");
        } else {
            let draft = draft.unwrap();
            assert_eq!(draft.base, base_of(&lib, a));
            assert_eq!(draft.after_job, None);
            let opened = drafts::open(lib.db(), &runtime, a).unwrap().unwrap();
            assert!(opened.fresh);
            assert_eq!(root_of(&opened.graph), root_of(&editing));
        }
    }
    // 捨てる：あれば真、なければ偽
    drafts::save(lib.db(), a, &d2, base_of(&lib, a), None).unwrap();
    assert!(drafts::discard(lib.db(), a).unwrap());
    assert!(!drafts::discard(lib.db(), a).unwrap());
    assert!(lib.db().read(|c| drafts::get(c, a)).unwrap().is_none());
    assert_consistent(&lib);
    drop(session);
    drop(dir);
}
