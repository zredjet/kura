//! コーデックのゴールデン（docs/editing.md §8.5・R2）。固定のエンコーダが書くバイト列と、読み直した値のダイジェストを凍結する。
//!
//! - ファイルは `goldens-codecs/<コーデックの ID の / を @ にしたもの>.jsonl`（例 `png-rs@1.jsonl`）。
//!   1 行に `{"file","format","input","name","options","value"}`（file は書いたバイト列、value は読み直した値のダイジェスト）。
//! - **追記だけ**（scripts/check-goldens-append-only.sh が見る）。値が変わったら、コーデックの約束の版を上げる（ID の `/1` を `/2` に）。
//!   png クレートやその依存（fdeflate・flate2・miniz_oxide）を上げて書くバイト列が変わったときも、ここで気づく。
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
use kura_engine::testfiles::FileGen;
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
    v
}

fn file_name(codec: &str) -> String {
    codec.replace('/', "@")
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens-codecs")
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
    let mut frozen: BTreeMap<String, Vec<Line>> = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir(root()) {
        for f in dir {
            let path = f.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with(".decode.jsonl") {
                continue;
            }
            let codec = path.file_stem().unwrap().to_string_lossy().to_string();
            let lines: Vec<Line> = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    serde_json::from_str(l).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                })
                .collect();
            frozen.insert(codec, lines);
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
        // 書いて読み直した値は、元の値と同じ形（可逆）
        assert_eq!(back.shape(), value.shape(), "{name}");
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
            value: dstamp.value_digest.to_hex(),
        };
        let key = file_name(&stamp.codec);
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
            Some(old) if old.file != line.file || old.value != line.value => {
                let _ = writeln!(
                    failures,
                    "{key} {name}：凍結した値と違う（file {} → {}、value {} → {}）。コーデックの約束の版を上げる",
                    old.file, line.file, old.value, line.value
                );
            }
            Some(_) => {}
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
                let path = root().join(format!("{key}.jsonl"));
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
    vec![
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
            },
        ),
        ("wav-pcm8", wav(1, 8, 2, false, false, 20)),
        ("wav-pcm32", wav(1, 32, 1, false, false, 21)),
        ("wav-float64", wav(3, 64, 2, false, false, 22)),
        ("wav-ext-float32", wav(3, 32, 3, true, false, 23)),
        ("wav-ext-pcm24-unknown-size", wav(1, 24, 2, true, true, 24)),
    ]
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
    let mut frozen: BTreeMap<String, Vec<DecodeLine>> = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir(root()) {
        for f in dir {
            let path = f.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(codec) = name.strip_suffix(".decode.jsonl") else {
                continue;
            };
            let lines = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| {
                    serde_json::from_str(l).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
                })
                .collect();
            frozen.insert(codec.to_string(), lines);
        }
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
        let key = file_name(&stamp.codec);
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
            Some(old) if old.value != line.value => {
                let _ = writeln!(
                    failures,
                    "{key} {name}：読み取りの値が凍結した値と違う（{} → {}）。デコーダの約束の版を上げる",
                    old.value, line.value
                );
            }
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
                let path = root().join(format!("{key}.decode.jsonl"));
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
