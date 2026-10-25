//! M13 段 1 の音声のチャンクの受け入れテスト（E36。docs/editing.md §17）。
//!
//! - 作り方（互換の `Audio::new`・`AudioBuilder`・`Audio::from_chunks`）と長さ（0・1・C−1・C・C+1・2C+37）と NaN で、
//!   `kura-pcm-v1` のダイジェスト（式は凍結。標本を 1 つずつ足したものと同じ）と等しさが変わらない。
//! - 無効なチャンクのある値（範囲の描画の値）はダイジェストを断る。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use kura_engine::Value;
use kura_engine::digest::{Hasher, canonical};
use kura_engine::value::{AUDIO_CHUNK, Audio, AudioBuilder, AudioMeta, Chunk, ChunkSet, LoopRange};

const C: usize = AUDIO_CHUNK;

/// チャンネルごとの決まった中身（チャンクの境の前後に NaN・±0・∞ を置く）。
fn planes(channels: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            (0..frames)
                .map(|i| match i % C {
                    0 if i > 0 => f32::NAN,
                    1 => -0.0,
                    _ if i + 1 == frames => f32::from_bits(0x7fc0_1234),
                    _ if i == C - 1 => f32::INFINITY,
                    _ => ((i * (c + 3)) % 1_000) as f32 * 1e-3 - 0.5,
                })
                .collect()
        })
        .collect()
}

/// 式どおりに標本を 1 つずつ足したダイジェスト（docs/editing.md §8.6）。
fn formula(rate: u32, planes: &[Vec<f32>], meta: &AudioMeta) -> kura_engine::Digest {
    let mut h = Hasher::new("kura-pcm-v1");
    h.u32(rate)
        .u32(planes.len() as u32)
        .u64(planes[0].len() as u64);
    for p in planes {
        for s in p {
            let bits = if s.is_nan() { 0x7fc0_0000 } else { s.to_bits() };
            h.raw(&bits.to_le_bytes());
        }
    }
    h.bytes_with_len(&canonical(meta).unwrap());
    h.finish()
}

/// 同じ中身を、作り方を変えて組む：互換の口・組み手（いろいろな刻み）・チャンクの表。
fn variants(rate: u32, planes: &[Vec<f32>], meta: &AudioMeta) -> Vec<(&'static str, Audio)> {
    let frames = planes[0].len();
    let compat = Audio::new(
        rate,
        planes.iter().cloned().map(Arc::new).collect(),
        meta.clone(),
    )
    .unwrap();
    let mut out = vec![("new", compat)];
    for (name, step) in [
        ("builder-1", 1usize),
        ("builder-4099", 4_099),
        ("builder-C", C),
    ] {
        // 刻み 1 は長いと遅いので、短いものだけ
        if step == 1 && frames > 5_000 {
            continue;
        }
        let mut b = AudioBuilder::new(rate, planes.len(), frames as u64).unwrap();
        let mut at = 0;
        while at < frames {
            let end = (at + step).min(frames);
            let views: Vec<&[f32]> = planes.iter().map(|p| &p[at..end]).collect();
            b.push_frames(&views).unwrap();
            at = end;
        }
        out.push((name, b.finish(meta.clone()).unwrap()));
    }
    // 見込みを外した組み手（小さすぎる・大きすぎる）とインターリーブ
    let mut inter = Vec::with_capacity(frames * planes.len());
    for f in 0..frames {
        for p in planes {
            inter.push(p[f]);
        }
    }
    for (name, hint) in [("inter-hint-0", 0u64), ("inter-hint-huge", u64::MAX)] {
        let mut b = AudioBuilder::new(rate, planes.len(), hint).unwrap();
        for part in inter.chunks(planes.len() * 10_007) {
            b.push_interleaved(part).unwrap();
        }
        out.push((name, b.finish(meta.clone()).unwrap()));
    }
    let table: Vec<Vec<Chunk>> = planes
        .iter()
        .map(|p| {
            p.chunks(C)
                .map(|c| Chunk::Data(Arc::new(c.to_vec())))
                .collect()
        })
        .collect();
    out.push((
        "from_chunks",
        Audio::from_chunks(rate, frames, table, meta.clone()).unwrap(),
    ));
    out
}

/// E36：作り方と長さ（チャンクの境の前後）と NaN によらず、ダイジェストは式どおりで、値は等しく、読む口は同じ中身を返す。
#[test]
fn chunked_values_keep_the_digest_and_equality() {
    let meta = AudioMeta {
        loop_range: Some(LoopRange { start: 0, end: 1 }),
        ..AudioMeta::default()
    };
    for (channels, frames) in [
        (1usize, 0usize),
        (2, 1),
        (1, C - 1),
        (3, C),
        (2, C + 1),
        (2, 2 * C + 37),
    ] {
        let ps = planes(channels, frames);
        let want = formula(44_100, &ps, &meta);
        let vs = variants(44_100, &ps, &meta);
        let first = vs[0].1.clone();
        for (name, a) in vs {
            let what = format!("{name} {channels}ch {frames}");
            assert_eq!(a.frames(), frames, "{what}");
            assert_eq!(a.chunk_count(), frames.div_ceil(C), "{what}");
            assert_eq!(
                Value::Audio(Arc::new(a.clone())).digest().unwrap(),
                want,
                "{what}"
            );
            // 中身（NaN の符号化まで）が同じ
            let got = a.to_planes().unwrap();
            for (g, w) in got.iter().zip(&ps) {
                assert!(
                    g.iter().zip(w).all(|(x, y)| x.to_bits() == y.to_bits()),
                    "{what}"
                );
            }
            // 共有していない実体の比べは f32 の比べ（NaN ≠ NaN）なので、NaN を含まない長さだけ照らす
            if !ps.iter().flatten().any(|v| v.is_nan()) {
                assert_eq!(a, first, "{what}");
            }
        }
        // 等しさ：NaN を有限の値に替えた中身で、どの作り方も等しく、1 標本でも違えば等しくない
        let clean: Vec<Vec<f32>> = ps
            .iter()
            .map(|p| {
                p.iter()
                    .map(|v| if v.is_nan() { 0.125 } else { *v })
                    .collect()
            })
            .collect();
        let vs = variants(44_100, &clean, &meta);
        for (name, a) in &vs {
            assert_eq!(a, &vs[0].1, "{name} {channels}ch {frames}");
        }
        if frames > 0 {
            let mut changed = clean.clone();
            changed[channels - 1][frames - 1] += 1.0;
            assert_ne!(variants(44_100, &changed, &meta)[0].1, vs[0].1);
        }
    }
}

/// 無効なチャンクのある値（範囲の描画で作った値）はダイジェストを断り、全部を有効に戻せば同じダイジェストになる。
#[test]
fn partial_values_refuse_the_digest() {
    let ps = planes(2, 2 * C + 37);
    let a = Audio::new(
        48_000,
        ps.iter().cloned().map(Arc::new).collect(),
        AudioMeta::default(),
    )
    .unwrap();
    let want = Value::Audio(Arc::new(a.clone())).digest().unwrap();
    let mut keep = ChunkSet::all(3);
    keep.remove(1);
    let part = Value::Audio(Arc::new(a.keep_only(&keep)));
    assert!(!part.is_complete());
    assert!(part.digest().is_err());
    let back = Value::Audio(Arc::new(a.invalidate(&ChunkSet::empty(3))));
    assert_eq!(back.digest().unwrap(), want);
    assert!(!part.holes_read());
    let Value::Audio(p) = &part else { panic!() };
    let mut out = vec![0f32; 4];
    assert!(p.read(0, C as u64, &mut out).is_err());
    assert!(part.holes_read());
}

/// 変わった区間の絞り込み（`refine`）はチャンク単位：推論の区間のうち、前後で全チャンネルが同じ実体のチャンクを外す。
/// 推論が全体でも、共有したチャンクは報告しない。無効なチャンク（表示の外）は推論のまま報告する。長さが違えば全体。
#[test]
fn refine_reports_only_unshared_chunks() {
    use kura_engine::render::delta::{OutputChange, refine};
    use kura_engine::render::region::{Area, Spans};
    let n = 3 * C + 10;
    let a = Audio::new(
        8_000,
        planes(2, n).into_iter().map(Arc::new).collect(),
        AudioMeta::default(),
    )
    .unwrap();
    // チャンク 1 だけを差し替えた値（ほかは共有）
    let mut table: Vec<Vec<Chunk>> = Vec::new();
    for ch in 0..2 {
        let mut row = Vec::new();
        for k in 0..a.chunk_count() {
            row.push(if k == 1 {
                Chunk::Data(Arc::new(vec![0.5; C]))
            } else {
                a.chunk(ch, k).unwrap().clone()
            });
        }
        table.push(row);
    }
    let b = Audio::from_chunks(8_000, n, table, AudioMeta::default()).unwrap();
    let (va, vb) = (Value::Audio(Arc::new(a.clone())), Value::Audio(Arc::new(b)));
    assert_eq!(
        refine(&va, &vb, &Area::All),
        OutputChange::Samples(Spans::one(C as u64, 2 * C as u64))
    );
    // 推論が狭ければ、その中だけ
    let mut s = Spans::new();
    s.push(10, 20);
    s.push(C as u64 + 5, C as u64 + 9);
    assert_eq!(
        refine(&va, &vb, &Area::Samples(s)),
        OutputChange::Samples(Spans::one(C as u64 + 5, C as u64 + 9))
    );
    assert_eq!(
        refine(&va, &va.clone(), &Area::All),
        OutputChange::Samples(Spans::new())
    );
    // 無効なチャンクは推論のまま報告する
    let mut keep = ChunkSet::all(4);
    keep.remove(3);
    let part = Value::Audio(Arc::new(a.keep_only(&keep)));
    assert_eq!(
        refine(&va, &part, &Area::All),
        OutputChange::Samples(Spans::one(3 * C as u64, n as u64))
    );
    // 長さが違えば全体
    let short = Value::Audio(Arc::new(
        Audio::new(
            8_000,
            planes(2, n - 1).into_iter().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap(),
    ));
    assert_eq!(refine(&va, &short, &Area::All), OutputChange::All);
}
