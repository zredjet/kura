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
