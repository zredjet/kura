//! 退役する読み手の移し替え（M16 段 5。`edit::retire`）の受け入れテスト。旧版の読み手を今の Ogg の読み手にした組で保存して
//! 旧識別子の版を作り、組を今のもの（旧版は旧版の読み手）・旧版を除いたものに差し替えて確かめる。旧版の読み手は、段 9 までは
//! symphonia の Ogg Vorbis（`ogg-vorbis-symphonia-dec/3`）、段 10 で外したあとは、今の Ogg Vorbis の読み手を別の識別子と
//! 退役の印で包んだ試験の読み手（`kura_engine::testing::RenamedDecoder`）。どのテストも終わりに `assert_consistent` で締める。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use kura_engine::codec::recorded::RecordedInput;
use kura_engine::codec::{CodecSet, Decoder};
use kura_engine::digest::canonical;
use kura_engine::doc::DecodeOptions;
use kura_engine::testgen::InputGen;
use kura_engine::testing::RenamedDecoder;
use kura_engine::{Digest, NoMonitor};
use rusqlite::params;
use serde_json::{Value as Json, json};

use super::anchors;
use super::retire::{self, DONE_KEY, RetireHook, RetireStage};
use super::save_stream_tests::{add_audio, audio_doc};
use super::save_tests::{
    Env, Setup, WAIT, assert_consistent, assert_done, gc_version, head, inputs_of, result_of,
    versions_of,
};
use super::services::fake::FakeEngineFactory;
use crate::jobs::{JobKind, JobRecord, JobState};
use crate::library::Library;
use crate::settings;

const LEGACY: &str = "ogg-vorbis-retiring-test-dec/1";
const CURRENT: &str = "ogg-vorbis-kura-dec/1";
/// 退役する読み手（試験の 3 つ。[`now_set`]）：Ogg Vorbis・m4a の AAC・ADTS の AAC を、それぞれ今の読み手で包んだもの。
const RETIRING: [&str; 3] = [
    LEGACY,
    "m4a-aac-retiring-test-dec/1",
    "aac-adts-retiring-test-dec/1",
];

/// 今の固定の組から、識別子 `of` の読み手を識別子 `id` で包んだ試験の旧版の読み手（`retiring` は退役の印）。
fn renamed(of: &str, id: &'static str, retiring: bool) -> Arc<dyn Decoder> {
    let inner = CodecSet::pinned().decoder_by_id(of).unwrap().clone();
    Arc::new(RenamedDecoder::new(inner, id, retiring))
}

/// 段 10 の前のアプリと同じ組：固定の組に、退役する読み手（[`RETIRING`]）を旧版として足したもの。
fn now_set() -> CodecSet {
    let mut set = CodecSet::pinned();
    for (of, id) in [
        (CURRENT, RETIRING[0]),
        ("m4a-aac-kura-dec/1", RETIRING[1]),
        ("aac-adts-kura-dec/1", RETIRING[2]),
    ] {
        set.add_legacy_decoder(renamed(of, id, true)).unwrap();
    }
    set
}

/// 8 kHz ステレオの雑音の Ogg Vorbis（自作の書き手の既定）。
fn ogg(seed: u64, frames: u64) -> Vec<u8> {
    let v = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames,
        seed,
    }
    .generate()
    .unwrap();
    CodecSet::pinned()
        .encode_bytes(&v, "ogg", &json!({}), &NoMonitor)
        .unwrap()
        .0
}

/// 固定の組（と試験の旧版の読み手）から識別子 `ids` のデコーダと WAV の書き手を取った組（旧版の読み手も今の版として足す）。
fn set_of(ids: &[&str]) -> CodecSet {
    let pinned = now_set();
    let mut set = CodecSet::default();
    for id in ids {
        set.add_decoder(pinned.decoder_by_id(id).unwrap().clone())
            .unwrap();
    }
    set.add_encoder(pinned.encoder_by_id("wav-kura-enc/1").unwrap().clone())
        .unwrap();
    set
}

/// 段 4 より前のアプリと同じ読み方：Ogg は旧版の読み手が今の読み手。
fn old_set() -> CodecSet {
    set_of(&[LEGACY, "wav-kura-dec/1", "anchor-kura-dec/1"])
}

/// 段 10 のあと：旧版の読み手を外した組。
fn removed_set() -> CodecSet {
    set_of(&[CURRENT, "wav-kura-dec/1", "anchor-kura-dec/1"])
}

/// Ogg の読み手を 1 つも持たない組（アンカーの中身の WAV だけを読める）。試験の旧版の読み手は今の読み手を包んだもので値が
/// 同じなので、外したあとの作り直しがアンカーから読んだこと（元を今の読み手で読み直していないこと）を、この組で確かめる。
fn anchor_only_set() -> CodecSet {
    set_of(&["wav-kura-dec/1", "anchor-kura-dec/1"])
}

/// 組を `set` に差し替える（次に使うときに作り直す）。
fn swap(env: &Env, shared: &Arc<Mutex<CodecSet>>, set: CodecSet) {
    *shared.lock().unwrap() = set;
    env.engine.reload_codecs();
}

/// 旧版の読み手の組で始める環境（`retire_hook` は移し替えの割り込み）。
fn env_with(retire_hook: Option<RetireHook>) -> (Env, Arc<Mutex<CodecSet>>) {
    let shared = Arc::new(Mutex::new(old_set()));
    let env = Env::setup(
        Setup {
            factory: FakeEngineFactory::with_codecs(shared.clone()),
            retire_hook,
            ..Setup::default()
        },
        |_| None,
    );
    (env, shared)
}

/// Ogg のアイテムを足して、旧版の読み手の組で `edits` 回上書きする（出力は WAV）。（アイテム, 元のハッシュ）。
fn legacy_item(env: &Env, seed: u64, edits: usize) -> (i64, [u8; 32]) {
    let (item, hash) = add_audio(&env.lib, &ogg(seed, 12_000), "ogg", &format!("音{seed}"));
    for k in 0..edits {
        assert_done(&env.overwrite(item, audio_doc(-1.0 - k as f64, Some("wav"))));
    }
    (item, hash)
}

/// 版 `v` の行の ID。
fn version_id(lib: &Library, item: i64, v: i64) -> i64 {
    versions_of(lib, item)
        .into_iter()
        .find(|x| x.version == v)
        .unwrap()
        .id
}

/// 版 `v` の入力の記録とアンカーの行。
fn input_of(lib: &Library, item: i64, v: i64) -> (RecordedInput, Option<i64>) {
    let inputs = inputs_of(lib, version_id(lib, item, v));
    assert_eq!(inputs.len(), 1);
    let rec = serde_json::from_str(&inputs[0].recorded).unwrap();
    (rec, inputs[0].anchor_id)
}

/// 移し替えのジョブを積み、動き終えるのを待つ。
fn run_retire(env: &Env) -> JobRecord {
    let id = env.jobs.enqueue(JobKind::Retire, "{}".into()).unwrap();
    assert!(env.jobs.wait_idle(WAIT));
    env.jobs.get(id).unwrap()
}

fn done_mark(lib: &Library) -> Option<Vec<String>> {
    lib.db().read(|c| settings::get(c, DONE_KEY)).unwrap()
}

/// 版の入力の記録を `f` で書き換える（版の行 `vid` の入力 0）。
fn rewrite_input(lib: &Library, vid: i64, f: impl FnOnce(&str) -> String + Send + 'static) {
    lib.db()
        .write(move |tx| {
            let recorded: String = tx.query_row(
                "SELECT recorded FROM item_version_inputs WHERE version_id = ?1 AND input = 0",
                [vid],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE item_version_inputs SET recorded = ?2 WHERE version_id = ?1 AND input = 0",
                params![vid, f(&recorded)],
            )?;
            Ok(())
        })
        .unwrap();
}

/// 旧版の読み手で読んだ版に、移し替えのジョブが 1 度だけアンカーを作り（同じ元の入力にまとめて結び付ける）、旧版の読み手を
/// 外した組でも、ファイルを手放した版に戻せる（アンカーから作り直して同じ中身）。移したアンカーは元 × 読み方で探しても
/// 見つからず、新しい保存は今の読み手で読む（利用者の決定）。2 回目は何もしない。
#[test]
fn legacy_versions_get_one_anchor_and_rebuild_after_the_reader_is_gone() {
    let (env, shared) = env_with(None);
    let (item, source) = legacy_item(&env, 1, 2);
    for v in [1, 2] {
        let (rec, anchor) = input_of(&env.lib, item, v);
        assert_eq!((rec.decoded.codec.as_str(), anchor), (LEGACY, None));
        assert!(rec.decoded.reproducible && rec.anchor.is_none());
    }
    // 旧版の読み手の組の GC が版 1 を照らして手放す（作り直しは記録の読み手で読む）
    gc_version(&env, item, 1);
    let v1 = versions_of(&env.lib, item)
        .into_iter()
        .find(|x| x.version == 1)
        .unwrap();
    let before: Vec<(RecordedInput, Option<i64>)> = [1, 2]
        .iter()
        .map(|v| input_of(&env.lib, item, *v))
        .collect();

    // 今の組（旧版は旧版の読み手）で移す
    swap(&env, &shared, now_set());
    let record = run_retire(&env);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(
        (&result["anchored"], &result["linked"], &result["failed"]),
        (&json!(1), &json!(2), &json!(0)),
        "{result}"
    );
    assert!(result.get("left").is_none(), "{result}");
    assert_eq!(
        done_mark(&env.lib),
        Some(RETIRING.map(String::from).to_vec())
    );
    let rows: Vec<(i64, Vec<u8>)> = env
        .lib
        .db()
        .read(|c| {
            let mut stmt = c.prepare("SELECT id, options_digest FROM edit_anchors")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    let key = anchors::retired_key(&DecodeOptions::default(), LEGACY).unwrap();
    assert_eq!(rows[0].1, key.to_vec(), "鍵は退役の鍵");
    let normal = anchors::options_digest(&DecodeOptions::default()).unwrap();
    assert!(
        env.lib
            .db()
            .read(|c| anchors::find(c, &source, &normal))
            .unwrap()
            .is_none(),
        "元 × 読み方では見つからない"
    );
    let anchor = env
        .lib
        .db()
        .read(|c| anchors::get(c, rows[0].0))
        .unwrap()
        .unwrap();
    assert_eq!(anchor.decoded, before[0].0.decoded);
    assert_eq!(anchor.ext, "wav");
    assert!(env.lib.layout().content_path(&anchor.hash, "wav").is_file());
    for (k, v) in [1, 2].iter().enumerate() {
        let (rec, anchor_id) = input_of(&env.lib, item, *v);
        assert_eq!(anchor_id, Some(anchor.id));
        assert_eq!(rec.anchor, Some(Digest(anchor.hash)));
        let mut was = before[k].0.clone();
        was.anchor = rec.anchor;
        assert_eq!(rec, was, "記録はアンカーのほかは同じ");
    }
    assert_consistent(&env.lib);
    // 2 回目：対象はもうない
    let again = result_of(&run_retire(&env));
    assert_eq!(
        (&again["anchored"], &again["linked"]),
        (&json!(0), &json!(0))
    );

    // 旧版の読み手を外した組：手放した版 1 に戻すとアンカーから作り直して同じ中身（Ogg の読み手のない組で、アンカーから
    // 読んだことを確かめる）
    swap(&env, &shared, anchor_only_set());
    assert!(env.engine.codecs().decoder_by_id(LEGACY).is_none());
    assert!(env.engine.codecs().decoders_for("ogg").is_empty());
    let reverted = env.revert(item, 1);
    assert_done(&reverted);
    assert_eq!(head(&env.lib, item).hash, v1.output_hash);
    let (rec, anchor_id) = input_of(&env.lib, item, 3);
    assert_eq!(
        (rec.decoded.codec.as_str(), anchor_id),
        (LEGACY, Some(anchor.id))
    );
    // GC の照合も移したアンカーから作り直す（版 2 は版 3 に置き換えられて消し待ち）
    gc_version(&env, item, 2);
    // 新しい保存は今の読み手で読む（移したアンカーを使わない）
    swap(&env, &shared, removed_set());
    assert_done(&env.overwrite(item, audio_doc(-7.0, Some("wav"))));
    let (rec, anchor_id) = input_of(&env.lib, item, 4);
    assert_eq!((rec.decoded.codec.as_str(), anchor_id), (CURRENT, None));
    assert!(rec.anchor.is_none());
    assert_consistent(&env.lib);
}

/// 移せないもの（記録が読めない・値が違う・封の記録だけ違う・元のファイルがない）は触らずに理由ごとに数え、ほかは移す。誤り
/// （読めない元）は失敗として数える。どちらが残っても印は置き（開くたびに読み直さない）、印を消す（`retired --again`）と次に
/// 開いたときにまた積む。JSON として壊れた記録は対象にしない（問い合わせを止めない）。
#[test]
fn left_and_failed_targets_are_counted_and_the_mark_is_set_anyway() {
    let (env, shared) = env_with(None);
    let (good, _) = legacy_item(&env, 11, 1);
    let (bad_record, _) = legacy_item(&env, 12, 1);
    let (mismatch, _) = legacy_item(&env, 13, 1);
    let (stamp, _) = legacy_item(&env, 16, 1);
    let (missing, missing_hash) = legacy_item(&env, 14, 1);
    let (broken, broken_hash) = legacy_item(&env, 15, 1);
    let (garbled, _) = legacy_item(&env, 17, 1);
    rewrite_input(&env.lib, version_id(&env.lib, bad_record, 1), |_| {
        format!(r#"{{"decoded":{{"codec":"{LEGACY}"}}}}"#)
    });
    rewrite_input(&env.lib, version_id(&env.lib, mismatch, 1), |r| {
        let mut rec: RecordedInput = serde_json::from_str(r).unwrap();
        rec.decoded.value_digest = Digest([9; 32]);
        String::from_utf8(canonical(&rec).unwrap()).unwrap()
    });
    // 値は同じで、封の読み取りの記録だけが違う（アンカーから読むときの照らしが記録の全体を比べる）
    rewrite_input(&env.lib, version_id(&env.lib, stamp, 1), |r| {
        let mut rec: RecordedInput = serde_json::from_str(r).unwrap();
        rec.decoded.tier = kura_engine::codec::Tier::Platform;
        String::from_utf8(canonical(&rec).unwrap()).unwrap()
    });
    let garbled_vid = version_id(&env.lib, garbled, 1);
    let garbled_was = Arc::new(Mutex::new(String::new()));
    {
        let was = garbled_was.clone();
        rewrite_input(&env.lib, garbled_vid, move |r| {
            *was.lock().unwrap() = r.to_string();
            "{not json".to_string()
        });
    }
    let layout = env.lib.layout().clone();
    let missing_path = layout.content_path(&missing_hash, "ogg");
    let missing_bytes = std::fs::read(&missing_path).unwrap();
    std::fs::remove_file(&missing_path).unwrap();
    let broken_path = layout.content_path(&broken_hash, "ogg");
    let broken_bytes = std::fs::read(&broken_path).unwrap();
    std::fs::write(&broken_path, b"OggS but not really").unwrap();

    swap(&env, &shared, now_set());
    let record = run_retire(&env);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(
        (&result["anchored"], &result["linked"], &result["failed"]),
        (&json!(1), &json!(1), &json!(1)),
        "{result}"
    );
    assert_eq!(
        result["left"],
        json!({ "badRecord": 1, "mismatch": 2, "missing": 1 }),
        "{result}"
    );
    assert!(input_of(&env.lib, good, 1).1.is_some());
    for item in [mismatch, stamp, missing, broken] {
        assert_eq!(input_of(&env.lib, item, 1).1, None, "{item}");
    }
    assert_eq!(
        done_mark(&env.lib),
        Some(RETIRING.map(String::from).to_vec()),
        "失敗・移せないものが残っても印を置く"
    );
    assert_eq!(
        retire::queue_on_open(env.lib.db(), &env.jobs, &RETIRING).unwrap(),
        None,
        "開くたびに読み直さない"
    );

    // 元を直して印を消すと（`retired --again`）、次に開いたときに移す
    std::fs::write(&broken_path, &broken_bytes).unwrap();
    std::fs::write(&missing_path, &missing_bytes).unwrap();
    retire::forget(env.lib.db()).unwrap();
    assert_eq!(done_mark(&env.lib), None);
    let id = retire::queue_on_open(env.lib.db(), &env.jobs, &RETIRING)
        .unwrap()
        .expect("また積む");
    assert!(env.jobs.wait_idle(WAIT));
    let result = result_of(&env.jobs.get(id).unwrap());
    assert_eq!(
        (&result["anchored"], &result["failed"]),
        (&json!(2), &json!(0)),
        "{result}"
    );
    assert_eq!(
        result["left"],
        json!({ "badRecord": 1, "mismatch": 2 }),
        "{result}"
    );
    assert_eq!(
        done_mark(&env.lib),
        Some(RETIRING.map(String::from).to_vec())
    );
    // 識別子の一覧が変われば（段を足した）また積む
    assert!(
        retire::queue_on_open(env.lib.db(), &env.jobs, &RETIRING[..1])
            .unwrap()
            .is_some()
    );
    assert!(env.jobs.wait_idle(WAIT));
    // 数え上げ（kura-devtools）は残りを識別子 × 版のファイルの状態ごとに数える（壊れた JSON は数えない）
    let left = env
        .lib
        .db()
        .read(|c| retire::remaining(c, &RETIRING))
        .unwrap();
    assert_eq!(left.total, 3);
    assert_eq!(left.by_codec[LEGACY]["hold"], 3, "{left:?}");
    assert_eq!(left.retired_anchors, 3);
    assert!(left.retired_bytes > 3 * 4 * 2 * 12_000, "{left:?}");
    assert_eq!(left.examples.len(), 3);
    // ライブラリのパスから読み取り専用で開いても同じ（最後の移し替えのジョブの結果も出す）
    let from_path = retire::remaining_in(env.lib.layout().root(), &RETIRING).unwrap();
    assert_eq!(
        (
            from_path.total,
            &from_path.by_codec,
            from_path.retired_anchors
        ),
        (3, &left.by_codec, 3)
    );
    let (state, job) = from_path.last_job.unwrap();
    assert_eq!(state, "done");
    assert_eq!(
        job.unwrap()["left"],
        json!({ "badRecord": 1, "mismatch": 2 })
    );
    assert!(retire::remaining_in(&env.dir.path().join("none"), &RETIRING).is_err());
    let was = garbled_was.lock().unwrap().clone();
    rewrite_input(&env.lib, garbled_vid, move |_| was);
    assert_consistent(&env.lib);
}

/// 元を共有する別のアイテム（同じ中身を取り込み直した）の入力は、先に作った移したアンカーに結び付ける（読み直さない）。読み取りの
/// 記録が違う入力は、同じ鍵の別の記録のアンカーとして数え、結び付けない。
#[test]
fn items_sharing_an_original_link_to_one_anchor() {
    let (env, shared) = env_with(None);
    let (a, source) = legacy_item(&env, 41, 1);
    let bytes = std::fs::read(env.lib.layout().content_path(&source, "ogg")).unwrap();
    // A の頭は WAV になったので、同じ中身を別のアイテムとして取り込める
    let (c, c_source) = add_audio(&env.lib, &bytes, "ogg", "C");
    assert_eq!(c_source, source);
    assert_done(&env.overwrite(c, audio_doc(-4.0, Some("wav"))));
    let (d, _) = add_audio(&env.lib, &bytes, "ogg", "D");
    assert_done(&env.overwrite(d, audio_doc(-5.0, Some("wav"))));
    rewrite_input(&env.lib, version_id(&env.lib, d, 1), |r| {
        let mut rec: RecordedInput = serde_json::from_str(r).unwrap();
        rec.decoded.value_digest = Digest([7; 32]);
        String::from_utf8(canonical(&rec).unwrap()).unwrap()
    });
    swap(&env, &shared, now_set());
    let result = result_of(&run_retire(&env));
    assert_eq!(
        (&result["anchored"], &result["linked"], &result["failed"]),
        (&json!(1), &json!(2), &json!(0)),
        "{result}"
    );
    assert_eq!(result["left"], json!({ "otherAnchor": 1 }), "{result}");
    let anchor = input_of(&env.lib, a, 1).1.unwrap();
    assert_eq!(input_of(&env.lib, c, 1).1, Some(anchor));
    assert_eq!(input_of(&env.lib, d, 1).1, None);
    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 1);
    assert_consistent(&env.lib);
}

/// 置いたあと・載せる前に落ちたら、アンカーの行はなく、置いたファイルと予告は開くときの照合（`settle_pending`）が片付ける。
/// 印を消してやり直すと移す。
#[test]
fn a_crash_after_placing_is_settled_on_open() {
    let shared = Arc::new(Mutex::new(old_set()));
    let env = Env::setup(
        Setup {
            factory: FakeEngineFactory::with_codecs(shared.clone()),
            retire_fault: Some(super::save::Fault::AfterPlace),
            ..Setup::default()
        },
        |_| None,
    );
    let (item, _) = legacy_item(&env, 51, 1);
    swap(&env, &shared, now_set());
    let result = result_of(&run_retire(&env));
    assert_eq!(
        (&result["anchored"], &result["failed"]),
        (&json!(0), &json!(1)),
        "{result}"
    );
    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 0);
    assert_eq!(env.count("SELECT count(*) FROM ledger_pending"), 1);
    let wavs = |lib: &Library| -> usize {
        super::save_tests::files(lib)
            .into_iter()
            .filter(|f| f.ends_with(".wav"))
            .count()
    };
    let placed = wavs(&env.lib);
    assert!(placed >= 2, "版の WAV と、置いたアンカー（{placed}）");
    // 開くときの照合
    assert_eq!(
        crate::library::ledger::settle_pending(env.lib.db(), env.lib.layout()).unwrap(),
        1
    );
    assert_eq!(env.count("SELECT count(*) FROM ledger_pending"), 0);
    assert_eq!(wavs(&env.lib), placed - 1, "置いたアンカーを消した");
    assert_consistent(&env.lib);
    retire::forget(env.lib.db()).unwrap();
    let result = result_of(&run_retire(&env));
    assert_eq!(result["anchored"], 1, "{result}");
    assert!(input_of(&env.lib, item, 1).1.is_some());
    assert_consistent(&env.lib);
}

/// ディスクの空きが足りなければジョブを失敗にし、印を置かない（次に開いたときにまた積む）。空きができれば移す。
#[test]
fn no_space_fails_the_job_and_leaves_the_mark_unset() {
    let (env, shared) = env_with(None);
    let (item, _) = legacy_item(&env, 61, 1);
    swap(&env, &shared, now_set());
    env.engine
        .work_pcm()
        .set_free_space(Some(Arc::new(|_: &std::path::Path| Some(0))));
    let record = run_retire(&env);
    assert_eq!(record.state, JobState::Failed, "{:?}", record.message);
    assert_eq!(done_mark(&env.lib), None);
    assert_eq!(input_of(&env.lib, item, 1).1, None);
    env.engine.work_pcm().set_free_space(None);
    assert!(
        retire::queue_on_open(env.lib.db(), &env.jobs, &RETIRING)
            .unwrap()
            .is_some()
    );
    assert!(env.jobs.wait_idle(WAIT));
    assert!(input_of(&env.lib, item, 1).1.is_some());
    assert!(done_mark(&env.lib).is_some());
    assert_consistent(&env.lib);
}

/// 版の入力がひとつもないライブラリは、積まずに印だけ置く。退役する識別子がなければ何もしない。
#[test]
fn an_empty_library_only_sets_the_mark() {
    let (env, _) = env_with(None);
    assert_eq!(
        retire::queue_on_open(env.lib.db(), &env.jobs, &[]).unwrap(),
        None
    );
    assert_eq!(done_mark(&env.lib), None);
    assert_eq!(
        retire::queue_on_open(env.lib.db(), &env.jobs, &RETIRING).unwrap(),
        None
    );
    assert_eq!(
        done_mark(&env.lib),
        Some(RETIRING.map(String::from).to_vec())
    );
    assert_eq!(
        env.count("SELECT count(*) FROM jobs WHERE kind = 'retire'"),
        0
    );
}

/// 1 件目のあと・2 件目の前で一度だけ止まる割り込み。止まったら `reached` に知らせ、`go` を待つ。
fn pause_before(second: Arc<Mutex<i64>>) -> (RetireHook, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let armed = AtomicBool::new(true);
    let (reached, go) = (Mutex::new(reached_tx), Mutex::new(go_rx));
    let hook: RetireHook = Arc::new(move |stage| {
        if stage == RetireStage::Target(*second.lock().unwrap())
            && armed.swap(false, Ordering::SeqCst)
        {
            reached.lock().unwrap().send(()).unwrap();
            let _ = go.lock().unwrap().recv();
        }
    });
    (hook, reached_rx, go_tx)
}

/// 1 件ごとに一時停止・中止を見て止まり、続けるとカーソルの次から移す（数は前回の分を引き継ぐ）。中止すると、それまでの数を
/// 結果に残し、移し終えた印を置かない。
#[test]
fn pauses_resume_from_the_cursor_and_cancels_keep_the_mark_unset() {
    for cancel in [false, true] {
        let second = Arc::new(Mutex::new(0));
        let (hook, reached, go) = pause_before(second.clone());
        let (env, shared) = env_with(Some(hook));
        let (a, _) = legacy_item(&env, 21, 1);
        let (b, _) = legacy_item(&env, 22, 1);
        *second.lock().unwrap() = version_id(&env.lib, b, 1);
        swap(&env, &shared, now_set());
        let id = env.jobs.enqueue(JobKind::Retire, "{}".into()).unwrap();
        reached.recv_timeout(WAIT).unwrap();
        if cancel {
            env.jobs.cancel(id).unwrap();
        } else {
            env.jobs.pause(id).unwrap();
        }
        go.send(()).unwrap();
        assert!(env.jobs.wait_idle(WAIT));
        let record = env.jobs.get(id).unwrap();
        assert!(input_of(&env.lib, a, 1).1.is_some());
        assert!(input_of(&env.lib, b, 1).1.is_none());
        if cancel {
            assert_eq!(record.state, JobState::Canceled);
            assert_eq!(result_of(&record)["anchored"], 1);
            assert_eq!(done_mark(&env.lib), None);
        } else {
            assert_eq!(record.state, JobState::Paused);
            let partial = result_of(&record);
            assert_eq!(
                partial["cursor"],
                json!([version_id(&env.lib, a, 1), 0]),
                "{partial}"
            );
            env.jobs.resume(id).unwrap();
            assert!(env.jobs.wait_idle(WAIT));
            let record = env.jobs.get(id).unwrap();
            assert_done(&record);
            assert_eq!(result_of(&record)["anchored"], 2, "前回の分を引き継ぐ");
            assert!(input_of(&env.lib, b, 1).1.is_some());
            assert!(done_mark(&env.lib).is_some());
        }
        assert_consistent(&env.lib);
    }
}

/// 読んで照らしている間にゴミ箱を空にして版がなくなったら、アンカーの行を足さず、置いたファイルと予告を片付ける（参照されない
/// アンカーを残さない）。
#[test]
fn versions_gone_while_reading_leave_no_anchor() {
    let target = Arc::new(Mutex::new((0i64, 0i64)));
    let lib_slot: Arc<Mutex<Option<Arc<Library>>>> = Arc::new(Mutex::new(None));
    let hook: RetireHook = {
        let (target, lib_slot) = (target.clone(), lib_slot.clone());
        Arc::new(move |stage| {
            let (vid, item) = *target.lock().unwrap();
            if stage == RetireStage::Commit(vid)
                && let Some(lib) = lib_slot.lock().unwrap().clone()
            {
                lib.db()
                    .write(move |tx| crate::items::trash(tx, &[item], 5).map(|_| ()))
                    .unwrap();
                lib.empty_trash().unwrap();
            }
        })
    };
    let (env, shared) = env_with(Some(hook));
    *lib_slot.lock().unwrap() = Some(env.lib.clone());
    let (a, _) = legacy_item(&env, 31, 1);
    *target.lock().unwrap() = (version_id(&env.lib, a, 1), a);
    swap(&env, &shared, now_set());
    let record = run_retire(&env);
    assert_done(&record);
    let result = result_of(&record);
    assert_eq!(result["anchored"], 0, "{result}");
    assert_eq!(result["left"], json!({ "noOriginal": 1 }), "{result}");
    assert_eq!(env.count("SELECT count(*) FROM edit_anchors"), 0);
    assert_eq!(env.count("SELECT count(*) FROM ledger_pending"), 0);
    let wavs: Vec<Json> = super::save_tests::files(&env.lib)
        .into_iter()
        .filter(|f| f.ends_with(".wav"))
        .map(Json::from)
        .collect();
    assert!(
        wavs.is_empty(),
        "置いたアンカーのファイルが残っている：{wavs:?}"
    );
    assert_consistent(&env.lib);
}

/// 対象の走査は版の入力の主キーの範囲で引く（記録の識別子は行ごとに見る。印が置かれるまでの一度きりの全件の走査。
/// docs/data-model.md §4）。結び付けの行は版の行の item_id の索引で引く。
#[test]
fn target_queries_use_the_primary_key() {
    let conn = crate::db::test_conn();
    let ids = serde_json::to_string(&RETIRING).unwrap();
    let plan = crate::db::query_plan(
        &conn,
        &format!(
            "SELECT i.version_id, i.input, v.item_id, i.recorded
               FROM item_version_inputs i JOIN item_versions v ON v.id = i.version_id
              WHERE (i.version_id, i.input) > (1, 0)
                AND +i.anchor_id IS NULL
                AND CASE WHEN json_valid(i.recorded) THEN json_extract(i.recorded, '$.decoded.codec') END
                    IN (SELECT value FROM json_each('{ids}'))
              ORDER BY i.version_id, i.input
              LIMIT 16"
        ),
    );
    assert!(plan.contains("PRIMARY KEY"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    let plan = crate::db::query_plan(
        &conn,
        "SELECT i.version_id, i.input, i.recorded
           FROM item_versions v JOIN item_version_inputs i ON i.version_id = v.id
          WHERE v.item_id = 1 AND +i.anchor_id IS NULL",
    );
    assert!(!plan.contains("SCAN v"), "{plan}");
    assert!(!plan.contains("SCAN i"), "{plan}");
}
