//! GC とゴミ箱を空にする処理の受け入れテスト（docs/editing.md §14.1 の S10〜S13・S16、7 日とバックアップの条件の境界、文書の
//! 部品の片付け、一時停止・再開・中止）。保存のテストの道具（`save_tests` の Env・`assert_consistent`）で、本当の保存のあとに
//! 本物の GC のジョブを動かす（時計とバックアップの一覧はテストが決める）。Linux でも回る。どのテストも `assert_consistent` で
//! 締める。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kura_engine::codec::{CodecEnv, CodecSet, Decoder, ReadSeek, Tier};
use kura_engine::doc::DecodeOptions;
use kura_engine::doc_parts::split;
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{EngineError, Graph, Limits, Monitor, Shape, Value};
use rusqlite::params;
use serde_json::{Value as Json, json};

use super::gc::{self, GcFault, GcHook, GcStage};
use super::save::SaveAction;
use super::save_tests::{
    DAY, Env, Setup, WAIT, add_item, assert_consistent, assert_done, base_of, count, doc, files,
    flip, hash_of, head, heic_codecs, heic_decoder, inputs_of, reopen_after_crash, result_of,
    sprite, states, versions_of,
};
use super::services::fake::FakeEngineFactory;
use super::versions::FileState;
use super::{ENGINE_WORK_DIR, EditHandler, EngineServices, drafts, start_save, store};
use crate::import::{ImportInput, ImportOptions, ImportResult, start_import};
use crate::jobs::{JobKind, JobManager, JobRecord, JobState};
use crate::library::{Library, ThumbSize, check_files, hex, ledger};
use crate::time::now_ms;

/// ジョブ `id` が終わるまで待つ（ほかのジョブが割り込みで止まっていても待てるよう、そのジョブだけを見る）。
fn wait_job(env: &Env, id: i64) -> JobRecord {
    let until = Instant::now() + WAIT;
    loop {
        let record = env.jobs.get(id).unwrap();
        if record.state.is_terminal() || record.state == JobState::Paused {
            return record;
        }
        assert!(Instant::now() < until, "ジョブ {id} が終わりません");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 2 回上書きしたアイテム（版 0 hold・版 1 waiting・版 2 hold）。版 1 の中身を返す。
fn edited_twice(env: &Env, seed: u64) -> (i64, [u8; 32]) {
    let (a, _, _) = env.png(seed);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let h1 = head(&env.lib, a).hash;
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    assert_eq!(
        states(&env.lib, a),
        vec![
            (0, FileState::Hold),
            (1, FileState::Waiting),
            (2, FileState::Hold)
        ]
    );
    (a, h1)
}

/// 派生のファイル（小さいサムネイル）を置く。
fn put_thumb(lib: &Library, hash: &[u8; 32]) -> std::path::PathBuf {
    let path = lib.layout().thumb_path(&hex(hash), ThumbSize::Small);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"t").unwrap();
    path
}

fn blob_exists(lib: &Library, hash: [u8; 32]) -> bool {
    lib.db()
        .read(|c| ledger::id_of(c, &hash))
        .unwrap()
        .is_some()
}

fn replaced_at(lib: &Library, item: i64, version: i64) -> i64 {
    versions_of(lib, item)
        .into_iter()
        .find(|v| v.version == version)
        .and_then(|v| v.replaced_at)
        .unwrap()
}

// ---- S10：7 日を過ぎた版の GC ----

/// S10：7 日を過ぎた（バックアップのない）消し待ちの版は、照合が一致すれば台帳の行・ファイル・派生のファイルが消え、版の
/// 記録（文書・値のダイジェスト・出力のハッシュ・書き出しの記録）は残る。7 日に満たないうちは消さない。
#[test]
fn s10_gc_after_seven_days_removes_the_file_and_keeps_the_record() {
    let env = Env::new();
    let (a, h1) = edited_twice(&env, 41);
    let thumb = put_thumb(&env.lib, &h1);
    let v1 = versions_of(&env.lib, a)[1].clone();
    let path1 = env.lib.layout().content_path(&h1, "png");
    // 7 日に 1 分足りない
    let record = env.gc_at(Some(v1.replaced_at.unwrap() + 7 * DAY - 60_000));
    assert_done(&record);
    assert_eq!(result_of(&record)["released"], 0);
    assert_eq!(states(&env.lib, a)[1], (1, FileState::Waiting));
    assert!(path1.exists());
    // 8 日たった
    let result = env.gc();
    assert_eq!(
        (&result["released"], &result["kept"], &result["files"]),
        (&json!(1), &json!(0), &json!(1)),
        "{result}"
    );
    assert!(!path1.exists());
    assert!(!thumb.exists(), "派生のファイルも消す");
    assert!(!blob_exists(&env.lib, h1));
    let after = versions_of(&env.lib, a)[1].clone();
    assert_eq!(after.file_state, FileState::Released);
    assert_eq!(after.file_blob_id, None);
    assert_eq!(
        (
            after.output_hash,
            after.value_digest,
            after.doc_id,
            &after.record
        ),
        (v1.output_hash, v1.value_digest, v1.doc_id, &v1.record),
        "版の記録は残る"
    );
    assert_eq!(env.count("SELECT count(*) FROM ledger_pending"), 0);
    // 消したあとでも、版 1 に戻せば文書から作り直せる
    let record = env.revert(a, 1);
    assert_done(&record);
    assert_eq!(result_of(&record)["revert"]["rebuilt"], true);
    assert_eq!(head(&env.lib, a).hash, h1);
    assert_consistent(&env.lib);
}

/// 7 日とバックアップの条件の境界：ちょうど 7 日は消し、1 ms 足りなければ残す。置き換えより前に作ったバックアップ（同じ秒・
/// 同じ時刻を含む）が残っていれば消さず、残っているいちばん古いバックアップが置き換えより後なら消す。
#[test]
fn gc_waits_seven_days_and_for_backups_made_before_the_replacement() {
    let env = Env::new();
    let (a, _) = edited_twice(&env, 42);
    assert_done(&env.overwrite(a, doc(&[flip("vertical"), flip("horizontal")], None)));
    // 版 1（置き換え T）と版 2（置き換え T + 10 秒）が消し待ち
    let t = replaced_at(&env.lib, a, 1);
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE item_versions SET replaced_at = ?2 WHERE item_id = ?1 AND version = 2",
                params![a, t + 10_000],
            )?;
            Ok(())
        })
        .unwrap();
    let state = |v: usize| states(&env.lib, a)[v].1;
    assert_done(&env.gc_at(Some(t + 7 * DAY - 1)));
    assert_eq!(
        (state(1), state(2)),
        (FileState::Waiting, FileState::Waiting)
    );
    assert_done(&env.gc_at(Some(t + 7 * DAY)));
    assert_eq!(
        (state(1), state(2)),
        (FileState::Released, FileState::Waiting),
        "ちょうど 7 日"
    );
    let later = Some(t + 30 * DAY);
    for (backups, kept) in [
        (vec![t + 5_000, t + 40 * DAY], true), // 置き換えより前のバックアップが残っている
        (vec![t + 10_000], true), // 同じ時刻（名前は秒の単位なので、前に作ったものとみなす）
        (vec![t + 10_001, t + 20 * DAY], false), // いちばん古いものが置き換えより後
    ] {
        env.clock.set_backups(&backups);
        assert_done(&env.gc_at(later));
        assert_eq!(
            state(2) == FileState::Waiting,
            kept,
            "{backups:?} {:?}",
            states(&env.lib, a)
        );
    }
    assert_consistent(&env.lib);
}

// ---- S11：照合が合わない ----

/// 同じ識別子・段で、別の絵を読むデコーダ（デコーダが変わって元を読んだ値が記録と違う場合を写す）。
struct OtherPixels(Arc<dyn Decoder>);

impl Decoder for OtherPixels {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn tier(&self) -> Tier {
        self.0.tier()
    }
    fn reproducible(&self) -> bool {
        self.0.reproducible()
    }
    fn env(&self) -> Option<CodecEnv> {
        self.0.env()
    }
    fn can_decode(&self, format: &str) -> bool {
        self.0.can_decode(format)
    }
    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        self.0.shape(src, format, opts, limits)
    }
    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        self.0.decode(src, format, opts, limits, mon)?;
        InputGen::PixelSprite {
            w: 24,
            h: 20,
            fmt: PixelFormat::Rgba8,
            seed: 999,
        }
        .generate()
    }
}

/// S11：照合が合わない（元を読む PNG のデコーダが、同じ識別子のまま別の値を読む）版は消さずに kept で残し、ファイルと台帳の
/// 行も残す。記録のエンコーダがこの環境にない版は照らせないので、消し待ちのまま次回へ回す（kept にしない）。
#[test]
fn s11_gc_keeps_a_version_whose_rebuild_does_not_match() {
    let set = Arc::new(Mutex::new(CodecSet::pinned()));
    let env = Env::with(FakeEngineFactory::with_codecs(set.clone()), None);
    let (a, h1) = edited_twice(&env, 43);
    let (b, hb) = edited_twice(&env, 44);
    // B の版 1 は、この組にないエンコーダで書いたことにする
    let rb = versions_of(&env.lib, b)[1].record.clone();
    let mut record: Json = serde_json::from_str(&rb).unwrap();
    record["encoded"]["codec"] = json!("gone-png-enc/9");
    let text = record.to_string();
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE item_versions SET record = ?1 WHERE item_id = ?2 AND version = 1",
                params![text, b],
            )?;
            Ok(())
        })
        .unwrap();
    // 元を読む PNG のデコーダを、別の値を読むものに差し替える（組を作り直させる）
    {
        let mut codecs = set.lock().unwrap();
        let png = codecs.decoders_for("png")[0].clone();
        codecs.replace_decoder(Arc::new(OtherPixels(png))).unwrap();
    }
    env.engine.reload_codecs();
    let result = env.gc();
    assert_eq!(
        (&result["released"], &result["kept"], &result["deferred"]),
        (&json!(0), &json!(1), &json!(1)),
        "{result}"
    );
    assert_eq!(states(&env.lib, a)[1], (1, FileState::Kept));
    assert!(env.lib.layout().content_path(&h1, "png").exists());
    assert!(blob_exists(&env.lib, h1));
    assert_eq!(states(&env.lib, b)[1], (1, FileState::Waiting), "次回へ");
    assert!(env.lib.layout().content_path(&hb, "png").exists());
    // kept はもう候補にならない。照らせない版は次の GC でも照らせなければ消し待ちのまま
    let result = env.gc();
    assert_eq!(
        (&result["kept"], &result["deferred"]),
        (&json!(0), &json!(1)),
        "{result}"
    );
    // kept のファイルは版に戻すときに使い直せる
    let record = env.revert(a, 1);
    assert_done(&record);
    assert_eq!(result_of(&record)["revert"]["reused"], true);
    assert_eq!(head(&env.lib, a).hash, h1);
    assert_eq!(states(&env.lib, a)[1], (1, FileState::Kept));
    assert_consistent(&env.lib);
}

// ---- S12：作り直している間に同じ中身を使う ----

/// 照合が一致したあと・門を取る前に一度だけ止まる GC の割り込み。止まったら `reached` に知らせ、`go` を待つ。
fn pause_after_verify() -> (GcHook, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let armed = AtomicBool::new(true);
    let (reached, go) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    let hook: GcHook = Arc::new(move |stage| {
        if matches!(stage, GcStage::Verified { .. }) && armed.swap(false, Ordering::SeqCst) {
            reached.lock().unwrap().send(()).unwrap();
            let _ = go.lock().unwrap().recv();
        }
    });
    (hook, reached_rx, go_tx)
}

/// S12：GC が版 1 を作り直して照らしている間に、(a) 版 1 に戻す（猶予中のファイルを使い直す）、(b) 別のアイテムの上書きが版 1
/// と同じ中身を出す、と台帳の行が共有になるので、GC は門の中で確かめ直して released にするだけで、ファイルは消さない。
/// (c) 同じ中身を取り込むと、過去の版の重複になり（アイテムは増えない。S14）、台帳の行を参照しないので、照合したとおり消す
/// （孤立したファイル・ファイルのない行は残らない）。
#[test]
fn s12_contents_used_while_gc_rebuilds_are_not_removed() {
    for case in ["revert", "other-save", "import"] {
        let (hook, reached, go) = pause_after_verify();
        let env = Env::setup(
            Setup {
                gc_hook: Some(hook),
                ..Setup::default()
            },
            |_| None,
        );
        let (a, h1) = edited_twice(&env, 45);
        let original = std::fs::read(
            env.lib
                .layout()
                .content_path(&versions_of(&env.lib, a)[0].output_hash, "png"),
        )
        .unwrap();
        let v1_bytes = std::fs::read(env.lib.layout().content_path(&h1, "png")).unwrap();
        // 版 1 だけを候補にする（いちばん古いバックアップを版 1 の置き換えの直後に置く。割り込みの保存の版は候補にならない）
        env.clock.set_backups(&[replaced_at(&env.lib, a, 1) + 1]);
        // B：A の元と同じ中身の別のアイテム（反転すると A の版 1 と同じ中身になる）
        let b = (case == "other-save").then(|| add_item(&env.lib, &original, "png", "B").0);
        env.clock.set(Some(now_ms() + 8 * DAY));
        let gc_job = env.jobs.enqueue(JobKind::Gc, "{}".into()).unwrap();
        reached.recv_timeout(WAIT).unwrap();
        match case {
            "revert" => {
                let id = env.start(a, base_of(&env.lib, a), SaveAction::Revert { to: 1 });
                let record = wait_job(&env, id);
                assert_done(&record);
                assert_eq!(result_of(&record)["revert"]["reused"], true);
            }
            "other-save" => {
                let b = b.unwrap();
                let id = env.start(
                    b,
                    base_of(&env.lib, b),
                    SaveAction::Overwrite {
                        graph: doc(&[flip("vertical")], None),
                    },
                );
                assert_done(&wait_job(&env, id));
                assert_eq!(head(&env.lib, b).hash, h1, "A の版 1 と同じ中身");
            }
            _ => {
                let src = env.dir.path().join("src");
                std::fs::create_dir_all(&src).unwrap();
                std::fs::write(src.join("past.png"), &v1_bytes).unwrap();
                let id = start_import(
                    &env.jobs,
                    &ImportInput {
                        paths: vec![src],
                        options: ImportOptions::default(),
                    },
                )
                .unwrap();
                let record = wait_job(&env, id);
                assert_done(&record);
                let r: ImportResult =
                    serde_json::from_str(record.result_json.as_deref().unwrap()).unwrap();
                assert_eq!((r.added, r.duplicates), (0, 1), "{case}");
            }
        }
        go.send(()).unwrap();
        let record = wait_job(&env, gc_job);
        assert_done(&record);
        env.clock.set(None);
        assert!(env.jobs.wait_idle(WAIT));
        let result = result_of(&record);
        let used = case != "import";
        let want = if used { (1, 0) } else { (0, 1) };
        assert_eq!(
            (
                result["shared"].as_u64().unwrap(),
                result["released"].as_u64().unwrap()
            ),
            want,
            "{case} {result}"
        );
        assert_eq!(states(&env.lib, a)[1], (1, FileState::Released), "{case}");
        assert_eq!(
            env.lib.layout().content_path(&h1, "png").exists(),
            used,
            "{case}"
        );
        assert_eq!(blob_exists(&env.lib, h1), used, "{case}");
        let items = env.count("SELECT count(*) FROM items");
        assert_eq!(items, if case == "other-save" { 2 } else { 1 }, "{case}");
        assert_consistent(&env.lib);
    }
}

// ---- S13：台帳の行を解放したあと、ファイルを消す前に落ちる ----

/// S13：GC が台帳の行を解放したあと・ファイルを消す前に落ちても、開き直すと予告の照合がファイルと派生のファイルを消し、孤立した
/// ファイルは 0（ジョブはカーソルの次から続けて終わる）。予告のあと・解放の前に落ちたときは、予告だけが消えてファイルと版は
/// 消し待ちのまま残る。
#[test]
fn s13_crash_after_releasing_the_ledger_row_leaves_no_orphan() {
    for at in [GcFault::AfterAnnounce, GcFault::AfterRelease] {
        let env = Env::setup(
            Setup {
                gc_fault: Some(at),
                ..Setup::default()
            },
            |_| None,
        );
        let (a, h1) = edited_twice(&env, 46);
        let thumb = put_thumb(&env.lib, &h1);
        let record = env.gc_at(None);
        assert_eq!(record.state, JobState::Failed, "{at:?}");
        let released = at == GcFault::AfterRelease;
        assert_eq!(env.count("SELECT count(*) FROM ledger_pending"), 1);
        assert_eq!(
            check_files(env.lib.db(), env.lib.layout()).unwrap(),
            (0, u64::from(released)),
            "{at:?}：解放したあとなら DB にないファイルが 1 つ"
        );
        assert_eq!(blob_exists(&env.lib, h1), !released);
        let job = record.id;
        let (dir, session) = reopen_after_crash(env, job, FakeEngineFactory::default());
        assert_done(&session.jobs().get(job).unwrap());
        let lib = session.library();
        assert_eq!(
            states(lib, a)[1].1,
            if released {
                FileState::Released
            } else {
                FileState::Waiting
            },
            "{at:?}"
        );
        assert_eq!(lib.layout().content_path(&h1, "png").exists(), !released);
        assert_eq!(thumb.exists(), !released, "{at:?}");
        assert_consistent(lib);
        drop(session);
        drop(dir);
    }
}

// ---- S16：編集したアイテムでゴミ箱を空にする ----

/// S16：編集したアイテム（偽の HEIC：元・アンカー・猶予中の版・今・下書き）をゴミ箱に入れて空にすると、元・猶予中のファイル・
/// アンカー（行とファイル）・今・文書の部品がすぐ消え（照合しない。決定 6）、参照されない台帳の行・予告は 0。別名で保存した
/// B が A の元とアンカーを共有していれば、B が残る限りそれらは残る。途中（台帳の行を解放したあと・ファイルを消す前）で
/// 落ちても、開き直すと孤立したファイルは 0。
#[test]
fn s16_emptying_the_trash_of_edited_items() {
    for (with_b, crash) in [(false, false), (true, false), (false, true)] {
        let case = format!("with_b={with_b} crash={crash}");
        let decoder = heic_decoder(false);
        let env = Env::with(FakeEngineFactory::with_codecs(heic_codecs(&decoder)), None);
        let (a, heic) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
        assert_done(&env.overwrite(a, doc(&[flip("vertical")], Some("png"))));
        let h1 = head(&env.lib, a).hash;
        assert_done(&env.overwrite(a, doc(&[flip("horizontal")], Some("png"))));
        let h2 = head(&env.lib, a).hash;
        let anchor: i64 = env.count("SELECT id FROM edit_anchors");
        let anchor_file: (Vec<u8>, String) = env
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
            .content_path(&anchor_file.0.clone().try_into().unwrap(), &anchor_file.1);
        let a_docs: Vec<i64> = versions_of(&env.lib, a)
            .iter()
            .filter_map(|v| v.doc_id)
            .collect();
        // 下書き（版に保存していない文書）も置く
        let draft = doc(&[flip("vertical"), flip("vertical")], Some("png"));
        drafts::save(env.lib.db(), a, &draft, base_of(&env.lib, a), None).unwrap();
        let draft_root = split(&draft).unwrap().root;
        let b = with_b.then(|| {
            let record = env.save_as(
                a,
                doc(&[flip("vertical"), flip("horizontal")], Some("png")),
                "別名",
                "alt",
            );
            assert_done(&record);
            let b = result_of(&record)["itemId"].as_i64().unwrap();
            assert_eq!(
                versions_of(&env.lib, b)[0].output_hash,
                heic,
                "B の版 0 は A の元"
            );
            assert_eq!(
                inputs_of(&env.lib, versions_of(&env.lib, b)[1].id)[0].anchor_id,
                Some(anchor),
                "B もアンカーを使う"
            );
            b
        });
        for hash in [h1, h2] {
            put_thumb(&env.lib, &hash);
        }
        env.lib
            .db()
            .write(move |tx| crate::items::trash(tx, &[a], 5).map(|_| ()))
            .unwrap();
        let (lib, _guard) = if crash {
            assert!(env.lib.empty_trash_crashing_after_release().is_err());
            assert!(env.count("SELECT count(*) FROM ledger_pending") > 0);
            let (_, orphans) = check_files(env.lib.db(), env.lib.layout()).unwrap();
            assert!(orphans > 0, "{case}");
            // 開き直す（予告の照合が、台帳から外したファイルを消す）
            let (dir, session) = reopen_after_crash_without_job(env);
            (session.library().clone(), Some((dir, session)))
        } else {
            let emptied = env.lib.empty_trash().unwrap();
            assert_eq!((emptied.items, emptied.failed), (1, 0), "{case}");
            (env.lib.clone(), None)
        };
        let exists = |hash: &[u8; 32], ext: &str| lib.layout().content_path(hash, ext).exists();
        assert!(!exists(&h1, "png") && !exists(&h2, "png"), "{case}");
        assert!(
            !lib.layout()
                .thumb_path(&hex(&h1), ThumbSize::Small)
                .exists(),
            "{case}"
        );
        assert_eq!(
            exists(&heic, "heic"),
            with_b,
            "{case}：元は B が持つ間だけ残る"
        );
        assert_eq!(anchor_path.exists(), with_b, "{case}：アンカーも");
        assert_eq!(
            count(&lib, "SELECT count(*) FROM edit_anchors"),
            i64::from(with_b),
            "{case}"
        );
        // A の版・下書きの文書の部品は、ほかから辿れなければ消える
        let gone = |id: i64| {
            count(
                &lib,
                &format!("SELECT count(*) FROM edit_objects WHERE id = {id}"),
            ) == 0
        };
        assert!(a_docs.iter().all(|d| gone(*d)), "{case}");
        assert!(
            lib.db()
                .read(|c| store::doc_id_by_digest(c, &draft_root))
                .unwrap()
                .is_none(),
            "{case}"
        );
        if let Some(b) = b {
            assert_eq!(head(&lib, b).ext, "png");
            assert_eq!(
                states(&lib, b),
                vec![(0, FileState::Hold), (1, FileState::Hold)]
            );
        } else {
            assert_eq!(
                count(&lib, "SELECT count(*) FROM edit_objects"),
                0,
                "{case}"
            );
            assert_eq!(count(&lib, "SELECT count(*) FROM blobs"), 0, "{case}");
        }
        assert_eq!(lib.db().read(ledger::unreferenced).unwrap(), 0, "{case}");
        assert_consistent(&lib);
    }
}

/// 強制終了したライブラリを開き直す（動いていたジョブはない。予告の照合・整合性チェックのあと）。
fn reopen_after_crash_without_job(env: Env) -> (tempfile::TempDir, crate::library::LibrarySession) {
    let Env { dir, lib, jobs, .. } = env;
    drop(jobs);
    let root = lib.layout().root().to_path_buf();
    lib.close();
    drop(lib);
    let session = crate::library::LibrarySession::open(
        &root,
        Arc::new(|_| {}),
        crate::library::Services::new(Arc::new(crate::media::fake::FakeBackend))
            .with_engine(Arc::new(FakeEngineFactory::default())),
    )
    .unwrap();
    session.wait_ready();
    let report = session.integrity().unwrap();
    assert_eq!((report.orphan_files, report.missing_files), (0, 0));
    assert!(session.jobs().wait_idle(WAIT));
    (dir, session)
}

// ---- 文書の部品の片付け ----

/// 編集のジョブの行を直接入れる（動かない状態で。文書は部品で置く）。根のダイジェストを返す。
fn insert_edit_job(lib: &Library, item: i64, graph: &Graph, state: &str, finished_at: Option<i64>) {
    let parts = split(graph).unwrap();
    let input = json!({
        "v": 1, "itemId": item,
        "base": { "hash": "00".repeat(32), "version": 0 },
        "action": { "type": "overwrite", "doc": parts.root },
    })
    .to_string();
    let state = state.to_string();
    lib.db()
        .write(move |tx| {
            store::put_doc(tx, &parts, 1)?;
            tx.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at, finished_at) VALUES ('edit', ?1, ?2, 1, ?3)",
                params![state, input, finished_at],
            )?;
            Ok(())
        })
        .unwrap();
}

fn has_doc(lib: &Library, g: &Graph) -> bool {
    let root = split(g).unwrap().root;
    lib.db()
        .read(|c| store::doc_id_by_digest(c, &root))
        .unwrap()
        .is_some()
}

/// 部品の片付け：置き換えた下書きの文書（部品の印が付く）と、7 日より前に中止した保存の文書は消し、版・今の下書き・終わって
/// いない保存（一時停止）・最近失敗した保存（やり直せる）の文書は残す。終わっていない保存があるうちは印を残し、なくなれば
/// 消す。消し待ちの版がなくても、印があれば GC は動く。
#[test]
fn gc_sweeps_orphaned_parts_and_keeps_roots_of_unfinished_or_recent_saves() {
    let env = Env::new();
    let (a, _, _) = env.png(51);
    let saved = doc(&[flip("vertical")], None);
    assert_done(&env.overwrite(a, saved.clone()));
    let base = base_of(&env.lib, a);
    let (old_draft, draft) = (
        doc(
            &[flip("horizontal"), flip("vertical"), flip("horizontal")],
            None,
        ),
        doc(&[flip("horizontal")], None),
    );
    drafts::save(env.lib.db(), a, &old_draft, base, None).unwrap();
    assert!(!env.lib.db().read(gc::parts_marked).unwrap());
    drafts::save(env.lib.db(), a, &draft, base, None).unwrap();
    assert!(
        env.lib.db().read(gc::parts_marked).unwrap(),
        "置き換えた下書き"
    );
    let now = now_ms();
    let paused = doc(&[flip("vertical"), flip("vertical")], None);
    let old_cancel = doc(&[flip("horizontal"), flip("horizontal")], None);
    let recent_fail = doc(
        &[flip("vertical"), flip("horizontal"), flip("vertical")],
        None,
    );
    insert_edit_job(&env.lib, a, &paused, "paused", None);
    insert_edit_job(&env.lib, a, &old_cancel, "canceled", Some(now - 8 * DAY));
    insert_edit_job(&env.lib, a, &recent_fail, "failed", Some(now - DAY));
    let files_before = files(&env.lib);
    let record = env.gc_at(Some(now));
    assert_done(&record);
    assert!(result_of(&record)["parts"].as_u64().unwrap() > 0);
    for (g, kept, what) in [
        (&saved, true, "版の文書"),
        (&draft, true, "今の下書き"),
        (&old_draft, false, "置き換えた下書き"),
        (&paused, true, "一時停止した保存"),
        (&old_cancel, false, "7 日より前に中止した保存"),
        (&recent_fail, true, "最近失敗した保存（やり直せる）"),
    ] {
        assert_eq!(has_doc(&env.lib, g), kept, "{what}");
    }
    assert_eq!(files(&env.lib), files_before, "ファイルは変えない");
    assert!(
        env.lib.db().read(gc::parts_marked).unwrap(),
        "一時停止した保存があるうちは印を残す"
    );
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "UPDATE jobs SET state = 'canceled', finished_at = ?1 WHERE state = 'paused'",
                [now],
            )?;
            Ok(())
        })
        .unwrap();
    // 中止したばかりの保存の文書は根として残すので、7 日以内に失敗・中止した保存があるうちは印も残す（消すと、7 日たって根から
    // 外れた部品を片付ける GC が積まれない）
    assert_done(&env.gc_at(Some(now)));
    assert!(env.lib.db().read(gc::parts_marked).unwrap());
    assert!(
        has_doc(&env.lib, &paused),
        "中止したばかりの保存の文書は残す"
    );
    // 8 日たつと、中止・失敗した保存の文書も根から外れて片付き、印も消える。版の文書・今の下書きは残る
    assert_done(&env.gc_at(Some(now + 8 * DAY)));
    assert!(!env.lib.db().read(gc::parts_marked).unwrap());
    for (g, kept, what) in [
        (&saved, true, "版の文書"),
        (&draft, true, "今の下書き"),
        (&paused, false, "8 日前に中止した保存"),
        (&recent_fail, false, "9 日前に失敗した保存"),
    ] {
        assert_eq!(has_doc(&env.lib, g), kept, "{what}");
    }
    assert_consistent(&env.lib);
}

/// 失敗した保存の部品の印は、7 日の間に GC が動いても消さず（その文書はやり直せるよう根として残す。ちょうど 7 日まで）、7 日を
/// 過ぎると候補として GC が積まれ、部品を片付けて印を消す（印だけ先に消えて、根から外れた部品が孤立したまま残ることがない）。
#[test]
fn the_parts_mark_outlives_failed_saves_until_their_roots_expire() {
    let env = Env::new();
    let (a, _, _) = env.png(57);
    let base = base_of(&env.lib, a);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    let stale = doc(&[flip("horizontal"), flip("horizontal")], None);
    let record = env.save(
        a,
        base,
        SaveAction::Overwrite {
            graph: stale.clone(),
        },
    );
    assert_eq!(record.state, JobState::Failed);
    let failed_at = record.finished_at.unwrap();
    let marked = || env.lib.db().read(gc::parts_marked).unwrap();
    assert!(marked());
    // 1 時間後（バックアップの 1 時間ごとの確かめ）・ちょうど 7 日後に GC が動いても、印と文書は残る
    for at in [failed_at + 3_600_000, failed_at + 7 * DAY] {
        assert_done(&env.gc_at(Some(at)));
        assert!(marked(), "{at}");
        assert!(has_doc(&env.lib, &stale), "{at}");
    }
    // 8 日後：印があるので候補になり、積む口が GC を積んで、部品を片付けて印を消す
    let later = failed_at + 8 * DAY;
    env.clock.set(Some(later));
    let clock = env.clock.gc();
    assert!(
        env.lib
            .db()
            .read(|c| gc::due(c, clock.cutoff(env.lib.layout())?))
            .unwrap()
    );
    assert!(
        gc::queue_if_due(&env.lib, &clock, || env
            .jobs
            .ensure_queued(JobKind::Gc, "{}"))
        .unwrap()
    );
    assert!(env.jobs.wait_idle(WAIT));
    env.clock.set(None);
    assert!(!has_doc(&env.lib, &stale));
    assert!(!marked());
    assert!(
        !env.lib
            .db()
            .read(|c| gc::due(c, gc::cutoff(later, &[])))
            .unwrap()
    );
    assert_consistent(&env.lib);
}

/// 待ち・一時停止のまま中止した上書き・別名の保存（ハンドラを通らない）も、積む前に置いた文書の部品の印を置く（GC が片付ける）。
/// 版に戻すは文書を持たないので置かない。
#[test]
fn canceling_an_idle_save_marks_its_parts() {
    let dir = tempfile::tempdir().unwrap();
    let lib = Arc::new(Library::create(&dir.path().join("L")).unwrap());
    let engine = Arc::new(EngineServices::new(
        Arc::new(FakeEngineFactory::default()),
        lib.layout().staging_dir().join(ENGINE_WORK_DIR),
    ));
    // ジョブは動かさない（start しない）ので、積んだ保存は待ちのまま
    let jobs = JobManager::new(
        lib.clone(),
        vec![(JobKind::Edit, Arc::new(EditHandler::new(engine)))],
        Arc::new(|_| {}),
    )
    .unwrap();
    let (a, hash) = add_item(&lib, &sprite(58), "png", "絵");
    let marked = || lib.db().read(gc::parts_marked).unwrap();
    let clear = || {
        lib.db()
            .write(|tx| {
                tx.execute("DELETE FROM settings WHERE key = 'gc.parts'", [])?;
                Ok(())
            })
            .unwrap();
    };
    let base = super::save::Base { hash, version: 0 };
    // 版に戻す（文書なし）：印を置かない
    let revert = json!({ "v": 1, "itemId": a, "base": { "hash": hex(&hash), "version": 1 },
                         "action": { "type": "revert", "to": 0 } })
    .to_string();
    let id = jobs.enqueue(JobKind::Edit, revert).unwrap();
    jobs.cancel(id).unwrap();
    assert_eq!(jobs.get(id).unwrap().state, JobState::Canceled);
    assert!(!marked());
    // 上書き（待ちから）・別名（一時停止から）：印を置く
    for save_as in [false, true] {
        clear();
        let graph = doc(
            &[flip(if save_as { "vertical" } else { "horizontal" })],
            None,
        );
        let action = if save_as {
            SaveAction::SaveAs {
                graph: graph.clone(),
                title: "写し".into(),
                stem: "copy".into(),
            }
        } else {
            SaveAction::Overwrite {
                graph: graph.clone(),
            }
        };
        let id = start_save(
            lib.db(),
            &jobs,
            &super::save::SaveRequest {
                item_id: a,
                base,
                action,
            },
        )
        .unwrap();
        assert!(has_doc(&lib, &graph));
        if save_as {
            jobs.pause(id).unwrap();
            assert_eq!(jobs.get(id).unwrap().state, JobState::Paused);
        }
        assert!(!marked());
        jobs.cancel(id).unwrap();
        assert_eq!(jobs.get(id).unwrap().state, JobState::Canceled);
        assert!(marked(), "save_as={save_as}");
    }
    drop(jobs);
    lib.close();
}

/// 保存のジョブが版を足さずに終わる（Conflict）と部品の印が付き、次の保存のコミットのあとに GC が積まれて片付ける。
#[test]
fn a_save_that_ends_without_a_version_marks_its_parts_for_gc() {
    let env = Env::new();
    let (a, _, _) = env.png(52);
    let base = base_of(&env.lib, a);
    assert_done(&env.overwrite(a, doc(&[flip("vertical")], None)));
    // 古い土台の保存は Conflict（積む前に置いた文書は孤立する）
    let stale = doc(&[flip("horizontal"), flip("horizontal")], None);
    let record = env.save(
        a,
        base,
        SaveAction::Overwrite {
            graph: stale.clone(),
        },
    );
    assert_eq!(record.state, JobState::Failed);
    assert!(env.lib.db().read(gc::parts_marked).unwrap());
    assert!(
        has_doc(&env.lib, &stale),
        "失敗したばかりの保存の文書はやり直せるよう残す"
    );
    // 8 日たってから次の保存をコミットすると、GC が積まれて片付ける
    env.clock.set(Some(now_ms() + 8 * DAY));
    assert_done(&env.overwrite(a, doc(&[flip("horizontal")], None)));
    env.clock.set(None);
    assert!(!has_doc(&env.lib, &stale));
    assert!(!env.lib.db().read(gc::parts_marked).unwrap());
    let gc_jobs = env.count("SELECT count(*) FROM jobs WHERE kind = 'gc' AND state = 'done'");
    assert!(gc_jobs >= 1);
    assert_consistent(&env.lib);
}

// ---- 一時停止・再開・中止 ----

/// GC は候補ごとに一時停止を見て止まり、続けるとカーソルの次から片付ける（数は前回の分を引き継ぐ）。中止すると、それまでの
/// 数を結果に残す。消している途中の候補は消し終えてから止まる。
#[test]
fn gc_pauses_resumes_from_its_cursor_and_cancels() {
    for cancel in [false, true] {
        let (hook, reached, go) = pause_after_verify();
        let env = Env::setup(
            Setup {
                gc_hook: Some(hook),
                ..Setup::default()
            },
            |_| None,
        );
        let (a, _) = edited_twice(&env, 53);
        let (b, _) = edited_twice(&env, 54);
        env.clock.set(Some(now_ms() + 8 * DAY));
        let id = env.jobs.enqueue(JobKind::Gc, "{}".into()).unwrap();
        reached.recv_timeout(WAIT).unwrap();
        if cancel {
            env.jobs.cancel(id).unwrap();
        } else {
            env.jobs.pause(id).unwrap();
        }
        go.send(()).unwrap();
        let record = wait_job(&env, id);
        let state = |item| states(&env.lib, item)[1].1;
        // 最初の候補（A の版 1）は消し終え、B の版 1 は残る
        assert_eq!(
            (state(a), state(b)),
            (FileState::Released, FileState::Waiting)
        );
        if cancel {
            assert_eq!(record.state, JobState::Canceled);
            assert_eq!(result_of(&record)["released"], 1);
        } else {
            assert_eq!(record.state, JobState::Paused);
            let partial = result_of(&record);
            assert!(partial["cursor"].is_object(), "{partial}");
            env.jobs.resume(id).unwrap();
            let record = wait_job(&env, id);
            assert_done(&record);
            assert_eq!(result_of(&record)["released"], 2, "前回の分を引き継ぐ");
            assert_eq!(state(b), FileState::Released);
        }
        env.clock.set(None);
        assert!(env.jobs.wait_idle(WAIT));
        assert_consistent(&env.lib);
    }
}

/// 実ファイルの中身が同じ別のアイテムの今と共有している版（別のアイテムの上書きが過去の版と同じ中身を出した）も、照合して
/// から released にし（ADR-170 (2)）、ファイルは残す（今のアイテムが持つ）。
#[test]
fn gc_verifies_shared_versions_before_releasing_them() {
    let env = Env::new();
    let (a, h1) = edited_twice(&env, 55);
    let original = std::fs::read(
        env.lib
            .layout()
            .content_path(&versions_of(&env.lib, a)[0].output_hash, "png"),
    )
    .unwrap();
    let (b, _) = add_item(&env.lib, &original, "png", "B");
    assert_done(&env.overwrite(b, doc(&[flip("vertical")], None)));
    assert_eq!(head(&env.lib, b).hash, h1);
    assert_eq!(
        hash_of(&std::fs::read(env.lib.layout().content_path(&h1, "png")).unwrap()),
        h1
    );
    let result = env.gc();
    assert_eq!(
        (&result["shared"], &result["released"], &result["kept"]),
        (&json!(1), &json!(0), &json!(0)),
        "{result}"
    );
    assert_eq!(states(&env.lib, a)[1], (1, FileState::Released));
    assert!(env.lib.layout().content_path(&h1, "png").exists());
    assert!(blob_exists(&env.lib, h1));
    assert_consistent(&env.lib);
}

/// 共有相手が消えたあとも、照合していない版のファイルが消えることはない（ADR-135 の安全網 2・ADR-170 (2)）：A の版 1 と同じ中身を
/// B の上書きが出し（共有）、B をゴミ箱に入れて GC → ゴミ箱を空にする → A を版 1 に戻す。照合が一致すれば released にし（ファイルは
/// B が持つ）、B を消すとファイルも消えるが、照合済みなので文書から作り直せる。デコーダがずれていれば照合が合わずに kept で
/// 残り（台帳の行を参照し続ける）、B を消してもファイルは残って版 1 に戻せる（使い直す）。
#[test]
fn shared_versions_survive_their_partner_unless_verified() {
    for drift in [false, true] {
        let set = Arc::new(Mutex::new(CodecSet::pinned()));
        let env = Env::with(FakeEngineFactory::with_codecs(set.clone()), None);
        let (a, h1) = edited_twice(&env, 56);
        let original = std::fs::read(
            env.lib
                .layout()
                .content_path(&versions_of(&env.lib, a)[0].output_hash, "png"),
        )
        .unwrap();
        let (b, _) = add_item(&env.lib, &original, "png", "B");
        assert_done(&env.overwrite(b, doc(&[flip("vertical")], None)));
        assert_eq!(head(&env.lib, b).hash, h1);
        if drift {
            let mut codecs = set.lock().unwrap();
            let png = codecs.decoders_for("png")[0].clone();
            codecs.replace_decoder(Arc::new(OtherPixels(png))).unwrap();
        }
        env.engine.reload_codecs();
        env.lib
            .db()
            .write(move |tx| crate::items::trash(tx, &[b], 5).map(|_| ()))
            .unwrap();
        let result = env.gc();
        let want = if drift { (0, 1) } else { (1, 0) };
        assert_eq!(
            (
                result["shared"].as_u64().unwrap(),
                result["kept"].as_u64().unwrap()
            ),
            want,
            "drift={drift} {result}"
        );
        assert_eq!(result["released"], 0, "{result}");
        let state = if drift {
            FileState::Kept
        } else {
            FileState::Released
        };
        assert_eq!(states(&env.lib, a)[1], (1, state));
        assert!(
            env.lib.layout().content_path(&h1, "png").exists(),
            "ゴミ箱の B が持つ"
        );
        let emptied = env.lib.empty_trash().unwrap();
        assert_eq!((emptied.items, emptied.failed), (1, 0));
        assert_eq!(
            env.lib.layout().content_path(&h1, "png").exists(),
            drift,
            "照合が合わずに残した版は台帳の行を参照し続ける"
        );
        assert_eq!(blob_exists(&env.lib, h1), drift);
        let record = env.revert(a, 1);
        assert_done(&record);
        let how = &result_of(&record)["revert"];
        assert_eq!(
            (&how["reused"], &how["rebuilt"]),
            (&json!(drift), &json!(!drift)),
            "drift={drift} {how}"
        );
        assert_eq!(head(&env.lib, a).hash, h1);
        assert_consistent(&env.lib);
    }
}

// ---- OS のデコーダ ----

/// GC は OS のデコーダを呼ばない（ADR-170 (3)）：偽の HEIC（Platform の段）で 2 回上書きした版 1 は、アンカーが壊れている・ない
/// と元を OS のデコーダでしか読めないので、照らさずに消し待ちのまま次回へ回す（kept にしない。デコーダは呼ばない）。ファイルと
/// 台帳の行は残り、期限を過ぎた消し待ち（整合性の overdueVersions）に数える。
#[test]
fn gc_never_calls_the_os_decoder() {
    for missing in [false, true] {
        let decoder = heic_decoder(false);
        let env = Env::with(FakeEngineFactory::with_codecs(heic_codecs(&decoder)), None);
        let (a, _) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
        assert_done(&env.overwrite(a, doc(&[flip("vertical")], Some("png"))));
        let h1 = head(&env.lib, a).hash;
        assert_done(&env.overwrite(a, doc(&[flip("horizontal")], Some("png"))));
        assert_eq!(states(&env.lib, a)[1], (1, FileState::Waiting));
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
        let anchor_bytes = std::fs::read(&anchor_path).unwrap();
        if missing {
            std::fs::remove_file(&anchor_path).unwrap();
        } else {
            std::fs::write(&anchor_path, b"broken anchor").unwrap();
        }
        let reads = decoder.reads();
        let later = now_ms() + 8 * DAY;
        let record = env.gc_at(Some(later));
        assert_done(&record);
        let result = result_of(&record);
        assert_eq!(
            (
                &result["deferred"],
                &result["kept"],
                &result["released"],
                &result["shared"]
            ),
            (&json!(1), &json!(0), &json!(0), &json!(0)),
            "missing={missing} {result}"
        );
        assert_eq!(decoder.reads(), reads, "GC は OS のデコーダを呼ばない");
        assert_eq!(states(&env.lib, a)[1], (1, FileState::Waiting));
        assert!(env.lib.layout().content_path(&h1, "png").exists());
        assert!(blob_exists(&env.lib, h1));
        assert_eq!(
            env.lib
                .db()
                .read(|c| gc::overdue(c, gc::cutoff(later, &[])))
                .unwrap(),
            1,
            "期限を過ぎた消し待ちに数える"
        );
        std::fs::write(&anchor_path, &anchor_bytes).unwrap();
        assert_consistent(&env.lib);
    }
}

// ---- アンカー ----

/// テストの補助：台帳の行 `blob` を持つアンカーの行を入れる（元 `source`。読み方の設定のダイジェストは中身にする）。
fn insert_anchor_on(lib: &Library, source: [u8; 32], blob: [u8; 32]) -> i64 {
    lib.db()
        .write(move |tx| {
            let id = ledger::id_of(tx, &blob)?.unwrap();
            Ok(tx.query_row(
                "INSERT INTO edit_anchors (source_hash, options_digest, options, decoded, value_digest, blob_id, created_at)
                 VALUES (?1, ?2, '{}', '{}', x'00', ?3, 1) RETURNING id",
                params![&source[..], &blob[..], id],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

/// GC のアンカーの段：どの版の入力からも参照されないアンカーの行・台帳の行・ファイル・派生のファイルを、'delete' を予告して
/// 片付ける。参照されたアンカーと、ほかのアイテムの今が台帳の行を持つアンカーのファイルは残す（行だけ消える）。予告のあと・
/// 台帳の行を解放したあと（ファイルを消す前）で落ちても、開き直すと食い違い 0 で、ジョブは続きから終わる。
#[test]
fn gc_sweeps_unreferenced_anchors() {
    for fault in [
        None,
        Some(GcFault::AnchorsAfterAnnounce),
        Some(GcFault::AnchorsAfterRelease),
    ] {
        let decoder = heic_decoder(false);
        let codecs = heic_codecs(&decoder);
        let env = Env::setup(
            Setup {
                factory: FakeEngineFactory::with_codecs(codecs.clone()),
                gc_fault: fault,
                ..Setup::default()
            },
            |_| None,
        );
        // 参照されたアンカー（偽の HEIC の最初の保存が作る）
        let (a, heic) = add_item(&env.lib, b"fake heic bytes", "heic", "写真");
        assert_done(&env.overwrite(a, doc(&[flip("vertical")], Some("png"))));
        let used: i64 = env.count("SELECT id FROM edit_anchors");
        // 参照されないアンカー：自分のファイル（台帳の行・派生のファイルつき）と、別のアイテム P の今と同じ台帳の行
        let free_bytes = b"unreferenced anchor bytes".to_vec();
        let free = hash_of(&free_bytes);
        let free_path = env.lib.layout().content_path(&free, "wav");
        std::fs::create_dir_all(free_path.parent().unwrap()).unwrap();
        std::fs::write(&free_path, &free_bytes).unwrap();
        let len = free_bytes.len() as u64;
        env.lib
            .db()
            .write(move |tx| ledger::register(tx, &free, "wav", len, 1).map(|_| ()))
            .unwrap();
        let thumb = put_thumb(&env.lib, &free);
        insert_anchor_on(&env.lib, heic, free);
        let (_, p_hash) = add_item(&env.lib, &sprite(59), "png", "P");
        insert_anchor_on(&env.lib, [9; 32], p_hash);
        assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 3);
        let record = env.gc_at(None);
        let case = format!("{fault:?}");
        let (_guard, lib) = match fault {
            None => {
                assert_done(&record);
                let result = result_of(&record);
                assert_eq!(
                    (&result["anchors"], &result["files"]),
                    (&json!(2), &json!(1)),
                    "{result}"
                );
                (None, env.lib.clone())
            }
            Some(at) => {
                assert_eq!(record.state, JobState::Failed, "{case}");
                assert_eq!(
                    env.count("SELECT count(*) FROM ledger_pending"),
                    2,
                    "{case}"
                );
                let released = at == GcFault::AnchorsAfterRelease;
                assert_eq!(
                    check_files(env.lib.db(), env.lib.layout()).unwrap(),
                    (0, u64::from(released)),
                    "{case}：解放したあとなら DB にないファイルが 1 つ"
                );
                let job = record.id;
                let (dir, session) =
                    reopen_after_crash(env, job, FakeEngineFactory::with_codecs(codecs.clone()));
                assert_done(&session.jobs().get(job).unwrap());
                let lib = session.library().clone();
                (Some((dir, session)), lib)
            }
        };
        assert_eq!(
            count(&lib, "SELECT id FROM edit_anchors"),
            used,
            "{case}：参照されたアンカーだけ残る"
        );
        assert_eq!(
            count(&lib, "SELECT count(*) FROM edit_anchors"),
            1,
            "{case}"
        );
        assert!(!free_path.exists(), "{case}");
        assert!(!thumb.exists(), "{case}：派生のファイルも消す");
        assert!(!blob_exists(&lib, free), "{case}");
        assert!(
            lib.layout().content_path(&p_hash, "png").exists() && blob_exists(&lib, p_hash),
            "{case}：P の今のファイルは残る"
        );
        assert_eq!(
            count(&lib, "SELECT count(*) FROM ledger_pending"),
            0,
            "{case}"
        );
        assert_consistent(&lib);
    }
}
