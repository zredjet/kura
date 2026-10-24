//! 書き出しのジョブのテスト。

use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;
use serde_json::Value;

use super::job::{Group, place_new, resolve};
use super::*;
use crate::jobs::{EventSink, JobHandler, JobKind, JobManager, JobState};
use crate::library::Library;
use crate::unit::LIST_FLAG;

const WAIT: Duration = Duration::from_secs(30);

struct Env {
    dir: tempfile::TempDir,
    library: Arc<Library>,
    jobs: JobManager,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
        let sink: EventSink = Arc::new(|_| {});
        let jobs = JobManager::new(
            library.clone(),
            vec![(
                JobKind::Export,
                Arc::new(ExportHandler) as Arc<dyn JobHandler>,
            )],
            sink,
        )
        .unwrap();
        jobs.start().unwrap();
        Self { dir, library, jobs }
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
                    "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, rating, note, source_url)
                     VALUES (?1, ?2, 'image', ?3, ?4, ?5, 1791648418942, 1, 4, 'メモ', 'https://example.com/a')",
                    params![hash.to_vec(), ext, size as i64, title, original_name],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .unwrap()
    }

    fn dest(&self, name: &str) -> std::path::PathBuf {
        let dest = self.dir.path().join(name);
        std::fs::create_dir_all(&dest).unwrap();
        dest
    }

    fn run(&self, dest: &std::path::Path, entries: Vec<i64>) -> (JobState, ExportResult) {
        let id = start_export(
            &self.jobs,
            &ExportInput {
                dest: dest.to_path_buf(),
                entries,
            },
        )
        .unwrap();
        assert!(self.jobs.wait_idle(WAIT));
        let record = self.jobs.get(id).unwrap();
        let result = record
            .result_json
            .as_deref()
            .map(|j| serde_json::from_str(j).unwrap())
            .unwrap_or_default();
        (record.state, result)
    }
}

fn names_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn exports_with_original_names_units_in_folders_and_a_manifest() {
    let env = Env::new();
    let a = env.item("夕方.jpg", "jpg", b"a");
    let b = env.item("夕方.jpg", "jpg", b"b");
    let c = env.item("IMG_0003.HEIC", "heic", b"c");
    let d = env.item("IMG_0004.HEIC", "heic", b"d");
    let (unit, tag) = env
        .library
        .db()
        .write(move |tx| {
            let unit = crate::unit::create_set(tx, "鎌倉/旅行", &[d, c], 1)?;
            tx.execute(
                "INSERT INTO tags (namespace, name, created_at) VALUES ('場所', '鎌倉', 1)",
                [],
            )?;
            let tag = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO item_tags (tag_id, item_id) VALUES (?1, ?2)",
                [tag, a],
            )?;
            Ok((unit, tag))
        })
        .unwrap();
    assert!(tag > 0);
    let dest = env.dest("out");

    let unit_entry = i64::from(LIST_FLAG | u32::try_from(unit).unwrap());
    // a を重ねて選んでも 1 回だけ
    let (state, result) = env.run(&dest, vec![unit_entry, a, b, a]);
    assert_eq!(state, JobState::Done);
    assert_eq!((result.total, result.exported, result.failed), (4, 4, 0));
    assert_eq!(result.manifest.as_deref(), Some("kura-export.json"));
    assert!(
        result.files.is_empty(),
        "終わったらファイルの一覧は残さない"
    );

    // 同じ名前は「名前 2」、ユニットは名前の「/」を変えたフォルダに並び順で
    assert_eq!(
        names_in(&dest),
        ["kura-export.json", "夕方 2.jpg", "夕方.jpg", "鎌倉_旅行"]
    );
    assert_eq!(
        names_in(&dest.join("鎌倉_旅行")),
        ["IMG_0003.HEIC", "IMG_0004.HEIC"]
    );
    assert_eq!(std::fs::read(dest.join("夕方 2.jpg")).unwrap(), b"b");

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(dest.join("kura-export.json")).unwrap()).unwrap();
    assert_eq!(manifest["app"], "Kura");
    let items = manifest["items"].as_array().unwrap();
    let files: Vec<&str> = items.iter().map(|i| i["file"].as_str().unwrap()).collect();
    assert_eq!(
        files,
        [
            "夕方.jpg",
            "夕方 2.jpg",
            "鎌倉_旅行/IMG_0004.HEIC",
            "鎌倉_旅行/IMG_0003.HEIC"
        ]
    );
    assert_eq!(items[0]["tags"], serde_json::json!(["場所:鎌倉"]));
    assert_eq!(items[0]["rating"], 4);
    assert_eq!(items[0]["note"], "メモ");
    assert_eq!(items[0]["sourceUrl"], "https://example.com/a");
    assert_eq!(items[0]["importedAt"], "2026-10-10T16:06:58.942Z");
    assert_eq!(items[2]["units"], serde_json::json!(["鎌倉/旅行"]));
}

#[test]
fn never_overwrites_files_or_folders_already_in_the_destination() {
    let env = Env::new();
    let a = env.item("a.jpg", "jpg", b"new");
    let c = env.item("c.jpg", "jpg", b"c");
    let unit = env
        .library
        .db()
        .write(move |tx| crate::unit::create_set(tx, "まとめ", &[c], 1))
        .unwrap();
    let dest = env.dest("out");
    std::fs::write(dest.join("a.jpg"), b"old").unwrap();
    std::fs::write(dest.join("kura-export.json"), b"{}").unwrap();
    std::fs::create_dir(dest.join("まとめ")).unwrap();
    std::fs::write(dest.join("まとめ").join("c.jpg"), b"keep").unwrap();

    let unit_entry = i64::from(LIST_FLAG | u32::try_from(unit).unwrap());
    let (state, result) = env.run(&dest, vec![a, unit_entry]);
    assert_eq!(state, JobState::Done);
    assert_eq!(result.manifest.as_deref(), Some("kura-export 2.json"));
    assert_eq!(std::fs::read(dest.join("a.jpg")).unwrap(), b"old");
    assert_eq!(std::fs::read(dest.join("a 2.jpg")).unwrap(), b"new");
    assert_eq!(std::fs::read(dest.join("kura-export.json")).unwrap(), b"{}");
    // 既存のフォルダには書かず、新しいフォルダを作る
    assert_eq!(
        std::fs::read(dest.join("まとめ").join("c.jpg")).unwrap(),
        b"keep"
    );
    assert_eq!(names_in(&dest.join("まとめ 2")), ["c.jpg"]);
    // 一時ファイルは残らない
    assert!(
        names_in(&dest).iter().all(|n| !n.ends_with(".part")),
        "{:?}",
        names_in(&dest)
    );
}

#[test]
fn counts_missing_library_files_as_failures_and_continues() {
    let env = Env::new();
    let a = env.item("a.jpg", "jpg", b"a");
    let b = env.item("b.jpg", "jpg", b"b");
    let hash = *blake3::hash(b"a").as_bytes();
    std::fs::remove_file(env.library.layout().content_path(&hash, "jpg")).unwrap();
    let dest = env.dest("out");

    let (state, result) = env.run(&dest, vec![a, b, 999]);
    assert_eq!(state, JobState::Done);
    assert_eq!((result.total, result.exported, result.failed), (3, 1, 2));
    assert_eq!(names_in(&dest), ["b.jpg", "kura-export.json"]);
}

/// 形式を変えて上書きしたアイテム（original_name は元の拡張子のまま。M12 の段 2 の決定 10）は、語幹に今の拡張子で書き出す。
/// 形式が同じなら（jpg と jpeg）元の名前のまま。
#[test]
fn exports_items_saved_in_another_format_with_the_current_extension() {
    let env = Env::new();
    let a = env.item("hero.psd", "png", b"a");
    let b = env.item("photo.jpeg", "jpg", b"b");
    let dest = env.dest("out");

    let (state, result) = env.run(&dest, vec![a, b]);
    assert_eq!(state, JobState::Done);
    assert_eq!((result.exported, result.failed), (2, 0));
    assert_eq!(
        names_in(&dest),
        ["hero.png", "kura-export.json", "photo.jpeg"]
    );
}

#[test]
fn rejects_a_missing_destination_and_an_empty_selection() {
    let env = Env::new();
    let a = env.item("a.jpg", "jpg", b"a");
    let (state, _) = env.run(&env.dir.path().join("ない"), vec![a]);
    assert_eq!(state, JobState::Failed);
    assert!(
        start_export(
            &env.jobs,
            &ExportInput {
                dest: env.dir.path().to_path_buf(),
                entries: vec![],
            },
        )
        .is_err()
    );
}

#[test]
fn resolves_loose_items_first_then_units_in_order() {
    let env = Env::new();
    let a = env.item("a.jpg", "jpg", b"a");
    let b = env.item("b.jpg", "jpg", b"b");
    let groups = env
        .library
        .db()
        .write(move |tx| {
            let unit = crate::unit::create_set(tx, "u", &[b, a], 1)?;
            let entry = i64::from(LIST_FLAG | u32::try_from(unit).unwrap_or(0));
            // ないユニットは飛ばす
            let missing = i64::from(LIST_FLAG | 4242);
            resolve(tx, &[entry, b, missing, a, b])
        })
        .unwrap();
    assert_eq!(
        groups,
        [
            Group {
                folder: None,
                items: vec![b, a]
            },
            Group {
                folder: Some("u".into()),
                items: vec![b, a]
            },
        ]
    );
}

#[test]
fn place_new_picks_the_next_free_name() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.bin");
    std::fs::write(&src, b"x").unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    for name in ["a.txt", "a 2.txt"] {
        std::fs::write(out.join(name), b"old").unwrap();
    }
    let placed = place_new(&src, &out, "a.txt", ".tmp.part").unwrap();
    assert_eq!(placed, "a 3.txt");
    assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"old");
    assert_eq!(std::fs::read(out.join("a 3.txt")).unwrap(), b"x");
    assert!(!out.join(".tmp.part").exists());
}
