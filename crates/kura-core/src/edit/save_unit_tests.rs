//! 保存の手順を 1 件ずつ動かす口（M14 段 2。`save::Saver`・`UnitCx`）の試験：ジョブの文脈なしで準備・コミットでき、結果の
//! 記録はコミットと同じトランザクションで書かれ（記録が失敗すれば版も残らない）、書きかけの印で並べて準備した 1 件ずつが
//! 別のファイルになり、順にコミットすると土台の変わった 2 件目は Conflict になる。版に戻すの使い直し・作り直しも同じ口で動く。
//! 保存のジョブの振る舞い（S1〜S24）は save_tests・save_stream_tests が見る。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kura_engine::{Digest, Graph};
use serde_json::{Value as Json, json};

use super::save::{Committed, Flow, Mode, Record, Reverted, Saver, UnitCx};
use super::save_stream_tests::{add_audio, audio_doc, opus_codecs, opus_decoder};
use super::save_tests::{
    Env, add_item, assert_consistent, assert_done, base_of, doc, flip, gc_version, head,
    heic_codecs, heic_decoder, render_png, staged, versions_of,
};
use super::services::fake::FakeEngineFactory;
use crate::CoreError;
use crate::import::{MediaKind, hash_file};
use crate::jobs::Control;
use crate::library::{Library, hex};

/// ジョブのない 1 件の場（読む前後の印・進みを覚える）。
struct TestUnit<'a> {
    lib: &'a Library,
    job: i64,
    tag: &'static str,
    progress: Mutex<Vec<f64>>,
    marks: Mutex<Vec<Json>>,
}

impl<'a> TestUnit<'a> {
    fn new(lib: &'a Library, job: i64, tag: &'static str) -> Self {
        Self {
            lib,
            job,
            tag,
            progress: Mutex::new(Vec::new()),
            marks: Mutex::new(Vec::new()),
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

    fn mark(&self, result_json: &str, _progress: f64, _message: &str) -> crate::Result<()> {
        self.marks
            .lock()
            .unwrap()
            .push(serde_json::from_str(result_json).unwrap());
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
/// 記録（コミットと同じトランザクション）に書かれ、版を足したアイテムの種類を返す（後のジョブは、それを見て呼び手が積む。
/// `UnitCx` にジョブを積む口はない）。固定の段で読むので、読む前の印は置かない。
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
    assert!(unit.marks.lock().unwrap().is_empty());
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

/// 同じアイテムを並べて準備した 2 件は、印で別の書きかけになる（同時に残る）。順にコミットすると、土台の変わった 2 件目は
/// 予告の前の Conflict で、その書きかけも片付く（別のアイテムを同じ持ち主のジョブでコミットするのは、版の `job_id` が一意
/// なので段 3 で持ち主を決めてから。`UnitCx::job`）。
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
    // 版の `job_id` は 1 ジョブ 1 版なので、別のジョブの場で（印もジョブの ID と違うもので、作り直しの書きかけの名前を見る）
    let unit = TestUnit::new(&env.lib, 9005, "9005-r");
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
    assert_eq!(names(&staged(&env.lib)), ["edit-9005-r-out.png.part"]);
    let done = saver
        .commit(&unit, written, record_to("test.rebuild"))
        .unwrap();
    assert_eq!(done.saved, Some(MediaKind::Image));
    assert_eq!(
        done.outcome["revert"],
        json!({ "to": 1, "reused": false, "rebuilt": true })
    );
    assert_eq!(
        setting(&env.lib, "test.rebuild"),
        Some(done.outcome.clone())
    );
    assert_eq!(hash_file(&path1).unwrap(), v1.output_hash);
    let last = versions_of(&env.lib, a).pop().unwrap();
    assert_eq!(
        (last.output_hash, last.job_id),
        (v1.output_hash, Some(9005))
    );
    assert_consistent(&env.lib);
}

/// 書きかけの名前は、アンカーを作る道（偽の HEIC の最初の保存）でも、音声を流して書いて作業用のファイルをアンカーに昇格する
/// 道（偽の Opus）でも、場の印（ジョブの ID と違うもの）で付く。OS のデコーダを通るので、読む前後の印を置く。
#[test]
fn the_staging_tag_names_every_part() {
    let decoding = || {
        vec![
            json!({ "v": 1, "decoding": true }),
            json!({ "v": 1, "decoding": false }),
        ]
    };
    let parts = |lib: &Library| {
        names(&staged(lib))
            .into_iter()
            .filter(|n| n.ends_with(".part"))
            .collect::<Vec<_>>()
    };
    // 画像：アンカー（png）と出力
    let decoder = heic_decoder(false);
    let env = Env::with(FakeEngineFactory::with_codecs(heic_codecs(&decoder)), None);
    let (a, _) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
    let saver = Saver::new(env.engine.clone());
    let unit = TestUnit::new(&env.lib, 9006, "9006-h");
    let written = prepared(&saver, &unit, a, &doc(&[flip("vertical")], Some("png")));
    assert_eq!(
        parts(&env.lib),
        ["edit-9006-h-anchor.png.part", "edit-9006-h-out.png.part"]
    );
    assert_eq!(*unit.marks.lock().unwrap(), decoding());
    let done = saver
        .commit(&unit, written, record_to("test.heic"))
        .unwrap();
    assert_eq!(done.outcome["anchor"], "created");
    assert_consistent(&env.lib);
    // 音声：作業用のファイルを写したアンカー（wav）と、流して書いた出力
    let decoder = opus_decoder(2, 65_536 + 11);
    let env = Env::with(FakeEngineFactory::with_codecs(opus_codecs(&decoder)), None);
    let (b, _) = add_audio(&env.lib, b"fake opus bytes", "opus", "声");
    let saver = Saver::new(env.engine.clone());
    let unit = TestUnit::new(&env.lib, 9007, "9007-o");
    let written = prepared(&saver, &unit, b, &audio_doc(-2.0, Some("wav")));
    assert_eq!(
        parts(&env.lib),
        ["edit-9007-o-anchor.wav.part", "edit-9007-o-out.wav.part"]
    );
    assert_eq!(*unit.marks.lock().unwrap(), decoding());
    let done = saver
        .commit(&unit, written, record_to("test.opus"))
        .unwrap();
    assert_eq!(done.saved, Some(MediaKind::Audio));
    assert_eq!(done.outcome["anchor"], "created");
    assert_consistent(&env.lib);
}

fn hash_of_png(png: &[u8], g: &Graph) -> [u8; 32] {
    *blake3::hash(&render_png(png, g)).as_bytes()
}
