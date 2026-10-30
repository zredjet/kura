//! 60 分の音声のエディタの 1 手の目安（M13 段 4。段 4C の A18 の前の手測り。ふだんは回さない）：48 kHz ステレオ・16 ビットの
//! WAV（約 660 MB）を一時に書き、見えている 10 秒の区間で開いて、gain の手・引数の手・フェードの手を当てる時間と、予算の
//! 使い方（取り置き・持ち続ける分・extra）を出す。
//!
//! `cargo test --release -p kura-core --lib sixty_minutes -- --ignored --nocapture`

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use rusqlite::params;
use serde_json::json;

use super::session_tests::*;
use crate::edit::editor::{AudioViewport, Viewport};

const RATE: u64 = 48_000;

/// 16 ビット・ステレオの WAV を流して書き、中身のハッシュを返す（値は決まった疑似乱数）。
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
    let mut x: u32 = 1;
    let mut buf = Vec::with_capacity(1 << 20);
    for _ in 0..frames {
        for _ in 0..2 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            buf.extend_from_slice(&((x >> 16) as i16 / 4).to_le_bytes());
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
    let id = env
        .lib
        .db()
        .write(move |tx| {
            tx.execute(
                "INSERT INTO items (hash, ext, kind, mime, size_bytes, title, original_name, original_path,
                                    source_site, imported_at, updated_at, thumb_state, proxy_state, analysis_state)
                 VALUES (?1, 'wav', 'audio', 'audio/wav', ?2, '長', '長.wav', '/src/長.wav', 'local', 1, 1,
                         'ready', 'not_needed', 'ready')",
                params![&hash[..], size],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .unwrap();
    let at = 10 * 60 * RATE;
    let vp = Viewport::Audio(AudioViewport {
        visible: Some((at, at + 10 * RATE)),
        spans: vec![(at, at + 10 * RATE)],
        work: false,
        ..AudioViewport::idle()
    });
    let t = Instant::now();
    let opened = env.open_with(id, vp.clone());
    println!(
        "開く：{:.1} ms（lazy = {}）",
        t.elapsed().as_secs_f64() * 1e3,
        opened.source.audio.as_ref().unwrap().lazy
    );
    let s = opened.session.clone();
    let report = |what: &str, t: Instant, out: &crate::edit::editor::StepOutcome| {
        let info = shown_audio(&out.view);
        let editor = env.editor(&s);
        let guard = editor.lock_state();
        let st = guard.as_ref().unwrap();
        let u = env.usage();
        println!(
            "{what}：往復 {:.1} ms（表示 {:.1} ms）・描いたノード {}・有効 {:?}・extra {:.1} MB・キャッシュ {:.1} MB・確保 {:.1} MB・\
             取り置き {} MB・持ち続ける {:.1} MB・波形の段 pending {}",
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
    // 裏の仕事（波形の段を全体から作る）を 1 回ぶん
    let work = Viewport::Audio(AudioViewport {
        work: true,
        ..match vp.clone() {
            Viewport::Audio(a) => a,
            Viewport::Image(_) => unreachable!(),
        }
    });
    let t = Instant::now();
    let out = env.step(&s, Vec::new(), &work);
    report("裏の仕事 1 回", t, &out);
    crate::edit::editor::close(&env.engine, &s);
    let u = env.usage();
    assert_eq!((u.reserved, u.interactive, u.standing), (0, 0, 0));
}
