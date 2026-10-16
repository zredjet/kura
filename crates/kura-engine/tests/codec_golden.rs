//! コーデックのゴールデン（docs/editing.md §8.5）。固定のエンコーダが書くバイト列（R2）と、固定のデコーダが読む値（R1）を凍結する。
//!
//! - ファイルは `goldens-codecs/<コーデックの ID の / を @ にしたもの>.jsonl`。ID は読み取り（`-dec`）と書き出し（`-enc`）で別。
//!   - `png-rs-enc@1.jsonl`：1 行に `{"decoder","file","format","input","name","options","value"}`。
//!     file は書いたバイト列のダイジェスト、value はそれを `decoder` で読み直した値のダイジェスト。
//!   - `png-rs-dec@1.jsonl`：1 行に `{"name","source","value"}`。下の層で組んだファイル（source）を読んだ値のダイジェスト。
//! - **追記だけ**（scripts/check-goldens-append-only.sh が見る）。
//!   - 書くバイト列が変わったら、エンコーダの約束の版を上げる（`-enc/1` → `-enc/2`）。旧版のエンコーダは残さなくてよい
//!     （そのファイルは照らさなくなる）。png クレートや依存（fdeflate・flate2・miniz_oxide）を上げたときも、ここで気づく。
//!   - 読む値は変えない。どうしても変えるなら、デコーダの約束の版を上げ、旧版を `CodecSet::add_legacy_decoder` で残す。
//!     デコーダのファイルは、その ID のデコーダ（今の版か旧版）がないと失敗にする（作り直しが記録の識別子で読むため。§9.1）。
//! - 足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test codec_golden` で足せる。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Cursor;
use std::path::PathBuf;

use kura_engine::Limits;
use kura_engine::NoMonitor;
use kura_engine::codec::CodecSet;
use kura_engine::digest::{Hasher, canonical};
use kura_engine::doc::DecodeOptions;
use kura_engine::testfiles::{
    ApngFrame, AseCel, AseFrame, AseLayer, AseSlice, FileGen, GifFrame, PsdGen,
};
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::value::{LoopRange, PixelFormat};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    name: String,
    format: String,
    options: serde_json::Value,
    input: InputGen,
    file: String,
    /// 読み直したデコーダ（`value` はこのデコーダで照らす）。
    decoder: String,
    value: String,
}

/// 例：（名前、形式、設定、入力の作り方）。例は消さない・変えない（新しい名前で足す）。
fn examples() -> Vec<(&'static str, &'static str, serde_json::Value, InputGen)> {
    let mut v = Vec::new();
    for (name, fmt) in [
        ("gray8", PixelFormat::Gray8),
        ("graya8", PixelFormat::GrayA8),
        ("rgb8", PixelFormat::Rgb8),
        ("rgba8", PixelFormat::Rgba8),
        ("gray16", PixelFormat::Gray16),
        ("graya16", PixelFormat::GrayA16),
        ("rgb16", PixelFormat::Rgb16),
        ("rgba16", PixelFormat::Rgba16),
        ("indexed8", PixelFormat::Indexed8),
    ] {
        v.push((
            name,
            "png",
            json!({}),
            InputGen::Noise {
                w: 37,
                h: 23,
                fmt,
                seed: 1,
            },
        ));
    }
    v.push((
        "sheet-high",
        "png",
        json!({ "compression": "high" }),
        InputGen::Sheet {
            cols: 3,
            rows: 2,
            cell: 8,
        },
    ));
    v.push((
        "sheet-fast",
        "png",
        json!({ "compression": "fast" }),
        InputGen::Sheet {
            cols: 3,
            rows: 2,
            cell: 8,
        },
    ));
    for bits in [8u64, 16, 24, 32] {
        let name: &'static str = match bits {
            8 => "pcm8",
            16 => "pcm16",
            24 => "pcm24",
            _ => "pcm32",
        };
        v.push((
            name,
            "wav",
            json!({ "bits": bits }),
            InputGen::AudioNoise {
                rate: 22_050,
                channels: 2,
                frames: 777,
                seed: 2,
            },
        ));
    }
    v.push((
        "float32",
        "wav",
        json!({ "float": true }),
        InputGen::AudioNoise {
            rate: 48_000,
            channels: 1,
            frames: 500,
            seed: 3,
        },
    ));
    v.push((
        "loop-6ch",
        "wav",
        json!({ "bits": 16 }),
        InputGen::Sine {
            rate: 8_000,
            channels: 6,
            frames: 400,
            freq: 300,
            loop_range: Some(LoopRange {
                start: 10,
                end: 390,
            }),
        },
    ));
    let meta = MetaGen {
        dpi: Some((144, 72)),
        text: vec![
            ("Software".into(), "Kura".into()),
            ("Title".into(), "ドット絵".into()),
        ],
        icc: Some((0..=255u8).collect()),
        ..MetaGen::default()
    };
    v.push((
        "meta-indexed",
        "png",
        json!({}),
        InputGen::WithMeta {
            base: Box::new(InputGen::Noise {
                w: 9,
                h: 5,
                fmt: PixelFormat::Indexed8,
                seed: 4,
            }),
            meta: meta.clone(),
        },
    ));
    v.push((
        "meta-rgba16",
        "png",
        json!({}),
        InputGen::WithMeta {
            base: Box::new(InputGen::Noise {
                w: 9,
                h: 5,
                fmt: PixelFormat::Rgba16,
                seed: 5,
            }),
            meta,
        },
    ));
    let audio_meta = MetaGen {
        loop_range: Some(LoopRange { start: 3, end: 90 }),
        markers: vec![(1, "attack".into()), (50, "".into()), (70, "終わり".into())],
        tags: vec![
            ("title".into(), "効果音".into()),
            ("artist".into(), "kura".into()),
            ("riff:ISBJ".into(), "se".into()),
        ],
        ..MetaGen::default()
    };
    v.push((
        "meta-pcm24",
        "wav",
        json!({ "bits": 24 }),
        InputGen::WithMeta {
            base: Box::new(InputGen::AudioNoise {
                rate: 44_100,
                channels: 2,
                frames: 100,
                seed: 6,
            }),
            meta: audio_meta,
        },
    ));
    // 段 4：JPEG・GIF・WebP・APNG・EXIF
    let exif = kura_engine::codec::exif::minimal(1, false);
    let noise = |w, h, fmt, seed| InputGen::Noise { w, h, fmt, seed };
    v.push((
        "jpeg-rgb-q90",
        "jpeg",
        json!({}),
        noise(37, 23, PixelFormat::Rgb8, 11),
    ));
    v.push((
        "jpeg-gray-q75",
        "jpeg",
        json!({ "quality": 75 }),
        noise(33, 17, PixelFormat::Gray8, 12),
    ));
    v.push((
        "jpeg-progressive-422",
        "jpeg",
        json!({ "progressive": true, "chroma": "422", "quality": 80 }),
        noise(40, 24, PixelFormat::Rgb8, 13),
    ));
    v.push((
        "jpeg-meta",
        "jpeg",
        json!({ "quality": 95, "chroma": "420" }),
        InputGen::WithMeta {
            base: Box::new(noise(16, 16, PixelFormat::Rgb8, 14)),
            meta: MetaGen {
                dpi: Some((300, 300)),
                icc: Some((0..=255u8).collect()),
                exif: Some(exif.clone()),
                ..MetaGen::default()
            },
        },
    ));
    v.push((
        "gif-indexed-frames",
        "gif",
        json!({}),
        InputGen::NoiseFrames {
            w: 21,
            h: 13,
            fmt: PixelFormat::Indexed8,
            count: 3,
            seed: 15,
        },
    ));
    v.push((
        "gif-sprite",
        "gif",
        json!({ "alphaThreshold": 1 }),
        InputGen::Sprite {
            w: 16,
            h: 12,
            rect: kura_engine::geom::IRect::new(3, 2, 7, 6),
        },
    ));
    v.push((
        "gif-quantized",
        "gif",
        json!({}),
        noise(30, 20, PixelFormat::Rgb8, 16),
    ));
    v.push((
        "webp-rgba",
        "webp",
        json!({}),
        noise(29, 19, PixelFormat::Rgba8, 17),
    ));
    v.push((
        "webp-gray",
        "webp",
        json!({}),
        noise(19, 11, PixelFormat::Gray8, 18),
    ));
    v.push((
        "webp-meta",
        "webp",
        json!({}),
        InputGen::WithMeta {
            base: Box::new(noise(9, 7, PixelFormat::Rgb8, 19)),
            meta: MetaGen {
                icc: Some(vec![3; 40]),
                exif: Some(exif.clone()),
                ..MetaGen::default()
            },
        },
    ));
    v.push((
        "apng-rgba16",
        "png",
        json!({}),
        InputGen::NoiseFrames {
            w: 11,
            h: 9,
            fmt: PixelFormat::Rgba16,
            count: 3,
            seed: 20,
        },
    ));
    v.push((
        "apng-indexed",
        "png",
        json!({ "compression": "fast" }),
        InputGen::NoiseFrames {
            w: 13,
            h: 7,
            fmt: PixelFormat::Indexed8,
            count: 2,
            seed: 21,
        },
    ));
    // 1 フレームの列（APNG は acTL、GIF は繰り返しの拡張があるので、読んでもフレームの列のまま）
    for (name, format) in [("apng-one-frame", "png"), ("gif-one-frame", "gif")] {
        v.push((
            name,
            format,
            json!({}),
            InputGen::NoiseFrames {
                w: 6,
                h: 5,
                fmt: PixelFormat::Indexed8,
                count: 1,
                seed: 23,
            },
        ));
    }
    v.push((
        "png-exif",
        "png",
        json!({}),
        InputGen::WithMeta {
            base: Box::new(noise(5, 4, PixelFormat::Rgb8, 22)),
            meta: MetaGen {
                exif: Some(exif),
                ..MetaGen::default()
            },
        },
    ));
    // 段 6：AIFF・FLAC（自作の書き出し）
    let audio_meta6 = MetaGen {
        loop_range: Some(LoopRange {
            start: 100,
            end: 4_500,
        }),
        markers: vec![(7, "attack".into()), (4_200, "終わり".into())],
        tags: vec![
            ("title".into(), "効果音".into()),
            ("artist".into(), "kura".into()),
        ],
        ..MetaGen::default()
    };
    for (name, format, options, channels, frames, seed) in [
        ("aiff-pcm8", "aiff", json!({ "bits": 8 }), 1, 300, 40),
        ("aiff-pcm16", "aiff", json!({ "bits": 16 }), 2, 777, 41),
        ("aiff-pcm24", "aiff", json!({ "bits": 24 }), 3, 500, 42),
        ("aiff-float32", "aiff", json!({ "float": true }), 2, 400, 43),
        ("flac-pcm8-mono", "flac", json!({ "bits": 8 }), 1, 1_000, 44),
        (
            "flac-pcm16-stereo",
            "flac",
            json!({ "bits": 16 }),
            2,
            10_000,
            45,
        ),
        (
            "flac-pcm24-6ch",
            "flac",
            json!({ "bits": 24 }),
            6,
            5_000,
            46,
        ),
    ] {
        v.push((
            name,
            format,
            options,
            InputGen::AudioNoise {
                rate: 44_100,
                channels,
                frames,
                seed,
            },
        ));
    }
    // 予測の効く形（正弦波）とメタデータ（ループ点・マーカー・タグ）
    for (name, format) in [("aiff-meta-sine", "aiff"), ("flac-meta-sine", "flac")] {
        v.push((
            name,
            format,
            json!({ "bits": 16 }),
            InputGen::WithMeta {
                base: Box::new(InputGen::Sine {
                    rate: 48_000,
                    channels: 2,
                    frames: 9_000,
                    freq: 440,
                    loop_range: None,
                }),
                meta: audio_meta6.clone(),
            },
        ));
    }
    v
}

fn file_name(codec: &str) -> String {
    codec.replace('/', "@")
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens-codecs")
}

/// ゴールデンのファイルの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Enc,
    Dec,
}

/// `goldens-codecs/` のファイルを種類ごとに読む：（種類, ID）→ 中身の行。名前の形が違うファイルは失敗にする。
fn read_dir() -> BTreeMap<(Kind, String), (PathBuf, String)> {
    let mut out = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(root()) else {
        return out;
    };
    for f in dir {
        let path = f.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let stem = name
            .strip_suffix(".jsonl")
            .unwrap_or_else(|| panic!("goldens-codecs/ には .jsonl だけ置けます: {name}"));
        let (base, ver) = stem
            .split_once('@')
            .unwrap_or_else(|| panic!("ファイル名は <ID の / を @ に>.jsonl: {name}"));
        assert!(
            ver.parse::<u16>().is_ok_and(|v| v.to_string() == ver),
            "ファイル名の版は数字だけ: {name}"
        );
        let kind = if base.ends_with("-enc") {
            Kind::Enc
        } else if base.ends_with("-dec") {
            Kind::Dec
        } else {
            panic!("コーデックの ID は -enc か -dec で終わる: {name}")
        };
        let text = std::fs::read_to_string(&path).unwrap();
        out.insert((kind, stem.replace('@', "/")), (path, text));
    }
    out
}

fn parse<T: serde::de::DeserializeOwned>(path: &std::path::Path, text: &str) -> Vec<T> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .collect()
}

fn decode_value(
    codecs: &CodecSet,
    decoder: &str,
    bytes: &[u8],
    format: &str,
) -> Result<String, String> {
    codecs
        .decode_with(
            decoder,
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .map(|(_, stamp)| stamp.value_digest.to_hex())
        .map_err(|e| e.to_string())
}

/// 同じ名前の行が 2 つあれば失敗に足す。
fn check_unique<'a>(key: &str, names: impl Iterator<Item = &'a str>, failures: &mut String) {
    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    for n in names {
        if seen.insert(n, ()).is_some() {
            let _ = writeln!(failures, "{key} {n}：同じ例の行が 2 つあります");
        }
    }
}

fn same<T: Serialize>(a: &T, b: &T) -> bool {
    canonical(a).unwrap() == canonical(b).unwrap()
}

#[test]
fn codec_goldens_match_and_are_complete() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let codecs = CodecSet::pinned();
    let mut failures = String::new();
    let mut to_append: BTreeMap<String, String> = BTreeMap::new();
    // エンコーダの ID → 行。今ないエンコーダ（版を上げた旧版）のファイルは照らさない（R2 の版上げ）
    let mut frozen: BTreeMap<String, Vec<Line>> = BTreeMap::new();
    for ((kind, id), (path, text)) in read_dir() {
        if kind == Kind::Enc {
            let lines: Vec<Line> = parse(&path, &text);
            check_unique(&id, lines.iter().map(|l| l.name.as_str()), &mut failures);
            frozen.insert(id, lines);
        }
    }
    for (name, format, options, input) in examples() {
        let value = input.generate().unwrap();
        let (bytes, stamp) = codecs
            .encode_bytes(&value, format, &options, &NoMonitor)
            .unwrap();
        let (back, dstamp) = codecs
            .decode(
                &mut Cursor::new(&bytes),
                format,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let key = stamp.codec.clone();
        // 書いて読み直した値は、元の値と同じ大きさ・種類（PNG・WAV は形式まで同じ。GIF・JPEG・WebP は形式が変わりうる）
        let size = |sh: &kura_engine::Shape| match sh {
            kura_engine::Shape::Image { w, h, .. } | kura_engine::Shape::Frames { w, h, .. } => {
                (w.max(), h.max())
            }
            kura_engine::Shape::Audio { frames, .. } => (frames.max(), 0),
        };
        assert_eq!(back.kind(), value.kind(), "{name}");
        assert_eq!(size(&back.shape()), size(&value.shape()), "{name}");
        if matches!(format, "png" | "wav" | "aiff" | "flac") {
            assert_eq!(back.shape(), value.shape(), "{name}");
        }
        let file = Hasher::new("kura-file-v1")
            .bytes_with_len(&bytes)
            .finish()
            .to_hex();
        let line = Line {
            name: name.into(),
            format: format.into(),
            options,
            input,
            file,
            decoder: dstamp.codec.clone(),
            value: dstamp.value_digest.to_hex(),
        };
        match frozen
            .get(&key)
            .and_then(|ls| ls.iter().find(|l| l.name == name))
        {
            Some(old)
                if !same(&old.options, &line.options)
                    || !same(&old.input, &line.input)
                    || old.format != line.format =>
            {
                let _ = writeln!(
                    failures,
                    "{key} {name}：凍結した例と設定・入力が違います（例は変えずに新しい名前で足す）"
                );
            }
            Some(old) if old.file != line.file => {
                let _ = writeln!(
                    failures,
                    "{key} {name}：書いたバイト列が凍結した値と違う（{} → {}）。エンコーダの約束の版を上げる（-enc/N）",
                    old.file, line.file
                );
            }
            // 読み直しは、凍結したときのデコーダ（旧版を含む）で照らす
            Some(old) => match decode_value(&codecs, &old.decoder, &bytes, format) {
                Ok(v) if v == old.value => {}
                Ok(v) => {
                    let _ = writeln!(
                        failures,
                        "{key} {name}：{} で読み直した値が凍結した値と違う（{} → {v}）。読む値は変えない（変えるならデコーダの版を上げて旧版を残す）",
                        old.decoder, old.value
                    );
                }
                Err(e) => {
                    let _ = writeln!(
                        failures,
                        "{key} {name}：凍結したときのデコーダ {} で読めません（旧版のデコーダを残す）: {e}",
                        old.decoder
                    );
                }
            },
            None => {
                let text = String::from_utf8(canonical(&line).unwrap()).unwrap();
                to_append.entry(key).or_default().push_str(&(text + "\n"));
            }
        }
    }
    // 凍結した例が消えていないこと
    let names: Vec<&str> = examples().iter().map(|e| e.0).collect();
    for (key, lines) in &frozen {
        for l in lines {
            if !names.contains(&l.name.as_str()) {
                let _ = writeln!(failures, "{key} {}：凍結した例が消えています", l.name);
            }
        }
    }
    if !to_append.is_empty() {
        if bless && failures.is_empty() {
            std::fs::create_dir_all(root()).unwrap();
            for (key, text) in to_append {
                let path = root().join(format!("{}.jsonl", file_name(&key)));
                let mut all = std::fs::read_to_string(&path).unwrap_or_default();
                all.push_str(&text);
                std::fs::write(&path, all).unwrap();
            }
        } else {
            for (key, text) in to_append {
                let _ = write!(
                    failures,
                    "{key}：行が足りません（KURA_BLESS=1 で追記できます）:\n{text}"
                );
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

/// 読み取りだけの例：（名前、作り方）。自前のエンコーダが書かない形を、下の層で組んだファイルから読む（R1 のうちデコーダだけの経路）。
fn decode_examples() -> Vec<(&'static str, FileGen)> {
    let png =
        |w, h, color, depth, interlace, palette_len, trns: Option<Vec<u8>>, seed| FileGen::Png {
            w,
            h,
            color,
            depth,
            interlace,
            palette_len,
            trns,
            text: vec![],
            icc: None,
            phys: None,
            seed,
            exif: None,
        };
    let wav = |tag, bits, channels, extensible, unknown_size, seed| FileGen::Wav {
        tag,
        bits,
        channels,
        rate: 32_000,
        frames: 50,
        extensible,
        smpl: Some((2, 40)),
        cues: vec![(3, 5), (1, 7)],
        labels: vec![(1, "first".into()), (3, "third".into())],
        info: vec![
            ("INAM".into(), "name".into()),
            ("ICMT".into(), "comment".into()),
            ("IXYZ".into(), "other".into()),
        ],
        unknown_size,
        seed,
    };
    let mut v = vec![
        ("gray1", png(13, 7, 0, 1, false, 0, None, 1)),
        (
            "gray2-trns",
            png(13, 7, 0, 2, false, 0, Some(vec![0, 2]), 2),
        ),
        ("gray4-interlaced", png(13, 7, 0, 4, true, 0, None, 3)),
        (
            "gray8-trns",
            png(9, 9, 0, 8, false, 0, Some(vec![0, 77]), 4),
        ),
        (
            "gray16-trns",
            png(9, 9, 0, 16, false, 0, Some(vec![0x12, 0x34]), 5),
        ),
        (
            "rgb8-trns",
            png(9, 9, 2, 8, false, 0, Some(vec![0, 1, 0, 2, 0, 3]), 6),
        ),
        (
            "rgb16-trns-interlaced",
            png(9, 9, 2, 16, true, 0, Some(vec![0, 1, 0, 2, 0, 3]), 7),
        ),
        ("palette1", png(17, 3, 3, 1, false, 2, None, 8)),
        (
            "palette2-trns",
            png(17, 3, 3, 2, false, 4, Some(vec![0, 64, 128]), 9),
        ),
        ("palette4-interlaced", png(17, 9, 3, 4, true, 16, None, 10)),
        (
            "palette8-trns",
            png(17, 9, 3, 8, false, 200, Some(vec![10; 30]), 11),
        ),
        ("graya8-interlaced", png(11, 11, 4, 8, true, 0, None, 12)),
        ("rgba16", png(11, 11, 6, 16, false, 0, None, 13)),
        (
            "meta-chunks",
            FileGen::Png {
                w: 3,
                h: 2,
                color: 2,
                depth: 8,
                interlace: false,
                palette_len: 0,
                trns: None,
                text: vec![
                    ("Author".into(), "someone".into()),
                    ("Comment".into(), "x".into()),
                ],
                icc: Some((0..200u8).collect()),
                phys: Some((3_780, 2_835, 1)),
                seed: 14,
                exif: None,
            },
        ),
        ("wav-pcm8", wav(1, 8, 2, false, false, 20)),
        ("wav-pcm32", wav(1, 32, 1, false, false, 21)),
        ("wav-float64", wav(3, 64, 2, false, false, 22)),
        ("wav-ext-float32", wav(3, 32, 3, true, false, 23)),
        ("wav-ext-pcm24-unknown-size", wav(1, 24, 2, true, true, 24)),
    ];
    // 段 4：PNG の eXIf と APNG
    v.push((
        "png-exif-orient6",
        FileGen::Png {
            w: 7,
            h: 3,
            color: 2,
            depth: 8,
            interlace: false,
            palette_len: 0,
            trns: None,
            text: vec![],
            icc: None,
            phys: None,
            seed: 30,
            exif: Some(kura_engine::codec::exif::minimal(6, true)),
        },
    ));
    let fr = |x, y, w, h, dispose, blend| ApngFrame {
        x,
        y,
        w,
        h,
        delay: (3, 50),
        dispose,
        blend,
    };
    let apng = |color, palette_len, trns: Option<Vec<u8>>, separate, frames, seed| FileGen::Apng {
        w: 10,
        h: 8,
        color,
        palette_len,
        trns,
        separate_default: separate,
        plays: 2,
        frames,
        exif: None,
        seed,
        interlace: false,
    };
    let ops = vec![
        fr(0, 0, 10, 8, 0, 0),
        fr(2, 1, 5, 4, 1, 0),
        fr(1, 2, 6, 5, 2, 1),
        fr(0, 0, 10, 8, 0, 1),
    ];
    v.push(("apng-rgba-ops", apng(6, 0, None, false, ops.clone(), 31)));
    v.push(("apng-rgb-separate", apng(2, 0, None, true, ops.clone(), 32)));
    v.push((
        "apng-indexed-exact",
        apng(3, 8, Some(vec![255, 0]), false, ops.clone(), 33),
    ));
    v.push((
        "apng-indexed-to-rgba",
        apng(3, 8, Some(vec![0, 128]), false, ops.clone(), 34),
    ));
    let mut interlaced = apng(6, 0, None, false, ops, 44);
    if let FileGen::Apng { interlace, .. } = &mut interlaced {
        *interlace = true;
    }
    v.push(("apng-interlaced-subframes", interlaced));
    v.push((
        "apng-one-frame",
        apng(2, 0, None, false, vec![fr(0, 0, 10, 8, 0, 0)], 45),
    ));
    // GIF
    let gf = |left, top, w, h, local_len, transparent, dispose, interlaced| GifFrame {
        left,
        top,
        w,
        h,
        local_len,
        transparent,
        dispose,
        delay: 7,
        interlaced,
    };
    v.push((
        "gif-global-dispose",
        FileGen::Gif {
            w: 12,
            h: 9,
            global_len: 16,
            repeat: Some(0),
            frames: vec![
                gf(0, 0, 12, 9, 0, None, 1, false),
                gf(2, 1, 5, 4, 0, Some(3), 2, true),
                gf(6, 5, 8, 6, 0, Some(1), 3, false),
            ],
            seed: 35,
        },
    ));
    v.push((
        "gif-local-palettes",
        FileGen::Gif {
            w: 10,
            h: 7,
            global_len: 4,
            repeat: Some(2),
            frames: vec![
                gf(0, 0, 10, 7, 8, None, 1, false),
                gf(3, 2, 4, 4, 0, Some(0), 2, false),
            ],
            seed: 36,
        },
    ));
    v.push((
        "gif-still-indexed",
        FileGen::Gif {
            w: 9,
            h: 9,
            global_len: 32,
            repeat: None,
            frames: vec![gf(0, 0, 9, 9, 0, Some(2), 0, true)],
            seed: 37,
        },
    ));
    // aseprite
    let layer = |flags, kind, level, blend, opacity| AseLayer {
        flags,
        kind,
        level,
        blend,
        opacity,
    };
    let cel = |layer: u16, x: i16, y: i16, kind: u16, w: u16, h: u16| AseCel {
        layer,
        x,
        y,
        opacity: 200,
        z: 0,
        kind,
        w,
        h,
        link: 0,
        flip: false,
    };
    let ase = |depth, flags, layers, frames, tileset, seed| FileGen::Aseprite {
        w: 16,
        h: 12,
        depth,
        flags,
        transparent: 1,
        palette_len: 24,
        layers,
        frames,
        tileset,
        tags: vec![("idle".into(), 0, 1)],
        slices: vec![AseSlice {
            name: "body".into(),
            rect: (2, 3, 8, 6),
            center: Some((1, 1, 6, 4)),
            pivot: Some((4, 5)),
        }],
        seed,
        opaque_palette: false,
    };
    let modes = vec![
        layer(1, 0, 0, 0, 255),
        layer(1, 0, 0, 1, 200),
        layer(1, 0, 0, 9, 255),
        layer(1, 0, 0, 12, 128),
        layer(1, 0, 0, 18, 255),
    ];
    let frames2 = vec![
        AseFrame {
            duration: 90,
            cels: (0..5)
                .map(|l| cel(l, l as i16 - 2, 1 - l as i16, 2, 12, 10))
                .collect(),
        },
        AseFrame {
            duration: 60,
            cels: vec![
                AseCel {
                    kind: 1,
                    link: 0,
                    ..cel(0, 0, 0, 1, 1, 1)
                },
                cel(3, 3, 3, 0, 6, 6),
            ],
        },
    ];
    v.push((
        "ase-rgba-modes",
        ase(32, 1, modes.clone(), frames2.clone(), None, 38),
    ));
    v.push((
        "ase-gray-modes",
        ase(16, 1, modes.clone(), frames2.clone(), None, 39),
    ));
    v.push((
        "ase-indexed-rgba",
        ase(8, 1, modes, frames2.clone(), None, 40),
    ));
    v.push((
        "ase-indexed-exact",
        ase(
            8,
            1,
            vec![layer(1 | 8, 0, 0, 0, 255), layer(1, 0, 0, 0, 255)],
            vec![AseFrame {
                duration: 100,
                cels: vec![
                    AseCel {
                        opacity: 255,
                        ..cel(0, 0, 0, 2, 16, 12)
                    },
                    AseCel {
                        opacity: 255,
                        ..cel(1, 2, 2, 0, 9, 7)
                    },
                ],
            }],
            None,
            41,
        ),
    ));
    v.push((
        "ase-groups-separate",
        ase(
            32,
            1 | 2,
            vec![
                layer(1, 0, 0, 0, 255),
                layer(1, 1, 0, 2, 150),
                layer(1, 0, 1, 0, 255),
                layer(1, 0, 1, 3, 255),
            ],
            vec![AseFrame {
                duration: 100,
                cels: vec![
                    cel(0, 0, 0, 2, 16, 12),
                    cel(2, 2, 2, 2, 9, 7),
                    AseCel {
                        z: -1,
                        ..cel(3, 4, 1, 0, 6, 9)
                    },
                ],
            }],
            None,
            42,
        ),
    ));
    v.push((
        "ase-tilemap",
        ase(
            32,
            1,
            vec![layer(1, 0, 0, 0, 255), layer(1, 2, 0, 0, 255)],
            vec![AseFrame {
                duration: 100,
                cels: vec![
                    cel(0, 0, 0, 2, 16, 12),
                    AseCel {
                        kind: 3,
                        ..cel(1, 1, -2, 3, 4, 3)
                    },
                ],
            }],
            Some((4, 5, 5)),
            43,
        ),
    ));
    // PSD（自前の書き手で作る形）
    let pb = PsdGen {
        w: 11,
        h: 7,
        mode: 3,
        depth: 8,
        extra_channels: 0,
        psb: false,
        compression: 1,
        layer_count: 0,
        merged_key: false,
        matted: false,
        real_merged: None,
        transparent_index: None,
        resolution: None,
        exif: None,
        seed: 46,
    };
    for (name, g) in [
        ("psd-gen-rgb8-rle", pb.clone()),
        (
            "psd-gen-rgb16-raw-psb",
            PsdGen {
                depth: 16,
                compression: 0,
                psb: true,
                ..pb.clone()
            },
        ),
        (
            "psd-gen-bitmap",
            PsdGen {
                mode: 0,
                depth: 1,
                ..pb.clone()
            },
        ),
        (
            "psd-gen-indexed-transparent",
            PsdGen {
                mode: 2,
                transparent_index: Some(2),
                ..pb.clone()
            },
        ),
        (
            "psd-gen-rgba-matted",
            PsdGen {
                extra_channels: 1,
                merged_key: true,
                matted: true,
                resolution: Some((100 << 16, 2)),
                ..pb.clone()
            },
        ),
        (
            "psd-gen-graya16-straight",
            PsdGen {
                mode: 1,
                depth: 16,
                extra_channels: 1,
                layer_count: -1,
                ..pb
            },
        ),
    ] {
        v.push((name, FileGen::Psd(g)));
    }
    // 他のツールで作ったフィクスチャ（作り方は fixtures/README.md）
    for path in [
        "jpeg-progressive.jpg",
        "jpeg-gray.jpg",
        "jpeg-422.jpg",
        "jpeg-restart.jpg",
        "webp-lossy.webp",
        "webp-lossy-alpha.webp",
        "webp-anim.webp",
        "psd-rgb8.psd",
        "psd-rgb8-raw.psd",
        "psd-rgba8.psd",
        "psd-gray8.psd",
        "psd-indexed.psd",
        "psd-rgb16.psd",
        "psb-rgb8.psb",
        "psb-rgba8.psb",
        // 段 6：音声（再現できるデコーダだけ。Vorbis・AAC は値が環境に依存するので凍結しない）
        "flac-16.flac",
        "flac-24.flac",
        "alac-16.m4a",
        "alac-24.m4a",
        "aiff-24.aiff",
        "aifc-f32.aif",
        "mp3-lame.mp3",
        "mp3-notag.mp3",
        "mp3-mpeg2-mono.mp3",
        "mp3-free.mp3",
        "mp3-info-frames0.mp3",
    ] {
        let name: &'static str = Box::leak(format!("fixture-{path}").into_boxed_str());
        v.push((name, FileGen::Fixture { path: path.into() }));
    }
    v
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodeLine {
    name: String,
    source: FileGen,
    value: String,
}

#[test]
fn decoder_goldens_match_and_are_complete() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let codecs = CodecSet::pinned();
    let mut failures = String::new();
    // デコーダの ID → 行。凍結した行は、その ID のデコーダ（今の版か旧版）で読み、照らす
    let mut frozen: BTreeMap<String, Vec<DecodeLine>> = BTreeMap::new();
    for ((kind, id), (path, text)) in read_dir() {
        if kind != Kind::Dec {
            continue;
        }
        let lines: Vec<DecodeLine> = parse(&path, &text);
        check_unique(&id, lines.iter().map(|l| l.name.as_str()), &mut failures);
        if codecs.decoder_by_id(&id).is_none() {
            let _ = writeln!(
                failures,
                "{id}：デコーダがありません。読む値を変えるときは、旧版のデコーダを残す（docs/editing.md §9.1）"
            );
        } else {
            for l in &lines {
                let bytes = l.source.bytes().unwrap();
                match decode_value(&codecs, &id, &bytes, l.source.format()) {
                    Ok(v) if v == l.value => {}
                    Ok(v) => {
                        let _ = writeln!(
                            failures,
                            "{id} {}：読み取りの値が凍結した値と違う（{} → {v}）。読む値は変えない（変えるならデコーダの版を上げて旧版を残す）",
                            l.name, l.value
                        );
                    }
                    Err(e) => {
                        let _ = writeln!(failures, "{id} {}：読めません: {e}", l.name);
                    }
                }
            }
        }
        frozen.insert(id, lines);
    }
    let mut to_append: BTreeMap<String, String> = BTreeMap::new();
    for (name, source) in decode_examples() {
        let bytes = source.bytes().unwrap();
        let (_, stamp) = codecs
            .decode(
                &mut Cursor::new(&bytes),
                source.format(),
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let key = stamp.codec.clone();
        if !stamp.reproducible {
            let _ = writeln!(
                failures,
                "{key} {name}：値を再現できないデコーダはゴールデンに入れません"
            );
            continue;
        }
        let line = DecodeLine {
            name: name.into(),
            source,
            value: stamp.value_digest.to_hex(),
        };
        match frozen
            .get(&key)
            .and_then(|ls| ls.iter().find(|l| l.name == name))
        {
            Some(old) if !same(&old.source, &line.source) => {
                let _ = writeln!(
                    failures,
                    "{key} {name}：凍結した作り方と違います（例は変えずに新しい名前で足す）"
                );
            }
            // 値は、凍結した行を読むところで照らした
            Some(_) => {}
            None => {
                let text = String::from_utf8(canonical(&line).unwrap()).unwrap();
                to_append.entry(key).or_default().push_str(&(text + "\n"));
            }
        }
    }
    let names: Vec<&str> = decode_examples().iter().map(|e| e.0).collect();
    for (key, lines) in &frozen {
        for l in lines {
            if !names.contains(&l.name.as_str()) {
                let _ = writeln!(failures, "{key} {}：凍結した例が消えています", l.name);
            }
        }
    }
    if !to_append.is_empty() {
        if bless && failures.is_empty() {
            std::fs::create_dir_all(root()).unwrap();
            for (key, text) in to_append {
                let path = root().join(format!("{}.jsonl", file_name(&key)));
                let mut all = std::fs::read_to_string(&path).unwrap_or_default();
                all.push_str(&text);
                std::fs::write(&path, all).unwrap();
            }
        } else {
            for (key, text) in to_append {
                let _ = write!(
                    failures,
                    "{key}：読み取りの行が足りません（KURA_BLESS=1 で追記できます）:\n{text}"
                );
            }
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}
