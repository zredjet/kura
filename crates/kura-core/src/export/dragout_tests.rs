//! ドラッグアウトのテスト（M15 段 5）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::params;

use super::*;
use crate::export::FormatTarget;
use crate::jobs::{EventSink, JobHandler, JobKind, JobManager};
use crate::library::Library;
use crate::unit::LIST_FLAG;

const WAIT: Duration = Duration::from_secs(30);

struct Env {
    dir: tempfile::TempDir,
    library: Arc<Library>,
    jobs: Arc<JobManager>,
    engine: Option<Arc<EngineServices>>,
}

impl Env {
    fn new(engine: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
        let sink: EventSink = Arc::new(|_| {});
        let engine = engine.then(|| {
            Arc::new(EngineServices::new(
                Arc::new(crate::edit::services::fake::FakeEngineFactory::default()),
                library
                    .layout()
                    .staging_dir()
                    .join(crate::edit::ENGINE_WORK_DIR),
            ))
        });
        let handlers: Vec<(JobKind, Arc<dyn JobHandler>)> = vec![(
            JobKind::Project,
            Arc::new(crate::project::scan::ProjectHandler) as Arc<dyn JobHandler>,
        )];
        let jobs = Arc::new(JobManager::new(library.clone(), handlers, sink).unwrap());
        jobs.start().unwrap();
        Self {
            dir,
            library,
            jobs,
            engine,
        }
    }

    fn drags(&self) -> DragOuts {
        DragOuts::new(self.library.clone(), self.engine.clone(), self.jobs.clone())
    }

    fn drags_with(&self, keep: Duration, scan: Duration) -> DragOuts {
        DragOuts::with_timing(
            self.library.clone(),
            self.engine.clone(),
            self.jobs.clone(),
            Timing { keep, scan },
        )
    }

    /// ライブラリにファイルを置いてアイテムを入れる。ID を返す。
    fn item(&self, original_name: &str, ext: &str, body: &[u8]) -> i64 {
        let hash = *blake3::hash(body).as_bytes();
        let path = self.library.layout().content_path(&hash, ext);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        let title = original_name
            .rsplit_once('.')
            .map_or(original_name, |(s, _)| s)
            .to_string();
        let (ext, original_name, size) = (ext.to_string(), original_name.to_string(), body.len());
        self.library
            .db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                     VALUES (?1, ?2, 'image', ?3, ?4, ?5, 1, 1)",
                    params![hash.to_vec(), ext, size as i64, title, original_name],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .unwrap()
    }

    /// Godot のプロジェクトを登録する。ID とフォルダを返す。
    fn godot(&self, name: &str) -> (i64, PathBuf) {
        let game = self.dir.path().join(name);
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("project.godot"), b"").unwrap();
        let existing = self.library.db().read(crate::project::list).unwrap();
        let (root, engine) =
            crate::project::prepare_root(&game, self.library.layout().root(), &existing, None)
                .unwrap();
        let id = self
            .library
            .db()
            .write(move |tx| crate::project::add(tx, &root, engine, None, 1))
            .unwrap();
        (id, game)
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `cond` が真になるまで待つ（試験の時間切れまで）。
fn eventually(mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < WAIT {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn unit_entry(unit: i64) -> i64 {
    i64::from(LIST_FLAG | u32::try_from(unit).unwrap())
}

#[test]
fn target_prefers_the_chosen_project_then_the_only_registered_one() {
    let env = Env::new(false);
    let none = Target {
        chosen: None,
        effective: None,
    };
    assert_eq!(env.library.db().read(target).unwrap(), none);
    let (a, _) = env.godot("a");
    // 登録が 1 つならそれ
    assert_eq!(
        env.library.db().read(target).unwrap(),
        Target {
            chosen: None,
            effective: Some(a),
        }
    );
    let (b, _) = env.godot("b");
    // 2 つあって選んでいなければ、名前も形式もそのまま
    assert_eq!(env.library.db().read(target).unwrap(), none);
    assert_eq!(
        env.library.db().read(options).unwrap(),
        ExportOptions::default()
    );
    env.library
        .db()
        .write(move |tx| set_target(tx, Some(b)))
        .unwrap();
    assert_eq!(
        env.library.db().read(target).unwrap(),
        Target {
            chosen: Some(b),
            effective: Some(b),
        }
    );
    let got = env.library.db().read(options).unwrap();
    assert_eq!(got.name.style, crate::export::names::NameStyle::Snake);
    assert_eq!(got.project, Some(b));
    // ないプロジェクトは選べない
    assert!(matches!(
        env.library.db().write(|tx| set_target(tx, Some(999))),
        Err(CoreError::NotFound(_))
    ));
    // 選んだプロジェクトの登録を外すと、選んでいないのと同じ（残りが 1 つならそれ）
    env.library
        .db()
        .write(move |tx| crate::project::remove(tx, b))
        .unwrap();
    assert_eq!(
        env.library.db().read(target).unwrap(),
        Target {
            chosen: None,
            effective: Some(a),
        }
    );
    env.library
        .db()
        .write(move |tx| set_target(tx, Some(a)))
        .unwrap();
    env.library.db().write(|tx| set_target(tx, None)).unwrap();
    let left: Option<i64> = env
        .library
        .db()
        .read(|c| settings::get(c, TARGET_KEY))
        .unwrap();
    assert_eq!(left, None);
}

#[test]
fn files_are_named_by_the_target_project_and_units_become_folders() {
    let env = Env::new(false);
    let (_, _) = env.godot("game");
    let a = env.item("Hero Idle.png", "png", b"a");
    let b = env.item("Hero Idle.png", "png", b"b");
    let c = env.item("Jump SE.png", "png", b"c");
    let d = env.item("Coin.png", "png", b"d");
    let unit = env
        .library
        .db()
        .write(move |tx| crate::unit::create_set(tx, "Enemy Set", &[d, c], 1))
        .unwrap();
    let drags = env.drags();
    let session = drags.begin(&[a, b, unit_entry(unit), a]).unwrap();
    let dir = session.dir().to_path_buf();
    assert!(dir.starts_with(env.library.layout().staging_dir().join(DRAGOUT_DIR)));
    // ばらのアイテムを先に（重ねて選んだものは 1 回）、続いてユニットのフォルダ
    let entries: Vec<(String, bool)> = session
        .entries()
        .iter()
        .map(|e| {
            (
                e.path.strip_prefix(&dir).unwrap().display().to_string(),
                e.folder,
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("hero_idle.png".to_string(), false),
            ("hero_idle 2.png".to_string(), false),
            ("enemy_set".to_string(), true),
        ]
    );
    for k in 0..entries.len() {
        assert!(session.wait(k), "{k}");
    }
    assert_eq!(std::fs::read(dir.join("hero_idle.png")).unwrap(), b"a");
    assert_eq!(std::fs::read(dir.join("hero_idle 2.png")).unwrap(), b"b");
    assert_eq!(
        names_in(&dir.join("enemy_set")),
        ["coin.png", "jump_se.png"]
    );
    assert_eq!(std::fs::read(dir.join("enemy_set/coin.png")).unwrap(), b"d");
    assert_eq!(session.skipped(), 0);
    assert_eq!(
        session.end(),
        DragOutcome {
            delivered: 3,
            failed: 0,
            late: 0,
            missed: 0,
        }
    );
}

#[test]
fn without_a_target_names_and_formats_stay_as_they_are() {
    let env = Env::new(false);
    env.godot("a");
    env.godot("b");
    let a = env.item("Hero Idle.psd", "psd", b"a");
    let drags = env.drags();
    let session = drags.begin(&[a]).unwrap();
    assert!(session.wait(0));
    assert_eq!(
        session.entries()[0].path,
        session.dir().join("Hero Idle.psd")
    );
}

#[test]
fn trashed_and_missing_items_are_skipped_and_nothing_left_is_refused() {
    let env = Env::new(false);
    let a = env.item("a.png", "png", b"a");
    let b = env.item("b.png", "png", b"b");
    env.library
        .db()
        .write(move |tx| {
            tx.execute("UPDATE items SET trashed_at = 5 WHERE id = ?1", [b])?;
            Ok(())
        })
        .unwrap();
    let drags = env.drags();
    let session = drags.begin(&[a, b, 999]).unwrap();
    assert_eq!(session.entries().len(), 1);
    assert_eq!(session.skipped(), 2);
    assert!(matches!(drags.begin(&[b, 999]), Err(CoreError::Invalid(_))));
    // 作るものがなければ回のフォルダを作らない
    assert_eq!(names_in(&drags.root()), [session.id().to_string()]);
}

#[test]
fn too_many_files_at_once_are_refused() {
    let env = Env::new(false);
    let ids: Vec<i64> = env
        .library
        .db()
        .write(|tx| {
            let mut ids = Vec::new();
            for k in 0..=MAX_FILES {
                tx.execute(
                    "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                     VALUES (?1, 'png', 'image', 1, ?2, ?2, 1, 1)",
                    params![blake3::hash(&k.to_le_bytes()).as_bytes().to_vec(), format!("i{k}.png")],
                )?;
                ids.push(tx.last_insert_rowid());
            }
            Ok(ids)
        })
        .unwrap();
    let drags = env.drags();
    assert!(matches!(drags.begin(&ids), Err(CoreError::Invalid(_))));
    assert!(drags.begin(&ids[..MAX_FILES]).is_ok());
}

#[test]
fn a_missing_library_file_fails_only_that_entry() {
    let env = Env::new(false);
    let a = env.item("a.png", "png", b"a");
    let b = env.item("b.png", "png", b"b");
    let hash = *blake3::hash(b"a").as_bytes();
    std::fs::remove_file(env.library.layout().content_path(&hash, "png")).unwrap();
    let drags = env.drags();
    let session = drags.begin(&[a, b]).unwrap();
    assert!(!session.wait(0));
    assert!(session.wait(1));
    assert_eq!(
        session.end(),
        DragOutcome {
            delivered: 1,
            failed: 1,
            late: 0,
            missed: 1,
        }
    );
}

/// 偽のエンジンの固定の段で書いた小さな PNG。
fn sprite_png() -> Vec<u8> {
    use kura_engine::testgen::InputGen;
    use kura_engine::value::PixelFormat;
    let value = InputGen::PixelSprite {
        w: 16,
        h: 12,
        fmt: PixelFormat::Rgba8,
        seed: 3,
    }
    .generate()
    .unwrap();
    kura_engine::codec::CodecSet::pinned()
        .encode_bytes(
            &value,
            "png",
            &serde_json::json!({}),
            &kura_engine::NoMonitor,
        )
        .unwrap()
        .0
}

#[test]
fn conversions_are_rendered_recorded_and_matched_after_the_drop() {
    let env = Env::new(true);
    let (project, game) = env.godot("game");
    let mut preset = crate::export::ExportPreset::for_engine(crate::project::Engine::Godot);
    preset.image = FormatTarget::To {
        format: "webp".into(),
    };
    env.library
        .db()
        .write(move |tx| crate::project::set_settings(tx, project, &preset, 1))
        .unwrap();
    let a = env.item("Hero.png", "png", &sprite_png());
    let drags = env.drags_with(Duration::from_secs(600), Duration::from_millis(20));
    let session = drags.begin(&[a]).unwrap();
    assert!(session.wait(0));
    let staged = session.entries()[0].path.clone();
    assert_eq!(staged.file_name().unwrap(), "hero.webp");
    let out = std::fs::read(&staged).unwrap();
    assert_eq!(&out[8..12], b"WEBP");
    // 出力の中身を記録する
    let out_hash = blake3::hash(&out).as_bytes().to_vec();
    let recorded: i64 = env
        .library
        .db()
        .read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM item_outputs WHERE hash = ?1 AND item_id = ?2",
                params![out_hash, a],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(recorded, 1);
    // 落とした先（Godot）がプロジェクトへコピーし、ドラッグが終わると少し後に照らし直して使った記録を付ける
    std::fs::copy(&staged, game.join("hero.webp")).unwrap();
    assert_eq!(session.end().delivered, 1);
    assert!(eventually(|| {
        env.library
            .db()
            .read(move |c| crate::project::used_items(c, project))
            .unwrap()
            == vec![a]
    }));
    // 一時置き場は残す（Finder は裏でコピーする）
    assert!(staged.is_file());
}

#[test]
fn converting_without_an_engine_fails_that_entry() {
    let env = Env::new(false);
    let (project, _) = env.godot("game");
    let mut preset = crate::export::ExportPreset::for_engine(crate::project::Engine::Godot);
    preset.image = FormatTarget::To {
        format: "webp".into(),
    };
    env.library
        .db()
        .write(move |tx| crate::project::set_settings(tx, project, &preset, 1))
        .unwrap();
    let a = env.item("a.png", "png", b"a");
    let drags = env.drags();
    let session = drags.begin(&[a]).unwrap();
    assert!(!session.wait(0));
    let outcome = session.end();
    assert_eq!((outcome.failed, outcome.missed), (1, 1));
}

#[test]
fn a_drag_with_nothing_delivered_is_cleaned_up_at_once() {
    let env = Env::new(false);
    let a = env.item("a.png", "png", b"a");
    let drags = env.drags_with(Duration::from_secs(600), Duration::from_secs(600));
    let session = drags.begin(&[a]).unwrap();
    let dir = session.dir().to_path_buf();
    assert!(dir.is_dir());
    // 求められずに終わった（やめた）回は、渡さなかったことを数えない
    assert_eq!(session.end(), DragOutcome::default());
    assert!(eventually(|| !dir.exists() && drags.live() == 0));
    assert!(session.retired());
    // 片付けたら渡さない
    assert!(!session.wait(0));
    // 始められなかった（ボタンが離れていた）回も同じ
    let other = drags.begin(&[a]).unwrap();
    let dir = other.dir().to_path_buf();
    other.abort();
    assert!(eventually(|| !dir.exists() && drags.live() == 0));
}

#[test]
fn a_delivered_drag_is_kept_for_a_while_and_then_removed() {
    let env = Env::new(false);
    let a = env.item("a.png", "png", b"a");
    let drags = env.drags_with(Duration::from_millis(300), Duration::from_millis(10));
    let session = drags.begin(&[a]).unwrap();
    let dir = session.dir().to_path_buf();
    assert!(session.wait(0));
    let ended = Instant::now();
    assert_eq!(session.end().delivered, 1);
    // 2 回目の end は数えるだけ
    assert_eq!(session.end().delivered, 1);
    assert!(eventually(|| !dir.exists()));
    assert!(ended.elapsed() >= Duration::from_millis(300));
    assert_eq!(drags.live(), 0);
    // 渡したものは、片付けるまでは何度求められても渡す（片付けたあとは渡さない）
    assert!(!session.wait(0));
}

#[test]
fn closing_stops_the_work_and_removes_every_drag() {
    let env = Env::new(false);
    let a = env.item("a.png", "png", b"a");
    let drags = env.drags();
    let one = drags.begin(&[a]).unwrap();
    let two = drags.begin(&[a]).unwrap();
    assert!(one.wait(0) && two.wait(0));
    assert_ne!(one.dir(), two.dir());
    one.end();
    drags.close();
    assert!(!drags.root().exists());
    assert!(!one.wait(0) && !two.wait(0));
    assert!(matches!(drags.begin(&[a]), Err(CoreError::Invalid(_))));
    // 何度閉じてもよい
    drags.close();
}

/// 作り終えていない項目は、回の最初に求められてから待つ長さまで待ち、過ぎたら渡さない（あとで求められても同じ長さで切る）。
#[test]
fn waiting_gives_up_after_the_limit() {
    let session = DragSession {
        id: 1,
        dir: PathBuf::from("/nonexistent"),
        entries: vec![
            DragEntry {
                path: PathBuf::from("/nonexistent/a.png"),
                folder: false,
                image: None,
                files: 0..1,
            },
            DragEntry {
                path: PathBuf::from("/nonexistent/u"),
                folder: true,
                image: None,
                files: 1..3,
            },
        ],
        slots: Vec::new(),
        order: Vec::new(),
        skipped: 0,
        state: Mutex::new(SessionState {
            slots: vec![SlotState::Pending; 3],
            asked: vec![false; 2],
            handed: vec![false; 2],
            late: vec![false; 2],
            deadline: None,
            ended: false,
            retired: false,
            expires: None,
            scan_at: None,
        }),
        cond: Condvar::new(),
        cancel: AtomicBool::new(false),
        worker: Mutex::new(None),
        owner: Weak::new(),
        timing: Timing::default(),
    };
    let start = Instant::now();
    assert!(!session.wait_for(0, Duration::from_millis(50)));
    assert!(start.elapsed() >= Duration::from_millis(50));
    // 期限は回で 1 つ（2 つ目はもう待たない）
    let again = Instant::now();
    assert!(!session.wait_for(1, Duration::from_secs(60)));
    assert!(again.elapsed() < Duration::from_secs(5));
    // フォルダは中のどれかができ、残りが終われば渡す
    session.set(1, SlotState::Failed);
    session.set(2, SlotState::Ready);
    assert!(session.wait_for(1, Duration::from_secs(60)));
    // 中が全部できなかったフォルダは渡さない
    session.set(2, SlotState::Failed);
    {
        let mut st = lock(&session.state);
        st.handed[1] = false;
    }
    assert!(!session.wait_for(1, Duration::from_secs(60)));
    assert_eq!(
        session.end(),
        DragOutcome {
            delivered: 0,
            failed: 2,
            late: 2,
            missed: 2,
        }
    );
    // ほかのスレッドで作り終えたら、待っている側が起きる
    let session = Arc::new(DragSession {
        state: Mutex::new(SessionState {
            slots: vec![SlotState::Pending; 3],
            asked: vec![false; 2],
            handed: vec![false; 2],
            late: vec![false; 2],
            deadline: None,
            ended: false,
            retired: false,
            expires: None,
            scan_at: None,
        }),
        entries: session.entries.clone(),
        dir: session.dir.clone(),
        id: 2,
        slots: Vec::new(),
        order: Vec::new(),
        skipped: 0,
        cond: Condvar::new(),
        cancel: AtomicBool::new(false),
        worker: Mutex::new(None),
        owner: Weak::new(),
        timing: Timing::default(),
    });
    let maker = session.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        maker.set(0, SlotState::Ready);
    });
    assert!(session.wait_for(0, Duration::from_secs(60)));
    t.join().unwrap();
}
