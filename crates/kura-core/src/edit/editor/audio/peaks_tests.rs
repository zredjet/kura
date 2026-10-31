//! エディタの波形の段（A8〜A10）：手のたびに公開した段が出力を `compute` したものと同じ・裏の仕事で小分けに進めても同じ・
//! 直している間は前の段を配る・土台（ライブラリの解析の段）・タイルの並びと古い rev。

use std::collections::BTreeMap;
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
use crate::edit::editor::{
    AudioViewInfo, AudioViewport, OpenRequest, Step, TileError, Viewport, open,
};
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

/// 裏の仕事を、終わるまで頼み直す（`work: true`）。段の世代と、何回頼んだか。
fn work_until_done(env: &Env, s: &str) -> (u64, usize) {
    let (info, i) = work_until_done_info(env, s);
    (info.peaks.rev.unwrap(), i)
}

/// 裏の仕事を、終わるまで頼み直す（`work: true`）。終えた表示の情報と、何回頼んだか。
fn work_until_done_info(env: &Env, s: &str) -> (AudioViewInfo, usize) {
    let vp = Viewport::Audio(AudioViewport {
        visible: Some((0, 10)),
        ..AudioViewport::idle()
    });
    for i in 1..=400 {
        let out = env.step(s, Vec::new(), &vp);
        let info = shown_audio(&out.view);
        if !info.peaks.pending {
            assert!(!info.work);
            return (info.clone(), i);
        }
        assert!(info.work, "直している間は work");
    }
    panic!("波形の段が終わりません");
}

/// ライブラリの解析のジョブが書く段（引数は `analysis::plan::peaks_params(frames)`。256 × 4,096 = 1,048,576 フレームを
/// 超えると、細かい段がない）。
fn library_peaks_of(a: &kura_engine::value::Audio) -> Peaks {
    let limits = Limits::default();
    let params: PeaksParams =
        serde_json::from_value(crate::analysis::plan::peaks_params(a.frames() as u64)).unwrap();
    compute(
        a,
        &params,
        &AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        },
    )
    .unwrap()
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
    let mut last = info.peaks.rev.unwrap();
    assert!(!info.peaks.pending);
    assert_eq!(info.peaks.changed, crate::edit::editor::ChangedSpans::All);
    assert_eq!(
        (info.peaks.rate, info.peaks.channels, info.peaks.frames),
        (8_000, 2, frames)
    );
    assert_eq!(info.peaks.bins, vec![256, 1_024, 4_096, 16_384, 65_536]);
    let original = rendered(&bytes, "wav", &Graph::new_single());
    assert_eq!(*published(&env, &s, last), peaks_of(&original));

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
        let generation = info.peaks.rev.unwrap();
        assert!(generation > last, "手のたびに新しい世代 {i}");
        last = generation;
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
        assert_eq!(*published(&env, &s, generation), peaks_of(&want), "{i}");
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

/// A9：手を当てていない元はライブラリの解析の段を土台にし、開いた瞬間に出す。長い音声のライブラリの段（解析のジョブの
/// 引数 `peaks_params(frames)`）は粗い段だけなので、それを仮の世代として出し（`bins` は持っている段だけ・pending）、
/// 細かい段は裏の仕事で全体を作ってから新しい世代で出す。全体に効く手で作り直す。
#[test]
fn library_peaks_are_the_base() {
    let env = Env::new();
    // 256 × 4,096 = 1,048,576 フレームを超える（ライブラリに 256・1,024 の段がない）
    let frames = 130 * C + 5;
    let bytes = noise_wav(8_000, 1, frames, 33);
    let id = add_audio(&env.lib, &bytes, "wav");
    let original = rendered(&bytes, "wav", &Graph::new_single());
    let lib_peaks = library_peaks_of(&original);
    let coarse: Vec<u32> = lib_peaks.levels.iter().map(|l| l.bin).collect();
    assert_eq!(coarse, vec![4_096, 16_384, 65_536], "解析のジョブが書く段");
    let full = peaks_of(&original);
    assert_eq!(
        lib_peaks.levels[..],
        full.levels[2..],
        "粗い段は全段の同じ段と同じ"
    );
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
    assert!(info.peaks.pending, "細かい段は裏で作る");
    assert_eq!(info.peaks.bins, coarse, "持っている段だけを示す");
    assert_eq!(
        (info.peaks.rate, info.peaks.channels, info.peaks.frames),
        (8_000, 1, frames)
    );
    let g0 = info.peaks.rev.expect("開いた瞬間に粗い段が出る");
    assert_eq!(*published(&env, &s, g0), lib_peaks);
    assert!(serve_peaks(&env.engine, &s, g0, 4_096, 0).is_ok());
    assert!(matches!(
        serve_peaks(&env.engine, &s, g0, 256, 0),
        Err(TileError::NotFound(_))
    ));
    let (done, _) = work_until_done_info(&env, &s);
    let g1 = done.peaks.rev.unwrap();
    assert!(g1 > g0, "全段は新しい世代");
    assert_eq!(done.peaks.bins, vec![256, 1_024, 4_096, 16_384, 65_536]);
    assert_eq!(done.peaks.changed, crate::edit::editor::ChangedSpans::All);
    assert_eq!(*published(&env, &s, g1), full);
    assert_eq!(
        *published(&env, &s, g0),
        lib_peaks,
        "仮の段は直前として残る"
    );
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
        *published(&env, &s, info.peaks.rev.unwrap()),
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

/// A9：長い音声の下書き（手のある文書）から開くと、ライブラリの粗い段（手の前の波形）を仮の世代で出し、裏の仕事で全体を
/// 作り直して新しい世代で出す。同じ世代のタイルは変わらず、直し終えた段はその rev の出力の `compute`。
#[test]
fn long_draft_shows_the_library_peaks_until_rebuilt() {
    let env = Env::new();
    let frames = 130 * C + 5;
    let bytes = noise_wav(8_000, 1, frames, 37);
    let id = add_audio(&env.lib, &bytes, "wav");
    let lib_peaks = library_peaks_of(&rendered(&bytes, "wav", &Graph::new_single()));
    place_analysis(&env.lib, id, &bytes, &lib_peaks);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    place_draft(&env.lib, id, &bytes, &g);
    let opened = open_draft(
        &env,
        id,
        Viewport::Audio(AudioViewport {
            work: false,
            ..AudioViewport::idle()
        }),
    );
    let s = opened.session.clone();
    let info = shown_audio(&opened.outcome.view).clone();
    assert!(info.peaks.pending);
    let g0 = info.peaks.rev.unwrap();
    assert_eq!(info.peaks.bins, vec![4_096, 16_384, 65_536]);
    assert_eq!(*published(&env, &s, g0), lib_peaks, "手の前の波形（仮）");
    let mut seen = Generations::default();
    seen.see(&env, &s, g0);
    let (done, _) = work_until_done_info(&env, &s);
    let g1 = done.peaks.rev.unwrap();
    assert_ne!(g1, g0);
    assert_eq!(
        *published(&env, &s, g1),
        peaks_of(&rendered(&bytes, "wav", &g))
    );
    seen.see(&env, &s, g1);
}

/// 公開した段のタイルをすべて（`/edit/{s}/{gen}/peaks/{bin}/{tile}` の中身。段の世代の今か直前でなければ None）。
fn tiles_of(env: &Env, s: &str, gen_: u64) -> Option<BTreeMap<(u32, u32), Vec<u8>>> {
    let p = env.editor(s).published_peaks(gen_)?;
    let mut out = BTreeMap::new();
    for l in &p.levels {
        for tile in 0.. {
            match serve_peaks(&env.engine, s, gen_, l.bin, tile) {
                Ok(b) => {
                    out.insert((l.bin, tile), b);
                }
                Err(_) => break,
            }
        }
    }
    Some(out)
}

/// 段の世代ごとに最初に見たタイルを覚え、同じ世代のタイルのバイト列が二度と変わらないことを確かめる。
#[derive(Default)]
struct Generations(BTreeMap<u64, BTreeMap<(u32, u32), Vec<u8>>>);

impl Generations {
    /// 今の段の世代 `gen_` のタイルを見る（前に見ていれば同じか確かめる）。前に見たほかの世代も、まだ配っていれば同じ。
    fn see(&mut self, env: &Env, s: &str, gen_: u64) {
        for (g, tiles) in &self.0 {
            if let Some(now) = tiles_of(env, s, *g) {
                assert_eq!(&now, tiles, "世代 {g} のタイルが変わった");
            }
        }
        let tiles = tiles_of(env, s, gen_).expect("今の世代を配る");
        assert!(!tiles.is_empty());
        self.0.entry(gen_).or_insert(tiles);
    }
}

/// 開くときの表示（操作中）。
fn interacting() -> Viewport {
    Viewport::Audio(AudioViewport {
        interacting: true,
        work: false,
        ..AudioViewport::idle()
    })
}

/// 下書きを置く（版 0 の頭に `g` を当てた文書）。
fn place_draft(lib: &Library, id: i64, bytes: &[u8], g: &Graph) {
    let hash = *blake3::hash(bytes).as_bytes();
    crate::edit::drafts::save(
        lib.db(),
        id,
        g,
        crate::edit::Base { hash, version: 0 },
        None,
    )
    .unwrap();
}

fn open_draft(env: &Env, id: i64, viewport: Viewport) -> crate::edit::editor::Opened {
    let opened = open(
        &env.env(),
        &OpenRequest {
            item_id: id,
            viewport,
            from_draft: true,
        },
    )
    .unwrap();
    assert!(opened.draft.opened);
    opened
}

/// A9：下書き（手のある文書）から開くと、土台（ライブラリの段＝手の前の波形）は仮の段として自分の世代で出し（pending）、
/// 直し終えたら新しい世代で公開する。同じ世代のタイルのバイト列は二度と変わらず、仮でない段はその rev の出力の `compute`。
#[test]
fn draft_base_is_provisional_and_generations_never_change() {
    let env = Env::new();
    let frames = 3 * C + 17;
    let bytes = noise_wav(8_000, 1, frames, 36);
    let id = add_audio(&env.lib, &bytes, "wav");
    // 短い音声のライブラリの段は全段（解析のジョブの引数がエディタと同じ）なので、部分更新の元にもなる
    let lib_peaks = library_peaks_of(&rendered(&bytes, "wav", &Graph::new_single()));
    assert_eq!(lib_peaks.levels.len(), 5);
    place_analysis(&env.lib, id, &bytes, &lib_peaks);
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 1)
        .unwrap();
    place_draft(&env.lib, id, &bytes, &g);
    // 操作中で開く：手の中で直さないので、土台を仮の段として出す
    let opened = open_draft(&env, id, interacting());
    let s = opened.session.clone();
    let info = shown_audio(&opened.outcome.view).clone();
    assert!(info.peaks.pending, "土台は手の前の波形（仮の段）");
    let g0 = info.peaks.rev.unwrap();
    assert_eq!(*published(&env, &s, g0), lib_peaks);
    let mut seen = Generations::default();
    seen.see(&env, &s, g0);
    // 手を止めると直し、新しい世代で公開する
    let out = env.step(&s, Vec::new(), &Viewport::audio_idle());
    let info = shown_audio(&out.view).clone();
    assert!(!info.peaks.pending);
    let g1 = info.peaks.rev.unwrap();
    assert_ne!(g1, g0, "直した段は別の世代");
    assert_eq!(
        *published(&env, &s, g1),
        peaks_of(&rendered(&bytes, "wav", &g))
    );
    seen.see(&env, &s, g1);
    // 続く手も、そのたびに新しい世代
    let out = env.step(
        &s,
        vec![add(
            1,
            "audio.fade",
            json!({ "fadeIn": 700, "fadeOut": 0, "curve": "linear" }),
        )],
        &Viewport::audio_idle(),
    );
    let g2 = shown_audio(&out.view).peaks.rev.unwrap();
    assert!(g2 != g1 && g2 != g0);
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 700, "fadeOut": 0, "curve": "linear" }),
        2,
    )
    .unwrap();
    assert_eq!(
        *published(&env, &s, g2),
        peaks_of(&rendered(&bytes, "wav", &g))
    );
    seen.see(&env, &s, g2);
    // 取り消しで同じ標本に戻っても、段は新しい世代で出す（同じ番号を使い回さない）
    let out = env.step(&s, vec![Step::Undo], &Viewport::audio_idle());
    let g3 = shown_audio(&out.view).peaks.rev.unwrap();
    assert!(g3 > g2);
    seen.see(&env, &s, g3);
}

/// A10：`/edit/{s}/{peaks.rev}/peaks/{bin}/{tile}` の中身はタイルの並び。古い世代・ない段・範囲の外は 404。
#[test]
fn peaks_tiles_are_served_by_rev() {
    let env = Env::new();
    let frames = 3 * C + 17;
    let bytes = noise_wav(8_000, 2, frames, 35);
    let id = add_audio(&env.lib, &bytes, "wav");
    let opened = env.open(id);
    let s = opened.session.clone();
    let r0 = shown_audio(&opened.outcome.view).peaks.rev.unwrap();
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
