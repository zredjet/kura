//! エディタの波形の段（A8〜A10）：手のたびに公開した段が出力を `compute` したものと同じ・裏の仕事で小分けに進めても同じ・
//! 直している間は前の段を配る・土台（ライブラリの解析の段）・タイルの並びと古い rev。

use std::sync::Arc;

use kura_engine::analysis::AnalyzeCx;
use kura_engine::analysis::audio::peaks::{Peaks, PeaksParams, compute};
use kura_engine::analysis::output::AnalysisOutput;
use kura_engine::codec::CodecSet;
use kura_engine::{Graph, Limits, NoMonitor};
use rusqlite::params;
use serde_json::json;

use super::peaks::{serve_peaks, tile_bytes};
use super::session_tests::*;
use crate::edit::editor::{AudioViewport, TileError, Viewport};
use crate::library::Library;

fn peaks_of(a: &kura_engine::value::Audio) -> Peaks {
    let limits = Limits::default();
    compute(
        a,
        &PeaksParams::default(),
        &AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        },
    )
    .unwrap()
}

/// 公開した今の段（rev つき）。
fn published(env: &Env, s: &str, rev: u64) -> Arc<Peaks> {
    env.editor(s).published_peaks(rev).unwrap()
}

/// 裏の仕事を、終わるまで頼み直す（`work: true`）。何回頼んだか。
fn work_until_done(env: &Env, s: &str) -> (u64, usize) {
    let vp = Viewport::Audio(AudioViewport {
        visible: Some((0, 10)),
        ..AudioViewport::idle()
    });
    for i in 1..=400 {
        let out = env.step(s, Vec::new(), &vp);
        let info = shown_audio(&out.view);
        if !info.peaks.pending {
            assert!(!info.work);
            return (info.peaks.rev.unwrap(), i);
        }
        assert!(info.work, "直している間は work");
    }
    panic!("波形の段が終わりません");
}

/// A8：短い音声は手の中で直し、公開した段は出力を `compute` したものと同じ。変わった区間はチャンクに揃う。
#[test]
fn peaks_follow_each_step() {
    let env = Env::new();
    let frames = 3 * C + 17;
    let bytes = noise_wav(8_000, 2, frames, 31);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let info = shown_audio(&opened.outcome.view).clone();
    assert_eq!(info.peaks.rev, Some(info.rev));
    assert!(!info.peaks.pending);
    assert_eq!(info.peaks.changed, crate::edit::editor::ChangedSpans::All);
    assert_eq!(
        (info.peaks.rate, info.peaks.channels, info.peaks.frames),
        (8_000, 2, frames)
    );
    assert_eq!(info.peaks.bins, vec![256, 1_024, 4_096, 16_384, 65_536]);
    let original = rendered(&bytes, "wav", &Graph::new_single());
    assert_eq!(*published(&env, &s, info.rev), peaks_of(&original));

    let vp = Viewport::audio_idle();
    let mut g = Graph::new_single();
    for (i, (step, params)) in [
        (
            add(
                0,
                "audio.fade",
                json!({ "fadeIn": 900, "fadeOut": 0, "curve": "linear" }),
            ),
            json!({ "fadeIn": 900, "fadeOut": 0, "curve": "linear" }),
        ),
        (
            set(
                1,
                json!({ "fadeIn": C + 9, "fadeOut": 0, "curve": "linear" }),
            ),
            json!({ "fadeIn": C + 9, "fadeOut": 0, "curve": "linear" }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let out = env.step(&s, vec![step], &vp);
        let info = shown_audio(&out.view).clone();
        assert_eq!(info.peaks.rev, Some(info.rev), "{i}");
        assert!(!info.peaks.pending);
        if i == 0 {
            g.push_linear("audio.fade", 1, params, 1).unwrap();
        } else {
            g.nodes[0].params = params;
            let crate::edit::editor::ChangedSpans::Spans(sp) = &info.peaks.changed else {
                panic!("{:?}", info.peaks.changed)
            };
            assert!(
                sp.iter()
                    .all(|(a, b)| a % C == 0 && (b % C == 0 || *b == frames)),
                "{sp:?}"
            );
            assert!(sp.iter().all(|(_, b)| *b <= 2 * C), "{sp:?}");
        }
        let want = rendered(&bytes, "wav", &g);
        assert_eq!(*published(&env, &s, info.rev), peaks_of(&want), "{i}");
    }
    // 操作中は直さない（前の段のまま pending）
    let moving = Viewport::Audio(AudioViewport {
        interacting: true,
        ..AudioViewport::idle()
    });
    let before = shown_audio(&env.step(&s, Vec::new(), &vp).view).peaks.rev;
    let out = env.step(
        &s,
        vec![set(
            1,
            json!({ "fadeIn": 5, "fadeOut": 0, "curve": "linear" }),
        )],
        &moving,
    );
    let info = shown_audio(&out.view);
    assert!(info.peaks.pending);
    assert_eq!(info.peaks.rev, before, "前の段を出す");
    let out = env.step(&s, Vec::new(), &vp);
    assert!(!shown_audio(&out.view).peaks.pending);
}

/// A8：長い音声（手の中で直せる量を超える）は裏の仕事で小分けに進め、終えた段は `compute` と同じ。直している間は前の段を
/// 配る（古い rev のタイルが取れる）。
#[test]
fn long_peaks_are_rebuilt_in_the_background() {
    let env = Env::new();
    // 8 kHz モノラルで 32 MiB を超える長さ（表示は区間だけ）
    let frames = 130 * C + 5;
    let bytes = noise_wav(8_000, 1, frames, 32);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open_with(
        id,
        Viewport::Audio(AudioViewport {
            work: false,
            ..AudioViewport::idle()
        }),
    );
    let s = opened.session.clone();
    let info = shown_audio(&opened.outcome.view);
    assert!(info.peaks.pending, "130 チャンクは手の中で直せる量を超える");
    assert_eq!(info.peaks.rev, None);
    let (first, asked) = work_until_done(&env, &s);
    assert!(asked >= 1);
    let original = rendered(&bytes, "wav", &Graph::new_single());
    assert_eq!(*published(&env, &s, first), peaks_of(&original));

    // 全体に効く手：裏で作り直す間は前の段を配る
    let out = env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -6.0 }))],
        &Viewport::Audio(AudioViewport {
            work: false,
            ..AudioViewport::idle()
        }),
    );
    let info = shown_audio(&out.view);
    assert!(info.peaks.pending);
    assert_eq!(info.peaks.rev, Some(first));
    assert!(
        serve_peaks(&env.engine, &s, first, 256, 0).is_ok(),
        "前の段を配る"
    );
    let (next, _) = work_until_done(&env, &s);
    assert!(next > first);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    assert_eq!(
        *published(&env, &s, next),
        peaks_of(&rendered(&bytes, "wav", &g))
    );
    // 前の段は直前として残る
    assert!(serve_peaks(&env.engine, &s, first, 65_536, 0).is_ok());
}

/// 解析の段のファイルと事実の行を置く（解析のジョブの代わり）。
fn place_analysis(lib: &Library, id: i64, bytes: &[u8], peaks: &Peaks) {
    let (_, stamp) = CodecSet::pinned()
        .decode(
            &mut std::io::Cursor::new(bytes),
            "wav",
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        )
        .unwrap();
    let hash = *blake3::hash(bytes).as_bytes();
    let path = lib.layout().peaks_path(&crate::library::hex(&hash));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, peaks.to_canonical().unwrap()).unwrap();
    let digest = stamp.value_digest.0.to_vec();
    let decoder = stamp.codec.clone();
    lib.db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO media_facts (blob_id, state, decoder, reproducible, value_digest, facts_v, analyzed_at)
                 SELECT blob_id, 'ok', ?2, 1, ?3, 1, 1 FROM items WHERE id = ?1",
                params![id, decoder, digest],
            )?;
            Ok(())
        })
        .unwrap();
}

/// A9：手を当てていない元はライブラリの解析の段をそのまま土台にし（開いた瞬間に出る）、全体に効く手で作り直す。
#[test]
fn library_peaks_are_the_base() {
    let env = Env::new();
    let frames = 130 * C + 5;
    let bytes = noise_wav(8_000, 1, frames, 33);
    let id = add_audio(&env.lib, &bytes, "wav");
    let original = rendered(&bytes, "wav", &Graph::new_single());
    let lib_peaks = peaks_of(&original);
    place_analysis(&env.lib, id, &bytes, &lib_peaks);
    let opened = env.open_with(
        id,
        Viewport::Audio(AudioViewport {
            work: false,
            ..AudioViewport::idle()
        }),
    );
    assert!(opened.source.audio.as_ref().unwrap().lazy);
    let s = opened.session.clone();
    let info = shown_audio(&opened.outcome.view);
    assert!(!info.peaks.pending, "土台がそのまま使える");
    assert_eq!(info.peaks.rev, Some(info.rev));
    assert_eq!(*published(&env, &s, info.rev), lib_peaks);
    // 操作を足す手は、エンジンの推論が保守的に全体を返すので裏で作り直す
    let idle = Viewport::Audio(AudioViewport {
        work: false,
        ..AudioViewport::idle()
    });
    let out = env.step(
        &s,
        vec![add(
            0,
            "audio.fade",
            json!({ "fadeIn": 100, "fadeOut": 0, "curve": "linear" }),
        )],
        &idle,
    );
    assert!(shown_audio(&out.view).peaks.pending);
    let (rev, _) = work_until_done(&env, &s);
    let mut g = Graph::new_single();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 100, "fadeOut": 0, "curve": "linear" }),
        1,
    )
    .unwrap();
    assert_eq!(
        *published(&env, &s, rev),
        peaks_of(&rendered(&bytes, "wav", &g))
    );
    // フェードの長さだけを変える手（始まりの 1 チャンク）は手の中で直る
    let out = env.step(
        &s,
        vec![set(
            1,
            json!({ "fadeIn": 300, "fadeOut": 0, "curve": "linear" }),
        )],
        &idle,
    );
    let info = shown_audio(&out.view);
    assert!(!info.peaks.pending, "{:?}", info.peaks);
    let crate::edit::editor::ChangedSpans::Spans(sp) = &info.peaks.changed else {
        panic!("{:?}", info.peaks.changed)
    };
    assert_eq!(sp, &vec![(0, C)]);
    g.nodes[0].params = json!({ "fadeIn": 300, "fadeOut": 0, "curve": "linear" });
    assert_eq!(
        *published(&env, &s, info.rev),
        peaks_of(&rendered(&bytes, "wav", &g))
    );
    // 全体に効く手は裏で作り直す
    let out = env.step(
        &s,
        vec![add(1, "audio.gain", json!({ "db": 3.0 }))],
        &Viewport::Audio(AudioViewport {
            work: false,
            ..AudioViewport::idle()
        }),
    );
    assert!(shown_audio(&out.view).peaks.pending);
    let (rev, _) = work_until_done(&env, &s);
    g.push_linear("audio.gain", 1, json!({ "db": 3.0 }), 2)
        .unwrap();
    assert_eq!(
        *published(&env, &s, rev),
        peaks_of(&rendered(&bytes, "wav", &g))
    );

    // 版 0 でも、事実の行のダイジェストが違えば土台にしない
    let other = noise_wav(8_000, 1, 2 * C, 34);
    let oid = add_audio(&env.lib, &other, "wav");
    place_analysis(&env.lib, oid, &bytes, &lib_peaks);
    let opened = env.open(oid);
    let info = shown_audio(&opened.outcome.view);
    assert_eq!(info.peaks.frames, 2 * C, "土台を使わずに作った");
}

/// A10：`/edit/{s}/{rev}/peaks/{bin}/{tile}` の中身はタイルの並び。古い rev・ない段・範囲の外は 404。
#[test]
fn peaks_tiles_are_served_by_rev() {
    let env = Env::new();
    let frames = 3 * C + 17;
    let bytes = noise_wav(8_000, 2, frames, 35);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let r0 = shown_audio(&opened.outcome.view).rev;
    let p0 = published(&env, &s, r0);
    for (bin, tile) in [(256, 0), (1_024, 0), (65_536, 0)] {
        assert_eq!(
            serve_peaks(&env.engine, &s, r0, bin, tile).unwrap(),
            tile_bytes(&p0, bin, tile).unwrap(),
            "{bin}/{tile}"
        );
    }
    assert!(matches!(
        serve_peaks(&env.engine, &s, r0, 256, 9),
        Err(TileError::NotFound(_))
    ));
    assert!(matches!(
        serve_peaks(&env.engine, &s, r0, 300, 0),
        Err(TileError::NotFound(_))
    ));
    assert!(matches!(
        serve_peaks(&env.engine, "abc", r0, 256, 0),
        Err(TileError::NotFound(_))
    ));
    let vp = Viewport::audio_idle();
    env.step(&s, vec![add(0, "audio.gain", json!({ "db": -1.0 }))], &vp);
    assert!(serve_peaks(&env.engine, &s, r0, 256, 0).is_ok(), "直前の段");
    env.step(&s, vec![set(1, json!({ "db": -2.0 }))], &vp);
    assert!(matches!(
        serve_peaks(&env.engine, &s, r0, 256, 0),
        Err(TileError::NotFound(_))
    ));
}
