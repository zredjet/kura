//! プロジェクトの登録と照らし直しのテスト（M15 段 2）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;

use super::scan::{ProjectHandler, ScanResult};
use super::*;
use crate::jobs::{EventSink, JobHandler, JobKind, JobManager, JobState};
use crate::library::Library;
use crate::time::now_ms;

const WAIT: Duration = Duration::from_secs(30);

struct Env {
    dir: tempfile::TempDir,
    library: Arc<Library>,
    jobs: JobManager,
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn hash(body: &[u8]) -> Vec<u8> {
    blake3::hash(body).as_bytes().to_vec()
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
        let sink: EventSink = Arc::new(|_| {});
        let jobs = JobManager::new(
            library.clone(),
            vec![(
                JobKind::Project,
                Arc::new(ProjectHandler) as Arc<dyn JobHandler>,
            )],
            sink,
        )
        .unwrap();
        jobs.start().unwrap();
        Self { dir, library, jobs }
    }

    fn folder(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 中身 `body` のアイテムを入れる（ファイルは置かない。照らしは DB のハッシュだけを見る）。
    fn item(&self, body: &[u8]) -> i64 {
        let h = hash(body);
        self.library
            .db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                     VALUES (?1, 'png', 'image', 1, 't', 't.png', 1, 1)",
                    params![h],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .unwrap()
    }

    fn prepare(&self, root: &Path, except: Option<i64>) -> Result<(String, Engine)> {
        let existing = self.library.db().read(list)?;
        prepare_root(root, self.library.layout().root(), &existing, except)
    }

    fn add(&self, root: &Path) -> Result<i64> {
        let (root, engine) = self.prepare(root, None)?;
        self.library
            .db()
            .write(move |tx| add(tx, &root, engine, None, now_ms()))
    }

    fn scan(&self) -> ScanResult {
        let id = queue_scan(&self.jobs).unwrap().unwrap();
        assert!(self.jobs.wait_idle(WAIT));
        let record = self.jobs.get(id).unwrap();
        assert_eq!(
            record.state,
            JobState::Done,
            "{:?} {:?}",
            record.error_code,
            record.message
        );
        serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap()
    }

    /// プロジェクトの行（パス → (アイテム, 何と一致したか)）。
    fn files(&self, project: i64) -> Vec<(String, Option<i64>, Option<String>)> {
        self.library
            .db()
            .read(move |c| {
                let mut stmt = c.prepare(
                    "SELECT path, item_id, matched FROM project_files WHERE project_id = ?1 ORDER BY path",
                )?;
                let rows = stmt.query_map([project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .unwrap()
    }
}

#[test]
fn prepare_root_checks_the_folder_and_the_library() {
    let env = Env::new();
    let lib = env.library.layout().root().to_path_buf();
    let game = env.folder("game");
    write(&game, "project.godot", b"");
    let (root, engine) = prepare_root(&game, &lib, &[], None).unwrap();
    assert_eq!(engine, Engine::Godot);
    assert!(Path::new(&root).is_absolute());
    // シンボリックリンク・相対の要素は正規化する
    std::os::unix::fs::symlink(&game, env.dir.path().join("link")).unwrap();
    assert_eq!(
        prepare_root(&env.dir.path().join("link"), &lib, &[], None)
            .unwrap()
            .0,
        root
    );
    assert!(matches!(
        prepare_root(Path::new("game"), &lib, &[], None),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        prepare_root(&env.dir.path().join("none"), &lib, &[], None),
        Err(CoreError::NotFound(_))
    ));
    write(&game, "a.png", b"x");
    assert!(matches!(
        prepare_root(&game.join("a.png"), &lib, &[], None),
        Err(CoreError::Invalid(_))
    ));
    // ライブラリの中・ライブラリを含むフォルダは断る
    assert!(matches!(
        prepare_root(&lib.join("files"), &lib, &[], None),
        Err(CoreError::Invalid(_)) | Err(CoreError::NotFound(_))
    ));
    std::fs::create_dir_all(lib.join("thumbs")).unwrap();
    assert!(matches!(
        prepare_root(&lib.join("thumbs"), &lib, &[], None),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        prepare_root(env.dir.path(), &lib, &[], None),
        Err(CoreError::Invalid(_))
    ));
}

/// 重なりは、パスの文字のほかにファイルの見分けでも確かめる（外側のフォルダ、`..` の入ったパス、大文字小文字だけが違う名前、
/// ファームリンクの道）。途中がファイルのパスは「フォルダを選んで」、名前を省いた長いフォルダ名は上限で切る。
#[test]
fn overlap_is_found_by_path_and_by_identity() {
    let env = Env::new();
    let inner = env.folder("outer/Game");
    let id = env.add(&inner).unwrap();
    let outer = env.dir.path().join("outer");
    assert!(matches!(env.add(&outer), Err(CoreError::Conflict(_))));
    assert!(matches!(
        env.prepare(&outer.join("Game/../Game"), None),
        Err(CoreError::Conflict(_))
    ));
    // 大文字小文字を区別しないボリュームなら、同じフォルダとして断る
    let lower = outer.join("game");
    if lower.exists() {
        assert!(matches!(env.add(&lower), Err(CoreError::Conflict(_))));
    }
    // ファームリンクの道（/System/Volumes/Data の下）でも同じフォルダ・ライブラリを含むフォルダとして断る
    let real = std::fs::canonicalize(&inner).unwrap();
    let firm = Path::new("/System/Volumes/Data").join(real.strip_prefix("/").unwrap());
    if firm.is_dir() {
        assert!(matches!(env.add(&firm), Err(CoreError::Conflict(_))));
        let lib = std::fs::canonicalize(env.library.layout().root()).unwrap();
        let firm_parent = Path::new("/System/Volumes/Data")
            .join(lib.parent().unwrap().strip_prefix("/").unwrap());
        assert!(matches!(
            env.prepare(&firm_parent, None),
            Err(CoreError::Invalid(_)) | Err(CoreError::Conflict(_))
        ));
    }
    // 自分自身は除いて選び直せる（同じ場所に）
    assert!(env.prepare(&inner, Some(id)).is_ok());
    // 途中がファイルのパスは「フォルダを選んで」
    write(&inner, "a.png", b"x");
    assert!(matches!(
        env.prepare(&inner.join("a.png/x"), None),
        Err(CoreError::Invalid(_))
    ));
    // 名前を省けばフォルダの名前を上限で切る（名前を書いたときは長すぎれば断る）
    let long = env.folder(&"a".repeat(MAX_NAME_CHARS + 10));
    let id = env.add(&long).unwrap();
    let p = env.library.db().read(move |c| get(c, id)).unwrap();
    assert_eq!(p.name, "a".repeat(MAX_NAME_CHARS));
    let root = p.root.clone();
    let renamed = env.library.db().write(move |tx| {
        remove(tx, id)?;
        add(
            tx,
            &root,
            Engine::Folder,
            Some(&"い".repeat(MAX_NAME_CHARS + 1)),
            1,
        )
    });
    assert!(matches!(renamed, Err(CoreError::Invalid(_))));
}

/// 読み切れなかったフォルダの下の前の行は消さない（照らしの途中で外付けが外れた・許されていないとき）。
#[test]
fn unreadable_folders_keep_their_rows() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let a = env.item(b"open");
    let b = env.item(b"shut");
    let game = env.folder("game");
    write(&game, "open/a.png", b"open");
    write(&game, "shut/b.png", b"shut");
    let id = env.add(&game).unwrap();
    let r = env.scan();
    assert_eq!((r.files, r.matched), (2, 2));
    std::fs::set_permissions(game.join("shut"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = env.scan();
    std::fs::set_permissions(game.join("shut"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!((r.files, r.removed), (1, 0));
    let rows: Vec<Option<i64>> = env.files(id).into_iter().map(|(_, item, _)| item).collect();
    assert_eq!(rows, vec![Some(a), Some(b)]);
    let r = env.scan();
    assert_eq!((r.files, r.hashed, r.removed), (2, 0, 0));
}

#[test]
fn projects_are_listed_renamed_relocated_and_removed() {
    let env = Env::new();
    let a = env.folder("a");
    let id = env.add(&a).unwrap();
    // 同じ・中・外側のフォルダは重なるので断る
    assert!(matches!(env.add(&a), Err(CoreError::Conflict(_))));
    let inner = env.folder("a/inner");
    assert!(matches!(env.add(&inner), Err(CoreError::Conflict(_))));
    let b = env.folder("b");
    let other = env.add(&b).unwrap();
    let list = env.library.db().read(list).unwrap();
    let names: Vec<(&str, Engine, f64)> = list
        .iter()
        .map(|p| (p.name.as_str(), p.engine, p.pos))
        .collect();
    assert_eq!(
        names,
        vec![("a", Engine::Folder, 1.0), ("b", Engine::Folder, 2.0)]
    );
    env.library
        .db()
        .write(move |tx| rename(tx, id, "  ゲーム ", 5))
        .unwrap();
    assert_eq!(
        env.library.db().read(move |c| get(c, id)).unwrap().name,
        "ゲーム"
    );
    assert!(matches!(
        env.library.db().write(move |tx| rename(tx, id, " ", 5)),
        Err(CoreError::Invalid(_))
    ));
    // 場所を選び直す：ほかのプロジェクトと重なれば断る
    let c = env.folder("c");
    let (croot, cengine) = env.prepare(&c, Some(id)).unwrap();
    let (broot, bengine) = prepare_root(&b, env.library.layout().root(), &[], None).unwrap();
    assert!(matches!(
        env.library
            .db()
            .write(move |tx| relocate(tx, id, &broot, bengine, 6)),
        Err(CoreError::Conflict(_))
    ));
    let moved = croot.clone();
    env.library
        .db()
        .write(move |tx| relocate(tx, id, &moved, cengine, 6))
        .unwrap();
    assert_eq!(
        env.library.db().read(move |c| get(c, id)).unwrap().root,
        croot
    );
    env.library.db().write(move |tx| remove(tx, other)).unwrap();
    assert!(matches!(
        env.library.db().read(move |c| get(c, other)),
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        env.library.db().write(move |tx| remove(tx, other)),
        Err(CoreError::NotFound(_))
    ));
}

#[test]
fn scan_matches_current_original_versions_and_outputs_and_follows_changes() {
    let env = Env::new();
    let a = env.item(b"cur");
    let b = env.item(b"bbb");
    let (orig, past, out) = (hash(b"orig"), hash(b"past"), hash(b"out"));
    env.library
        .db()
        .write(move |tx| {
            tx.execute_batch(
                "INSERT INTO edit_objects (id, digest, kind, body, created_at) VALUES (50, x'd5', 'doc', CAST('{}' AS BLOB), 1);",
            )?;
            tx.execute(
                "INSERT INTO item_versions (item_id, version, kind, doc_id, base_version, revert_to, output_hash, ext,
                                            size_bytes, record, file_blob_id, file_state, replaced_at, created_at)
                 VALUES (?1, 0, 'original', NULL, NULL, NULL, ?2, 'png', 4, '{}', NULL, 'released', 1, 1),
                        (?1, 1, 'edit', 50, 0, NULL, ?3, 'png', 4, '{}', NULL, 'released', 1, 1)",
                params![a, orig, past],
            )?;
            tx.execute(
                "INSERT INTO item_outputs (hash, item_id, source_hash, ext, size_bytes, created_at)
                 VALUES (?1, ?2, x'00', 'ogg', 3, 1)",
                params![out, b],
            )?;
            Ok(())
        })
        .unwrap();
    let game = env.folder("game");
    write(&game, "project.godot", b"config");
    write(&game, "art/cur.png", b"cur");
    write(&game, "art/cur.png.import", b"[remap]");
    write(&game, "art/orig.png", b"orig");
    write(&game, "art/past.png", b"past");
    write(&game, "sfx/out.ogg", b"out");
    write(&game, "art/other.png", b"zzz");
    write(&game, "scripts/player.gd", b"extends Node");
    write(&game, ".godot/imported/cur.png", b"cur");
    let id = env.add(&game).unwrap();

    let r = env.scan();
    assert_eq!(
        (
            r.projects, r.files, r.hashed, r.matched, r.removed, r.missing
        ),
        (1, 5, 5, 4, 0, 0)
    );
    let s = |v: &str| Some(v.to_string());
    assert_eq!(
        env.files(id),
        vec![
            ("art/cur.png".into(), Some(a), s("current")),
            ("art/orig.png".into(), Some(a), s("original")),
            ("art/other.png".into(), None, None),
            ("art/past.png".into(), Some(a), s("version")),
            ("sfx/out.ogg".into(), Some(b), s("output")),
        ]
    );
    let p = env.library.db().read(move |c| get(c, id)).unwrap();
    assert_eq!(
        (p.engine, p.files, p.items, p.missing),
        (Engine::Godot, 5, 2, false)
    );
    assert!(p.scanned_at.is_some());
    assert_eq!(
        env.library.db().read(move |c| used_items(c, id)).unwrap(),
        vec![a, b]
    );
    let uses = env.library.db().read(move |c| item_projects(c, a)).unwrap();
    assert_eq!(uses.len(), 1);
    assert_eq!(
        uses[0].paths,
        vec!["art/cur.png", "art/orig.png", "art/past.png"]
    );

    // 変わらなければハッシュし直さない
    let r = env.scan();
    assert_eq!((r.files, r.hashed, r.matched, r.removed), (5, 0, 4, 0));

    // 消したファイルの行は消え、変えたファイルと新しいファイルだけハッシュする。あとから取り込んだ中身にも付け直す
    std::fs::remove_file(game.join("art/orig.png")).unwrap();
    write(&game, "art/other.png", b"zzzz");
    write(&game, "sfx/new.wav", b"cur");
    let c = env.item(b"zzzz");
    let r = env.scan();
    assert_eq!((r.files, r.hashed, r.matched, r.removed), (5, 2, 5, 1));
    assert_eq!(
        env.files(id),
        vec![
            ("art/cur.png".into(), Some(a), s("current")),
            ("art/other.png".into(), Some(c), s("current")),
            ("art/past.png".into(), Some(a), s("version")),
            ("sfx/new.wav".into(), Some(a), s("current")),
            ("sfx/out.ogg".into(), Some(b), s("output")),
        ]
    );

    // フォルダが見つからなければ記録を残して「見つからない」。場所を選び直すと照らし直す（大きさと時刻が同じならハッシュしない）
    let moved = env.dir.path().join("moved");
    std::fs::rename(&game, &moved).unwrap();
    let r = env.scan();
    assert_eq!((r.projects, r.missing, r.files), (1, 1, 0));
    let p = env.library.db().read(move |c| get(c, id)).unwrap();
    assert!(p.missing);
    assert_eq!(env.files(id).len(), 5);
    let (root, engine) = env.prepare(&moved, Some(id)).unwrap();
    env.library
        .db()
        .write(move |tx| relocate(tx, id, &root, engine, now_ms()))
        .unwrap();
    let r = env.scan();
    assert_eq!((r.missing, r.files, r.hashed, r.removed), (0, 5, 0, 0));
    assert!(!env.library.db().read(move |c| get(c, id)).unwrap().missing);

    // アイテムを消すと行は残って未一致、登録を外すと行も消える
    env.library
        .db()
        .write(move |tx| {
            tx.execute("DELETE FROM items WHERE id = ?1", [c])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        env.files(id)[1],
        ("art/other.png".into(), None, s("current"))
    );
    let r = env.scan();
    assert_eq!(r.matched, 4);
    assert_eq!(env.files(id)[1], ("art/other.png".into(), None, None));
    env.library.db().write(move |tx| remove(tx, id)).unwrap();
    assert!(env.files(id).is_empty());
}

#[test]
fn opening_queues_a_scan_only_when_projects_exist() {
    let env = Env::new();
    assert_eq!(queue_on_open(env.library.db(), &env.jobs).unwrap(), None);
    let game = env.folder("game");
    env.add(&game).unwrap();
    let queued = queue_on_open(env.library.db(), &env.jobs).unwrap();
    assert!(queued.is_some());
    assert!(env.jobs.wait_idle(WAIT));
}

#[test]
fn scan_queries_use_the_keys() {
    let conn = crate::db::test_conn();
    for (sql, want) in [
        (
            "SELECT path, size_bytes, mtime_ns, hash FROM project_files WHERE project_id = 1",
            "PRIMARY KEY",
        ),
        (
            "DELETE FROM project_files WHERE project_id = 1 AND seen_at < 5",
            "PRIMARY KEY",
        ),
        (
            "SELECT item_id FROM item_outputs WHERE hash = x'01' ORDER BY item_id LIMIT 1",
            "PRIMARY KEY",
        ),
        (
            "SELECT DISTINCT item_id FROM project_files WHERE project_id = 1 AND item_id IS NOT NULL ORDER BY item_id",
            "PRIMARY KEY",
        ),
        (
            "SELECT p.id, p.name, p.missing, f.path FROM project_files f JOIN projects p ON p.id = f.project_id
              WHERE f.item_id = 1 ORDER BY p.pos, p.id, f.path",
            "idx_project_files_item",
        ),
    ] {
        let plan = crate::db::query_plan(&conn, sql);
        assert!(plan.contains(want), "{sql}\n{plan}");
        assert!(!plan.contains("SCAN f") && !plan.contains("SCAN project_files"), "{sql}\n{plan}");
    }
}
