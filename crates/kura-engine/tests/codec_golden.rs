//! コーデックのゴールデン（docs/editing.md §8.5）。固定のエンコーダが書くバイト列（R2）と、固定のデコーダが読む値（R1）を凍結する。
//!
//! - ファイルは `goldens-codecs/<コーデックの ID の / を @ にしたもの>.jsonl`。ID は読み取り（`-dec`）と書き出し（`-enc`）で別。
//!   - `png-rs-enc@1.jsonl`：1 行に `{"decoder","file","format","input","name","options","value"}`。
//!     file は書いたバイト列のダイジェスト、value はそれを `decoder` で読み直した値のダイジェスト。
//!     読み直しのデコーダが値の再現を約束しない（`reproducible()` が偽）行は value を書かず、
//!     書いたバイト列（file）と、読み直した形（長さ・レート・チャンネル）が元の値と同じことだけを照らす。値を持たないのに
//!     読み直しのデコーダが再現できる行、値を持つのに再現できない行は失敗にする。
//!     例外は、再現を約束しなかったデコーダの版を上げて旧版を残さなかったとき（`SUPERSEDED`。段 9 の
//!     `ogg-vorbis-symphonia-dec/2` → `/3`）：旧版の識別子の行（value なし）は書き換えず、後の版で読み直して形だけを照らし、
//!     後の版で読み直した値は、同じ例を新しい名前（`<名前>-dec3`）で足した行が凍結する。
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
    /// 読み直した値のダイジェスト。`decoder` が値の再現を約束しないときは持たない（形だけを照らす）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<String>,
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
    // 段 7：Ogg Vorbis（自作の書き出し）。段 7・8 の読み直しのデコーダ（`ogg-vorbis-symphonia-dec/2`）は値の再現を
    // 約束しなかったので、この名前の行は value を持たず、書いたバイト列と読み直した形（長さ・レート・チャンネル）だけを
    // 照らす（段 9 からは `/3` で読み直す。`SUPERSEDED`）。`/3` で読み直した値は、関数の終わりで足す `<名前>-dec3` の行が凍結する
    let sine = |rate, channels, frames| InputGen::Sine {
        rate,
        channels,
        frames,
        freq: 440,
        loop_range: None,
    };
    let audio_noise = |rate, channels, frames, seed| InputGen::AudioNoise {
        rate,
        channels,
        frames,
        seed,
    };
    for (name, options, input) in [
        ("ogg-sine-44k-stereo", json!({}), sine(44_100, 2, 30_000)),
        (
            "ogg-noise-48k-mono-q0",
            json!({ "quality": 0 }),
            audio_noise(48_000, 1, 20_000, 50),
        ),
        (
            "ogg-noise-8k-mono-qm1",
            json!({ "quality": -1 }),
            audio_noise(8_000, 1, 9_000, 51),
        ),
        (
            "ogg-sine-22k-stereo-q10",
            json!({ "quality": 10 }),
            sine(22_050, 2, 15_000),
        ),
        (
            "ogg-noise-96k-stereo-q5",
            json!({ "quality": 5 }),
            audio_noise(96_000, 2, 20_000, 52),
        ),
        ("ogg-sine-44k-3ch", json!({}), sine(44_100, 3, 12_000)),
        (
            "ogg-noise-48k-6ch-q3",
            json!({ "quality": 3 }),
            audio_noise(48_000, 6, 10_000, 53),
        ),
        (
            "ogg-meta-loop-tags",
            json!({ "quality": 4 }),
            InputGen::WithMeta {
                base: Box::new(sine(44_100, 2, 20_000)),
                meta: MetaGen {
                    loop_range: Some(LoopRange {
                        start: 1_000,
                        end: 15_000,
                    }),
                    tags: vec![
                        ("title".into(), "効果音".into()),
                        ("artist".into(), "kura".into()),
                    ],
                    ..MetaGen::default()
                },
            },
        ),
        ("ogg-noise-short", json!({}), audio_noise(44_100, 2, 37, 54)),
        ("ogg-empty", json!({}), audio_noise(44_100, 1, 0, 55)),
    ] {
        v.push((name, "ogg", options, input));
    }
    // 段 7 の検証で足した行：エンコーダの 16 のテンプレート（8 つのレートの族 × stereo・uncoupled）と 9 つのレートの
    // stereo・uncoupled（`ogg_templates_are_covered` が確かめる）、4・5・7・8 チャンネル、品質の端（−1・10）、26 kHz 以上の
    // 衝撃の入力（短い窓と aoTuV の M3）、44.1 kHz・品質 0 の衝撃の入力（aoTuV の M2）、だんだん大きくなるトーン（aoTuV の
    // M9）。
    // CI は照合用の C を持たないので、書くバイト列の変化に気づけるのはここだけ（ほかの設定はテストが値の近さしか見ない）
    let bursts = |rate: u32, channels, frames, seed| InputGen::Bursts {
        rate,
        channels,
        frames,
        period: u64::from(rate / 8),
        seed,
    };
    // だんだん大きくなるトーン（0.5 秒。stereo の長い窓で、前のブロックより大きくなる。aoTuV の M9 の epeak が point stereo の
    // 域で効く。周波数と大きさは、M9 を切る変更と閾値（1 dB）を変える変更でバイト列が変わるものを選んだ）
    let swell = |rate: u32, freqs: &[u32], from_db, to_db| InputGen::Swell {
        rate,
        channels: 2,
        frames: u64::from(rate / 2),
        freqs: freqs.to_vec(),
        from_db,
        to_db,
    };
    for (name, quality, input) in [
        (
            "ogg-noise-8k-stereo-q2",
            2,
            audio_noise(8_000, 2, 4_000, 60),
        ),
        (
            "ogg-noise-11k-stereo-q10",
            10,
            audio_noise(11_025, 2, 4_000, 61),
        ),
        (
            "ogg-noise-11k-mono-q1",
            1,
            audio_noise(11_025, 1, 4_000, 62),
        ),
        (
            "ogg-noise-16k-stereo-qm1",
            -1,
            audio_noise(16_000, 2, 6_000, 63),
        ),
        ("ogg-noise-16k-4ch-q6", 6, audio_noise(16_000, 4, 4_000, 64)),
        (
            "ogg-noise-22k-mono-q7",
            7,
            audio_noise(22_050, 1, 6_000, 65),
        ),
        ("ogg-noise-22k-5ch-q2", 2, audio_noise(22_050, 5, 4_000, 66)),
        (
            "ogg-noise-32k-stereo-q4",
            4,
            audio_noise(32_000, 2, 8_000, 67),
        ),
        (
            "ogg-noise-32k-mono-q8",
            8,
            audio_noise(32_000, 1, 8_000, 68),
        ),
        (
            "ogg-noise-44k-stereo-qm1",
            -1,
            audio_noise(44_100, 2, 6_000, 69),
        ),
        (
            "ogg-noise-44k-stereo-q10",
            10,
            audio_noise(44_100, 2, 6_000, 70),
        ),
        ("ogg-noise-44k-7ch-q1", 1, audio_noise(44_100, 7, 4_000, 71)),
        (
            "ogg-noise-48k-stereo-q9",
            9,
            audio_noise(48_000, 2, 8_000, 72),
        ),
        (
            "ogg-noise-96k-mono-q6",
            6,
            audio_noise(96_000, 1, 10_000, 73),
        ),
        (
            "ogg-noise-192k-stereo-q10",
            10,
            audio_noise(192_000, 2, 12_000, 74),
        ),
        (
            "ogg-noise-192k-8ch-qm1",
            -1,
            audio_noise(192_000, 8, 6_000, 75),
        ),
        ("ogg-bursts-48k-stereo-q0", 0, bursts(48_000, 2, 24_000, 76)),
        ("ogg-bursts-32k-mono-qm1", -1, bursts(32_000, 1, 16_000, 77)),
        ("ogg-bursts-96k-3ch-q5", 5, bursts(96_000, 3, 36_000, 78)),
        (
            "ogg-swell-32k-stereo-qm1",
            -1,
            swell(32_000, &[2_000, 6_000, 10_000], -50, -3),
        ),
        (
            "ogg-swell-44k-stereo-qm1",
            -1,
            swell(44_100, &[9_000, 12_000], -40, -10),
        ),
        (
            "ogg-swell-48k-stereo-q0",
            0,
            swell(48_000, &[2_000, 6_000, 10_000], -50, -3),
        ),
        (
            "ogg-swell-96k-stereo-q0",
            0,
            swell(96_000, &[9_000, 12_000], -30, -3),
        ),
        // aoTuV の M2（後エコーの補正）に届く衝撃の入力。M2 の上限（`poste` と `no1 + 30` の 30 dB）を変える変更、M2 を
        // 切る変更、npeak を −1 にしない変更のどれでもバイト列が変わる行を選んだ（上の 3 つの衝撃の行は M2 の変更で
        // 変わらない。届くかはレート・品質・種による）。M2 の前の判定（`postnoise_detection` の閾値）を変える変更では、
        // どの行も変わらない
        ("ogg-bursts-44k-mono-q0", 0, bursts(44_100, 1, 22_050, 79)),
        ("ogg-bursts-44k-stereo-q0", 0, bursts(44_100, 2, 22_050, 80)),
    ] {
        v.push((name, "ogg", json!({ "quality": quality }), input));
    }
    // 段 9：上の Ogg Vorbis の例を、同じ設定・入力のまま `<名前>-dec3` の名前でもう一度並べる。読み直しのデコーダが
    // 値を再現できる `ogg-vorbis-symphonia-dec/3` になったので、この行が読み直した値（value）を凍結する（元の名前の行は
    // `/2` の識別子のまま value を持たず、書き換えないため。`SUPERSEDED`）。書いたバイト列は元の名前の行と同じ
    let ogg: Vec<_> = v
        .iter()
        .filter(|e| e.1 == "ogg" && !e.0.ends_with(REREAD_SUFFIX))
        .cloned()
        .collect();
    for (name, format, options, input) in ogg {
        let twin: &'static str = Box::leak(format!("{name}{REREAD_SUFFIX}").into_boxed_str());
        v.push((twin, format, options, input));
    }
    v
}

/// 段 9 で、読み直した値を凍結するために同じ Ogg Vorbis の例を足し直した行の名前の接尾辞。
const REREAD_SUFFIX: &str = "-dec3";

/// 再現を約束しなかったデコーダの版を上げ、旧版を残さなかったもの：（凍結した行の識別子, 今の版の識別子）。
/// 凍結した行（value なし）は書き換えないので、今の版で読み直して形だけを照らす（値は `REREAD_SUFFIX` の行が凍結する）。
/// 足すのは、旧版が値の再現を約束しなかった（行が value を持たない）ときだけ。
const SUPERSEDED: &[(&str, &str)] = &[
    // 段 9：symphonia の表の超越関数を libm クレートにして、値を再現できるようにした（公開前なので旧版は残さない）
    ("ogg-vorbis-symphonia-dec/2", "ogg-vorbis-symphonia-dec/3"),
];

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
    decode_stamped(codecs, decoder, bytes, format).map(|(_, stamp)| stamp.value_digest.to_hex())
}

/// 識別子のデコーダで読み、値と記録を返す。
fn decode_stamped(
    codecs: &CodecSet,
    decoder: &str,
    bytes: &[u8],
    format: &str,
) -> Result<(kura_engine::Value, kura_engine::codec::DecodeStamp), String> {
    codecs
        .decode_with(
            decoder,
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .map_err(|e| e.to_string())
}

/// 凍結した行の読み直しを、凍結したときのデコーダ（旧版を含む）で照らす。値を持つ行は値のダイジェストを、持たない行
/// （デコーダが値の再現を約束しない）は読み直した形が元の値の形（長さ・レート・チャンネル）と同じことを照らす。
fn check_reread(
    codecs: &CodecSet,
    key: &str,
    old: &Line,
    bytes: &[u8],
    want_shape: &kura_engine::Shape,
    failures: &mut String,
) {
    let name = &old.name;
    // 旧版を残さなかったデコーダの行は、今の版で読み直して形だけを照らす（`SUPERSEDED`）
    if let Some(&(_, now)) = SUPERSEDED.iter().find(|(gone, _)| *gone == old.decoder) {
        if old.value.is_some() {
            let _ = writeln!(
                failures,
                "{key} {name}：{} の行が値を持っています（旧版を残さずに版を上げてよいのは、値の再現を約束しなかったデコーダだけ）",
                old.decoder
            );
            return;
        }
        if codecs.decoder_by_id(&old.decoder).is_some() {
            let _ = writeln!(
                failures,
                "{key} {name}：{} はまだあります（SUPERSEDED から外す）",
                old.decoder
            );
            return;
        }
        match decode_stamped(codecs, now, bytes, &old.format) {
            Ok((back, _)) if &back.shape() == want_shape => {}
            Ok((back, _)) => {
                let _ = writeln!(
                    failures,
                    "{key} {name}：{now}（{} の後の版）で読み直した形が元の値と違う（{want_shape:?} → {:?}）",
                    old.decoder,
                    back.shape()
                );
            }
            Err(e) => {
                let _ = writeln!(
                    failures,
                    "{key} {name}：{now}（{} の後の版）で読めません: {e}",
                    old.decoder
                );
            }
        }
        return;
    }
    let decoder = &old.decoder;
    let (back, stamp) = match decode_stamped(codecs, decoder, bytes, &old.format) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(
                failures,
                "{key} {name}：凍結したときのデコーダ {decoder} で読めません（旧版のデコーダを残す）: {e}"
            );
            return;
        }
    };
    match (&old.value, stamp.reproducible) {
        (Some(want), true) => {
            let got = stamp.value_digest.to_hex();
            if &got != want {
                let _ = writeln!(
                    failures,
                    "{key} {name}：{decoder} で読み直した値が凍結した値と違う（{want} → {got}）。読む値は変えない（変えるならデコーダの版を上げて旧版を残す）"
                );
            }
        }
        (Some(_), false) => {
            let _ = writeln!(
                failures,
                "{key} {name}：{decoder} は値の再現を約束しないので、読み直した値（value）を凍結できません"
            );
        }
        (None, true) => {
            let _ = writeln!(
                failures,
                "{key} {name}：{decoder} は値を再現できるのに、行が読み直した値（value）を持っていません"
            );
        }
        (None, false) => {
            if &back.shape() != want_shape {
                let _ = writeln!(
                    failures,
                    "{key} {name}：{decoder} で読み直した形が元の値と違う（{want_shape:?} → {:?}）",
                    back.shape()
                );
            }
        }
    }
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
    // 同じ設定・入力の例（`REREAD_SUFFIX` の行）は、書いたものを使い回す（書くバイト列は同じ。時間を倍にしない）
    let mut written: BTreeMap<Vec<u8>, (Vec<u8>, kura_engine::codec::EncodedStamp)> =
        BTreeMap::new();
    for (name, format, options, input) in examples() {
        let value = input.generate().unwrap();
        let memo = canonical(&(format, &options, &input)).unwrap();
        let (bytes, stamp) = written
            .entry(memo)
            .or_insert_with(|| {
                codecs
                    .encode_bytes(&value, format, &options, &NoMonitor)
                    .unwrap()
            })
            .clone();
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
        if matches!(format, "png" | "wav" | "aiff" | "flac" | "ogg") {
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
            // 値を再現できないデコーダで読み直す行は、値を書かない（形だけを照らす）
            value: dstamp.reproducible.then(|| dstamp.value_digest.to_hex()),
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
            Some(old) => check_reread(&codecs, &key, old, &bytes, &value.shape(), &mut failures),
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

/// Ogg Vorbis のエンコーダ（aoTuV の `setup_list`。kura の範囲で選ばれる 16 個）のテンプレートの名前：レートの範囲
/// （`samplerate_min_restriction`〜`max` の両端を含む。表の先頭（48・44・32・22・16・11・8・X の順）から最初に合うもの
/// なので、境目のレートは前の族になる）とチャンネル（2 は stereo、ほかは uncoupled）から。8 kHz 以下の XX は、8 kHz の
/// テンプレートと品質の範囲が同じなので選ばれない。
fn ogg_template(rate: u32, channels: usize) -> String {
    let family = match rate {
        45_000..=50_000 => "48",
        40_000..=44_999 => "44",
        26_000..=39_999 => "32",
        19_000..=25_999 => "22",
        15_000..=18_999 => "16",
        9_000..=14_999 => "11",
        8_000..=8_999 => "8",
        _ => "X",
    };
    let coupling = if channels == 2 { "stereo" } else { "uncoupled" };
    format!("{family}_{coupling}")
}

/// コーデックのゴールデンの `ogg-vorbis-kura-enc/1` の行が、エンコーダの書き方の分かれ目を覆う（抜けたら落ちる。
/// 照合用の C のない CI で、書くバイト列の変化に気づくため）：16 のテンプレート（8 つのレートの族 ×
/// stereo・uncoupled）と 9 つのレート（X は 96・192 kHz）の stereo・uncoupled、1〜8 チャンネル、品質 −1 と 10、
/// 26 kHz 以上の衝撃の入力（`bursts`。stereo と uncoupled。aoTuV の M3）、44.1 kHz・品質 0 の衝撃の入力（stereo と
/// uncoupled。aoTuV の M2。M2 の変更でバイト列が変わることを確かめた行）、26 kHz 以上の stereo の低い品質のだんだん大きく
/// なるトーン（`swell`。aoTuV の M9 の epeak は point stereo の域でだけ効く）。
#[test]
fn ogg_templates_are_covered() {
    let dir = read_dir();
    let (path, text) = dir
        .get(&(Kind::Enc, "ogg-vorbis-kura-enc/1".to_string()))
        .expect("ogg-vorbis-kura-enc@1.jsonl がありません");
    let lines: Vec<Line> = parse(path, text);
    let mut templates = BTreeMap::new();
    let mut rates = BTreeMap::new();
    let mut channels = BTreeMap::new();
    let mut qualities = BTreeMap::new();
    let mut bursts = BTreeMap::new();
    let mut m2_bursts = BTreeMap::new();
    let mut swells = 0;
    for l in &lines {
        let kura_engine::Value::Audio(a) = l.input.generate().unwrap() else {
            panic!("{}：音声ではありません", l.name);
        };
        let (rate, ch) = (a.rate(), a.channels());
        let q = l
            .options
            .get("quality")
            .and_then(|q| q.as_i64())
            .unwrap_or(5);
        let coupling = if ch == 2 { "stereo" } else { "uncoupled" };
        templates.insert(ogg_template(rate, ch), ());
        rates.insert((rate, coupling), ());
        channels.insert(ch, ());
        qualities.insert(q, ());
        if matches!(l.input, InputGen::Bursts { .. }) && rate >= 26_000 && a.frames() > 0 {
            bursts.insert(coupling, ());
        }
        if matches!(l.input, InputGen::Bursts { .. }) && rate == 44_100 && q == 0 && a.frames() > 0
        {
            m2_bursts.insert(coupling, ());
        }
        if matches!(l.input, InputGen::Swell { .. }) && rate >= 26_000 && ch == 2 && q <= 0 {
            swells += 1;
        }
    }
    let mut missing = Vec::new();
    for family in ["8", "11", "16", "22", "32", "44", "48", "X"] {
        for coupling in ["stereo", "uncoupled"] {
            let t = format!("{family}_{coupling}");
            if !templates.contains_key(&t) {
                missing.push(format!("テンプレート {t}"));
            }
        }
    }
    for rate in [
        8_000u32, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
    ] {
        for coupling in ["stereo", "uncoupled"] {
            if !rates.contains_key(&(rate, coupling)) {
                missing.push(format!("{rate} Hz の {coupling}"));
            }
        }
    }
    for ch in 1..=8usize {
        if !channels.contains_key(&ch) {
            missing.push(format!("{ch} チャンネル"));
        }
    }
    for q in [-1i64, 10] {
        if !qualities.contains_key(&q) {
            missing.push(format!("品質 {q}"));
        }
    }
    for coupling in ["stereo", "uncoupled"] {
        if !bursts.contains_key(coupling) {
            missing.push(format!("26 kHz 以上の衝撃の入力（{coupling}）"));
        }
        if !m2_bursts.contains_key(coupling) {
            missing.push(format!(
                "44.1 kHz・品質 0 の衝撃の入力（aoTuV の M2。{coupling}）"
            ));
        }
    }
    if swells == 0 {
        missing.push("26 kHz 以上・stereo・品質 0 以下のだんだん大きくなるトーン".into());
    }
    assert!(
        missing.is_empty(),
        "ogg-vorbis-kura-enc@1.jsonl に行がありません（新しい名前の行を足す）: {missing:?}"
    );
    // テンプレートの表の端（範囲の境目と kura の範囲の端）
    assert_eq!(ogg_template(8_000, 1), "8_uncoupled");
    assert_eq!(ogg_template(9_000, 2), "11_stereo");
    assert_eq!(ogg_template(26_000, 2), "32_stereo");
    assert_eq!(ogg_template(40_000, 2), "44_stereo");
    assert_eq!(ogg_template(45_000, 2), "48_stereo");
    assert_eq!(ogg_template(50_000, 1), "48_uncoupled");
    assert_eq!(ogg_template(50_001, 3), "X_uncoupled");
    assert_eq!(ogg_template(192_000, 8), "X_uncoupled");
}

/// 旧版を残さなかったデコーダ（`SUPERSEDED`）の行には、今の版で読み直した値を凍結した `<名前>-dec3` の行が必ずある：
/// 同じ設定・入力・書いたバイト列で、読み直しのデコーダが今の版、value を持つ。今の版は登録されていて値を再現できる。
#[test]
fn superseded_rows_have_frozen_rereads() {
    let codecs = CodecSet::pinned();
    for &(gone, now) in SUPERSEDED {
        assert!(
            codecs.decoder_by_id(gone).is_none(),
            "{gone} はまだあります"
        );
        let d = codecs
            .decoder_by_id(now)
            .unwrap_or_else(|| panic!("{now} がありません"));
        assert!(d.reproducible(), "{now} は値を再現できるはず");
    }
    let mut seen = 0;
    for ((kind, id), (path, text)) in read_dir() {
        if kind != Kind::Enc {
            continue;
        }
        let lines: Vec<Line> = parse(&path, &text);
        for old in lines.iter() {
            let Some(&(_, now)) = SUPERSEDED.iter().find(|(gone, _)| *gone == old.decoder) else {
                continue;
            };
            seen += 1;
            let twin = format!("{}{REREAD_SUFFIX}", old.name);
            let t = lines
                .iter()
                .find(|l| l.name == twin)
                .unwrap_or_else(|| panic!("{id} {}：{twin} の行がありません", old.name));
            assert_eq!(
                (&t.format, &t.file, t.decoder.as_str()),
                (&old.format, &old.file, now),
                "{id} {twin}"
            );
            assert!(
                same(&t.options, &old.options) && same(&t.input, &old.input),
                "{id} {twin}"
            );
            assert!(t.value.is_some(), "{id} {twin}：読み直した値がありません");
        }
    }
    // 段 7 の Ogg Vorbis の 35 行
    assert_eq!(seen, 35);
}

/// 値を凍結する symphonia の Vorbis・AAC の経路（`third_party/` の写し）は、std の超越関数（OS の libm）と `mul_add`
/// を呼ばない（段 9。上流の版を上げて写しを作り直したとき、差し替えを当て忘れると、OS によっては値が変わってもこの
/// 機械のゴールデンが通ってしまう（手元の macOS では Vorbis の値が OS の libm でも同じだった）ので、ソースで見張る）。
/// 超越関数は libm クレート（`libm::`）か、各写しの `kura_math` を使う。`sqrt`・`floor` などの IEEE で決まるものはよい。
#[test]
fn symphonia_copies_do_not_call_os_libm() {
    const BANNED: &[&str] = &[
        "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh", "asinh",
        "acosh", "atanh", "exp", "exp2", "exp_m1", "ln", "log", "log2", "log10", "ln_1p", "powf",
        "powi", "cbrt", "hypot", "sin_cos", "mul_add",
    ];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party");
    let mut files = 0;
    let mut bad = Vec::new();
    for krate in [
        "symphonia-core",
        "symphonia-codec-aac",
        "symphonia-codec-vorbis",
        "symphonia-format-ogg",
    ] {
        // 再帰せず、積み上げで src/ の下をたどる
        let mut dirs = vec![root.join(krate).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in
                std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                files += 1;
                let text = std::fs::read_to_string(&path).unwrap();
                for (no, line) in text.lines().enumerate() {
                    // 行の注釈は見ない（文字列の中の // は、この写しにはない）
                    let code = line.split("//").next().unwrap_or("");
                    for f in BANNED {
                        let calls = [format!(".{f}("), format!("f32::{f}("), format!("f64::{f}(")];
                        if calls.iter().any(|c| code.contains(c.as_str())) {
                            bad.push(format!("{}:{}: {}", path.display(), no + 1, line.trim()));
                        }
                    }
                }
            }
        }
    }
    assert!(files > 50, "写しのソースが見つかりません（{files} 個）");
    assert!(
        bad.is_empty(),
        "OS の libm を呼んでいます:\n{}",
        bad.join("\n")
    );
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
        // 段 6：音声（再現できるデコーダだけ。段 6〜8 の Vorbis・AAC は値が環境に依存したので凍結しなかった）
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
        // 段 9：Vorbis・AAC（symphonia の表の超越関数を libm クレートにして値を再現できるようにした。
        // `ogg-vorbis-symphonia-dec/3`・`m4a-aac-symphonia-dec/2`・`aac-adts-symphonia-dec/2`）。libvorbis（aoTuV）・
        // ffmpeg・afconvert が書いたもの。3・5 チャンネルは residue 1 の uncoupled（段 7 の不具合の修正の経路）
        "ogg-vorbis-loop.ogg",
        "ogg-vorbis-ffmpeg.ogg",
        "ogg-vorbis-3ch.ogg",
        "ogg-vorbis-5ch.ogg",
        "aac-lc-afconvert.m4a",
        "aac-lc-ffmpeg.m4a",
        "aac-lc.aac",
    ] {
        let name: &'static str = Box::leak(format!("fixture-{path}").into_boxed_str());
        v.push((name, FileGen::Fixture { path: path.into() }));
    }
    // 段 9：アンカー（anchor-kura-dec/1）。画素の形式・ICC・向き 6 の EXIF・テキスト・スライス・格子・ピボット、
    // フレームの列（パレット・16 ビット）、音声（拡張形式・ループ点・マーカー・タグ・NaN と ±∞）。
    // `FileGen::Anchor` はその場の書き手で書いて読み直す（`write_verified`）ので、ここの値は作り方の値のダイジェストと必ず
    // 同じで、守るのは値の作り方だけ（書き手と読み手をそろえて変えても動かない）。決まったバイト列から読めることは、同じ形を
    // 凍結したアンカー（fixtures/anchor-v1-*。tests/anchor.rs の frozen_v1_anchors_still_read）で守る。
    let anchor = |input: InputGen| FileGen::Anchor { input };
    let rich = MetaGen {
        dpi: Some((1, 300)),
        text: vec![
            ("".into(), "空のキー".into()),
            ("Title".into(), "ドット絵\0終わり".into()),
        ],
        icc: Some((0..=255u8).rev().collect()),
        exif: Some(kura_engine::codec::exif::minimal(6, true)),
        slices: vec![kura_engine::value::Slice {
            name: "9 分割".into(),
            rect: kura_engine::geom::IRect::new(1, 1, 5, 3),
            center: Some(kura_engine::geom::IRect::new(2, 2, 3, 1)),
            pivot: Some(kura_engine::geom::IPoint { x: -1, y: 2 }),
        }],
        ..MetaGen::default()
    };
    for (name, fmt) in [
        ("anchor-gray8", PixelFormat::Gray8),
        ("anchor-rgba8", PixelFormat::Rgba8),
        ("anchor-rgb16", PixelFormat::Rgb16),
        ("anchor-indexed8", PixelFormat::Indexed8),
    ] {
        v.push((
            name,
            anchor(InputGen::WithMeta {
                base: Box::new(InputGen::Noise {
                    w: 19,
                    h: 70,
                    fmt,
                    seed: 31,
                }),
                meta: rich.clone(),
            }),
        ));
    }
    v.push((
        "anchor-sheet",
        anchor(InputGen::Sheet {
            cols: 3,
            rows: 2,
            cell: 8,
        }),
    ));
    v.push((
        "anchor-frames-indexed8",
        anchor(InputGen::NoiseFrames {
            w: 12,
            h: 9,
            fmt: PixelFormat::Indexed8,
            count: 3,
            seed: 32,
        }),
    ));
    v.push((
        "anchor-frames-rgba16",
        anchor(InputGen::WithMeta {
            base: Box::new(InputGen::NoiseFrames {
                w: 7,
                h: 5,
                fmt: PixelFormat::Rgba16,
                count: 2,
                seed: 33,
            }),
            meta: rich,
        }),
    ));
    v.push((
        "anchor-audio-meta",
        anchor(InputGen::WithMeta {
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
        }),
    ));
    v.push((
        "anchor-audio-6ch",
        anchor(InputGen::Sine {
            rate: 48_000,
            channels: 6,
            frames: 1_000,
            freq: 440,
            loop_range: None,
        }),
    ));
    v.push((
        "anchor-audio-nonfinite",
        anchor(InputGen::NonFinite {
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
        }),
    ));
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
