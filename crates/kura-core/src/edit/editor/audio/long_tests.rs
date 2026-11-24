//! 60 分の音声のエディタの 1 手の目安（M13 段 4。段 4C の A18 の前の手測り。ふだんは回さない）：48 kHz ステレオ・16 ビットの
//! WAV（約 660 MB）を一時に書き、見えている 10 秒の区間で開いて、gain の手・引数の手・フェードの手を当てる時間と、予算の
//! 使い方（取り置き・持ち続ける分・extra）を出す。段 4B：同じ値の FLAC を作業用のファイルで開く時間と、長い音声の
//! ラウドネスを裏の仕事で流して計る時間も出す。
//!
//! `cargo test --release -p kura-core --lib sixty_minutes -- --ignored --nocapture`
//!
//! 段 4C の A18（F15）：同じ WAV を 48 チャンクの区間の表示で開き、gain・fade・trim の終わり・ループ点の手を当てて、描いたのが
//! 区間の分だけ・源の読みが区間の分だけ・持ち続ける分が 70 MiB 以下・閉じると取り置き・確保・作業用のファイルが 0 を確かめる。
//!
//! `cargo test --release -p kura-core --lib a18_ -- --ignored --nocapture`

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use kura_engine::value::{Audio, AudioMeta, AudioSource};
use kura_engine::{NoMonitor, Value};
use rusqlite::params;
use serde_json::json;

use super::measure::{LoudnessState, measure};
use super::peaks::PEAKS_SYNC_FRAMES;
use super::session_tests::*;
use crate::edit::editor::{AudioViewport, Step, Viewport};

const RATE: u64 = 48_000;

/// 決まった疑似乱数の 16 ビットの標本（左右の順に交互）。WAV と FLAC で同じ値にする。
struct Lcg(u32);

impl Lcg {
    fn next(&mut self) -> i16 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 16) as i16 / 4
    }
}

/// 16 ビット・ステレオの WAV を流して書き、中身のハッシュを返す。
fn write_wav(path: &std::path::Path, frames: u64) -> [u8; 32] {
    let data = frames * 4;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut h = blake3::Hasher::new();
    let mut put = |b: &[u8]| {
        h.update(b);
        f.write_all(b).unwrap();
    };
    put(b"RIFF");
    put(&u32::try_from(36 + data).unwrap().to_le_bytes());
    put(b"WAVEfmt ");
    put(&16u32.to_le_bytes());
    put(&1u16.to_le_bytes());
    put(&2u16.to_le_bytes());
    put(&(RATE as u32).to_le_bytes());
    put(&(RATE as u32 * 4).to_le_bytes());
    put(&4u16.to_le_bytes());
    put(&16u16.to_le_bytes());
    put(b"data");
    put(&u32::try_from(data).unwrap().to_le_bytes());
    let mut x = Lcg(1);
    let mut buf = Vec::with_capacity(1 << 20);
    for _ in 0..frames {
        for _ in 0..2 {
            buf.extend_from_slice(&x.next().to_le_bytes());
        }
        if buf.len() >= 1 << 20 {
            put(&buf);
            buf.clear();
        }
    }
    put(&buf);
    drop(put);
    f.flush().unwrap();
    *h.finalize().as_bytes()
}

/// 同じ値の FLAC（固定の段のエンコーダ・16 ビット）のバイト列。
fn flac_bytes(frames: u64) -> Vec<u8> {
    let n = usize::try_from(frames).unwrap();
    let mut left = Vec::with_capacity(n);
    let mut right = Vec::with_capacity(n);
    let mut x = Lcg(1);
    for _ in 0..n {
        left.push(f32::from(x.next()) / 32_768.0);
        right.push(f32::from(x.next()) / 32_768.0);
    }
    let audio = Audio::new(
        RATE as u32,
        vec![Arc::new(left), Arc::new(right)],
        AudioMeta {
            source: Some(AudioSource {
                bits: 16,
                float: false,
                rate: RATE as u32,
            }),
            ..AudioMeta::default()
        },
    )
    .unwrap();
    kura_engine::codec::CodecSet::pinned()
        .encode_bytes(
            &Value::Audio(Arc::new(audio)),
            "flac",
            &json!({}),
            &NoMonitor,
        )
        .unwrap()
        .0
}

/// アイテムの行を入れる。
fn insert(env: &Env, hash: [u8; 32], ext: &'static str, size: i64) -> i64 {
    env.lib
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, ?3, 'audio', 'audio/' || ?3, ?2, '長', '長.' || ?3, '/src/長.' || ?3, 'local', 1, 1,
                         'ready', 'not_needed', 'ready')",
                params![&hash[..], size, ext],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap()
}

/// 見えている 10 秒の区間で開き、手を当て、裏の仕事（波形の段の作り直し・ラウドネス）を済むまで頼んで、時間と予算を出す。
fn steps_and_measure(env: &Env, id: i64, label: &str) {
    let at = 10 * 60 * RATE;
    let vp = Viewport::Audio(AudioViewport {
        visible: Some((at, at + 10 * RATE)),
        spans: vec![(at, at + 10 * RATE)],
        work: false,
        ..AudioViewport::idle()
    });
    let t = Instant::now();
    let opened = env.open_with(id, vp.clone());
    let source = opened.source.audio.as_ref().unwrap();
    println!(
        "[{label}] 開く：{:.1} ms（lazy = {}・デコーダ {}・作業用のファイル {:?} MB）",
        t.elapsed().as_secs_f64() * 1e3,
        source.lazy,
        source.decoder,
        {
            let (n, bytes) = env.engine.work_pcm().usage();
            (n > 0).then_some(bytes >> 20)
        },
    );
    let s = opened.session.clone();
    let report = |what: &str, t: Instant, out: &crate::edit::editor::StepOutcome| {
        let info = shown_audio(&out.view);
        let editor = env.editor(&s);
        let guard = editor.lock_state();
        let st = guard.as_ref().unwrap();
        let u = env.usage();
        println!(
            "[{label}] {what}：往復 {:.1} ms（表示 {:.1} ms）・描いたノード {}・有効 {:?}・extra {:.1} MB・キャッシュ {:.1} MB・\
             確保 {:.1} MB・取り置き {} MB・持ち続ける {:.1} MB・波形の段 pending {}",
            t.elapsed().as_secs_f64() * 1e3,
            info.ms,
            info.drawn_nodes,
            info.valid,
            st.lease.extra() as f64 / 1e6,
            st.cache.bytes() as f64 / 1e6,
            st.lease.bytes() as f64 / 1e6,
            u.reserved >> 20,
            u.standing as f64 / 1e6,
            info.peaks.pending,
        );
    };
    for (what, steps) in [
        (
            "gain を足す",
            vec![add(0, "audio.gain", json!({ "db": -3.0 }))],
        ),
        ("gain の引数", vec![set(1, json!({ "db": -4.0 }))]),
        (
            "fade を足す",
            vec![add(
                1,
                "audio.fade",
                json!({ "fadeIn": 48_000, "fadeOut": 0, "curve": "linear" }),
            )],
        ),
        (
            "fade の長さ",
            vec![set(
                2,
                json!({ "fadeIn": 96_000, "fadeOut": 0, "curve": "linear" }),
            )],
        ),
        (
            "ループ点",
            vec![add(
                2,
                "audio.set_loop",
                json!({ "loop": { "start": at, "end": at + RATE } }),
            )],
        ),
    ] {
        let t = Instant::now();
        let out = env.step(&s, steps, &vp);
        assert!(out.rejected.is_none(), "{what}: {:?}", out.rejected);
        report(what, t, &out);
    }
    // 長い音声のラウドネスを予約し、裏の仕事（波形の段を先に、残りで計り）を済むまで頼む
    let work = Viewport::Audio(AudioViewport {
        work: true,
        ..match vp.clone() {
            Viewport::Audio(a) => a,
            Viewport::Image(_) => unreachable!(),
        }
    });
    let t = Instant::now();
    let state = measure(&env.env(), &s).unwrap();
    println!(
        "[{label}] edit_measure（予約）：{:.1} ms・{state:?}",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let mut calls = 0u32;
    let mut slowest = 0f64;
    let mut peaks_done: Option<(u32, f64)> = None;
    let state = loop {
        calls += 1;
        let one = Instant::now();
        let out = env.step(&s, Vec::new(), &work);
        slowest = slowest.max(one.elapsed().as_secs_f64() * 1e3);
        let info = shown_audio(&out.view);
        if !info.peaks.pending && peaks_done.is_none() {
            peaks_done = Some((calls, t.elapsed().as_secs_f64()));
        }
        match &info.loudness {
            LoudnessState::Measuring { .. } => {}
            other => break other.clone(),
        }
        assert!(calls < 100_000, "計り終えません");
    };
    println!(
        "[{label}] 裏の仕事：波形の段 {:?}（回・秒）、ラウドネス {calls} 回・{:.2} 秒・1 回の最大 {slowest:.1} ms・{state:?}",
        peaks_done,
        t.elapsed().as_secs_f64(),
    );
    crate::edit::editor::close(&env.engine, &s);
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
    assert_eq!(
        env.engine.work_pcm().usage().0,
        0,
        "閉じたら作業用のファイルは消える"
    );
}

#[test]
#[ignore = "60 分の WAV（約 660 MB）を一時に書く。--release で回す"]
fn sixty_minutes_one_step() {
    let env = Env::with(Arc::new(Budgeted(2 << 30)));
    let frames = 60 * 60 * RATE;
    let tmp = env.lib.layout().staging_dir().join("long.wav");
    std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
    let t = Instant::now();
    let hash = write_wav(&tmp, frames);
    let path = env.lib.layout().content_path(&hash, "wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    let size = std::fs::metadata(&path).unwrap().len() as i64;
    println!(
        "WAV を書いた：{} MB・{:.1} 秒",
        size >> 20,
        t.elapsed().as_secs_f64()
    );
    let id = insert(&env, hash, "wav", size);
    steps_and_measure(&env, id, "WAV");
}

/// 段 4B：60 分の FLAC（直接読めない形式）を作業用のファイルで開く。
#[test]
#[ignore = "60 分の FLAC を作る（値 1.4 GB を持つ）。--release で回す"]
fn sixty_minutes_flac_from_a_work_file() {
    let env = Env::with(Arc::new(Budgeted(2 << 30)));
    let frames = 60 * 60 * RATE;
    let t = Instant::now();
    let bytes = flac_bytes(frames);
    let hash = *blake3::hash(&bytes).as_bytes();
    let path = env.lib.layout().content_path(&hash, "flac");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let size = bytes.len() as i64;
    drop(bytes);
    println!(
        "FLAC を書いた：{} MB・{:.1} 秒",
        size >> 20,
        t.elapsed().as_secs_f64()
    );
    let id = insert(&env, hash, "flac", size);
    steps_and_measure(&env, id, "FLAC");
}

/// A18 の区間：10 分の位置から 48 チャンク（`MAX_VIEW_CHUNKS`。区間で頼める上限で、持ち続ける表示の値がいちばん大きい形）。
const A18_AREA: u64 = super::MAX_VIEW_CHUNKS as u64;

/// A18 の 1 手のあとの数（区間だけを描き、全体に落ちない）を確かめ、表を出す。返すのは数。
fn a18_check(
    env: &Env,
    s: &str,
    what: &str,
    t: Instant,
    out: &crate::edit::editor::StepOutcome,
    nodes: u64,
    area: (u64, u64),
) -> super::AudioCounts {
    assert!(out.rejected.is_none(), "{what}: {:?}", out.rejected);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let info = shown_audio(&out.view);
    let counts = env.editor(s).audio_counts().unwrap();
    let a = counts.area;
    println!(
        "[A18] {what}：往復 {ms:.1} ms・描いたチャンク {}（範囲 {}・全体 {}・共有 {}・継ぎはぎ {}・落ちた {}）・有効 {:?}・\
         源の読み {:?}・extra {:.1} MiB・キャッシュ {:.1} MiB・波形の段 pending {}",
        a.drawn_chunks,
        a.region_nodes,
        a.full_nodes,
        a.shared_chunks,
        a.reused_chunks,
        a.fallbacks,
        info.valid,
        counts.source,
        counts.extra as f64 / MIB,
        counts.cache_bytes as f64 / MIB,
        info.peaks.pending,
    );
    assert_eq!(a.fallbacks, 0, "{what}: 範囲の描画が全体に落ちた");
    assert_eq!(a.full_nodes, 0, "{what}: 全体を描いたノードがある");
    assert!(
        a.drawn_chunks <= nodes * A18_AREA,
        "{what}: 描いたチャンク {} > ノード {nodes} × 区間 {A18_AREA}",
        a.drawn_chunks
    );
    let valid: u64 = info.valid.iter().map(|(x, y)| y - x).sum();
    assert!(
        info.valid.iter().all(|(x, y)| area.0 <= *x && *y <= area.1) && valid <= A18_AREA * C,
        "{what}: 公開した表示が区間の外を持つ: {:?}",
        info.valid
    );
    assert!(
        counts.extra <= A18_EXTRA,
        "{what}: 持ち続ける分 {} バイト > 70 MiB",
        counts.extra
    );
    counts
}

/// 持ち続ける分の上限（design-stage46.md §1.7 の「区間の表示 24 MiB × 2 ＋ 段 3 つ 22 MB で 70 MB 程度」）。
const A18_EXTRA: u64 = 70 << 20;
const MIB: f64 = (1u64 << 20) as f64;

/// 源の読みの数（遅延の入力の源）。
fn reads(env: &Env, s: &str) -> u64 {
    env.editor(s)
        .audio_counts()
        .and_then(|c| c.source)
        .expect("遅延の入力")
        .reads
}

/// A18（F15。docs/editing.md §12.12）：60 分（48 kHz ステレオ・16 ビットの WAV）を、10 分の位置から 48 チャンクの区間の表示で
/// 開き、gain・fade・trim の終わり・ループ点の手（足す・引数を変える）を当てる。
///
/// - 描くのは区間の分だけ：どの手も、表示の範囲の描画が全体に落ちず（`fallbacks`・`full_nodes` が 0）、描いたチャンクがノード
///   × 区間のチャンク以下で、公開した表示の有効な区間は区間の中だけ。
/// - 源の読みは区間の分だけ：開いて 8 手を当てるまでに源から読んだチャンクが区間のチャンク以下（全体は 2,637 チャンク）。
///   裏の仕事（全体を流す）の読みは源の LRU に入れない（段 7）ので、波形の段を作り直したあとも区間は LRU に残り、fade の手は
///   手の中で直す波形の段の汚れ（`PEAKS_SYNC_FRAMES`）まで、gain の手は 0。
/// - 持ち続ける分（`extra`）：手のあとも、裏の仕事で 60 分の波形の段を作り直す間（公開した全段の段 2 つを持ったまま組み直す
///   gain のあとを含む）も 70 MiB 以下。
/// - 閉じると取り置き・対話・持ち続ける分・作業用のファイルが 0。
#[test]
#[ignore = "60 分の WAV（約 660 MB）を一時に書く。--release で回す"]
fn a18_sixty_minutes_one_step_draws_only_the_spans() {
    let env = Env::with(Arc::new(Budgeted(2 << 30)));
    let frames = 60 * 60 * RATE;
    let tmp = env.lib.layout().staging_dir().join("long.wav");
    std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
    let hash = write_wav(&tmp, frames);
    let path = env.lib.layout().content_path(&hash, "wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    let id = insert(
        &env,
        hash,
        "wav",
        std::fs::metadata(&path).unwrap().len() as i64,
    );

    let at = 10 * 60 * RATE / C * C;
    let area = (at, at + A18_AREA * C);
    let vp = Viewport::Audio(AudioViewport {
        visible: Some(area),
        spans: vec![area],
        work: false,
        ..AudioViewport::idle()
    });
    let t = Instant::now();
    let opened = env.open_with(id, vp.clone());
    let s = opened.session.clone();
    assert!(opened.source.audio.as_ref().unwrap().lazy, "遅延の源で開く");
    let open_reads = reads(&env, &s);
    println!(
        "[A18] 開く：{:.1} ms・源の読み {open_reads}",
        t.elapsed().as_secs_f64() * 1e3
    );

    let r = RATE;
    let steps: Vec<(&str, Step, u64)> = vec![
        (
            "gain を足す",
            add(0, "audio.gain", json!({ "db": -3.0 })),
            1,
        ),
        ("gain の引数", set(1, json!({ "db": -4.0 })), 1),
        (
            "fade を足す",
            add(
                1,
                "audio.fade",
                json!({ "fadeIn": r, "fadeOut": 0, "curve": "linear" }),
            ),
            2,
        ),
        (
            "fade の長さ",
            set(
                2,
                json!({ "fadeIn": 2 * r, "fadeOut": 0, "curve": "linear" }),
            ),
            2,
        ),
        (
            "trim（終わり）を足す",
            add(2, "audio.trim", json!({ "start": 0, "end": frames - r })),
            3,
        ),
        (
            "trim の終わり",
            set(3, json!({ "start": 0, "end": frames - 2 * r })),
            3,
        ),
        (
            "ループ点を足す",
            add(
                3,
                "audio.set_loop",
                json!({ "loop": { "start": at, "end": at + r } }),
            ),
            4,
        ),
        (
            "ループ点",
            set(
                4,
                json!({ "loop": { "start": at + r / 2, "end": at + 3 * r / 2 } }),
            ),
            4,
        ),
    ];
    for (what, step, nodes) in steps {
        let t = Instant::now();
        let out = env.step(&s, vec![step], &vp);
        a18_check(&env, &s, what, t, &out, nodes, area);
    }
    let step_reads = reads(&env, &s);
    println!("[A18] 開いて 8 手を当てるまでの源の読み：{step_reads} チャンク（区間 {A18_AREA}）");
    assert!(
        step_reads <= A18_AREA,
        "源の読み {step_reads} チャンク > 区間 {A18_AREA} チャンク"
    );

    // 裏の仕事で 60 分の波形の段を作り直す（区間の表示 2 つと、公開した段・組んでいる段を持つ間も 70 MiB 以下）
    let work = Viewport::Audio(AudioViewport {
        work: true,
        ..match vp.clone() {
            Viewport::Audio(a) => a,
            Viewport::Image(_) => unreachable!(),
        }
    });
    let background = |when: &str| {
        let before = reads(&env, &s);
        let t = Instant::now();
        let mut calls = 0u32;
        let mut max_extra = 0u64;
        loop {
            calls += 1;
            let out = env.step(&s, Vec::new(), &work);
            let info = shown_audio(&out.view);
            let extra = env.editor(&s).audio_counts().unwrap().extra;
            max_extra = max_extra.max(extra);
            assert!(
                extra <= A18_EXTRA,
                "裏の仕事（{when}）：持ち続ける分 {extra} バイト > 70 MiB"
            );
            if !info.peaks.pending {
                break;
            }
            assert!(calls < 10_000, "波形の段を作り直せません");
        }
        println!(
            "[A18] 裏の仕事（{when}・波形の段の作り直し）：{calls} 回・{:.2} 秒・源の読み {}・extra の最大 {:.1} MiB",
            t.elapsed().as_secs_f64(),
            reads(&env, &s) - before,
            max_extra as f64 / MIB
        );
    };
    background("8 手のあと");

    // 段がそろったあとの fade の手：変わった区間（頭の数チャンク）は手の中で直す
    let before = reads(&env, &s);
    let t = Instant::now();
    let out = env.step(
        &s,
        vec![set(
            2,
            json!({ "fadeIn": 3 * r, "fadeOut": 0, "curve": "linear" }),
        )],
        &vp,
    );
    a18_check(&env, &s, "段がそろったあとの fade の長さ", t, &out, 4, area);
    assert!(
        !shown_audio(&out.view).peaks.pending,
        "fade の汚れは手の中で直す"
    );
    let sync = PEAKS_SYNC_FRAMES / C;
    let fade_reads = reads(&env, &s) - before;
    println!(
        "[A18] その手の源の読み：{fade_reads} チャンク（手の中で直す汚れ {sync} まで。区間は源の LRU に残る）"
    );
    assert!(
        fade_reads <= sync,
        "源の読み {fade_reads} チャンク > 手の中で直す汚れ {sync}"
    );
    // 全体に効く gain の手：区間だけを描き、段は裏の仕事に回す。裏の仕事の読みは源の LRU に入れない（読み通し。段 7）ので、
    // 区間のチャンクは LRU に残り、読み直さない
    let before = reads(&env, &s);
    let t = Instant::now();
    let out = env.step(&s, vec![set(1, json!({ "db": -5.0 }))], &vp);
    a18_check(&env, &s, "段がそろったあとの gain の引数", t, &out, 4, area);
    assert!(
        shown_audio(&out.view).peaks.pending,
        "gain の段は裏の仕事で直す"
    );
    let gain_reads = reads(&env, &s) - before;
    println!("[A18] 裏の仕事のあとの gain の手の源の読み：{gain_reads} チャンク");
    assert_eq!(
        gain_reads, 0,
        "裏の仕事のあとの gain の手は区間を源から読み直さない"
    );
    // 公開した段 2 つ（どちらも全段）を持ったまま組み直す：持ち続ける分がいちばん大きくなる形
    background("gain のあと");

    crate::edit::editor::close(&env.engine, &s);
    let u = env.usage();
    assert_eq!(
        (u.reserved, u.interactive, u.standing),
        (0, 0, 0),
        "閉じたら取り置き・確保・持ち続ける分は 0"
    );
    assert_eq!(
        env.engine.work_pcm().usage(),
        (0, 0),
        "作業用のファイルは 0"
    );
    assert!(env.engine.current_editor().is_none());
}

/// M17 段 3 の手測り：60 分の WAV（48 kHz ステレオ）に 8 段の EQ を足し、引数を変えた手が古い rev の表示を返すまでの時間、
/// 裏の仕事の区切り（40 ms）の数・1 回の最大・解き終えるまでの合計を出す（プール 2 スレッド・予算 2 GiB）。
///
/// `cargo test --release -p kura-core --lib sixty_minutes_eq -- --ignored --nocapture`
#[test]
#[ignore = "60 分の WAV を書く（約 660 MB）。--release で回す"]
fn sixty_minutes_eq_solves_in_the_background() {
    let env = Env::with(Arc::new(Budgeted(2 << 30)));
    let frames = 60 * 60 * RATE;
    let tmp = env.lib.layout().staging_dir().join("long.wav");
    std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
    let hash = write_wav(&tmp, frames);
    let path = env.lib.layout().content_path(&hash, "wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    let size = std::fs::metadata(&path).unwrap().len() as i64;
    let id = insert(&env, hash, "wav", size);
    let c = kura_engine::value::AUDIO_CHUNK as u64;
    let at = 10 * 60 * RATE / c * c;
    let view = |work: bool, interacting: bool| {
        Viewport::Audio(AudioViewport {
            spans: vec![(at, at + 4 * c)],
            work,
            interacting,
            ..AudioViewport::idle()
        })
    };
    let s = env.open_with(id, view(false, false)).session.clone();
    let eq = |g: f64| {
        let bands: Vec<serde_json::Value> = [
            60.0, 120.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0,
        ]
        .iter()
        .map(|f| json!({ "type": "bell", "freq": f, "gainDb": g, "q": 1.2 }))
        .collect();
        json!({ "bands": bands, "phase": "minimum", "gainDb": 0.0 })
    };
    // 最初の EQ：前に公開した表示（EQ なし）で描きながら解く
    for (round, g) in [(0, 3.0), (1, 4.5)] {
        let t = Instant::now();
        let out = if round == 0 {
            env.step(&s, vec![add(0, "audio.eq", eq(g))], &view(false, false))
        } else {
            env.step(&s, vec![set(1, eq(g))], &view(false, true))
        };
        let first = t.elapsed().as_secs_f64() * 1e3;
        let info = shown_audio(&out.view);
        assert!(info.solving.is_some(), "裏の仕事にならない");
        let (mut slices, mut most) = (0u32, 0.0f64);
        let total = Instant::now();
        loop {
            let t = Instant::now();
            let out = env.step(&s, Vec::new(), &view(true, round == 1));
            let ms = t.elapsed().as_secs_f64() * 1e3;
            most = most.max(ms);
            slices += 1;
            if shown_audio(&out.view).solving.is_none() {
                break;
            }
            assert!(slices < 10_000);
        }
        println!(
            "{}：手の応え {first:.1} ms・区切り {slices} 回（1 回の最大 {most:.1} ms）・解き終えるまで {:.2} 秒",
            if round == 0 {
                "EQ を足す"
            } else {
                "引数を変える（ドラッグ中）"
            },
            total.elapsed().as_secs_f64()
        );
    }
}

/// 手測り（M17 段 4）：60 分・48 kHz ステレオの WAV に gain を足した文書で、スペクトルのタイル（和）を表示の頼みに添えた
/// 1 手の応え、裏の仕事で 48 チャンクのタイルを作り終えるまで、全体（⌘A）の区間の平均を裏の仕事で求め終えるまでを出す
/// （プール 2 スレッド・予算 2 GiB）。
///
/// `cargo test --release -p kura-core --lib sixty_minutes_spectrum -- --ignored --nocapture`
#[test]
#[ignore = "60 分の WAV を書く（約 660 MB）。--release で回す"]
fn sixty_minutes_spectrum() {
    use super::spec::{SpecRequest, SpecSide};
    use super::spec_average::{
        SpecAveragePhase, SpecAverageRequest, SpecAverageState, spec_average,
    };
    use kura_engine::audio::spec::SpecChannel;

    let env = Env::with(Arc::new(Budgeted(2 << 30)));
    let frames = 60 * 60 * RATE;
    let tmp = env.lib.layout().staging_dir().join("long.wav");
    std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
    let hash = write_wav(&tmp, frames);
    let path = env.lib.layout().content_path(&hash, "wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    let size = std::fs::metadata(&path).unwrap().len() as i64;
    let id = insert(&env, hash, "wav", size);
    let c = kura_engine::value::AUDIO_CHUNK as u64;
    let at = 10 * 60 * RATE / c * c;
    let view = |spec: &[(u64, u64)], work: bool, interacting: bool| {
        Viewport::Audio(AudioViewport {
            spans: vec![(at, at + 4 * c)],
            work,
            interacting,
            spec: vec![SpecRequest {
                node: None,
                side: SpecSide::Output,
                channel: SpecChannel::Sum,
                spans: spec.to_vec(),
            }],
            ..AudioViewport::idle()
        })
    };
    let s = env.open_with(id, view(&[], false, false)).session.clone();
    env.step(
        &s,
        vec![add(0, "audio.gain", json!({ "db": -3.0 }))],
        &view(&[], false, false),
    );
    // 見えている 4 チャンク：手の応えで 2 チャンク
    let t = Instant::now();
    let out = env.step(&s, Vec::new(), &view(&[(at, at + 4 * c)], false, false));
    let first = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(shown_audio(&out.view).spec[0].ready.len(), 1);
    // 48 チャンクを裏の仕事で
    let want = [(at, at + 48 * c)];
    let (mut slices, mut most) = (0u32, 0.0f64);
    let total = Instant::now();
    loop {
        let t = Instant::now();
        let out = env.step(&s, Vec::new(), &view(&want, true, false));
        most = most.max(t.elapsed().as_secs_f64() * 1e3);
        slices += 1;
        if !shown_audio(&out.view).spec[0].pending {
            break;
        }
        assert!(slices < 10_000);
    }
    println!(
        "タイル（和）：2 チャンクを添えた手の応え {first:.1} ms・48 チャンクを区切り {slices} 回（1 回の最大 {most:.1} ms）・{:.2} 秒",
        total.elapsed().as_secs_f64()
    );
    // 全体の区間の平均
    let req = SpecAverageRequest {
        node: None,
        side: SpecSide::Output,
        channel: SpecChannel::Sum,
        start: 0,
        end: frames,
    };
    let t = Instant::now();
    let r = spec_average(&env.env(), &s, &req).unwrap();
    let first = t.elapsed().as_secs_f64() * 1e3;
    assert!(matches!(r, SpecAverageState::Measuring { .. }), "{r:?}");
    let (mut slices, mut most) = (0u32, 0.0f64);
    let total = Instant::now();
    loop {
        let t = Instant::now();
        let out = env.step(&s, Vec::new(), &view(&[], true, false));
        most = most.max(t.elapsed().as_secs_f64() * 1e3);
        slices += 1;
        if shown_audio(&out.view).spec_average[0].phase == SpecAveragePhase::Ready {
            break;
        }
        assert!(slices < 100_000);
    }
    let secs = total.elapsed().as_secs_f64();
    let SpecAverageState::Ready(v) = spec_average(&env.env(), &s, &req).unwrap() else {
        panic!("覚えた値");
    };
    assert_eq!(v.frames, frames.div_ceil(1024));
    println!(
        "全体の平均（和）：予約の応え {first:.1} ms・区切り {slices} 回（1 回の最大 {most:.1} ms）・{secs:.2} 秒"
    );
    // 選び直し（まとまり・チャンクの要約から、端のまとまりだけ）
    let total = Instant::now();
    let r = spec_average(
        &env.env(),
        &s,
        &SpecAverageRequest {
            start: 5 * c + 123,
            end: frames - 77,
            ..req
        },
    )
    .unwrap();
    let sync = matches!(r, SpecAverageState::Ready(_));
    let mut slices = 0u32;
    if !sync {
        loop {
            let out = env.step(&s, Vec::new(), &view(&[], true, false));
            slices += 1;
            if shown_audio(&out.view).spec_average[0].phase == SpecAveragePhase::Ready {
                break;
            }
            assert!(slices < 10_000);
        }
    }
    println!(
        "選び直し：{}・区切り {slices} 回・{:.1} ms",
        if sync { "その場" } else { "予約" },
        total.elapsed().as_secs_f64() * 1e3
    );
}
