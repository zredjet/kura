//! stable で回す、変異によるファズ（docs/editing.md §13）。`fuzz/`（cargo-fuzz、nightly）と同じ対象
//! （`testing::fuzz_decode`・`fuzz_document`）を、種のファイルに ChaCha8 で変異をかけて呼ぶ。カバレッジは見ない。
//!
//! - ふだんの `cargo test` では、対象ごとに決めた回数だけ回す（`quick`）。
//! - `KURA_FUZZ_SECS=60 cargo test --release -p kura-engine --test fuzz_smoke -- --ignored` で、対象ごとに決めた秒数だけ回す。
//!   `KURA_FUZZ_ONLY=svg,wav` で対象を選べる。
//! - 見つけた入力は `target/fuzz-smoke/` に書き、テストを失敗にする。`replay` で流し直せる。

// 回した回数を stderr に出す（`--nocapture` で見る）
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::codec::CodecSet;
use kura_engine::doc::Graph;
use kura_engine::testfiles::{ApngFrame, AseCel, AseFrame, AseLayer, FileGen, GifFrame, PsdGen};
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::{fuzz_decode, fuzz_document};
use kura_engine::value::{LoopRange, PixelFormat};
use kura_engine::{NoMonitor, Value};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

fn encoded(format: &'static str, v: &Value, opts: serde_json::Value) -> Vec<u8> {
    CodecSet::pinned()
        .encode_bytes(v, format, &opts, &NoMonitor)
        .unwrap()
        .0
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name),
    )
    .unwrap()
}

/// 対象と種のファイル。
fn targets() -> Vec<(&'static str, Vec<Vec<u8>>)> {
    let noise = |fmt| {
        InputGen::Noise {
            w: 23,
            h: 17,
            fmt,
            seed: 1,
        }
        .generate()
        .unwrap()
    };
    let frames = InputGen::NoiseFrames {
        w: 9,
        h: 7,
        fmt: PixelFormat::Indexed8,
        count: 3,
        seed: 2,
    }
    .generate()
    .unwrap();
    let mut with_exif = match noise(PixelFormat::Rgb8) {
        Value::Image(i) => (*i).clone(),
        _ => panic!(),
    };
    with_exif.meta.exif = Some(Arc::new(kura_engine::codec::exif::minimal(6, true)));
    with_exif.meta.icc = Some(Arc::new(vec![7; 100]));
    let with_exif = Value::Image(Arc::new(with_exif));
    let fr = |x, y, w, h, dispose, blend| ApngFrame {
        x,
        y,
        w,
        h,
        delay: (1, 10),
        dispose,
        blend,
    };
    let apng = |color, interlace| {
        FileGen::Apng {
            w: 12,
            h: 9,
            color,
            palette_len: 8,
            trns: (color == 3).then(|| vec![255, 0]),
            separate_default: false,
            plays: 2,
            frames: vec![
                fr(0, 0, 12, 9, 1, 0),
                fr(2, 1, 5, 4, 2, 1),
                fr(1, 1, 8, 6, 0, 1),
            ],
            exif: Some(kura_engine::codec::exif::minimal(3, false)),
            seed: 4,
            interlace,
        }
        .bytes()
        .unwrap()
    };
    let gf = |left, top, w, h, local_len, transparent, dispose, interlaced| GifFrame {
        left,
        top,
        w,
        h,
        local_len,
        transparent,
        dispose,
        delay: 3,
        interlaced,
    };
    let lay = |flags, kind, level, blend, opacity| AseLayer {
        flags,
        kind,
        level,
        blend,
        opacity,
    };
    let cel = |layer, kind, w, h| AseCel {
        layer,
        x: -1,
        y: 1,
        opacity: 200,
        z: 0,
        kind,
        w,
        h,
        link: 0,
        flip: false,
    };
    let ase = |depth| {
        FileGen::Aseprite {
            w: 12,
            h: 10,
            depth,
            flags: 3,
            transparent: 1,
            palette_len: 16,
            layers: vec![
                lay(1, 0, 0, 0, 255),
                lay(1, 1, 0, 2, 150),
                lay(1, 0, 1, 1, 200),
                lay(1, 2, 0, 0, 255),
            ],
            frames: vec![
                AseFrame {
                    duration: 40,
                    cels: vec![cel(0, 2, 12, 10), cel(2, 0, 6, 5), cel(3, 3, 3, 2)],
                },
                AseFrame {
                    duration: 60,
                    cels: vec![AseCel {
                        kind: 1,
                        ..cel(0, 1, 1, 1)
                    }],
                },
            ],
            tileset: Some((4, 4, 3)),
            tags: vec![("t".into(), 0, 1)],
            slices: vec![],
            seed: 5,
            opaque_palette: false,
        }
        .bytes()
        .unwrap()
    };
    let psd = |g: PsdGen| FileGen::Psd(g).bytes().unwrap();
    let psd_base = PsdGen {
        w: 11,
        h: 7,
        mode: 3,
        depth: 8,
        extra_channels: 1,
        psb: false,
        compression: 1,
        layer_count: -1,
        merged_key: true,
        matted: true,
        real_merged: Some(true),
        transparent_index: None,
        resolution: Some((72 << 16, 1)),
        exif: Some(kura_engine::codec::exif::minimal(1, true)),
        seed: 6,
    };
    let wav_meta = InputGen::WithMeta {
        base: Box::new(InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: 300,
            seed: 7,
        }),
        meta: MetaGen {
            loop_range: Some(LoopRange {
                start: 10,
                end: 200,
            }),
            markers: vec![(5, "a".into())],
            tags: vec![("title".into(), "t".into())],
            ..MetaGen::default()
        },
    }
    .generate()
    .unwrap();
    let mut doc = Graph::new_single();
    doc.push_linear(
        "image.crop",
        1,
        json!({ "rect": { "x": 1, "y": 1, "w": 8, "h": 8 } }),
        3,
    )
    .unwrap();
    doc.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    let mut audio_doc = Graph::new_single();
    audio_doc
        .push_linear("audio.gain", 1, json!({ "db": -3.5 }), 0)
        .unwrap();
    vec![
        (
            "png",
            vec![
                encoded("png", &noise(PixelFormat::Rgba16), json!({})),
                encoded("png", &noise(PixelFormat::Indexed8), json!({})),
                encoded("png", &with_exif, json!({})),
                encoded("png", &frames, json!({})),
                apng(3, false),
                apng(6, true),
            ],
        ),
        (
            "jpeg",
            vec![
                encoded("jpeg", &with_exif, json!({ "progressive": true })),
                encoded("jpeg", &noise(PixelFormat::Gray8), json!({ "chroma": "420" })),
                fixture("jpeg-422.jpg"),
            ],
        ),
        (
            "gif",
            vec![
                encoded("gif", &frames, json!({})),
                FileGen::Gif {
                    w: 14,
                    h: 9,
                    global_len: 16,
                    repeat: Some(2),
                    frames: vec![gf(0, 0, 14, 9, 0, Some(2), 2, false), gf(3, 2, 20, 9, 8, None, 3, true)],
                    seed: 8,
                }
                .bytes()
                .unwrap(),
            ],
        ),
        (
            "webp",
            vec![
                encoded("webp", &with_exif, json!({})),
                fixture("webp-lossy-alpha.webp"),
                fixture("webp-anim.webp"),
            ],
        ),
        ("aseprite", vec![ase(32), ase(8), ase(16)]),
        (
            "psd",
            vec![
                psd(psd_base.clone()),
                psd(PsdGen { psb: true, depth: 16, ..psd_base.clone() }),
                psd(PsdGen { mode: 2, compression: 0, transparent_index: Some(1), extra_channels: 0, ..psd_base.clone() }),
                psd(PsdGen { mode: 0, depth: 1, extra_channels: 0, ..psd_base }),
            ],
        ),
        (
            "svg",
            vec![
                br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><defs><g id="a"><rect width="3" height="3" fill="#f00"/></g><marker id="m"><circle r="1"/></marker></defs><use xlink:href="#a" x="4"/><path d="M0 0 L5 5 L9 2" stroke="#000" marker-mid="url(#m)"/></svg>"##.to_vec(),
                br#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY ns "http://www.w3.org/2000/svg"><!-- c --><!ATTLIST svg x CDATA "1">]><svg xmlns="&ns;" width="4" height="4"><circle cx="2" cy="2" r="1" opacity="0.5"/></svg>"#.to_vec(),
            ],
        ),
        (
            "wav",
            vec![
                encoded("wav", &wav_meta, json!({ "bits": 24 })),
                encoded("wav", &wav_meta, json!({ "float": true })),
            ],
        ),
        ("document", vec![doc.to_canonical_json().unwrap(), audio_doc.to_canonical_json().unwrap()]),
    ]
}

const INTERESTING: [u32; 8] = [
    0,
    1,
    0x7F,
    0xFF,
    0xFFFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
];

/// 1 回の変異。
fn mutate(r: &mut ChaCha8Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    let mut b = seeds[r.next_u32() as usize % seeds.len()].clone();
    for _ in 0..1 + r.next_u32() % 6 {
        if b.is_empty() {
            b.push(0);
        }
        let len = b.len();
        let at = r.next_u32() as usize % len;
        match r.next_u32() % 8 {
            0 => b[at] ^= 1 << (r.next_u32() % 8),
            1 => b[at] = r.next_u32() as u8,
            2 => {
                let v = INTERESTING[r.next_u32() as usize % INTERESTING.len()];
                let bytes = if r.next_u32().is_multiple_of(2) {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                };
                for (k, x) in bytes.iter().enumerate() {
                    if let Some(c) = b.get_mut(at + k) {
                        *c = *x;
                    }
                }
            }
            3 => {
                let n = 1 + r.next_u32() as usize % 16;
                let ins: Vec<u8> = (0..n).map(|_| r.next_u32() as u8).collect();
                b.splice(at..at, ins);
            }
            4 => {
                let n = (1 + r.next_u32() as usize % 32).min(len - at);
                b.drain(at..at + n);
            }
            5 => {
                let n = (1 + r.next_u32() as usize % 64).min(len - at);
                let copy = b[at..at + n].to_vec();
                let to = r.next_u32() as usize % b.len();
                b.splice(to..to, copy);
            }
            6 => {
                let other = &seeds[r.next_u32() as usize % seeds.len()];
                if !other.is_empty() {
                    let from = r.next_u32() as usize % other.len();
                    let n = (1 + r.next_u32() as usize % 128).min(other.len() - from);
                    let end = (at + n).min(b.len());
                    b.splice(at..end, other[from..from + n].iter().copied());
                }
            }
            _ => b.truncate(at.max(1)),
        }
    }
    b
}

fn run(target: &str, input: &[u8]) -> Result<(), String> {
    if target == "document" {
        fuzz_document(input)
    } else {
        fuzz_decode(target, input)
    }
}

fn found_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fuzz-smoke")
}

/// 対象ごとに `budget`（回数か秒数）だけ回し、見つけたものを返す。1 つの対象で 5 件見つけたら、次の対象へ移る。
/// `KURA_FUZZ_ONLY`（`svg,wav` のようにカンマで区切る）があれば、その対象だけを回す。
fn fuzz(budget: impl Fn(u64, f64) -> bool) -> Vec<String> {
    let mut found = Vec::new();
    let dir = found_dir();
    #[allow(clippy::disallowed_methods)]
    let only = std::env::var("KURA_FUZZ_ONLY").ok();
    for (k, (target, seeds)) in targets().into_iter().enumerate() {
        if only
            .as_ref()
            .is_some_and(|o| !o.split(',').any(|t| t == target))
        {
            continue;
        }
        let mut here = 0;
        // 種そのものは通る
        for s in &seeds {
            if let Err(e) = run(target, s) {
                found.push(format!("{target}: 種で {e}"));
            }
        }
        let mut r = kura_engine::determinism::rng(0xF022 + k as u64);
        #[allow(clippy::disallowed_methods)]
        let start = std::time::Instant::now();
        let mut n = 0u64;
        #[allow(clippy::disallowed_methods)]
        while budget(n, start.elapsed().as_secs_f64()) {
            let input = mutate(&mut r, &seeds);
            if let Err(e) = run(target, &input) {
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{target}-{n}.bin"));
                std::fs::write(&path, &input).unwrap();
                found.push(format!("{target}: {e}（{}）", path.display()));
                here += 1;
                if here >= 5 {
                    break;
                }
            }
            n += 1;
        }
        eprintln!("{target}: {n} 回");
    }
    found
}

/// ふだんのテスト：対象ごとに 150 回。
#[test]
fn quick() {
    let found = fuzz(|n, _| n < 150);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}

/// 長く回す：`KURA_FUZZ_SECS` 秒（既定 30）ずつ。
#[test]
#[ignore]
fn long() {
    #[allow(clippy::disallowed_methods)]
    let secs: f64 = std::env::var("KURA_FUZZ_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30.0);
    let found = fuzz(|_, t| t < secs);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}

/// 見つけた入力（`target/fuzz-smoke/<対象>-<回>.bin`）を読み直して流す。直したあとに確かめる。
#[test]
#[ignore]
fn replay() {
    let Ok(dir) = std::fs::read_dir(found_dir()) else {
        return;
    };
    let mut found = Vec::new();
    let mut n = 0;
    for entry in dir {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some((target, _)) = name.split_once('-') else {
            continue;
        };
        n += 1;
        if let Err(e) = run(target, &std::fs::read(&path).unwrap()) {
            found.push(format!("{name}: {e}"));
        }
    }
    eprintln!("{n} 件を流しました");
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
