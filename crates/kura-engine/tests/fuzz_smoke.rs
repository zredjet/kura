//! stable で回す、変異によるファズ（docs/editing.md §13）。`fuzz/`（cargo-fuzz、nightly）と同じ対象
//! （`testing::fuzz_decode`・`fuzz_encode`・`fuzz_document`・`fuzz_peaks`・`fuzz_model`・`fuzz_svg_text`）を、種のファイルに ChaCha8 で変異をかけて呼ぶ。
//! カバレッジは見ない。対象の名前は、デコーダは形式（`ogg`）、自前のエンコーダは `encode_<形式>`（`encode_ogg`）、
//! 文書は `document`、波形の段の読み口は `peaks`（`fuzz/` では `decode_peaks`）、3D モデル（glb）は `model_glb`、
//! フォントを渡した SVG の文字は `svg_text`（`fuzz/` では `decode_svg_text`）。
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
use kura_engine::testing::{
    FuzzEncodeMeta, fuzz_decode, fuzz_document, fuzz_encode, fuzz_encode_input,
    fuzz_encode_input_meta, fuzz_model, fuzz_peaks, fuzz_svg_text,
};
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
    // パレットの番号のまま描ける形（合成は通常・不透明度はすべて 255・パレットは不透明）。カバレッジで、ほかの種からは
    // この経路（index_mode）に届いていなかった
    let ase_indexed = || {
        let opaque = |c: AseCel| AseCel { opacity: 255, ..c };
        FileGen::Aseprite {
            w: 12,
            h: 10,
            depth: 8,
            flags: 3,
            transparent: 1,
            palette_len: 16,
            layers: vec![
                lay(1, 0, 0, 0, 255),
                lay(1, 1, 0, 0, 255),
                lay(1, 0, 1, 0, 255),
                lay(1, 2, 0, 0, 255),
            ],
            frames: vec![
                AseFrame {
                    duration: 40,
                    cels: vec![
                        opaque(cel(0, 2, 12, 10)),
                        opaque(cel(2, 0, 6, 5)),
                        opaque(cel(3, 3, 3, 2)),
                    ],
                },
                AseFrame {
                    duration: 60,
                    cels: vec![opaque(AseCel {
                        kind: 1,
                        ..cel(0, 1, 1, 1)
                    })],
                },
            ],
            tileset: Some((4, 4, 3)),
            tags: vec![],
            slices: vec![],
            seed: 6,
            opaque_palette: true,
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
    // 段 5 の画像の操作と描画
    let mut paint_doc = Graph::new_single();
    paint_doc
        .push_linear(
            "paint.actions",
            1,
            json!({ "actions": [
                { "type": "stroke", "tool": "pen", "color": { "rgba": [255, 0, 0, 200] }, "blend": "over",
                  "brush": { "shape": "circle", "size": 3 }, "pixelPerfect": true, "frame": 0,
                  "points": kura_engine::paint::PointList(vec![
                      kura_engine::geom::IPoint { x: 1, y: 1 },
                      kura_engine::geom::IPoint { x: 20, y: 9 },
                  ]).pack() },
                { "type": "fill", "at": { "x": 3, "y": 3 }, "color": { "rgba": [0, 0, 0, 0] }, "tolerance": 30,
                  "connectivity": "eight", "contiguous": true, "frame": 0 }
            ] }),
            0,
        )
        .unwrap();
    paint_doc
        .push_linear(
            "image.trim_alpha",
            1,
            json!({ "threshold": 0, "mode": "union" }),
            0,
        )
        .unwrap();
    let mut chain_doc = Graph::new_single();
    chain_doc
        .push_linear(
            "image.canvas",
            1,
            json!({ "rect": { "x": -2, "y": -2, "w": 30, "h": 20 }, "fill": null }),
            0,
        )
        .unwrap();
    chain_doc
        .push_linear(
            "image.resize",
            1,
            json!({ "width": { "scale": { "num": 3, "den": 2 } }, "height": "auto", "filter": "lanczos3" }),
            0,
        )
        .unwrap();
    chain_doc
        .push_linear("image.rotate90", 1, json!({ "turn": "ccw" }), 0)
        .unwrap();
    chain_doc
        .push_linear(
            "image.convert",
            1,
            json!({ "format": "indexed8", "colors": 7, "dither": "errorDiffusion", "matte": [255, 255, 255] }),
            0,
        )
        .unwrap();
    let mut sprite_doc = Graph::new_single();
    sprite_doc
        .push_linear(
            "sprite.split_grid",
            1,
            json!({ "grid": { "cellW": 4, "cellH": 4, "offsetX": 0, "offsetY": 0, "gapX": 1, "gapY": 0 },
                    "durationMs": 100, "skipEmpty": true }),
            0,
        )
        .unwrap();
    sprite_doc
        .push_linear(
            "sprite.pack_grid",
            1,
            json!({ "columns": 2, "gapX": 1, "gapY": 1, "fill": [1, 2, 3, 255] }),
            0,
        )
        .unwrap();
    // 段 7：自作の Ogg Vorbis の書き出し（見出しの 19 バイトで設定とメタデータ、残りは f32 の標本。testing::fuzz_encode）
    let sine_samples = |n: usize, step: f64| -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * kura_engine::math::sin(i as f64 * step)) as f32)
            .collect()
    };
    let mut noise_rng = kura_engine::determinism::rng(0x0E7C);
    let noise_samples: Vec<f32> = (0..256)
        .map(|_| (f64::from(noise_rng.next_u32()) / f64::from(u32::MAX) * 2.0 - 1.0) as f32)
        .collect();
    let mut odd_samples = sine_samples(64, 0.3);
    odd_samples.extend([
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1e30,
        -1e-40,
        3.0,
        -0.0,
        f32::MIN_POSITIVE,
    ]);
    let encode_ogg = vec![
        // 44.1 kHz・2 ch・q5・4,410 フレーム（正弦波）
        fuzz_encode_input(5, 7, 2, 4_410, &sine_samples(400, 0.0628)),
        // 8 kHz・1 ch・q−1・3,000 フレーム（雑音）
        fuzz_encode_input(-1, 0, 1, 3_000, &noise_samples),
        // 96 kHz・6 ch・q10・2,000 フレーム（NaN・∞・巨大な値・非正規化数を含む）
        fuzz_encode_input(10, 12, 6, 2_000, &odd_samples),
        // 22.05 kHz・3 ch・q3・37 フレーム（短い）
        fuzz_encode_input(3, 4, 3, 37, &noise_samples[..90]),
        // メタデータ：範囲の中のループ点と、別名で重複する名前・= を含む値・長い値のタグ
        fuzz_encode_input_meta(
            4,
            7,
            2,
            3_000,
            &FuzzEncodeMeta {
                loop_mode: 1,
                loop_a: 100,
                loop_b: 2_900,
                tags: vec![(1, 4), (9, 2), (13, 3), (14, 12)],
            },
            &sine_samples(300, 0.05),
        ),
        // 範囲の外のループ点（終わりが長さを超える）と、DESCRIPTION・ENCODER・書けない名前・空・NUL の値
        fuzz_encode_input_meta(
            0,
            8,
            1,
            2_000,
            &FuzzEncodeMeta {
                loop_mode: 1,
                loop_a: 1_000,
                loop_b: 2_005,
                tags: vec![(3, 1), (10, 5), (16, 0), (21, 7)],
            },
            &noise_samples,
        ),
        // 逆向きで u64 の端に近いループ点と、小文字の別名・LOOPSTART の名前・ASCII の外の名前・4,096 バイトを超える値
        fuzz_encode_input_meta(
            -1,
            16,
            3,
            1_500,
            &FuzzEncodeMeta {
                loop_mode: 2,
                loop_a: 0,
                loop_b: 10,
                tags: vec![(8, 8), (18, 1), (23, 9), (17, 11)],
            },
            &odd_samples,
        ),
        // 空の音声（0 フレーム）に、終わりが u64 の端のループ点と 4,096 バイトちょうどの値
        fuzz_encode_input_meta(
            10,
            12,
            2,
            0,
            &FuzzEncodeMeta {
                loop_mode: 3,
                loop_a: 0,
                loop_b: 0,
                tags: vec![(0, 10)],
            },
            &[],
        ),
        // copyright と、読み取りが copyright に読む小文字の cprt・大文字の CPRT（別の名前）
        fuzz_encode_input_meta(
            0,
            3,
            1,
            1_000,
            &FuzzEncodeMeta {
                loop_mode: 0,
                loop_a: 0,
                loop_b: 0,
                tags: vec![(6, 1), (28, 2), (29, 3)],
            },
            &noise_samples,
        ),
    ];
    let ogg_sine3 = InputGen::Sine {
        rate: 44_100,
        channels: 3,
        frames: 2_000,
        freq: 440,
        loop_range: None,
    }
    .generate()
    .unwrap();
    let ogg_one = InputGen::AudioNoise {
        rate: 48_000,
        channels: 1,
        frames: 1,
        seed: 9,
    }
    .generate()
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
        ("aseprite", vec![ase(32), ase(8), ase(16), ase_indexed()]),
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
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="12" viewBox="0 0 8 6"><defs><linearGradient id="g" x1="0" x2="1"><stop offset="0" stop-color="#f00"/><stop offset="1" stop-color="#00f" stop-opacity="0.3"/></linearGradient><clipPath id="c"><rect width="6" height="4"/></clipPath><pattern id="p" width="2" height="2" patternUnits="userSpaceOnUse"><rect width="1" height="1"/></pattern></defs><g clip-path="url(#c)" transform="rotate(10 4 3)"><rect width="8" height="6" fill="url(#g)"/><ellipse cx="4" cy="3" rx="3" ry="1.5" fill="url(#p)" stroke="#0a0" stroke-width="0.5" stroke-dasharray="1 0.5"/></g><polyline points="0,0 2,5 4,1 7,6" fill="none" stroke="black" style="marker-end:url(#m)"/></svg>"##.to_vec(),
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><style>.a{fill:#123456;opacity:.5}</style><filter id="f"><feGaussianBlur stdDeviation="1"/></filter><mask id="k"><rect width="10" height="5" fill="#fff"/></mask><path class="a" d="M1 1h8v8H1zM3 3l4 4m0-4q-2 3-4 0c1-1 2 2 3 0a2 2 0 1 1 1 1z" mask="url(#k)" filter="url(#f)"/><text x="1" y="9">x</text></svg>"##.to_vec(),
            ],
        ),
        // M11 の検証 S1：フォントを渡した SVG の文字（塊・位置・span・代わりのフォント・use・textPath・textLength・拡大）
        (
            "svg_text",
            vec![
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12" font-family="KuraTest" font-size="6"><text x="0 2 4" y="6">ABCあ<tspan x="1" y="10" font-weight="bold">DEب</tspan>AB</text></svg>"##.as_bytes().to_vec(),
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 12 12" font-family="KuraTest, KuraFallback" font-size="2"><defs><path id="p" d="M1 6 C3 1 9 1 11 6"/><text id="t" y="2" textLength="8">ABAB</text></defs><text><textPath href="#p" startOffset="10%">ABCD<tspan dy="1">E</tspan></textPath></text><use href="#t" transform="scale(2) rotate(10)"/><use href="#t" x="1"/></svg>"##.as_bytes().to_vec(),
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><style>text{font-family:KuraFallback;letter-spacing:1px} .r{direction:rtl}</style><g transform="matrix(1 0 0.5 1 0 0)"><text class="r" y="8" xml:space="preserve">  DE ب A  <tspan y="12">C</tspan></text></g><svg viewBox="0 0 4 4" width="8" height="8"><text y="3" font-size="3">A</text></svg></svg>"##.as_bytes().to_vec(),
            ],
        ),
        (
            "wav",
            vec![
                encoded("wav", &wav_meta, json!({ "bits": 24 })),
                encoded("wav", &wav_meta, json!({ "float": true })),
            ],
        ),
        // 段 6 の音声の読み取り（自作の AIFF・FLAC の書き出しと、他のツールで作ったフィクスチャ）
        (
            "aiff",
            vec![
                encoded("aiff", &wav_meta, json!({ "bits": 24 })),
                encoded("aiff", &wav_meta, json!({ "float": true })),
                fixture("aifc-f32.aif"),
            ],
        ),
        (
            "flac",
            vec![
                encoded("flac", &wav_meta, json!({ "bits": 16 })),
                encoded("flac", &wav_meta, json!({ "bits": 24 })),
                fixture("flac-24.flac"),
            ],
        ),
        (
            "mp3",
            vec![
                fixture("mp3-mpeg2-mono.mp3"),
                fixture("mp3-lame.mp3"),
                fixture("mp3-free.mp3"),
                fixture("mp2-layer2.mp3"),
            ],
        ),
        (
            "ogg",
            vec![
                fixture("ogg-vorbis-loop.ogg"),
                fixture("ogg-opus.ogg"),
                // 自作の書き出し（段 7。短いもの）
                encoded("ogg", &wav_meta, json!({})),
                encoded("ogg", &ogg_sine3, json!({ "quality": 0 })),
                encoded("ogg", &ogg_one, json!({ "quality": 10 })),
            ],
        ),
        ("encode_ogg", encode_ogg),
        (
            "m4a",
            vec![
                fixture("alac-24.m4a"),
                fixture("aac-lc-afconvert.m4a"),
                fixture("aac-lc-ffmpeg.m4a"),
                fixture("aac-he.m4a"),
            ],
        ),
        ("aac", vec![fixture("aac-lc.aac")]),
        (
            "document",
            vec![
                doc.to_canonical_json().unwrap(),
                audio_doc.to_canonical_json().unwrap(),
                paint_doc.to_canonical_json().unwrap(),
                chain_doc.to_canonical_json().unwrap(),
                sprite_doc.to_canonical_json().unwrap(),
            ],
        ),
        // 波形の段のバイナリ（kura-peaks。解析 audio.peaks の正規の形。cargo-fuzz の対象は decode_peaks）
        ("peaks", peak_seeds()),
        // 段 9：アンカー（PNG・APNG・f32 の WAV ＋私用のチャンク。cargo-fuzz の対象は decode_anchor）
        ("anchor", anchor_seeds(&with_exif, &frames, &wav_meta)),
        // M11：3D モデル（glb の情報・焼き込み・描画。cargo-fuzz の対象も model_glb）
        (
            "model_glb",
            kura_engine::model::sample_glbs()
                .unwrap()
                .into_iter()
                .map(|(_, b)| b)
                // M11 の検証 S3：数と長さを抑えて読む項目（拡張の名前・属性のキー・min・max・uri・generator・材質と
                // プリミティブの extensions）と、確保しない項目（extras・animations）
                .chain([kura_engine::model::glb_bytes(
                    &json!({
                        "asset": { "version": "2.0", "generator": "あ".repeat(100) },
                        "extensionsUsed": ["KHR_materials_unlit", "KHR_draco_mesh_compression", "X"],
                        "extensionsRequired": ["KHR_materials_unlit"],
                        "extras": [[1, 2, { "a": "b" }], "c"],
                        "animations": [{ "channels": [], "samplers": [] }],
                        "scene": 0,
                        "scenes": [{ "nodes": [0, 0, 1] }],
                        "nodes": [{ "mesh": 0, "children": [1] }, { "mesh": 0, "matrix": [1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1] }],
                        "meshes": [{ "primitives": [{
                            "attributes": { "POSITION": 0, "TEXCOORD_0": 0, "TEXCOORD_01": 0, "_X": 0, "POSITION": 0 },
                            "material": 0,
                            "extensions": { "EXT_meshopt_compression": { "x": [1] } }
                        }] }],
                        "materials": [{ "alphaMode": "MASK", "extensions": { "KHR_materials_unlit": {} } }],
                        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                                        "min": [0, 0, 0, 9, 9], "max": [1, 1, 0] }],
                        "bufferViews": [{ "buffer": 0, "byteLength": 36 }],
                        "buffers": [{ "byteLength": 36, "uri": "data:," }],
                        "images": [{ "uri": "x.png", "mimeType": "image/png" }]
                    }),
                    &[0u8; 36],
                )])
                .collect(),
        ),
    ]
}

/// アンカーの種：EXIF と ICC 付きの画像・パレットのフレームの列・メタデータ付きの音声。
fn anchor_seeds(image: &Value, frames: &Value, audio: &Value) -> Vec<Vec<u8>> {
    use kura_engine::codec::anchor::{AnchorContext, write_verified};
    [image, frames, audio]
        .into_iter()
        .map(|v| {
            let stamp = kura_engine::codec::DecodeStamp {
                codec: "fake-platform-dec/1".into(),
                tier: kura_engine::codec::Tier::Platform,
                reproducible: false,
                value_digest: v.digest().unwrap(),
                env: Some(kura_engine::testing::FakePlatformDecoder::default_env()),
            };
            write_verified(
                v,
                &stamp,
                &AnchorContext::default(),
                &kura_engine::Limits::default(),
                &NoMonitor,
            )
            .unwrap()
            .0
        })
        .collect()
}

/// 波形の段のバイナリの種：5 段・一部の段・1 段・長さ 0。
fn peak_seeds() -> Vec<Vec<u8>> {
    let engine = kura_engine::Engine::new().unwrap();
    let audio = |frames, channels| {
        InputGen::AudioNoise {
            rate: 22_050,
            channels,
            frames,
            seed: 5,
        }
        .generate()
        .unwrap()
    };
    [
        (audio(70_001, 2), json!({})),
        (
            audio(9_000, 1),
            json!({ "finest": 4_096, "coarsest": 16_384 }),
        ),
        (audio(300, 3), json!({ "finest": 1_024, "coarsest": 1_024 })),
        (audio(0, 1), json!({})),
    ]
    .into_iter()
    .map(|(v, params)| {
        engine
            .analyze_dyn(
                &kura_engine::analysis::AnalysisRequest {
                    id: "audio.peaks".into(),
                    v: None,
                    params,
                },
                &v,
                &NoMonitor,
            )
            .unwrap()
            .output
    })
    .collect()
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
    } else if target == "peaks" {
        fuzz_peaks(input)
    } else if target == "model_glb" {
        fuzz_model(input)
    } else if target == "svg_text" {
        fuzz_svg_text(input)
    } else if let Some(format) = target.strip_prefix("encode_") {
        fuzz_encode(format, input)
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

/// 種を cargo-fuzz の種の置き場（`fuzz/corpus/<対象>/`。git には入れない）に書き出す（fuzz/README.md）。
/// 対象の名前は、デコーダは `decode_<形式>`、自前のエンコーダは `encode_<形式>`、文書は `document`、3D モデルは `model_glb`。
#[test]
#[ignore]
fn export_seeds() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus");
    let mut n = 0;
    for (target, seeds) in targets() {
        let name = if target == "document" || target == "model_glb" || target.starts_with("encode_")
        {
            target.to_string()
        } else {
            format!("decode_{target}")
        };
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, s) in seeds.iter().enumerate() {
            std::fs::write(dir.join(format!("seed-{i:03}")), s).unwrap();
            n += 1;
        }
    }
    // 登録されたすべての操作の例を、1 ノードの文書にして種にする（引数の形の種類が多い）
    let engine = kura_engine::Engine::new().unwrap();
    let dir = root.join("document");
    for op in engine.registry().all() {
        for ex in op.examples().unwrap() {
            let mut g = Graph::new_single();
            g.push_linear(op.id(), op.version(), ex.params.clone(), 0)
                .unwrap();
            let file = format!("op-{}-v{}-{}", op.id(), op.version(), ex.name);
            std::fs::write(dir.join(file), g.to_canonical_json().unwrap()).unwrap();
            n += 1;
        }
    }
    // デコーダのゴールデンに凍結した作り方（`FileGen`）のファイルも種にする（形の種類が多い）
    let goldens = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens-codecs");
    for entry in std::fs::read_dir(&goldens).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.contains("-dec@") {
            continue;
        }
        for (i, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let file: FileGen = serde_json::from_value(v["source"].clone()).unwrap();
            let Ok(bytes) = file.bytes() else { continue };
            let dir = root.join(format!("decode_{}", file.format()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("golden-{}-{i:03}", name.trim_end_matches(".jsonl"))),
                bytes,
            )
            .unwrap();
            n += 1;
        }
    }
    eprintln!("{n} 個の種を {} に書きました", root.display());
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

/// 種が、デコーダの主な経路に届く（カバレッジで見つけた穴：aseprite のパレットの番号のまま描く経路には、どの種も届いていなかった）。
#[test]
fn seeds_reach_indexed_aseprite() {
    let (_, seeds) = targets()
        .into_iter()
        .find(|(f, _)| *f == "aseprite")
        .unwrap();
    let indexed = seeds.iter().any(|b| {
        let v = CodecSet::pinned().decode(
            &mut std::io::Cursor::new(b.clone()),
            "aseprite",
            &Default::default(),
            &Default::default(),
            &NoMonitor,
        );
        match v {
            Ok((Value::Frames(f), _)) => f
                .frames()
                .iter()
                .all(|x| x.px.format() == PixelFormat::Indexed8),
            Ok((Value::Image(i), _)) => i.px.format() == PixelFormat::Indexed8,
            _ => false,
        }
    });
    assert!(indexed);
}
