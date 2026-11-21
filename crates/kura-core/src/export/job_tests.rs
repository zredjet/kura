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
        Self::build(false)
    }

    /// エンジンの部品（偽のファクトリ。固定の段だけの組）とプロジェクトの照らし直しもある環境（M15 段 3）。
    fn with_engine() -> Self {
        Self::build(true)
    }

    fn build(engine: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Arc::new(Library::create(&dir.path().join("L.kuralib")).unwrap());
        let sink: EventSink = Arc::new(|_| {});
        let services = engine.then(|| {
            Arc::new(crate::edit::EngineServices::new(
                Arc::new(crate::edit::services::fake::FakeEngineFactory::default()),
                library
                    .layout()
                    .staging_dir()
                    .join(crate::edit::ENGINE_WORK_DIR),
            ))
        });
        let mut handlers: Vec<(JobKind, Arc<dyn JobHandler>)> = vec![(
            JobKind::Export,
            Arc::new(ExportHandler::new(services)) as Arc<dyn JobHandler>,
        )];
        if engine {
            handlers.push((
                JobKind::Project,
                Arc::new(crate::project::scan::ProjectHandler) as Arc<dyn JobHandler>,
            ));
        }
        let jobs = JobManager::new(library.clone(), handlers, sink).unwrap();
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
        self.run_with(dest, entries, ExportOptions::default())
    }

    fn run_with(
        &self,
        dest: &std::path::Path,
        entries: Vec<i64>,
        options: ExportOptions,
    ) -> (JobState, ExportResult) {
        let id = start_export(
            &self.jobs,
            &ExportInput {
                dest: dest.to_path_buf(),
                entries,
                options,
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
                options: ExportOptions::default(),
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
                unit: None,
                items: vec![b, a]
            },
            Group {
                folder: Some("u".into()),
                unit: Some("u".into()),
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

// ---- M15 段 3：名前の規則・置き換え・形式の変換・プロジェクト ----

fn rule(
    template: Option<&str>,
    style: crate::export::names::NameStyle,
    ascii: bool,
) -> ExportOptions {
    ExportOptions {
        name: crate::export::names::NameRule {
            template: template.map(str::to_string),
            style,
            ascii,
        },
        manifest: false,
        ..ExportOptions::default()
    }
}

#[test]
fn name_rules_shape_file_and_folder_names() {
    use crate::export::names::NameStyle;
    let env = Env::new();
    let a = env.item("Hero Idle.png", "png", b"a");
    let b = env.item("夕方の空.png", "png", b"b");
    let c = env.item("Jump Up.png", "png", b"c");
    let unit = env
        .library
        .db()
        .write(move |tx| crate::unit::create_set(tx, "Boss Fight", &[c], 1))
        .unwrap();
    let unit_entry = i64::from(LIST_FLAG | u32::try_from(unit).unwrap());
    let dest = env.dest("snake");
    let (state, result) = env.run_with(
        &dest,
        vec![a, b, unit_entry],
        rule(None, NameStyle::Snake, true),
    );
    assert_eq!(state, JobState::Done);
    assert_eq!((result.exported, result.manifest.as_deref()), (3, None));
    // ASCII だけにして空になった名前は item-ID。ユニットのフォルダにも書き方を当てる
    assert_eq!(
        names_in(&dest),
        ["boss_fight", "hero_idle.png", &format!("item_{b}.png")]
    );
    assert_eq!(names_in(&dest.join("boss_fight")), ["jump_up.png"]);
    // テンプレート：番号は書き出す順、{ユニット} はユニットの名前
    let dest = env.dest("template");
    let (_, result) = env.run_with(
        &dest,
        vec![a, unit_entry],
        rule(Some("{ユニット}{番号}-{名前}"), NameStyle::Kebab, false),
    );
    assert_eq!(result.exported, 2);
    assert_eq!(names_in(&dest), ["001-hero-idle.png", "boss-fight"]);
    assert_eq!(
        names_in(&dest.join("boss-fight")),
        ["boss-fight002-jump-up.png"]
    );
    // 知らない語のテンプレートは積む前に断る
    assert!(
        start_export(
            &env.jobs,
            &ExportInput {
                dest: dest.clone(),
                entries: vec![a],
                options: rule(Some("{色}"), NameStyle::Keep, false),
            },
        )
        .is_err()
    );
}

#[test]
fn files_kura_wrote_for_the_same_item_are_replaced_and_others_kept() {
    let env = Env::new();
    let a = env.item("hero.png", "png", b"a");
    let b = env.item("hero.png", "png", b"b");
    let dest = env.dest("out");
    let options = rule(None, crate::export::names::NameStyle::Keep, false);
    let (_, first) = env.run_with(&dest, vec![a], options.clone());
    assert_eq!((first.exported, first.replaced), (1, 0));
    // 同じアイテムをもう一度：同じ中身なので書かずに同じ名前（「hero 2」にしない）
    let (_, again) = env.run_with(&dest, vec![a], options.clone());
    assert_eq!((again.exported, again.replaced), (1, 1));
    assert_eq!(names_in(&dest), ["hero.png"]);
    // ほかのアイテムの同じ名前は壊さず番号を付ける
    let (_, other) = env.run_with(&dest, vec![b], options.clone());
    assert_eq!(other.replaced, 0);
    assert_eq!(names_in(&dest), ["hero 2.png", "hero.png"]);
    assert_eq!(std::fs::read(dest.join("hero.png")).unwrap(), b"a");
    // 利用者が書き換えたファイルは Kura が書いたものでないので残す
    std::fs::write(dest.join("hero.png"), b"edited by hand").unwrap();
    let (_, kept) = env.run_with(&dest, vec![a], options.clone());
    assert_eq!(kept.replaced, 0);
    assert_eq!(
        std::fs::read(dest.join("hero.png")).unwrap(),
        b"edited by hand"
    );
    assert_eq!(std::fs::read(dest.join("hero 3.png")).unwrap(), b"a");
    // アイテムの過去の中身（持ち出した出力）なら置き換える
    let old = *blake3::hash(b"old export").as_bytes();
    env.library
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO item_outputs (hash, item_id, source_hash, ext, size_bytes, created_at)
                 VALUES (?1, ?2, x'00', 'png', 10, 1)",
                params![old.to_vec(), a],
            )?;
            Ok(())
        })
        .unwrap();
    let fresh = env.dest("fresh");
    std::fs::write(fresh.join("hero.png"), b"old export").unwrap();
    let (_, swapped) = env.run_with(&fresh, vec![a], options);
    assert_eq!(swapped.replaced, 1);
    assert_eq!(names_in(&fresh), ["hero.png"]);
    assert_eq!(std::fs::read(fresh.join("hero.png")).unwrap(), b"a");
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
fn converted_exports_are_rendered_recorded_and_matched_in_projects() {
    use crate::export::formats::FormatTarget;
    let env = Env::with_engine();
    let a = env.item("hero.png", "png", &sprite_png());
    let game = env.dest("game");
    std::fs::write(game.join("project.godot"), b"").unwrap();
    std::fs::create_dir_all(game.join("art")).unwrap();
    let project = {
        let existing = env.library.db().read(crate::project::list).unwrap();
        let (root, engine) =
            crate::project::prepare_root(&game, env.library.layout().root(), &existing, None)
                .unwrap();
        env.library
            .db()
            .write(move |tx| crate::project::add(tx, &root, engine, None, 1))
            .unwrap()
    };
    let mut options =
        crate::export::ExportPreset::for_engine(crate::project::Engine::Godot).options(project);
    options.image = FormatTarget::To {
        format: "webp".into(),
    };
    let (state, result) = env.run_with(&game.join("art"), vec![a], options.clone());
    assert_eq!(state, JobState::Done);
    assert_eq!(
        (result.exported, result.converted, result.manifest),
        (1, 1, None)
    );
    let out = std::fs::read(game.join("art/hero.webp")).unwrap();
    assert_eq!(&out[..4], b"RIFF");
    assert_eq!(&out[8..12], b"WEBP");
    // 形式を変えた出力の中身を記録し、プロジェクトの照らし直しが「持ち出した出力」として付ける
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
    assert!(env.jobs.wait_idle(WAIT));
    let used = env
        .library
        .db()
        .read(move |c| crate::project::used_items(c, project))
        .unwrap();
    assert_eq!(used, vec![a]);
    // 同じ設定でもう一度書き出すと同じバイト列で、書かずに置き換えたことにする
    let (_, again) = env.run_with(&game.join("art"), vec![a], options);
    assert_eq!((again.converted, again.replaced), (1, 1));
    assert_eq!(std::fs::read(game.join("art/hero.webp")).unwrap(), out);
    assert_eq!(names_in(&game.join("art")), ["hero.webp"]);
    // 変えない（書けない形式だけ変える）：PNG は書けるのでそのまま写す
    let mut keep = rule(None, crate::export::names::NameStyle::Keep, false);
    keep.image = FormatTarget::Playable;
    let plain = env.dest("plain");
    let (_, copied) = env.run_with(&plain, vec![a], keep);
    assert_eq!(copied.converted, 0);
    assert_eq!(std::fs::read(plain.join("hero.png")).unwrap(), sprite_png());
}

#[test]
fn converting_without_an_engine_fails_that_item() {
    use crate::export::formats::FormatTarget;
    let env = Env::new();
    let a = env.item("hero.png", "png", b"a");
    let b = env.item("photo.webp", "webp", b"b");
    let mut options = rule(None, crate::export::names::NameStyle::Keep, false);
    options.image = FormatTarget::To {
        format: "webp".into(),
    };
    let dest = env.dest("out");
    let (state, result) = env.run_with(&dest, vec![a, b], options);
    assert_eq!(state, JobState::Done);
    // 形式を変える 1 件は描けずに失敗し、同じ形式の 1 件はそのまま写す
    assert_eq!((result.exported, result.failed), (1, 1));
    assert_eq!(names_in(&dest), ["photo.webp"]);
}
