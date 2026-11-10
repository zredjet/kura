//! 保存の手順を 1 件ずつ動かす口（M14 段 2。`save::Saver`・`UnitCx`）の試験：ジョブの文脈なしで準備・コミットでき、結果の
//! 記録はコミットと同じトランザクションで書かれ（記録が失敗すれば版も残らない）、書きかけの印で並べて準備した 1 件ずつが
//! 別のファイルになり、順にコミットすると土台の変わった 2 件目は Conflict になる。版に戻すの使い直し・作り直しも同じ口で動く。
//! 保存のジョブの振る舞い（S1〜S24）は save_tests・save_stream_tests が見る。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kura_engine::{Digest, Graph};
use serde_json::{Value as Json, json};

use super::save::{Committed, Flow, Mode, Record, Reverted, Saver, UnitCx};
use super::save_tests::{
    Env, assert_consistent, assert_done, base_of, doc, flip, gc_version, head, render_png, staged,
    versions_of,
};
use crate::CoreError;
use crate::import::{MediaKind, hash_file};
use crate::jobs::Control;
use crate::library::{Library, hex};

/// ジョブのない 1 件の場（印・進みを覚える）。
struct TestUnit<'a> {
    lib: &'a Library,
    job: i64,
    tag: &'static str,
    progress: Mutex<Vec<f64>>,
}

impl<'a> TestUnit<'a> {
    fn new(lib: &'a Library, job: i64, tag: &'static str) -> Self {
        Self {
            lib,
            job,
            tag,
            progress: Mutex::new(Vec::new()),
        }
    }
}

impl UnitCx for TestUnit<'_> {
    fn library(&self) -> &Library {
        self.lib
    }

    fn control(&self) -> Control {
        Control::Continue
    }

    fn progress(&self, progress: f64, _message: &str) {
        self.progress.lock().unwrap().push(progress);
    }

    fn log(&self, _line: &str) {}

    fn mark(&self, _result_json: &str, _progress: f64, _message: &str) -> crate::Result<()> {
        Ok(())
    }

    fn job(&self) -> i64 {
        self.job
    }

    fn tag(&self) -> String {
        self.tag.to_string()
    }
}

/// 文書を DB に置き、根のダイジェストを返す（保存の口が積む前に置くのと同じ）。
fn put(lib: &Library, g: &Graph) -> Digest {
    let parts = kura_engine::doc_parts::split(g).unwrap();
    let root = parts.root;
    lib.db()
        .write(move |tx| super::store::put_doc(tx, &parts, crate::time::now_ms()))
        .unwrap();
    root
}

/// 結果を settings の `key` に書く記録。
fn record_to(key: &'static str) -> Record {
    Arc::new(move |tx, outcome| {
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, outcome.to_string()],
        )?;
        Ok(())
    })
}

fn setting(lib: &Library, key: &str) -> Option<Json> {
    lib.db()
        .read(|c| {
            use rusqlite::OptionalExtension;
            Ok(
                c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?,
            )
        })
        .unwrap()
        .map(|s| serde_json::from_str(&s).unwrap())
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut n: Vec<String> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    n.sort();
    n
}

/// 上書きの準備（段 0〜5）が止まらずに書き終える。
fn prepared(saver: &Saver, unit: &TestUnit<'_>, item: i64, g: &Graph) -> super::save::Written {
    let root = put(unit.lib, g);
    match saver
        .prepare(
            unit,
            Mode::Overwrite,
            item,
            base_of(unit.lib, item),
            root,
            None,
        )
        .unwrap()
    {
        Flow::Go(w) => w,
        Flow::Stop(end) => panic!("止まりました：{end:?}"),
    }
}

/// ジョブの文脈なしで 1 件を上書きできる：版の `job_id` と台帳の予告の持ち主は場のジョブ、書きかけは場の印の名前、結果は
/// 記録（コミットと同じトランザクション）に書かれ、後のジョブは積まない（呼び手が積む）。
#[test]
fn a_unit_saves_without_a_job_and_records_in_the_same_transaction() {
    let env = Env::new();
    let (a, h0, png) = env.png(1);
    let g = doc(&[flip("vertical")], None);
    let saver = Saver::new(env.engine.clone());
    let unit = TestUnit::new(&env.lib, 9001, "9001-3");
    let written = prepared(&saver, &unit, a, &g);
    assert_eq!(names(&staged(&env.lib)), ["edit-9001-3-out.png.part"]);
    let done: Committed = saver
        .commit(&unit, written, record_to("test.unit"))
        .unwrap();
    let want = hash_of_png(&png, &g);
    assert_ne!(want, h0);
    assert_eq!(done.saved, Some(MediaKind::Image));
    assert_eq!(done.message, "保存しました");
    assert_eq!(done.outcome["outcome"], "saved");
    assert_eq!(done.outcome["version"], 1);
    assert_eq!(done.outcome["hash"], hex(&want));
    assert_eq!(setting(&env.lib, "test.unit"), Some(done.outcome.clone()));
    let vs = versions_of(&env.lib, a);
    assert_eq!(vs.len(), 2);
    assert_eq!((vs[1].job_id, vs[1].output_hash), (Some(9001), want));
    assert_eq!(head(&env.lib, a).hash, want);
    assert_eq!(
        env.count("SELECT count(*) FROM jobs WHERE kind <> 'import'"),
        0,
        "後のジョブは呼び手が積む"
    );
    // 進みは準備とコミットの順に増える
    let p = unit.progress.lock().unwrap().clone();
    assert!(p.windows(2).all(|w| w[0] <= w[1]), "{p:?}");
    assert_eq!(p.last(), Some(&0.9));
    assert_consistent(&env.lib);
}

/// 記録が失敗すれば、トランザクション 2 ごと戻る：版も、アイテムの置き換えも、台帳の予告も、置いたファイルも残らない
/// （一括のジョブのアイテムの行と版を「ちょうど 1 回」にそろえる約束）。
#[test]
fn a_failing_record_leaves_no_version() {
    let env = Env::new();
    let (a, h0, _) = env.png(2);
    let saver = Saver::new(env.engine.clone());
    let unit = TestUnit::new(&env.lib, 9002, "9002");
    let written = prepared(&saver, &unit, a, &doc(&[flip("vertical")], None));
    let failing: Record = Arc::new(|_, _| Err(CoreError::Invalid("記録できません".into())));
    let err = saver.commit(&unit, written, failing).unwrap_err();
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    assert!(versions_of(&env.lib, a).is_empty());
    let h = head(&env.lib, a);
    assert_eq!((h.hash, h.version), (h0, 0));
    assert_eq!(env.count("SELECT count(*) FROM blobs"), 1, "元だけ");
    assert_consistent(&env.lib);
}

/// 同じジョブで並べて準備した 2 件は、印で別の書きかけになる（同時に残る）。順にコミットすると、土台の変わった 2 件目は
/// Conflict で、その書きかけも片付く。
#[test]
fn units_prepared_side_by_side_commit_in_turn() {
    let env = Env::new();
    let (a, _, png) = env.png(3);
    let saver = Saver::new(env.engine.clone());
    let one = TestUnit::new(&env.lib, 9003, "9003-1");
    let two = TestUnit::new(&env.lib, 9003, "9003-2");
    let g1 = doc(&[flip("vertical")], None);
    let w1 = prepared(&saver, &one, a, &g1);
    let w2 = prepared(&saver, &two, a, &doc(&[flip("horizontal")], None));
    assert_eq!(
        names(&staged(&env.lib)),
        ["edit-9003-1-out.png.part", "edit-9003-2-out.png.part"]
    );
    saver.commit(&one, w1, record_to("test.one")).unwrap();
    let err = saver.commit(&two, w2, record_to("test.two")).unwrap_err();
    assert!(matches!(err, CoreError::Conflict(_)), "{err:?}");
    assert_eq!(head(&env.lib, a).hash, hash_of_png(&png, &g1));
    assert_eq!(setting(&env.lib, "test.two"), None);
    assert_consistent(&env.lib);
}

/// 版に戻すも同じ口で動く：版 k のファイルがあれば使い直してすぐコミットし（描かない）、なければ作り直しの計画を返して、
/// 準備 → コミットで戻す。
#[test]
fn revert_reuses_or_rebuilds_through_the_unit() {
    let env = Env::new();
    let (a, h0, _) = env.png(4);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let saver = Saver::new(env.engine.clone());
    let unit = TestUnit::new(&env.lib, 9004, "9004");
    // 使い直す（元はいつも hold）
    let Reverted::Committed(done) = saver
        .revert_reuse(&unit, a, base_of(&env.lib, a), 0, &record_to("test.reuse"))
        .unwrap()
    else {
        panic!("元のファイルがあるのに作り直そうとしました");
    };
    assert_eq!(done.saved, Some(MediaKind::Image));
    assert_eq!(
        done.outcome["revert"],
        json!({ "to": 0, "reused": true, "rebuilt": false })
    );
    assert_eq!(setting(&env.lib, "test.reuse"), Some(done.outcome.clone()));
    assert_eq!(head(&env.lib, a).hash, h0);
    // 作り直す：版 1 のファイルを GC で消す
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    let v1 = versions_of(&env.lib, a)[1].clone();
    gc_version(&env, a, 1);
    let path1 = env.lib.layout().content_path(&v1.output_hash, "png");
    assert!(!path1.exists());
    // 版の `job_id` は 1 ジョブ 1 版なので、別のジョブの場で
    let unit = TestUnit::new(&env.lib, 9005, "9005");
    let Reverted::Rebuild(plan) = saver
        .revert_reuse(&unit, a, base_of(&env.lib, a), 1, &record_to("test.unused"))
        .unwrap()
    else {
        panic!("版 1 のファイルがないのに使い直しました");
    };
    assert_eq!(setting(&env.lib, "test.unused"), None);
    let written = match saver.prepare_rebuild(&unit, *plan).unwrap() {
        Flow::Go(w) => w,
        Flow::Stop(end) => panic!("止まりました：{end:?}"),
    };
    assert_eq!(names(&staged(&env.lib)), ["edit-9005-out.png.part"]);
    let done = saver
        .commit(&unit, written, record_to("test.rebuild"))
        .unwrap();
    assert_eq!(
        done.outcome["revert"],
        json!({ "to": 1, "reused": false, "rebuilt": true })
    );
    assert_eq!(hash_file(&path1).unwrap(), v1.output_hash);
    let last = versions_of(&env.lib, a).pop().unwrap();
    assert_eq!(
        (last.output_hash, last.job_id),
        (v1.output_hash, Some(9005))
    );
    assert_consistent(&env.lib);
}

fn hash_of_png(png: &[u8], g: &Graph) -> [u8; 32] {
    *blake3::hash(&render_png(png, g)).as_bytes()
}
