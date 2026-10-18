//! アンカー（docs/editing.md §9.4。`codec::anchor`）の約束を、外から確かめる。
//!
//! - 往復：実際のデコーダの値（再現を約束しない SVG と、非可逆の音声の例として Vorbis・AAC）と、ChaCha8 でランダムに作った値（画素の形式・フレームの
//!   列・音声・メタデータ）で、`write → verify → read` の値のダイジェストが元の読み取りの記録と同じ。
//! - 凍結したアンカー（`fixtures/anchor-v1-*`）：版 1 の入れ物を、今の読み手が同じ値に読む（アンカーは消えないので、
//!   書き手を変えても古いアンカーを読み続ける）。足りなければ `KURA_BLESS=1` で作る（作ったものは変えない）。
//! - ヘッダの形の凍結：書くヘッダの正規の JSON（形を変えるなら `v` を上げる）。
//! - 壊れた入力：ChaCha8 で崩したアンカーを `testing::fuzz_decode` に通し、自前のコードの panic・`Internal`・形と中身の
//!   食い違いがない。
//! - 速さと大きさ（ふだんは回さない）：`cargo test --release -p kura-engine --test anchor -- --ignored --nocapture`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::codec::anchor::{self, AnchorContent, AnchorContext};
use kura_engine::codec::{CodecEnv, CodecSet, DecodeStamp, Tier};
use kura_engine::digest::Digest;
use kura_engine::doc::DecodeOptions;
use kura_engine::geom::{IPoint, IRect};
use kura_engine::testfiles::FileGen;
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::FakePlatformDecoder;
use kura_engine::value::{
    Audio, AudioMeta, AudioSource, Frame, Frames, Grid, Image, LoopRange, Marker, PixelFormat,
    Slice,
};
use kura_engine::{EngineError, Limits, NoMonitor, Value};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;

fn fake_stamp(v: &Value) -> DecodeStamp {
    DecodeStamp {
        codec: "fake-platform-dec/1".into(),
        tier: Tier::Platform,
        reproducible: false,
        value_digest: v.digest().unwrap(),
        env: Some(FakePlatformDecoder::default_env()),
    }
}

/// 書いて読み直し（`write_verified`）、もう一度読んで、値のダイジェストが記録と同じことを確かめる。
fn round_trip(v: &Value, stamp: &DecodeStamp) -> Vec<u8> {
    let cx = AnchorContext {
        source: Some(Digest([0xAB; 32])),
        format: Some("x".into()),
        options: DecodeOptions::default(),
    };
    let (bytes, s) = anchor::write_verified(v, stamp, &cx, &Limits::default(), &NoMonitor).unwrap();
    assert_eq!(s.value_digest, stamp.value_digest);
    let r = anchor::read(&mut Cursor::new(&bytes), &Limits::default(), &NoMonitor).unwrap();
    assert_eq!(r.value.digest().unwrap(), stamp.value_digest);
    assert_eq!(r.header.decoded, *stamp);
    bytes
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// 実際のデコーダの値（再現を約束しない SVG と、非可逆の音声の例として Vorbis・AAC。読めたものだけ。Vorbis・AAC は段 9 から
/// 再現でき、アンカーは要らないが、入れ物の往復の例として使う）：アンカーが要るかは記録の `reproducible` の否定で、メタデータ（`source`・`vorbis:` のタグ・ループ点）ごと往復する。
#[test]
fn real_decoder_values_round_trip() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30" viewBox="0 0 8 6"><defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f" stop-opacity="0.3"/></linearGradient></defs><ellipse cx="4" cy="3" rx="3.3" ry="2.1" fill="url(#g)" transform="rotate(17 4 3)"/></svg>"##.to_vec();
    let mut inputs = vec![("svg", svg)];
    for f in [
        "ogg-vorbis-3ch.ogg",
        "ogg-vorbis-5ch.ogg",
        "ogg-vorbis-loop.ogg",
        "ogg-vorbis-ffmpeg.ogg",
        "aac-lc.aac",
        "aac-lc-afconvert.m4a",
        "aac-lc-ffmpeg.m4a",
    ] {
        let format = if f.ends_with(".ogg") {
            "ogg"
        } else if f.ends_with(".aac") {
            "aac"
        } else {
            "m4a"
        };
        inputs.push((format, fixture(f)));
    }
    let set = CodecSet::pinned();
    let mut n = 0;
    for (format, bytes) in inputs {
        let Ok((v, stamp)) = set.decode(
            &mut Cursor::new(&bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        ) else {
            continue;
        };
        assert_eq!(anchor::needs_anchor(&stamp), !stamp.reproducible);
        let anchor = round_trip(&v, &stamp);
        let back = anchor::read(&mut Cursor::new(&anchor), &Limits::default(), &NoMonitor)
            .unwrap()
            .value;
        assert_eq!(back, v, "{format}");
        n += 1;
    }
    assert!(n >= 5, "{n}");
}

fn random_text(r: &mut ChaCha8Rng) -> String {
    const PIECES: [&str; 8] = ["a", "日本語", "\0", "", "=", "🎵", "\u{1F}", "Title"];
    (0..r.next_u32() % 4)
        .map(|_| PIECES[(r.next_u32() % 8) as usize])
        .collect()
}

fn random_bytes(r: &mut ChaCha8Rng, max: u32) -> Vec<u8> {
    (0..r.next_u32() % max)
        .map(|_| r.next_u32() as u8)
        .collect()
}

/// ChaCha8 でランダムな値（画素の形式・大きさ・フレームの列・音声・メタデータ）を 200 個作り、すべて往復する。
#[test]
fn random_values_round_trip() {
    let mut r = kura_engine::determinism::rng(0xA1C0);
    for i in 0..200u32 {
        let fmt = PixelFormat::ALL[(r.next_u32() % 9) as usize];
        let (w, h) = (1 + r.next_u32() % 40, 1 + r.next_u32() % 140);
        let seed = u64::from(r.next_u32());
        let mut meta = MetaGen::default();
        if r.next_u32().is_multiple_of(2) {
            meta.icc = Some(random_bytes(&mut r, 300));
        }
        if r.next_u32().is_multiple_of(2) {
            meta.exif = Some(random_bytes(&mut r, 60));
        }
        if r.next_u32().is_multiple_of(2) {
            meta.dpi = Some((r.next_u32(), r.next_u32()));
        }
        for _ in 0..r.next_u32() % 3 {
            meta.text.push((random_text(&mut r), random_text(&mut r)));
            meta.tags.push((random_text(&mut r), random_text(&mut r)));
            meta.markers
                .push((u64::from(r.next_u32()) << 20, random_text(&mut r)));
        }
        let base = match i % 3 {
            0 => InputGen::Noise { w, h, fmt, seed },
            1 => InputGen::NoiseFrames {
                w,
                h: h % 30 + 1,
                fmt,
                count: 1 + r.next_u32() % 4,
                seed,
            },
            _ => InputGen::AudioNoise {
                rate: 1 + r.next_u32() % 192_000,
                channels: 1 + (r.next_u32() % 8) as u16,
                frames: u64::from(r.next_u32() % 40_000),
                seed,
            },
        };
        let mut v = InputGen::WithMeta {
            base: Box::new(base),
            meta,
        }
        .generate()
        .unwrap();
        // 作り方で付けられないメタデータ（格子・ピボット・スライス・アニメのタグ・表示時間・繰り返し・音声の source・ループ点）
        v = match v {
            Value::Image(img) => {
                let mut img = Arc::unwrap_or_clone(img);
                img.meta.grid = (r.next_u32().is_multiple_of(2)).then(|| Grid {
                    cell_w: r.next_u32() % 9,
                    cell_h: r.next_u32(),
                    offset_x: 0,
                    offset_y: r.next_u32() % 3,
                    gap_x: 1,
                    gap_y: 0,
                });
                img.meta.pivot = Some(IPoint {
                    x: i64::from(r.next_u32() as i32),
                    y: -1,
                });
                img.meta.slices.push(Slice {
                    name: random_text(&mut r),
                    rect: IRect::new(-3, 2, r.next_u32() % 50, 1),
                    center: None,
                    pivot: None,
                });
                Value::Image(Arc::new(img))
            }
            Value::Frames(fr) => {
                let frames: Vec<Frame> = fr
                    .frames()
                    .iter()
                    .map(|f| Frame::full(f.px.clone(), r.next_u32()))
                    .collect();
                Value::Frames(Arc::new(
                    Frames::new(frames, fr.canvas(), fr.meta.clone(), r.next_u32()).unwrap(),
                ))
            }
            Value::Audio(a) => {
                let mut meta = a.meta.clone();
                meta.source = (r.next_u32().is_multiple_of(2)).then(|| AudioSource {
                    bits: [8, 16, 24, 32][(r.next_u32() % 4) as usize],
                    float: r.next_u32().is_multiple_of(2),
                    rate: r.next_u32(),
                });
                meta.loop_range = Some(LoopRange {
                    start: u64::from(r.next_u32()),
                    end: u64::MAX,
                });
                let mut planes = a.planes().to_vec();
                if let Some(s) = Arc::make_mut(&mut planes[0]).first_mut() {
                    *s = f32::from_bits(r.next_u32());
                }
                Value::Audio(Arc::new(Audio::new(a.rate(), planes, meta).unwrap()))
            }
        };
        let stamp = fake_stamp(&v);
        round_trip(&v, &stamp);
    }
}

/// 凍結したアンカーの値（作り方。変えない）。
fn frozen_values() -> Vec<(&'static str, Value)> {
    let image = InputGen::WithMeta {
        base: Box::new(InputGen::Noise {
            w: 9,
            h: 5,
            fmt: PixelFormat::Rgba16,
            seed: 41,
        }),
        meta: MetaGen {
            dpi: Some((144, 72)),
            text: vec![("Title".into(), "ドット絵".into())],
            icc: Some((0..=255u8).collect()),
            exif: Some(kura_engine::codec::exif::minimal(6, false)),
            ..MetaGen::default()
        },
    }
    .generate()
    .unwrap();
    let image = match image {
        Value::Image(img) => {
            let mut img = Arc::unwrap_or_clone(img);
            img.meta.pivot = Some(IPoint { x: 4, y: -2 });
            Value::Image(Arc::new(img))
        }
        _ => unreachable!(),
    };
    let frames = InputGen::NoiseFrames {
        w: 12,
        h: 9,
        fmt: PixelFormat::Indexed8,
        count: 3,
        seed: 42,
    }
    .generate()
    .unwrap();
    let frames = match frames {
        Value::Frames(fr) => {
            let fs: Vec<Frame> = fr
                .frames()
                .iter()
                .zip([0u32, 70_001, 100])
                .map(|(f, ms)| Frame::full(f.px.clone(), ms))
                .collect();
            Value::Frames(Arc::new(
                Frames::new(fs, fr.canvas(), fr.meta.clone(), 3).unwrap(),
            ))
        }
        _ => unreachable!(),
    };
    let audio = InputGen::AudioNoise {
        rate: 44_100,
        channels: 3,
        frames: 500,
        seed: 43,
    }
    .generate()
    .unwrap();
    let audio = match audio {
        Value::Audio(a) => {
            let mut planes = a.planes().to_vec();
            Arc::make_mut(&mut planes[1])[0] = -0.0;
            Arc::make_mut(&mut planes[2])[7] = f32::NAN;
            Value::Audio(Arc::new(
                Audio::new(
                    a.rate(),
                    planes,
                    AudioMeta {
                        loop_range: Some(LoopRange { start: 5, end: 450 }),
                        markers: vec![Marker {
                            pos: 9,
                            name: "印".into(),
                        }],
                        source: Some(AudioSource {
                            bits: 16,
                            float: false,
                            rate: 44_100,
                        }),
                        tags: [("vorbis:ENCODER".to_string(), "Lavf".to_string())]
                            .into_iter()
                            .collect(),
                    },
                )
                .unwrap(),
            ))
        }
        _ => unreachable!(),
    };
    let mut out = vec![
        ("anchor-v1-image.png", image),
        ("anchor-v1-frames.png", frames),
        ("anchor-v1-audio.wav", audio),
    ];
    out.extend(frozen_values_golden_shapes());
    out
}

/// 段 9 の検証で足した凍結（作り方。変えない）：コーデックのゴールデン（`anchor-kura-dec@1`）の例の形と、まだ凍結して
/// いなかった入れ物の形（8 ビットのグレー・RGB(A)・GrayA、Gray16・GrayA16、パレットの静止画、EXIF のない画像（`kuEx`
/// なし）、ICC・EXIF つきのフレームの列、2 チャンネル以下の WAV（拡張形式でない）、6 チャンネル、NaN・±∞）。ゴールデンは
/// 今の書き手で書いて読むので読み手を固定しない。こちらは決まったバイト列から読めることを守る。
fn frozen_values_golden_shapes() -> Vec<(&'static str, Value)> {
    // ゴールデンの `rich` と同じメタデータ（ICC・向き 6 の EXIF・テキスト・スライス）
    let rich = MetaGen {
        dpi: Some((1, 300)),
        text: vec![
            ("".into(), "空のキー".into()),
            ("Title".into(), "ドット絵\0終わり".into()),
        ],
        icc: Some((0..=255u8).rev().collect()),
        exif: Some(kura_engine::codec::exif::minimal(6, true)),
        slices: vec![Slice {
            name: "9 分割".into(),
            rect: IRect::new(1, 1, 5, 3),
            center: Some(IRect::new(2, 2, 3, 1)),
            pivot: Some(IPoint { x: -1, y: 2 }),
        }],
        ..MetaGen::default()
    };
    let icc_only = MetaGen {
        icc: Some((0..=255u8).collect()),
        ..MetaGen::default()
    };
    let still = |fmt: PixelFormat, w: u32, h: u32, seed: u64, meta: Option<&MetaGen>| {
        let base = InputGen::Noise { w, h, fmt, seed };
        match meta {
            Some(m) => InputGen::WithMeta {
                base: Box::new(base),
                meta: m.clone(),
            },
            None => base,
        }
    };
    let gens: Vec<(&'static str, InputGen)> = vec![
        // ゴールデンの anchor-gray8・rgba8・rgb16・indexed8 と同じ値
        (
            "anchor-v1-gray8.png",
            still(PixelFormat::Gray8, 19, 70, 31, Some(&rich)),
        ),
        (
            "anchor-v1-rgba8.png",
            still(PixelFormat::Rgba8, 19, 70, 31, Some(&rich)),
        ),
        (
            "anchor-v1-rgb16.png",
            still(PixelFormat::Rgb16, 19, 70, 31, Some(&rich)),
        ),
        (
            "anchor-v1-indexed8.png",
            still(PixelFormat::Indexed8, 19, 70, 31, Some(&rich)),
        ),
        // メタデータのない画像（iCCP・kuEx なし）と、ICC だけの画像
        (
            "anchor-v1-graya8.png",
            still(PixelFormat::GrayA8, 5, 3, 51, None),
        ),
        (
            "anchor-v1-rgb8.png",
            still(PixelFormat::Rgb8, 4, 6, 52, None),
        ),
        (
            "anchor-v1-gray16.png",
            still(PixelFormat::Gray16, 3, 3, 53, None),
        ),
        (
            "anchor-v1-graya16.png",
            still(PixelFormat::GrayA16, 6, 2, 54, Some(&icc_only)),
        ),
        // ゴールデンの anchor-sheet（格子つき）
        (
            "anchor-v1-sheet.png",
            InputGen::Sheet {
                cols: 3,
                rows: 2,
                cell: 8,
            },
        ),
        // ゴールデンの anchor-frames-rgba16（ICC・EXIF つきのフレームの列）と、メタデータのない 8 ビットのフレームの列
        (
            "anchor-v1-frames-rgba16.png",
            InputGen::WithMeta {
                base: Box::new(InputGen::NoiseFrames {
                    w: 7,
                    h: 5,
                    fmt: PixelFormat::Rgba16,
                    count: 2,
                    seed: 33,
                }),
                meta: rich,
            },
        ),
        (
            "anchor-v1-frames-rgba8.png",
            InputGen::NoiseFrames {
                w: 6,
                h: 4,
                fmt: PixelFormat::Rgba8,
                count: 4,
                seed: 55,
            },
        ),
        // ゴールデンの anchor-audio-meta（2 チャンネル：拡張形式でない WAV。ループ点・マーカー・タグ）
        (
            "anchor-v1-audio-stereo.wav",
            InputGen::WithMeta {
                base: Box::new(InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 3_000,
                    seed: 34,
                }),
                meta: MetaGen {
                    loop_range: Some(LoopRange {
                        start: 10,
                        end: 2_990,
                    }),
                    markers: vec![(0, "".into()), (2_999, "終わり".into())],
                    tags: vec![
                        ("title".into(), "効果音".into()),
                        ("vorbis:ENCODER".into(), "aoTuV".into()),
                    ],
                    ..MetaGen::default()
                },
            },
        ),
        // ゴールデンの anchor-audio-6ch（拡張形式）
        (
            "anchor-v1-audio-6ch.wav",
            InputGen::Sine {
                rate: 48_000,
                channels: 6,
                frames: 1_000,
                freq: 440,
                loop_range: None,
            },
        ),
        // ゴールデンの anchor-audio-nonfinite（1 チャンネル：NaN・±∞）
        (
            "anchor-v1-audio-mono.wav",
            InputGen::NonFinite {
                base: Box::new(InputGen::AudioNoise {
                    rate: 8_000,
                    channels: 1,
                    frames: 64,
                    seed: 35,
                }),
                at: vec![
                    (0, 1, kura_engine::testgen::NonFiniteSample::Nan),
                    (0, 2, kura_engine::testgen::NonFiniteSample::Inf),
                    (0, 3, kura_engine::testgen::NonFiniteSample::NegInf),
                ],
            },
        ),
    ];
    gens.into_iter()
        .map(|(name, g)| (name, g.generate().unwrap()))
        .collect()
}

/// 中の画素・標本が値と同じか（外のツールで開けることの確かめ。固定の PNG・WAV の読み手は私用のチャンクを知らないので、
/// 向き・テキストなどのヘッダの値は当てない。画素・ICC・標本だけを比べる。NaN はどれも同じとみなす）。
fn same_body(plain: &Value, v: &Value) -> bool {
    let same_sample = |a: &f32, b: &f32| a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan());
    match (plain, v) {
        (Value::Image(p), Value::Image(v)) => p.px == v.px && p.meta.icc == v.meta.icc,
        (Value::Frames(p), Value::Frames(v)) => {
            p.frames().len() == v.frames().len()
                && p.frames().iter().zip(v.frames()).all(|(a, b)| a.px == b.px)
                && p.meta.icc == v.meta.icc
        }
        (Value::Audio(p), Value::Audio(v)) => {
            p.rate() == v.rate()
                && p.planes().len() == v.planes().len()
                && p.planes().iter().zip(v.planes()).all(|(a, b)| {
                    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_sample(x, y))
                })
        }
        _ => false,
    }
}

fn frozen_stamp(v: &Value) -> DecodeStamp {
    DecodeStamp {
        codec: "fake-platform-dec/1".into(),
        tier: Tier::Platform,
        reproducible: false,
        value_digest: v.digest().unwrap(),
        env: Some(CodecEnv::new("fake", "1.0").with("build", "F1")),
    }
}

/// 版 1 の入れ物（凍結したファイル）を、今の読み手が同じ値に読む。外のツール（固定の PNG・WAV の読み手）でも開ける。
#[test]
fn frozen_v1_anchors_still_read() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    for (name, v) in frozen_values() {
        let path = dir.join(name);
        let stamp = frozen_stamp(&v);
        if !path.exists() {
            assert!(
                bless,
                "{name} がありません（KURA_BLESS=1 で作る。作ったものは変えない）"
            );
            let cx = AnchorContext {
                source: Some(Digest([1; 32])),
                format: Some("heic".into()),
                options: DecodeOptions::default(),
            };
            let (bytes, _) =
                anchor::write_verified(&v, &stamp, &cx, &Limits::default(), &NoMonitor).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let r = anchor::read(&mut Cursor::new(&bytes), &Limits::default(), &NoMonitor).unwrap();
        // NaN を含む音声は PartialEq で等しくならないので、ダイジェスト（NaN をそろえる）とメタデータで照らす
        assert_eq!(r.value.digest().unwrap(), stamp.value_digest, "{name}");
        assert_eq!(r.value.shape(), v.shape(), "{name}");
        assert_eq!(r.header.decoded, stamp, "{name}");
        assert_eq!(
            (r.header.v, r.header.anchor.as_str()),
            (1, "anchor-kura-enc/1")
        );
        let plain = if name.ends_with(".png") { "png" } else { "wav" };
        let (p, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(&bytes),
                plain,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(p.kind(), v.kind(), "{name}");
        assert!(same_body(&p, &v), "{name}：中の画素・標本が値と違います");
        // 中のデコーダの識別子は凍結した表のもの。アンカーのデコーダ（コーデックの組の口）でも同じ値
        assert!(
            anchor::BODY_DECODERS.contains(&r.header.body.as_str()),
            "{name}"
        );
        let (_, s) = CodecSet::pinned()
            .decode_with(
                anchor::DEC_ID,
                &mut Cursor::new(&bytes),
                anchor::FORMAT,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(s.value_digest, stamp.value_digest, "{name}");
    }
}

/// 書くヘッダの正規の JSON を凍結する（形を変えるときは `v` を上げ、古い `v` も読み続ける）。
#[test]
fn header_shape_is_frozen() {
    let px = kura_engine::value::Raster::filled(1, 1, PixelFormat::Gray8, None, &[7]).unwrap();
    let v = Value::Image(Arc::new(Image {
        px,
        meta: kura_engine::value::ImageMeta::default(),
    }));
    let stamp = DecodeStamp {
        codec: "heic-imageio-dec/1".into(),
        tier: Tier::Platform,
        reproducible: false,
        value_digest: v.digest().unwrap(),
        env: Some(CodecEnv::new("macOS", "26.7 (25G229)").with("imageio", "2784.6.4")),
    };
    let cx = AnchorContext {
        source: Some(Digest([0x11; 32])),
        format: Some("heic".into()),
        options: DecodeOptions::default(),
    };
    let (bytes, _) =
        anchor::write_verified(&v, &stamp, &cx, &Limits::default(), &NoMonitor).unwrap();
    let r = anchor::read(&mut Cursor::new(&bytes), &Limits::default(), &NoMonitor).unwrap();
    let json = String::from_utf8(kura_engine::digest::canonical(&r.header).unwrap()).unwrap();
    let want = format!(
        concat!(
            r#"{{"anchor":"anchor-kura-enc/1","body":"png-rs-dec/2","#,
            r#""content":{{"exif":false,"icc":false,"kind":"image","meta":{{"dpi":null,"grid":null,"pivot":null,"slices":[],"text":{{}}}}}},"#,
            r#""decoded":{{"codec":"heic-imageio-dec/1","env":{{"components":{{"imageio":"2784.6.4"}},"tool":"macOS","version":"26.7 (25G229)"}},"reproducible":false,"tier":"platform","valueDigest":"{}"}},"#,
            r#""format":"heic","options":{{}},"source":"{}","v":1}}"#
        ),
        stamp.value_digest,
        Digest([0x11; 32]),
    );
    assert_eq!(json, want);
    assert!(matches!(r.header.content, AnchorContent::Image { .. }));
}

/// 固定の段の組：アンカーのデコーダは内部の形式 `anchor` だけを読み、能力の表には出ない。`FileGen::Anchor`（コーデックの
/// ゴールデン）の値は、作り方の値そのもの。
#[test]
fn codec_set_has_the_anchor_decoder() {
    let set = CodecSet::pinned();
    let ids: Vec<&str> = set
        .decoders_for(anchor::FORMAT)
        .iter()
        .map(|d| d.id())
        .collect();
    assert_eq!(ids, [anchor::DEC_ID]);
    assert!(set.format_caps(anchor::FORMAT).is_none());
    assert!(
        set.capabilities()
            .iter()
            .all(|f| f.read.iter().all(|r| r.codec != anchor::DEC_ID))
    );
    for input in [
        InputGen::Sheet {
            cols: 2,
            rows: 2,
            cell: 4,
        },
        InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 9,
            seed: 1,
        },
    ] {
        let want = input.generate().unwrap().digest().unwrap();
        let f = FileGen::Anchor { input };
        assert_eq!(f.format(), anchor::FORMAT);
        let (_, stamp) = set
            .decode_with(
                anchor::DEC_ID,
                &mut Cursor::new(f.bytes().unwrap()),
                anchor::FORMAT,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(stamp.value_digest, want);
    }
}

/// ChaCha8 で崩したアンカー（バイトの書き換え・挿入・削除・切り詰め）を `fuzz_decode` に通す：自前のコードの panic・
/// `Internal`・形と中身の食い違いがない。崩したものは、読めないか、読めたら元と同じ値（ヘッダで照らすため）。
#[test]
fn mutated_anchors_never_panic() {
    let seeds: Vec<Vec<u8>> = frozen_values()
        .iter()
        .map(|(_, v)| {
            anchor::write_verified(
                v,
                &frozen_stamp(v),
                &AnchorContext::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap()
            .0
        })
        .collect();
    let mut r = kura_engine::determinism::rng(0xF022);
    let set = CodecSet::pinned();
    for i in 0..450usize {
        let seed = &seeds[i % seeds.len()];
        let mut b = seed.clone();
        for _ in 0..1 + r.next_u32() % 4 {
            if b.is_empty() {
                break;
            }
            let at = (r.next_u32() as usize) % b.len();
            match r.next_u32() % 4 {
                0 => b[at] ^= 1 << (r.next_u32() % 8),
                1 => b.insert(at, r.next_u32() as u8),
                2 => {
                    b.remove(at);
                }
                _ => b.truncate(at),
            }
        }
        if let Err(e) = kura_engine::testing::fuzz_decode(anchor::FORMAT, &b) {
            panic!("{i}: {e}");
        }
        if let Ok((v, _)) = set.decode(
            &mut Cursor::new(&b),
            anchor::FORMAT,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        ) {
            let (_, want) = &frozen_values()[i % seeds.len()];
            assert_eq!(v.digest().unwrap(), want.digest().unwrap(), "{i}");
        }
    }
}

/// 上限：アンカーを読むときも、呼び手の上限（形・メタデータ）で断る。
#[test]
fn reading_respects_limits() {
    let (_, v) = frozen_values().remove(0);
    let stamp = frozen_stamp(&v);
    let bytes = round_trip(&v, &stamp);
    let small = Limits {
        max_pixels: 10,
        ..Limits::default()
    };
    assert!(matches!(
        anchor::read(&mut Cursor::new(&bytes), &small, &NoMonitor),
        Err(EngineError::Limit(_))
    ));
    let tiny_meta = Limits {
        max_meta_bytes: 64,
        ..Limits::default()
    };
    assert!(matches!(
        anchor::read(&mut Cursor::new(&bytes), &tiny_meta, &NoMonitor),
        Err(EngineError::Limit(_))
    ));
}

/// 速さと大きさ（release で `--ignored --nocapture`）：24MP の Rgba8（写真に近い滑らかな画像）と、3 分の 44.1 kHz ステレオ。
#[test]
#[ignore]
#[allow(clippy::disallowed_methods)]
fn bench_anchor_sizes() {
    use std::time::Instant;
    let w = 6_000u32;
    let h = 4_000u32;
    let px = kura_engine::value::Raster::from_tile_fn(
        w,
        h,
        PixelFormat::Rgba8,
        None,
        &NoMonitor,
        |rect, out| {
            for (i, p) in out.chunks_exact_mut(4).enumerate() {
                let x = rect.x as u32 + (i as u32 % rect.w);
                let y = rect.y as u32 + (i as u32 / rect.w);
                p.copy_from_slice(&[
                    (x / 24) as u8,
                    (y / 16) as u8,
                    ((x ^ y) & 7) as u8 + 100,
                    255,
                ]);
            }
            Ok(())
        },
    )
    .unwrap();
    let img = Value::Image(Arc::new(Image {
        px,
        meta: kura_engine::value::ImageMeta::default(),
    }));
    let snd = InputGen::Sine {
        rate: 44_100,
        channels: 2,
        frames: 44_100 * 180,
        freq: 440,
        loop_range: None,
    }
    .generate()
    .unwrap();
    for (name, v) in [("24MP Rgba8", img), ("3 分のステレオ", snd)] {
        let stamp = fake_stamp(&v);
        let t = Instant::now();
        let mut bytes = Vec::new();
        anchor::write(
            &v,
            &stamp,
            &AnchorContext::default(),
            &Limits::default(),
            &mut bytes,
            &NoMonitor,
        )
        .unwrap();
        let tw = t.elapsed();
        let t = Instant::now();
        anchor::read(&mut Cursor::new(&bytes), &Limits::default(), &NoMonitor).unwrap();
        let tr = t.elapsed();
        eprintln!(
            "{name}: アンカー {:.1} MB、書き {:.0} ms、読み（照合込み）{:.0} ms",
            bytes.len() as f64 / 1e6,
            tw.as_secs_f64() * 1e3,
            tr.as_secs_f64() * 1e3
        );
    }
}
