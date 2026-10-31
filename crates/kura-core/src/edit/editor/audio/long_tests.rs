//! 60 分の音声のエディタの 1 手の目安（M13 段 4。段 4C の A18 の前の手測り。ふだんは回さない）：48 kHz ステレオ・16 ビットの
//! WAV（約 660 MB）を一時に書き、見えている 10 秒の区間で開いて、gain の手・引数の手・フェードの手を当てる時間と、予算の
//! 使い方（取り置き・持ち続ける分・extra）を出す。段 4B：同じ値の FLAC を作業用のファイルで開く時間と、長い音声の
//! ラウドネスを裏の仕事で流して計る時間も出す。
//!
//! `cargo test --release -p kura-core --lib sixty_minutes -- --ignored --nocapture`

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use kura_engine::value::{Audio, AudioMeta, AudioSource};
use kura_engine::{NoMonitor, Value};
use rusqlite::params;
use serde_json::json;

use super::measure::{LoudnessState, measure};
use super::session_tests::*;
use crate::edit::editor::{AudioViewport, Viewport};

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
