//! 作業用のファイル（A15。M13 段 4B）：直接読めない形式（FLAC など）の長い音声は、作業用のファイル（封をした f32 WAV）に
//! 流して書いてから遅延の源で開き、閉じると消える。作る間の予算は対話の枠で待たずに確保し（足りなければ Busy）、閉じる・
//! ほかを開くと止まる。試聴の `/pcm` も同じ鍵の作業用のファイルを使い、エディタと使い回す。

use std::io::Cursor;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kura_engine::codec::CodecSet;
use kura_engine::doc::InputId;
use kura_engine::render::cache::Fingerprint;
use kura_engine::testgen::InputGen;
use kura_engine::value::LoopRange;
use kura_engine::{Digest, Graph, Monitor, NoMonitor, Value};
use serde_json::json;

use super::session_tests::*;
use super::source::{WorkCx, make_work, read_shape_staged, work_threshold};
use crate::edit::editor::{EditorError, OpenRequest, Viewport, close, open};

/// 予算 32 MiB：丸ごと読む上限と作業用のファイルの境は 8 MiB（8 kHz モノラルで約 2.1 M フレーム）。
const SMALL: u64 = 32 << 20;

/// 長い FLAC（8 kHz モノラルの正弦波・ループ点つき。`frames` フレーム）。
fn long_flac(frames: u64) -> Vec<u8> {
    let value = InputGen::Sine {
        rate: 8_000,
        channels: 1,
        frames,
        freq: 440,
        loop_range: Some(LoopRange {
            start: 1_000,
            end: frames - 7,
        }),
    }
    .generate()
    .unwrap();
    encode(&value, "flac")
}

/// 丸ごと読んだ値と記録（正）。
fn decode_whole(
    bytes: &[u8],
) -> (
    Arc<kura_engine::value::Audio>,
    kura_engine::codec::DecodeStamp,
) {
    let (value, stamp) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes),
            "flac",
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        )
        .unwrap();
    match value {
        Value::Audio(a) => (a, stamp),
        _ => panic!("音声ではありません"),
    }
}

/// 置き場の中のファイル（名前の順）。
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// 全チャンネルの全標本が同じ。
fn same_samples(got: &kura_engine::value::Audio, want: &kura_engine::value::Audio) {
    assert_eq!(
        (got.rate(), got.channels(), got.frames()),
        (want.rate(), want.channels(), want.frames())
    );
    for ch in 0..want.channels() {
        let mut a = vec![0f32; want.frames()];
        let mut b = vec![0f32; want.frames()];
        got.read(ch, 0, &mut a).unwrap();
        want.read(ch, 0, &mut b).unwrap();
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "チャンネル {ch} の標本が違います"
        );
    }
}

fn g_gain(db: f64) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": db }), 1)
        .unwrap();
    g
}

/// A15：丸ごと読む上限を超える FLAC は作業用のファイルから遅延で開く。値・記録・指紋は丸ごと読んだものと同じで、閉じると
/// 作業用のファイルが消え、予算も 0 に戻る。
#[test]
fn long_flac_opens_from_a_work_file_and_closing_removes_it() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let frames = 40 * C;
    assert!(frames * 4 > work_threshold(SMALL));
    let flac = long_flac(frames);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let opened = env.open(id);
    let a = opened.source.audio.clone().unwrap();
    assert!(a.lazy, "作業用のファイルから遅延で開く");
    assert!(a.decoder.starts_with("flac"), "{}", a.decoder);
    assert_eq!((a.rate, a.channels, a.frames), (8_000, 1, frames));
    assert_eq!(a.loop_range, Some((1_000, frames - 7)));
    let made = files(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].extension().is_some_and(|x| x == "wav"));
    assert_eq!(env.engine.work_pcm().usage().0, 1);

    let (whole, stamp) = decode_whole(&flac);
    let s = opened.session.clone();
    let editor = env.editor(&s);
    {
        let guard = editor.lock_state();
        let st = guard.as_ref().unwrap();
        let info = &st.audio.as_ref().unwrap().input;
        assert_eq!(
            info.decoded.as_ref(),
            Some(&stamp),
            "記録は流れのダイジェストから"
        );
        assert_eq!(info.work.as_ref().unwrap().path(), made[0]);
        assert_eq!(
            st.inputs[&InputId(0)].fingerprint,
            Fingerprint::of_decoded(&stamp)
        );
    }
    let (input, decoder) = editor.published_input().unwrap();
    assert_eq!(decoder, stamp.codec);
    let Value::Audio(input) = input else { panic!() };
    same_samples(&input, &whole);
    assert_eq!(input.meta, whole.meta, "メタデータも丸ごと読んだものと同じ");

    // 丸ごと読むライブラリで開いたものと同じ指紋（samplesId）
    let whole_env = Env::new();
    let wid = add_audio(&whole_env.lib, &flac, "flac");
    let w = whole_env.open(wid);
    assert!(!w.source.audio.unwrap().lazy, "予算が大きければ丸ごと読む");
    assert_eq!(
        shown_audio(&w.outcome.view).samples_id,
        shown_audio(&opened.outcome.view).samples_id
    );
    assert_eq!(whole_env.engine.work_pcm().usage().0, 0);

    // 手の表示は全体の描画と同じ
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -3.0 }))],
        &Viewport::audio_idle(),
    );
    let info = shown_audio(&out.view);
    assert_eq!(info.valid, vec![(0, frames)]);
    let (value, _) = editor.published_audio(info.rev).unwrap();
    let want = rendered(&flac, "flac", &g_gain(-3.0));
    let mut got = vec![0f32; want.frames()];
    value.audio_read(0, 0, &mut got).unwrap();
    let mut expect = vec![0f32; want.frames()];
    want.read(0, 0, &mut expect).unwrap();
    assert_eq!(got, expect);

    // 閉じると作業用のファイルが消える
    close(&env.engine, &s);
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(env.engine.work_pcm().usage(), (0, 0));
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
}

/// A15：作る間の予算が空いていなければ、待たずに Busy で断る（作業用のファイル・取り置きを残さない）。空けば開ける。
#[test]
fn work_file_is_refused_when_the_budget_is_busy() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let flac = long_flac(40 * C);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let budget = env.engine.runtime().unwrap().budget().clone();
    // 形を読む分（数 MiB）は空いているが、流して書く分（窓・デコーダ・ダイジェスト・書き手）は空いていない
    let held = budget
        .try_acquire(SMALL - (6 << 20), kura_engine::budget::Class::Interactive)
        .unwrap()
        .unwrap();
    let err = open(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport: Viewport::audio_idle(),
            from_draft: false,
        },
    )
    .unwrap_err();
    assert!(
        matches!(&err, EditorError::Busy(m) if m.contains("作業用のファイルにする")),
        "{err}"
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    let u = env.usage();
    assert_eq!(u.reserved, 0, "断ったら取り置きを残さない");
    assert_eq!(u.interactive, held.bytes(), "作る分を返した");
    assert!(env.engine.current_editor().is_none());
    drop(held);
    let opened = env.open(id);
    assert!(opened.source.audio.unwrap().lazy);
    assert_eq!(files(&dir).len(), 1);
}

/// 置き場に書きかけ（.part）が現れたら中止を頼む見張り（読みの途中で止める）。
struct CancelWhenWriting(PathBuf);

impl Monitor for CancelWhenWriting {
    fn canceled(&self) -> bool {
        files(&self.0)
            .iter()
            .any(|p| p.extension().is_some_and(|x| x == "part"))
    }
}

/// A15：作っている途中の中止は Canceled で、書きかけを残さない（作業用のファイルの口で決まった形に止める）。エディタでは
/// 閉じると止まる。
#[test]
fn work_file_creation_stops_on_cancel() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let flac = long_flac(40 * C);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let layout = env.lib.layout();
    let hash = *blake3::hash(&flac).as_bytes();
    let path = layout.content_path(&hash, "flac");
    let runtime = env.engine.runtime().unwrap().clone();
    let codecs = env.engine.codecs();
    let (opts, limits) = (Default::default(), Default::default());
    let mon = CancelWhenWriting(dir.clone());
    let cx = WorkCx {
        runtime: &runtime,
        codecs: &codecs,
        store: env.engine.work_pcm(),
        path: &path,
        source: Digest(hash),
        format: "flac",
        opts: &opts,
        limits: &limits,
        mon: &mon,
    };
    let (shape, stage) = read_shape_staged(&cx, None).unwrap().unwrap();
    let err = make_work(&cx, shape, stage).err().unwrap();
    assert!(matches!(err, EditorError::Canceled), "{err}");
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(env.engine.work_pcm().usage().0, 0);
    assert_eq!(runtime.budget().usage().unwrap().interactive, 0);

    // エディタ：開いている途中で閉じると止まる
    let engine = env.engine.clone();
    let jobs_env = &env;
    std::thread::scope(|scope| {
        let opening = scope.spawn(move || {
            open(
                &jobs_env.env(),
                &OpenRequest {
                    item_id: id,
                    viewport: Viewport::audio_idle(),
                    from_draft: false,
                },
            )
        });
        let started = std::time::Instant::now();
        while !files(&dir)
            .iter()
            .any(|p| p.extension().is_some_and(|x| x == "part"))
        {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "書き始めません"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        engine.close_editor();
        let r = opening.join().unwrap();
        assert!(matches!(r, Err(EditorError::Canceled)), "{:?}", r.err());
    });
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(env.engine.work_pcm().usage().0, 0);
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
}

/// 試聴の `/pcm`：長い FLAC は作業用のファイルから読み、応答は丸ごと読んだものとバイト列まで同じ（M11 の値・kura チャンク）。
/// エディタとは同じ鍵のファイルを使い回し（どちらが先でも作り直さない）、どちらも手放すと消える。
#[test]
fn pcm_reads_long_flac_from_the_work_file_shared_with_the_editor() {
    let env = Env::with(Arc::new(Budgeted(SMALL)));
    let frames = 40 * C;
    let flac = long_flac(frames);
    let id = add_audio(&env.lib, &flac, "flac");
    let dir = env.engine.work_pcm().dir().to_path_buf();
    let hash = crate::library::hex(blake3::hash(&flac).as_bytes());
    let root = env.lib.layout().root().to_path_buf();
    let src = crate::serve::pcm_source(env.lib.layout(), &hash).unwrap();
    let serve = |a: u64, b: u64| crate::asset::pcm::serve(&env.engine, &root, &hash, &src, a, b);
    let first = serve(1_000, 9_000).unwrap();
    let made = files(&dir);
    assert_eq!(made.len(), 1, "{made:?}");
    let ino = std::fs::metadata(&made[0]).unwrap().ino();
    assert_eq!(
        env.engine
            .pcm_cache()
            .work()
            .map(|w| w.path().to_path_buf()),
        Some(made[0].clone())
    );
    // 予算の大きいライブラリ（丸ごと読む）の同じ要求と、バイト列まで同じ
    let big = Env::with(Arc::new(Budgeted(256 << 20)));
    add_audio(&big.lib, &flac, "flac");
    let big_src = crate::serve::pcm_source(big.lib.layout(), &hash).unwrap();
    for (a, b) in [(1_000, 9_000), (0, 10), (frames - 100, frames)] {
        let whole =
            crate::asset::pcm::serve(&big.engine, big.lib.layout().root(), &hash, &big_src, a, b)
                .unwrap();
        let got = if a == 1_000 {
            first.clone()
        } else {
            serve(a, b).unwrap()
        };
        assert_eq!(got, whole, "{a}-{b}");
    }
    assert_eq!(big.engine.work_pcm().usage().0, 0, "丸ごと読んだ");
    assert!(big.engine.pcm_cache().work().is_none());

    // エディタは試聴が作ったファイルを使い回す（試聴の置き場を空にしても消さない）
    let opened = env.open(id);
    assert!(opened.source.audio.clone().unwrap().lazy);
    assert!(env.engine.pcm_cache().cached().is_none());
    assert_eq!(files(&dir), made);
    assert_eq!(
        std::fs::metadata(&made[0]).unwrap().ino(),
        ino,
        "作り直さない"
    );
    // 試聴はエディタのファイルを使い回す
    let again = serve(1_000, 9_000).unwrap();
    assert_eq!(again, first);
    assert_eq!(files(&dir), made);
    assert_eq!(std::fs::metadata(&made[0]).unwrap().ino(), ino);
    assert_eq!(env.engine.work_pcm().usage().0, 1);
    // 閉じても試聴が持っている間は残り、試聴を手放すと消える
    close(&env.engine, &opened.session);
    assert!(made[0].is_file());
    env.engine.pcm_cache().clear();
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    // 試聴の予算も返した
    assert_eq!(env.usage().interactive, 0);
}
