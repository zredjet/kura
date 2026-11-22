//! M13 段 1 の音声のチャンクの受け入れテスト（E36。docs/editing.md §17）。
//!
//! - 作り方（互換の `Audio::new`・`AudioBuilder`・`Audio::from_chunks`）と長さ（0・1・C−1・C・C+1・2C+37）と NaN で、
//!   `kura-pcm-v1` のダイジェスト（式は凍結。標本を 1 つずつ足したものと同じ）と等しさが変わらない。
//! - 無効なチャンクのある値（範囲の描画の値）はダイジェストを断る。
//! - 音声の源（WAV・AIFF・アンカー。全ての符号化・1〜8 チャンネル・フィクスチャ）を、ランダムな順のチャンクで読んだ値が
//!   `decode` と同じ。源の値のダイジェストが読み取りの記録の `value_digest` と同じ。LRU が上限を超えない。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::audio::source::{AudioSource, ChunkLru, MemorySource, Opener};
use kura_engine::codec::{CodecSet, ReadSeek};
use kura_engine::digest::{Hasher, canonical};
use kura_engine::doc::DecodeOptions;
use kura_engine::testfiles::FileGen;
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::value::{AUDIO_CHUNK, Audio, AudioBuilder, AudioMeta, Chunk, ChunkSet, LoopRange};
use kura_engine::{EngineError, Limits, NoMonitor, Value};
use rand_chacha::rand_core::Rng;

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
    // 長さだけが違えば、今の長さで切って、共有していないチャンクを報告する（M13 段 1 の C11。長さの違うチャンクは
    // 共有とみなさない）。レート・チャンネルが違えば全体
    let short = Value::Audio(Arc::new(
        Audio::new(
            8_000,
            planes(2, n - 1).into_iter().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap(),
    ));
    assert_eq!(
        refine(&va, &short, &Area::All),
        OutputChange::Samples(Spans::one(0, n as u64 - 1))
    );
    let shorter = Audio::from_chunks(
        8_000,
        3 * C,
        (0..2)
            .map(|ch| (0..3).map(|k| a.chunk(ch, k).unwrap().clone()).collect())
            .collect(),
        AudioMeta::default(),
    )
    .unwrap();
    assert_eq!(
        refine(
            &va,
            &Value::Audio(Arc::new(shorter)),
            &Area::span(2 * C as u64, n as u64)
        ),
        OutputChange::Samples(Spans::new()),
        "頭の 3 チャンクを共有して縮めた値は、何も報告しない（尻は値の長さで分かる）"
    );
    let mono = Value::Audio(Arc::new(
        Audio::new(
            8_000,
            planes(1, n).into_iter().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap(),
    ));
    assert_eq!(refine(&va, &mono, &Area::All), OutputChange::All);
}

/// バイト列を読む開き方（呼ぶたびに新しい読み口）。
fn opener_of(bytes: Vec<u8>) -> Opener {
    let bytes = Arc::new(bytes);
    Arc::new(move || -> Result<Box<dyn ReadSeek + Send>, EngineError> {
        Ok(Box::new(Cursor::new(bytes.as_ref().clone())))
    })
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

fn audio_of(v: &Value) -> &Arc<Audio> {
    match v {
        Value::Audio(a) => a,
        _ => panic!("音声ではありません"),
    }
}

/// 源として開いた値を、ランダムな順のチャンクで読んで `decode` と照らす。ダイジェストは読み取りの記録と同じ。
/// 開くときに全体を読まない（源が読んだチャンクの数は 0）ことも確かめる。
fn check_source(what: &str, bytes: Vec<u8>, format: &str, seed: u64) -> Arc<Audio> {
    let set = CodecSet::pinned();
    let (decoded, stamp) = set
        .decode(
            &mut Cursor::new(bytes.clone()),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    let opened = set
        .open_audio_source(
            &opener_of(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            4 << 20,
        )
        .unwrap_or_else(|e| panic!("{what}: {e}"))
        .unwrap_or_else(|| panic!("{what}: 源として開けない"));
    let (want, got) = (audio_of(&decoded), audio_of(&opened.value));
    assert_eq!(
        (got.rate(), got.channels(), got.frames()),
        (want.rate(), want.channels(), want.frames()),
        "{what}"
    );
    assert_eq!(got.meta, want.meta, "{what}");
    assert!(got.is_complete(), "{what}");
    // ランダムな順（ChaCha8 で並べ替えた (チャンネル, チャンク)）
    let mut order: Vec<(usize, usize)> = (0..got.channels())
        .flat_map(|ch| (0..got.chunk_count()).map(move |k| (ch, k)))
        .collect();
    let mut r = kura_engine::determinism::rng(seed);
    for i in (1..order.len()).rev() {
        let j = (r.next_u32() as usize) % (i + 1);
        order.swap(i, j);
    }
    for (ch, k) in order {
        let (g, w) = (
            got.chunk_data(ch, k).unwrap(),
            want.chunk_data(ch, k).unwrap(),
        );
        assert!(
            g.len() == w.len()
                && g.iter()
                    .zip(w.iter())
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{what}: チャンネル {ch}・チャンク {k}"
        );
    }
    let digest = opened.value.digest().unwrap();
    assert_eq!(digest, stamp.value_digest, "{what}");
    assert_eq!(opened.decoder, stamp.codec, "{what}");
    if let Some(rec) = &opened.recorded {
        assert_eq!(rec.value_digest, digest, "{what}：記録した元の読み取り");
    }
    got.clone()
}

/// E36：WAV（PCM の 8・16・24・32 ビット、浮動小数の 32・64 ビット、拡張形式、大きさの分からない data、メタデータ）と
/// AIFF（8・16・24・32 ビット・fl32）と、他のツールで作ったフィクスチャを、1〜8 チャンネル・チャンクの境をまたぐ長さで
/// 源として開くと、ランダムな順のチャンクで読んだ値が `decode` と同じで、ダイジェストも読み取りの記録と同じ。
#[test]
fn pcm_sources_read_the_decoded_values() {
    let mut seed = 0u64;
    let mut next = || {
        seed += 1;
        seed
    };
    for (tag, bits) in [(1u16, 8u16), (1, 16), (1, 24), (1, 32), (3, 32), (3, 64)] {
        for (channels, frames, extensible, unknown_size) in [
            (1u16, 3_001, false, false),
            (2, 2_000, true, false),
            (3, 1_000, false, true),
            (8, 700, true, false),
            (2, 0, false, false),
        ] {
            let bytes = FileGen::Wav {
                tag,
                bits,
                channels,
                rate: 44_100,
                frames,
                extensible,
                smpl: (frames > 10).then_some((3, 9)),
                cues: vec![(1, 5)],
                labels: vec![(1, "a".into())],
                info: vec![("INAM".into(), "t".into())],
                unknown_size,
                seed: next(),
            }
            .bytes()
            .unwrap();
            check_source(
                &format!("wav {tag}/{bits} {channels}ch {frames}"),
                bytes,
                "wav",
                next(),
            );
        }
    }
    // 他の作り手（FileGen）の WAV でチャンクの境をまたぐもの（作るのが遅いので 1 つだけ）
    let long = FileGen::Wav {
        tag: 1,
        bits: 24,
        channels: 1,
        rate: 8_000,
        frames: (C + 37) as u32,
        extensible: true,
        smpl: Some((10, 70_000)),
        cues: vec![],
        labels: vec![],
        info: vec![],
        unknown_size: false,
        seed: 77,
    }
    .bytes()
    .unwrap();
    check_source("wav 24 C+37", long, "wav", next());
    let base = |channels: u16| {
        InputGen::WithMeta {
            base: Box::new(InputGen::AudioNoise {
                rate: 48_000,
                channels,
                frames: (2 * C + 37) as u64,
                seed: u64::from(channels),
            }),
            meta: MetaGen {
                loop_range: Some(LoopRange {
                    start: 100,
                    end: 70_000,
                }),
                markers: vec![(5, "m".into())],
                ..MetaGen::default()
            },
        }
        .generate()
        .unwrap()
    };
    for channels in [1u16, 2, 6] {
        let v = base(channels);
        for opts in [
            serde_json::json!({ "bits": 8 }),
            serde_json::json!({ "bits": 16 }),
            serde_json::json!({ "bits": 24 }),
            serde_json::json!({ "bits": 32 }),
            serde_json::json!({ "float": true }),
        ] {
            for format in ["wav", "aiff"] {
                let bytes = CodecSet::pinned()
                    .encode_bytes(&v, format, &opts, &NoMonitor)
                    .unwrap()
                    .0;
                check_source(
                    &format!("{format} {opts} {channels}ch"),
                    bytes,
                    format,
                    next(),
                );
            }
        }
    }
    for (name, format) in [
        ("aifc-f32.aif", "aiff"),
        ("aiff-24.aiff", "aiff"),
        ("audio-src16.wav", "wav"),
        ("audio-src24.wav", "wav"),
    ] {
        check_source(name, fixture(name), format, next());
    }
    // 源として開けない形式は None（呼び手は作業用のファイルへ回す）
    let flac = CodecSet::pinned()
        .encode_bytes(&base(2), "flac", &serde_json::json!({}), &NoMonitor)
        .unwrap()
        .0;
    assert!(
        CodecSet::pinned()
            .open_audio_source(
                &opener_of(flac),
                "flac",
                &DecodeOptions::default(),
                &Limits::default(),
                1 << 20
            )
            .unwrap()
            .is_none()
    );
}

/// AIFF-C のファイルを手で組む（FVER・COMM（圧縮 `kind`・`numSampleFrames` は `frames`）・SSND）。
fn aifc_bytes(kind: &[u8; 4], channels: u16, bits: u16, frames: u32, data: &[u8]) -> Vec<u8> {
    let mut comm = Vec::new();
    comm.extend_from_slice(&channels.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&bits.to_be_bytes());
    comm.extend_from_slice(&kura_engine::codec::aiff::write_ext80(8_000));
    comm.extend_from_slice(kind);
    comm.extend_from_slice(&[0, 0]); // 空の pstring ＋ 詰め物
    let mut body = b"AIFC".to_vec();
    let mut put = |id: &[u8; 4], b: &[u8]| {
        body.extend_from_slice(id);
        body.extend_from_slice(&(b.len() as u32).to_be_bytes());
        body.extend_from_slice(b);
        if b.len() % 2 == 1 {
            body.push(0);
        }
    };
    put(b"FVER", &0xA280_5140u32.to_be_bytes());
    put(b"COMM", &comm);
    let mut ssnd = vec![0u8; 8];
    ssnd.extend_from_slice(data);
    put(b"SSND", &ssnd);
    let mut out = b"FORM".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// 検証の直し（M13 段 1。E36 の「AIFF のすべての符号化」）：AIFF-C の圧縮 `sowt`（16・8 ビット）・`raw `・`twos`（12 ビットは
/// 2 バイトの入れ物）・`NONE`（20 ビットは 3 バイトの入れ物）・`in24`・`in32`・`fl32`/`FL32`・`fl64`/`FL64` を手で組み、
/// 源として開いた値が `decode` と同じで、どちらも別に求めた標本の値（入れ物の幅で割る `int_to_f32` の式・f64 → f32 の丸め）と
/// ビット単位で同じ。長さはチャンクの境をまたぐ（C+37）。`ulaw`・`alaw` は decode が `Unsupported` で、源は開かない（None）。
#[test]
fn aiff_c_encodings_read_known_values() {
    let frames = C + 37;
    let mut r = kura_engine::determinism::rng(36);
    // 標本 i（チャンネルを並べた順）の（バイト列, 値）。チャンクの境の前後に端の値を置く
    type Sample = Box<dyn Fn(usize, u32) -> (Vec<u8>, f32)>;
    let edge = |i: usize, ch: usize| {
        let f = i / ch;
        (C - 2..C + 2).contains(&f) || f < 2
    };
    let cases: Vec<(&str, &[u8; 4], u16, Sample)> = vec![
        (
            "sowt16",
            b"sowt",
            16,
            Box::new(|i, u| {
                let v = match i % 7 {
                    0 => i16::MIN,
                    1 => i16::MAX,
                    _ => u as i16,
                };
                (v.to_le_bytes().to_vec(), f32::from(v) / 32_768.0)
            }),
        ),
        (
            "sowt8",
            b"sowt",
            8,
            Box::new(|i, u| {
                let v = if i % 5 == 0 { i8::MIN } else { u as i8 };
                (vec![v as u8], f32::from(v) / 128.0)
            }),
        ),
        (
            "raw8",
            b"raw ",
            8,
            Box::new(|_, u| {
                let v = u as u8;
                (vec![v], (i32::from(v) - 128) as f32 / 128.0)
            }),
        ),
        (
            "twos12",
            b"twos",
            12,
            Box::new(|_, u| {
                // 12 ビットは 2 バイトの入れ物に左詰め
                let v = ((u as i16) >> 4) << 4;
                (v.to_be_bytes().to_vec(), f32::from(v) / 32_768.0)
            }),
        ),
        (
            "none20",
            b"NONE",
            20,
            Box::new(|_, u| {
                let v = ((u as i32) >> 12) << 4; // 20 ビットを 3 バイトの入れ物に左詰め（24 ビットの整数）
                let b = v.to_be_bytes();
                (vec![b[1], b[2], b[3]], v as f32 / 8_388_608.0)
            }),
        ),
        (
            "in24",
            b"in24",
            24,
            Box::new(|i, u| {
                let v = match i % 9 {
                    0 => -(1 << 23),
                    1 => (1 << 23) - 1,
                    _ => (u as i32) >> 8,
                };
                let b = v.to_be_bytes();
                (vec![b[1], b[2], b[3]], v as f32 / 8_388_608.0)
            }),
        ),
        (
            "in32",
            b"in32",
            32,
            Box::new(|i, u| {
                let v = match i % 11 {
                    0 => i32::MIN,
                    1 => i32::MAX,
                    _ => u as i32,
                };
                (
                    v.to_be_bytes().to_vec(),
                    (f64::from(v) / 2_147_483_648.0) as f32,
                )
            }),
        ),
        (
            "fl32",
            b"fl32",
            32,
            Box::new(|i, u| {
                let v = match i % 13 {
                    0 => f32::NAN,
                    1 => f32::NEG_INFINITY,
                    2 => -0.0,
                    _ => (f64::from(u) / 4_294_967_296.0 - 0.5) as f32,
                };
                (v.to_be_bytes().to_vec(), v)
            }),
        ),
        (
            "FL32",
            b"FL32",
            32,
            Box::new(|_, u| {
                let v = (f64::from(u) / 4_294_967_296.0 * 3.0 - 1.5) as f32;
                (v.to_be_bytes().to_vec(), v)
            }),
        ),
        (
            "fl64",
            b"fl64",
            64,
            Box::new(|i, u| {
                let v = match i % 13 {
                    0 => f64::NAN,
                    1 => f64::INFINITY,
                    2 => 1e300,
                    3 => -1e-320,
                    4 => -0.0,
                    _ => f64::from(u) / 4_294_967_296.0 - 0.5 + 1e-12,
                };
                (v.to_be_bytes().to_vec(), v as f32)
            }),
        ),
        (
            "FL64",
            b"FL64",
            64,
            Box::new(|_, u| {
                let v = f64::from(u) / 1_048_576.0 - 2_048.0;
                (v.to_be_bytes().to_vec(), v as f32)
            }),
        ),
    ];
    for (k, (name, kind, bits, sample)) in cases.iter().enumerate() {
        let channels = 1 + (k % 3);
        let mut data = Vec::new();
        let mut want = vec![Vec::with_capacity(frames); channels];
        for i in 0..frames * channels {
            let u = if edge(i, channels) {
                [0, u32::MAX, 1 << 31, (1 << 31) - 1][i % 4]
            } else {
                r.next_u32()
            };
            let (b, v) = sample(i, u);
            data.extend_from_slice(&b);
            want[i % channels].push(v);
        }
        let bytes = aifc_bytes(kind, channels as u16, *bits, frames as u32, &data);
        let got = check_source(&format!("aiff-c {name}"), bytes, "aiff", k as u64);
        assert_eq!((got.channels(), got.frames()), (channels, frames), "{name}");
        let planes = got.read_planes(0, frames).unwrap();
        for (ch, (g, w)) in planes.iter().zip(&want).enumerate() {
            for (i, (a, b)) in g.iter().zip(w).enumerate() {
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "{name}：チャンネル {ch}・フレーム {i}（{a} と {b}）"
                );
            }
        }
    }
    // 読まない圧縮：decode は Unsupported、源は開かない
    for kind in [b"ulaw", b"alaw"] {
        let bytes = aifc_bytes(kind, 1, 16, 100, &[0u8; 100]);
        let set = CodecSet::pinned();
        let d = set.decode(
            &mut Cursor::new(bytes.clone()),
            "aiff",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        );
        assert!(matches!(d, Err(EngineError::Unsupported(_))), "{kind:?}");
        let o = set
            .open_audio_source(
                &opener_of(bytes),
                "aiff",
                &DecodeOptions::default(),
                &Limits::default(),
                1 << 20,
            )
            .unwrap();
        assert!(o.is_none(), "{kind:?}");
    }
}

/// E36：音声のアンカー（その場で書いたもの・凍結したフィクスチャ）を源として開くと、ヘッダのメタデータの値になり、
/// ダイジェストはヘッダに記録した元の読み取りの `value_digest` と同じ。画像のアンカーは None。
#[test]
fn anchor_sources_read_the_recorded_value() {
    let mut seed = 100;
    for input in [
        InputGen::AudioNoise {
            rate: 22_050,
            channels: 2,
            frames: (C + 77) as u64,
            seed: 7,
        },
        InputGen::WithMeta {
            base: Box::new(InputGen::Sine {
                rate: 8_000,
                channels: 1,
                frames: (2 * C + 1) as u64,
                freq: 440,
                loop_range: None,
            }),
            meta: MetaGen {
                loop_range: Some(LoopRange { start: 1, end: 9 }),
                ..MetaGen::default()
            },
        },
    ] {
        let bytes = FileGen::Anchor {
            input: input.clone(),
        }
        .bytes()
        .unwrap();
        seed += 1;
        let got = check_source(&format!("anchor {input:?}"), bytes, "anchor", seed);
        assert_eq!(Value::Audio(got), input.generate().unwrap());
    }
    for name in [
        "anchor-v1-audio.wav",
        "anchor-v1-audio-stereo.wav",
        "anchor-v1-audio-6ch.wav",
        "anchor-v1-audio-mono.wav",
    ] {
        seed += 1;
        check_source(name, fixture(name), "anchor", seed);
    }
    let image = FileGen::Anchor {
        input: InputGen::Noise {
            w: 5,
            h: 3,
            fmt: kura_engine::value::PixelFormat::Rgb8,
            seed: 1,
        },
    }
    .bytes()
    .unwrap();
    assert!(
        CodecSet::pinned()
            .open_audio_source(
                &opener_of(image),
                "anchor",
                &DecodeOptions::default(),
                &Limits::default(),
                1 << 20
            )
            .unwrap()
            .is_none()
    );
}

/// `ChunkLru::get_or_load` の読み手の終わり方（[`race`] の閉包）。
#[derive(Clone, Copy, PartialEq, Debug)]
enum FirstLoad {
    Ok,
    Err,
    Panic,
}

/// [`race`] の読み手：呼ばれた数・同時に読んでいる数の最大を数え、少し待ってから返す（1 回目だけ `first` の終わり方）。
struct Loader {
    first: FirstLoad,
    calls: std::sync::atomic::AtomicUsize,
    inflight: std::sync::atomic::AtomicUsize,
    max_inflight: std::sync::atomic::AtomicUsize,
}

impl Loader {
    fn new(first: FirstLoad) -> Self {
        Self {
            first,
            calls: Default::default(),
            inflight: Default::default(),
            max_inflight: Default::default(),
        }
    }

    fn load(&self) -> Result<kura_engine::audio::source::ChunkPlanes, EngineError> {
        use std::sync::atomic::Ordering::SeqCst;
        let k = self.calls.fetch_add(1, SeqCst);
        let now = self.inflight.fetch_add(1, SeqCst) + 1;
        self.max_inflight.fetch_max(now, SeqCst);
        // ほかの仕事が同じ番号を読みに来て待つ間（読みの重なり）を作る
        std::thread::sleep(std::time::Duration::from_millis(30));
        self.inflight.fetch_sub(1, SeqCst);
        match (k, self.first) {
            (0, FirstLoad::Err) => Err(EngineError::Internal("読みの失敗（テスト）".into())),
            (0, FirstLoad::Panic) => panic!("読みの panic（テスト）"),
            _ => Ok((0..2).map(|_| Arc::new(vec![k as f32; 1_000])).collect()),
        }
    }
}

/// `threads` 個のスレッドが同時に `lru.get_or_load(7, …)` を呼んだ結果（Ok は（1 つ目の標本, 当たり）、Err は文言、panic は
/// "panic"）。どの呼び手も 20 秒以内に戻らなければ失敗にする（待つ側が永久に待たないこと）。
#[allow(clippy::disallowed_methods)] // 並列の読みを試すので std::thread を使う（描画の外のテスト）
fn race(
    lru: &Arc<ChunkLru>,
    loader: &Arc<Loader>,
    threads: usize,
) -> Vec<Result<(f32, bool), String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let start = Arc::new(std::sync::Barrier::new(threads));
    for _ in 0..threads {
        let (lru, loader, tx, start) = (lru.clone(), loader.clone(), tx.clone(), start.clone());
        std::thread::spawn(move || {
            start.wait();
            let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lru.get_or_load(7, || loader.load())
            }));
            let out = match got {
                Ok(Ok((planes, hit))) => Ok((planes[0][0], hit)),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("panic".to_string()),
            };
            let _ = tx.send(out);
        });
    }
    (0..threads)
        .map(|i| {
            rx.recv_timeout(std::time::Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("{i} 番目の呼び手が戻らない（待ちが解けない）"))
        })
        .collect()
}

/// 検証の直し（M13 段 1）：`ChunkLru::get_or_load` は、同じ番号を同時に読もうとしたら 1 回だけ読み、ほかは待って同じ値を
/// 使う。読みが Err・panic で終わっても待つ側を起こし（永久に待たない）、次の 1 つが読み直す。上限を超えるチャンク（LRU に
/// 入らない）は待つ側が自分で読むが、同じ番号を同時に 2 つは読まない。
#[test]
fn chunk_lru_loads_once_and_wakes_waiters() {
    use std::sync::atomic::Ordering::SeqCst;
    let threads = 16;
    let big = 2 * 4 * 1_000;
    // 成功：読みは 1 回、全員が同じ値、当たりは 1 回目のほかの全員
    let lru = Arc::new(ChunkLru::new(4 * big));
    let loader = Arc::new(Loader::new(FirstLoad::Ok));
    let got = race(&lru, &loader, threads);
    assert_eq!(loader.calls.load(SeqCst), 1);
    assert!(
        got.iter().all(|g| matches!(g, Ok((v, _)) if *v == 0.0)),
        "{got:?}"
    );
    assert_eq!(
        got.iter().filter(|g| matches!(g, Ok((_, false)))).count(),
        1
    );
    // 成功のあとは当たる（読まない）
    let again = race(&lru, &loader, 4);
    assert!(
        again.iter().all(|g| matches!(g, Ok((0.0, true)))),
        "{again:?}"
    );
    assert_eq!(loader.calls.load(SeqCst), 1);
    // 1 回目が Err・panic：その呼び手だけが失敗し、待っていた側の 1 つが読み直して、残りはそれに当たる
    for first in [FirstLoad::Err, FirstLoad::Panic] {
        let lru = Arc::new(ChunkLru::new(4 * big));
        let loader = Arc::new(Loader::new(first));
        let got = race(&lru, &loader, threads);
        let failed = got.iter().filter(|g| g.is_err()).count();
        assert_eq!(failed, 1, "{first:?}: {got:?}");
        assert!(
            got.iter()
                .filter_map(|g| g.as_ref().ok())
                .all(|(v, _)| *v == 1.0),
            "{first:?}: {got:?}"
        );
        assert_eq!(loader.calls.load(SeqCst), 2, "{first:?}");
        assert_eq!(loader.max_inflight.load(SeqCst), 1, "{first:?}");
        if first == FirstLoad::Panic {
            assert!(got.iter().any(|g| g.as_ref().is_err_and(|m| m == "panic")));
        }
    }
    // 上限を超えるチャンク：LRU に入らないので、待っていた側がそれぞれ読む（同時に読むのは 1 つだけ）
    let lru = Arc::new(ChunkLru::new(big / 2));
    let loader = Arc::new(Loader::new(FirstLoad::Ok));
    let got = race(&lru, &loader, 8);
    assert!(got.iter().all(|g| matches!(g, Ok((_, false)))), "{got:?}");
    assert_eq!(loader.calls.load(SeqCst), 8);
    assert_eq!(loader.max_inflight.load(SeqCst), 1);
    assert_eq!(lru.resident(), 0);
}

/// E36：源の LRU は上限を超えない（1 つで上限を超えるものは入れない）。ファイルの源は、開くときに読まず、同じチャンクは
/// LRU に当たり、上限の小さい源でも値は同じ（読み直すだけ）。値の源（`MemorySource`）は実体を写さずに返す。
#[test]
fn source_lru_stays_within_its_capacity() {
    let chunk = |n: usize, v: f32| vec![Arc::new(vec![v; n]), Arc::new(vec![v; n])];
    let lru = ChunkLru::new(3 * 2 * 4 * 1_000);
    let mut r = kura_engine::determinism::rng(9);
    for step in 0..500u32 {
        let i = r.next_u32() % 10;
        let n = 1 + (r.next_u32() % 1_500) as usize;
        if step % 3 == 0 {
            lru.insert(i, chunk(n, i as f32));
        } else if let Some(got) = lru.get(i) {
            assert!(got.iter().all(|c| c.iter().all(|v| *v == i as f32)));
        }
        assert!(lru.resident() <= lru.capacity(), "{step}");
    }
    lru.insert(99, chunk(10_000, 1.0));
    assert!(lru.get(99).is_none(), "上限を超える 1 つは入れない");

    // ファイルの源：f32 の WAV（ステレオ・4 チャンクと少し）
    let frames = 4 * C + 3;
    let noise = InputGen::AudioNoise {
        rate: 48_000,
        channels: 2,
        frames: frames as u64,
        seed: 3,
    }
    .generate()
    .unwrap();
    let bytes = CodecSet::pinned()
        .encode_bytes(
            &noise,
            "wav",
            &serde_json::json!({ "float": true }),
            &NoMonitor,
        )
        .unwrap()
        .0;
    let decoded = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes.clone()),
            "wav",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap()
        .0;
    // LRU は 2 チャンク分（ステレオ）
    let cap = 2 * 2 * C as u64 * 4;
    let opened = CodecSet::pinned()
        .open_audio_source(
            &opener_of(bytes),
            "wav",
            &DecodeOptions::default(),
            &Limits::default(),
            cap,
        )
        .unwrap()
        .unwrap();
    let lazy = audio_of(&opened.value).as_ref().clone();
    let Chunk::Source(r) = lazy.chunk(0, 0).unwrap() else {
        panic!("源の参照ではありません")
    };
    let src = r.src.clone();
    assert_eq!(src.stats().reads, 0, "開くときに読まない");
    assert_eq!(src.rate(), 48_000);
    assert_eq!(src.resident_bytes(), cap);
    for k in [0usize, 0, 1, 2, 3, 4, 0, 4, 4] {
        for ch in 0..2 {
            let (g, w) = (
                lazy.chunk_data(ch, k).unwrap(),
                audio_of(&decoded).chunk_data(ch, k).unwrap(),
            );
            assert_eq!(&g[..], &w[..]);
        }
    }
    let s = src.stats();
    assert!(s.hits > 0 && s.reads >= 5, "{s:?}");
    assert_eq!(Value::Audio(Arc::new(lazy.clone())), decoded);
    // 遅延の値の予算の数え：源 1 つにつき LRU の上限を 1 回（値のチャンクの数によらない）
    assert_eq!(lazy.resident_bytes(), 0);

    // 値の源
    let base = audio_of(&decoded).clone();
    let mem = Arc::new(MemorySource::new(base.clone()).unwrap());
    let lazy = Audio::from_source(mem.clone(), base.meta.clone()).unwrap();
    let Chunk::Data(want) = base.chunk(1, 2).unwrap() else {
        panic!()
    };
    let got = mem.read_chunk(2).unwrap();
    assert!(Arc::ptr_eq(&got[1], want), "値の源は実体を写さない");
    assert_eq!(lazy, *base);
    assert!(lazy.same_chunk(&lazy.with_meta(AudioMeta::default()), 0, 0));
    assert!(!lazy.same_chunk(&base, 0, 0));
    assert!(MemorySource::new(Arc::new(base.keep_only(&ChunkSet::empty(5)))).is_err());
}

/// 読みを数える安い源（チャンク `k` のチャンネル `ch` は `k + ch / 10` の定数。LRU は全体が収まらないファイルの源と同じ `for_channels`）。
struct CheapSource {
    channels: usize,
    frames: u64,
    lru: ChunkLru,
    loads: std::sync::atomic::AtomicU64,
}

impl AudioSource for CheapSource {
    fn rate(&self) -> u32 {
        8_000
    }
    fn channels(&self) -> usize {
        self.channels
    }
    fn frames(&self) -> u64 {
        self.frames
    }
    fn read_chunk(&self, index: u32) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        let (planes, _) = self.lru.get_or_load(index, || {
            self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (a, b) = kura_engine::audio::source::chunk_span(self.frames, index).unwrap();
            Ok((0..self.channels)
                .map(|ch| Arc::new(vec![index as f32 + ch as f32 / 10.0; (b - a) as usize]))
                .collect())
        })?;
        Ok(planes)
    }
    fn resident_bytes(&self) -> u64 {
        self.lru.capacity()
    }
}

/// 流れの窓を集める受け手。
struct Collect(Vec<Vec<f32>>);

impl kura_engine::audio::stream::AudioSink for Collect {
    fn begin(&mut self, info: &kura_engine::audio::stream::StreamInfo) -> Result<(), EngineError> {
        self.0 = vec![Vec::new(); info.channels];
        Ok(())
    }
    fn feed(
        &mut self,
        _start: u64,
        planes: &[&[f32]],
        _: &dyn kura_engine::Monitor,
    ) -> Result<(), EngineError> {
        for (o, p) in self.0.iter_mut().zip(planes) {
            o.extend_from_slice(p);
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// M13 段 7：流して読む読み（`read_through`）は源の LRU の主の段を変えない。手の読みで入れた見えている区間のチャンクは、
/// 計画の出力（gain）を全体に流したあとも同じ順で残り（核の読みは `par_blocks` の仕事のスレッドで、読み方を引き継ぐ）、
/// どのチャンクも 1 回だけ読む（チャンネルごとの読みを流しの段がまとめる）。流した標本は手の読みで流したものとビット単位で
/// 同じ。手の読みで流すと、見えている区間は追い出される（直す前の形）。
#[test]
fn read_through_streams_leave_the_kept_chunks_alone() {
    use kura_engine::audio::source::{ReadMode, read_mode};
    use kura_engine::doc::{Graph, InputId, OutputId};
    use kura_engine::render::Engine;
    use kura_engine::render::cache::{Fingerprint, FingerprintedInput, NodeCache};

    let (channels, chunks) = (2usize, 65u64);
    let frames = (chunks - 1) * C as u64 + 123;
    let one = channels as u64 * C as u64 * 4;
    // 流しの段は 18 チャンク（窓 16 ＋ 2）、主の段は 54 チャンク（全体の 65 より小さい）
    let cap = 72 * one;
    let engine = Engine::new().unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, serde_json::json!({ "db": -3.0 }), 1)
        .unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .build()
        .unwrap();
    let run = |mode: ReadMode| {
        let src = Arc::new(CheapSource {
            channels,
            frames,
            lru: ChunkLru::for_channels(cap, channels),
            loads: Default::default(),
        });
        assert_eq!(src.lru.stream_capacity(), 18 * one);
        let a = Audio::from_source(src.clone(), AudioMeta::default()).unwrap();
        // 手の読み：見えている区間（チャンク 2..6）
        for k in 2..6 {
            for ch in 0..channels {
                a.chunk_data(ch, k).unwrap();
            }
        }
        let kept = src.lru.kept();
        assert_eq!(kept, vec![2, 3, 4, 5]);
        let input = Value::Audio(Arc::new(a));
        let plan = engine
            .compile(
                &g,
                &std::collections::BTreeMap::from([(InputId(0), input.shape())]),
            )
            .unwrap();
        let inputs = std::collections::BTreeMap::from([(
            InputId(0),
            FingerprintedInput {
                value: input,
                source: kura_engine::Digest([1; 32]),
                fingerprint: Fingerprint(kura_engine::Digest([2; 32])),
            },
        )]);
        let mut cache = NodeCache::new(0);
        let mut sink = Collect(Vec::new());
        let stats = pool.install(|| {
            kura_engine::audio::source::with_read_mode(mode, || {
                engine.stream_audio(
                    &plan,
                    &inputs,
                    &mut cache,
                    OutputId(0),
                    None,
                    &mut sink,
                    &NoMonitor,
                )
            })
        });
        assert!(stats.unwrap().windows >= 5);
        assert_eq!(read_mode(), ReadMode::Keep);
        assert!(src.lru.resident() <= cap);
        (src, kept, sink.0)
    };
    let (src, kept, through) = run(ReadMode::Through);
    assert_eq!(src.lru.kept(), kept, "主の段の中身も順も同じ");
    assert_eq!(
        src.loads.load(std::sync::atomic::Ordering::SeqCst),
        chunks,
        "どのチャンクも 1 回だけ読む"
    );
    assert_eq!(src.lru.streamed(), chunks - 4);
    let (src, _, keep) = run(ReadMode::Keep);
    assert!(
        !src.lru.kept().contains(&2),
        "手の読みで流すと見えている区間を追い出す"
    );
    assert_eq!(src.lru.streamed(), 0);
    assert_eq!(through.len(), channels);
    assert_eq!(through[0].len() as u64, frames);
    for (x, y) in through.iter().zip(&keep) {
        assert!(x.iter().zip(y).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
}
