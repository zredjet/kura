//! 保存のジョブの受け入れテスト（docs/editing.md §14.1 の S1・S2・S5〜S9・S14・S15 の前半と、M12 の段 2 の決定 9〜12）。
//! 固定の段のコーデック（と偽の Platform のデコーダ）と偽のエンジンの部品で、Linux でも回る。どのテストも終わりに
//! [`assert_consistent`]（整合性・予告・staging・版の不変条件・ジョブ）で締める。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kura_engine::codec::CodecSet;
use kura_engine::doc::{FormatChoice, InputId};
use kura_engine::render::InputValue;
use kura_engine::testgen::InputGen;
use kura_engine::testing::platform::FakePlatformDecoder;
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, Engine, Graph, NoMonitor};
use rusqlite::params;
use serde_json::{Value as Json, json};

use super::save::{Base, EditHandler, Fault, SaveAction, SaveRequest, start_save};
use super::services::fake::FakeEngineFactory;
use super::versions::{self, FileState, Head, Version, VersionKind};
use super::{ENGINE_WORK_DIR, EngineServices};
use crate::import::{ImportInput, ImportOptions, ImportResult, hash_file, start_import};
use crate::jobs::{EventSink, JobEvent, JobKind, JobManager, JobRecord, JobState};
use crate::library::{
    IntegrityDepth, Library, LibrarySession, Services, check_files, check_integrity, hex, ledger,
};
use crate::media::fake::FakeBackend;
use crate::{CoreError, Result};

const WAIT: Duration = Duration::from_secs(60);

// ---- 道具 ----

/// ドット絵の PNG（固定の段で書いたもの。読み直して書くと同じバイト列になる）。
fn sprite(seed: u64) -> Vec<u8> {
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

fn hash_of(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn flip(axis: &str) -> (&'static str, Json) {
    ("image.flip", json!({ "axis": axis }))
}

/// 元に対する累積の文書（`to` があれば出力の形式）。
fn doc(ops: &[(&'static str, Json)], to: Option<&str>) -> Graph {
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
fn render_png(png: &[u8], g: &Graph) -> Vec<u8> {
    let codecs = CodecSet::pinned();
    let engine = Engine::new().unwrap();
    let (value, _) = codecs
        .decode(
            &mut std::io::Cursor::new(png),
            "png",
            &Default::default(),
            engine.limits(),
            &NoMonitor,
        )
        .unwrap();
    let plan = engine
        .compile(g, &BTreeMap::from([(InputId(0), value.shape())]))
        .unwrap();
    let inputs = BTreeMap::from([(
        InputId(0),
        InputValue {
            value,
            source: Digest(hash_of(png)),
        },
    )]);
    let out = engine.render(&plan, &inputs, &NoMonitor).unwrap();
    codecs
        .encode_bytes(&out.outputs[0].value, "png", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

fn sink(events: &Arc<Mutex<Vec<JobEvent>>>) -> EventSink {
    let events = events.clone();
    Arc::new(move |e| events.lock().unwrap().push(e))
}

fn count(lib: &Library, sql: &str) -> i64 {
    lib.db()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(lib: &Library, sql: &str, id: i64) -> T {
    lib.db()
        .read(|c| Ok(c.query_row(sql, [id], |r| r.get(0))?))
        .unwrap()
}

/// アイテムのファイルを置いて行を入れる（サムネイル・解析は済んだものとして）。
fn add_item(lib: &Library, bytes: &[u8], ext: &str, title: &str) -> (i64, [u8; 32]) {
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

fn head(lib: &Library, id: i64) -> Head {
    lib.db().read(|c| versions::head(c, id)).unwrap().unwrap()
}

fn base_of(lib: &Library, id: i64) -> Base {
    let h = head(lib, id);
    Base {
        hash: h.hash,
        version: h.version,
    }
}

fn versions_of(lib: &Library, id: i64) -> Vec<Version> {
    lib.db().read(|c| versions::list(c, id)).unwrap()
}

fn states(lib: &Library, id: i64) -> Vec<(i64, FileState)> {
    versions_of(lib, id)
        .iter()
        .map(|v| (v.version, v.file_state))
        .collect()
}

fn result_of(record: &JobRecord) -> Json {
    serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap()
}

/// `files/` のファイル（相対パス）の一覧。
fn files(lib: &Library) -> Vec<String> {
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
fn staged(lib: &Library) -> Vec<PathBuf> {
    walkdir::WalkDir::new(lib.layout().staging_dir())
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// 台帳・版・ファイルのスナップショット（Conflict で何も変わらないことを照らす）。
/// (files/ のファイル, 台帳の (hash, ext), 版の (アイテム, 版, 状態), アイテムの数)。
type Snapshot = (
    Vec<String>,
    Vec<(Vec<u8>, String)>,
    Vec<(i64, i64, String)>,
    i64,
);

fn snapshot(lib: &Library) -> Snapshot {
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
fn assert_consistent(lib: &Library) {
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
            "SELECT count(*) FROM jobs WHERE kind = 'edit' AND state NOT IN ('done', 'failed', 'canceled')"
        ),
        0
    );
}

/// 保存のジョブだけを動かすライブラリ（サムネイル・解析のジョブは登録しないので、置き換えたあとの pending が見える）。
struct Env {
    dir: tempfile::TempDir,
    lib: Arc<Library>,
    jobs: JobManager,
    events: Arc<Mutex<Vec<JobEvent>>>,
}

impl Env {
    fn new() -> Self {
        Self::with(FakeEngineFactory::default(), None)
    }

    fn with(factory: FakeEngineFactory, fault: Option<Fault>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let engine = Arc::new(EngineServices::new(
            Arc::new(factory),
            lib.layout().staging_dir().join(ENGINE_WORK_DIR),
        ));
        let mut handler = EditHandler::new(engine);
        if let Some(fault) = fault {
            handler = handler.with_fault(fault);
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let jobs = JobManager::new(
            lib.clone(),
            vec![(JobKind::Edit, Arc::new(handler))],
            sink(&events),
        )
        .unwrap();
        jobs.start().unwrap();
        Self {
            dir,
            lib,
            jobs,
            events,
        }
    }

    fn png(&self, seed: u64) -> (i64, [u8; 32], Vec<u8>) {
        let bytes = sprite(seed);
        let (id, hash) = add_item(&self.lib, &bytes, "png", &format!("絵{seed}"));
        (id, hash, bytes)
    }

    fn start(&self, item_id: i64, base: Base, action: SaveAction) -> i64 {
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

    fn save(&self, item_id: i64, base: Base, action: SaveAction) -> JobRecord {
        let id = self.start(item_id, base, action);
        assert!(self.jobs.wait_idle(WAIT));
        self.jobs.get(id).unwrap()
    }

    fn overwrite(&self, item_id: i64, graph: Graph) -> JobRecord {
        self.save(
            item_id,
            base_of(&self.lib, item_id),
            SaveAction::Overwrite { graph },
        )
    }

    fn save_as(&self, item_id: i64, graph: Graph, title: &str, stem: &str) -> JobRecord {
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

    fn count(&self, sql: &str) -> i64 {
        count(&self.lib, sql)
    }
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

fn assert_conflict(record: &JobRecord) {
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

/// 元に戻す保存は段 2 の C で足す（今は積まずに Unsupported）。
#[test]
fn revert_is_not_yet_supported() {
    let env = Env::new();
    let (a, h0, _) = env.png(9);
    let err = start_save(
        env.lib.db(),
        &env.jobs,
        &SaveRequest {
            item_id: a,
            base: Base {
                hash: h0,
                version: 0,
            },
            action: SaveAction::Revert { to: 0 },
        },
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::Unsupported(_)), "{err}");
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
    assert_eq!(env.count("SELECT count(*) FROM jobs"), 0);
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

/// 段ごとの、強制終了した直後の状態を確かめる。
fn assert_crashed_state(env: &Env, job: i64, at: Fault, out: [u8; 32]) {
    let lib = &env.lib;
    let pending = count(lib, "SELECT count(*) FROM ledger_pending");
    let placed = lib.layout().content_path(&out, "png").exists();
    let (_, orphans) = check_files(lib.db(), lib.layout()).unwrap();
    let committed = lib
        .db()
        .read(|c| versions::by_job(c, job))
        .unwrap()
        .is_some();
    let parts = staged(lib).len();
    match at {
        Fault::AfterRender => assert_eq!((pending, placed, committed, parts), (0, false, false, 1)),
        Fault::AfterAnnounce => {
            assert_eq!((pending, placed, committed, parts), (1, false, false, 1))
        }
        Fault::AfterPlace => {
            assert_eq!((pending, placed, committed, orphans), (1, true, false, 1));
        }
        Fault::AfterCommit => {
            assert_eq!((pending, placed, committed, orphans), (0, true, true, 0));
        }
    }
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
            let env = Env::with(FakeEngineFactory::default(), Some(at));
            let (a, _, png) = env.png(13);
            let graph = doc(&[flip("vertical")], None);
            let out = hash_of(&render_png(&png, &graph));
            let record = if save_as {
                env.save_as(a, graph, "別名", "alt")
            } else {
                env.overwrite(a, graph)
            };
            assert_eq!(record.state, JobState::Failed, "{save_as} {at:?}");
            assert_crashed_state(&env, record.id, at, out);
            // 強制終了：ジョブは running のまま
            let job = record.id;
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
                Services::new(Arc::new(FakeBackend))
                    .with_engine(Arc::new(FakeEngineFactory::default())),
            )
            .unwrap();
            session.wait_ready();
            let report = session.integrity().unwrap();
            assert_eq!(
                (report.orphan_files, report.missing_files),
                (0, 0),
                "{save_as} {at:?}"
            );
            assert!(session.jobs().wait_idle(WAIT));
            let record = session.jobs().get(job).unwrap();
            assert_done(&record);
            let lib = session.library();
            if save_as {
                assert!(versions_of(lib, a).is_empty(), "{at:?}");
                assert_eq!(count(lib, "SELECT count(*) FROM items"), 2, "{at:?}");
                let b = result_of(&record)["itemId"].as_i64().unwrap();
                assert_eq!(
                    states(lib, b),
                    vec![(0, FileState::Hold), (1, FileState::Hold)]
                );
                assert_eq!(head(lib, b).hash, out);
            } else {
                assert_eq!(count(lib, "SELECT count(*) FROM items"), 1, "{at:?}");
                assert_eq!(
                    states(lib, a),
                    vec![(0, FileState::Hold), (1, FileState::Hold)]
                );
                assert_eq!(head(lib, a).hash, out);
            }
            assert_eq!(
                count(
                    lib,
                    "SELECT count(*) FROM item_versions WHERE job_id IS NOT NULL"
                ),
                1
            );
            // 保存のあとのサムネイル・解析も終わっている
            assert_eq!(
                count(
                    lib,
                    "SELECT count(*) FROM items WHERE thumb_state = 'pending' OR analysis_state = 'pending'"
                ),
                0,
                "{at:?}"
            );
            assert_consistent(lib);
            drop(session);
            drop(dir);
        }
    }
}

// ---- S14：保存した中身の取り込み ----

/// 段 3 の GC の代わり：版 `v` のファイルを手放し（released）、台帳の行を解放してファイルを消す。
fn simulate_gc(lib: &Library, item: i64, v: i64) {
    let _gate = lib.ingest_exclusive();
    let released = lib
        .db()
        .write(move |tx| -> Result<Vec<ledger::ReleasedBlob>> {
            let hash: Vec<u8> = tx.query_row(
                "SELECT output_hash FROM item_versions WHERE item_id = ?1 AND version = ?2",
                params![item, v],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE item_versions SET file_state = 'released', file_blob_id = NULL WHERE item_id = ?1 AND version = ?2",
                params![item, v],
            )?;
            let hash: [u8; 32] = hash.try_into().unwrap();
            ledger::release(tx, &[hash])
        })
        .unwrap();
    for blob in released {
        let _ = std::fs::remove_file(lib.layout().content_path(&blob.hash, &blob.ext));
        for path in lib.layout().derived_paths(&hex(&blob.hash)) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// S14：本当の保存のあと、元・GC で消えた過去の版・今と同じ中身を取り込むと、どれもそのアイテムの重複になり（アイテムは
/// 増えない）、孤立したファイルが残らない。
#[test]
fn s14_importing_saved_contents_is_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let session = LibrarySession::create(
        &dir.path().join("L"),
        Arc::new(|_| {}),
        Services::new(Arc::new(FakeBackend)).with_engine(Arc::new(FakeEngineFactory::default())),
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
    simulate_gc(&lib, a, 1);
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
