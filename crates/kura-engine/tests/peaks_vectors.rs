//! 波形の段（`audio.peaks` v1 の正規の形 `kura-peaks` 版 1）を TS と共有するテストベクタ（M11。docs/design.md §3.1 の波形）。
//!
//! - `tests/vectors/peaks/valid.json`：エンジンの口（`Engine::analyze_dyn`）で作ったバイト列（16 進）と、読んだ値
//!   （レート・チャンネル・フレーム・解析の版・段ごとのビンのフレーム数・ビンの数・[min, max] の列）。
//! - `tests/vectors/peaks/invalid.json`：正しいバイト列を 1 か所だけ壊したもの。Rust の `from_canonical` も TS の
//!   `parsePeaks` も読まない（並びの約束の照らし合わせ。解析の版の違いは並びの約束ではないので入れない。TS は解析の版を見ない）。
//! - フロントは `src/lib/asset/peaks.test.ts` が同じファイルを読み、`src/lib/asset/peaks.ts` の `parsePeaks` と照らす。
//! - ベクタは消す・変えるのを認めない（並びを変えるなら並びの版を上げ、新しいファイルを足す）。足りなければ
//!   `KURA_BLESS=1 cargo test -p kura-engine --test peaks_vectors` で書く。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use kura_engine::analysis::audio::peaks::{Peaks, PeaksAnalysis};
use kura_engine::analysis::{Analysis, AnalysisOutput, AnalysisRequest};
use kura_engine::value::{Audio, AudioMeta};
use kura_engine::{Engine, NoMonitor, Value};
use serde_json::{Value as Json, json};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/peaks")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 決まった標本（整数の式から作る。libm を通さない）。チャンネルごとに位相をずらす。
fn sample(i: usize, ch: usize) -> f32 {
    let v = (i * 7_919 + ch * 104_729) % 2_001;
    (v as f32 - 1_000.0) / 1_000.0 * 0.9
}

fn audio(rate: u32, channels: usize, frames: usize) -> Value {
    audio_with(rate, channels, frames, sample)
}

/// 標本を式 `f(フレーム, チャンネル)` で作る。
fn audio_with(rate: u32, channels: usize, frames: usize, f: impl Fn(usize, usize) -> f32) -> Value {
    let planes = (0..channels)
        .map(|ch| Arc::new((0..frames).map(|i| f(i, ch)).collect::<Vec<f32>>()))
        .collect();
    Value::Audio(Arc::new(
        Audio::new(rate, planes, AudioMeta::default()).unwrap(),
    ))
}

/// エンジンの口で波形の段を作る。
fn peaks_bytes(v: &Value, params: Option<(u32, u32)>) -> Vec<u8> {
    let mut req = AnalysisRequest::new("audio.peaks");
    if let Some((finest, coarsest)) = params {
        req.params = json!({ "finest": finest, "coarsest": coarsest });
    }
    Engine::new()
        .unwrap()
        .analyze_dyn(&req, v, &NoMonitor)
        .unwrap()
        .output
}

fn valid_case(name: &str, v: &Value, params: Option<(u32, u32)>) -> (Json, Vec<u8>) {
    let bytes = peaks_bytes(v, params);
    let p = Peaks::from_canonical(&bytes).unwrap();
    let levels: Vec<Json> = p
        .levels
        .iter()
        .map(|l| {
            json!({
                "binFrames": l.bin,
                "bins": p.frames.div_ceil(u64::from(l.bin)),
                "data": l.data,
            })
        })
        .collect();
    let case = json!({
        "name": name,
        "params": params.map(|(finest, coarsest)| json!({ "finest": finest, "coarsest": coarsest })),
        "rate": p.rate,
        "channels": p.channels,
        "frames": p.frames,
        "analysisVersion": PeaksAnalysis::VERSION,
        "levels": levels,
        "hex": hex(&bytes),
    });
    (case, bytes)
}

/// 正しいバイト列の例（最初の例を壊して読めない例を作る）。
fn valid() -> (Json, Vec<u8>) {
    let cases = vec![
        // 1 チャンネル・3 段。1 ビンの段は 4 バイトなので、段の間に 4 バイトの詰め物が入る
        valid_case(
            "mono-three-levels",
            &audio(22_050, 1, 1_000),
            Some((256, 4_096)),
        ),
        // 3 チャンネル：段 256 は 3 ビン × 3 チャンネル（36 バイト）で詰め物 4 バイト
        valid_case("three-channels", &audio(48_000, 3, 700), Some((256, 1_024))),
        // NaN・±∞・1 を超える値（16 ビットの格子で端に止まる。NaN のあるビンは 0 を含む）、全部が NaN のビン（3 つ目）
        valid_case(
            "nan-inf-and-clip",
            &audio_with(44_100, 1, 600, |i, ch| match i {
                3 => f32::NAN,
                10 => 1.5,
                11 => -1.5,
                300 => f32::INFINITY,
                301 => f32::NEG_INFINITY,
                512.. => f32::NAN,
                _ => sample(i, ch),
            }),
            Some((256, 256)),
        ),
        // 既定の引数（5 段とも）。半端な最後のビン
        valid_case("default-five-levels", &audio(48_000, 1, 70_000), None),
        // 2 チャンネル・最も粗い段だけ（1 ビン）
        valid_case(
            "stereo-coarsest-only",
            &audio(96_000, 2, 5_000),
            Some((65_536, 65_536)),
        ),
        // 長さ 0：ビンは 0 個、データは空
        valid_case("empty", &audio(8_000, 2, 0), Some((256, 1_024))),
    ];
    let first = cases[0].1.clone();
    (
        json!({ "version": 1, "cases": cases.into_iter().map(|(c, _)| c).collect::<Vec<_>>() }),
        first,
    )
}

/// 読めないバイト列（`mono-three-levels` を 1 か所だけ壊す）。Rust が読まないことをここで確かめる。
fn invalid(good: &[u8]) -> Json {
    assert!(Peaks::from_canonical(good).is_ok());
    let edit = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = good.to_vec();
        f(&mut b);
        b
    };
    let put_u16 =
        |b: &mut Vec<u8>, at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put_u32 =
        |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put_u64 =
        |b: &mut Vec<u8>, at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    let get_u64 = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    // 段 0 のデータの位置と、段 1 のデータの終わり（段 2 の前の詰め物）
    let level0 = get_u64(good, 32 + 16) as usize;
    let level1 = get_u64(good, 32 + 24 + 16) as usize;
    let level2 = get_u64(good, 32 + 48 + 16) as usize;
    assert!(level1 + 4 < level2, "詰め物のある例を選ぶ");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("too-short", good[..31].to_vec()),
        ("bad-magic", edit(&|b| b[0] = b'K')),
        ("unknown-layout-version", edit(&|b| put_u16(b, 8, 2))),
        ("zero-channels", edit(&|b| put_u16(b, 12, 0))),
        ("no-levels", edit(&|b| b[14] = 0)),
        ("too-many-levels", edit(&|b| b[14] = 17)),
        ("unknown-value-type", edit(&|b| b[15] = 2)),
        ("reserved-not-zero", edit(&|b| put_u32(b, 20, 1))),
        ("frames-do-not-match-bins", edit(&|b| put_u64(b, 24, 2_000))),
        ("level-reserved-not-zero", edit(&|b| put_u32(b, 32 + 4, 1))),
        (
            "bin-sizes-not-increasing",
            edit(&|b| put_u32(b, 32 + 24, 256)),
        ),
        ("zero-bin-size", edit(&|b| put_u32(b, 32, 0))),
        ("wrong-bin-count", edit(&|b| put_u64(b, 32 + 8, 5))),
        (
            "wrong-data-offset",
            edit(&|b| put_u64(b, 32 + 16, level0 as u64 + 8)),
        ),
        ("nonzero-padding", edit(&|b| b[level1 + 4] = 1)),
        (
            "min-greater-than-max",
            edit(&|b| {
                b[level0..level0 + 2].copy_from_slice(&100i16.to_le_bytes());
                b[level0 + 2..level0 + 4].copy_from_slice(&99i16.to_le_bytes());
            }),
        ),
        ("truncated", good[..good.len() - 1].to_vec()),
        ("trailing-byte", edit(&|b| b.push(0))),
    ];
    for (name, bytes) in &cases {
        assert!(
            Peaks::from_canonical(bytes).is_err(),
            "Rust が読んでしまう: {name}"
        );
    }
    json!({
        "version": 1,
        "cases": cases.iter().map(|(name, b)| json!({ "name": name, "hex": hex(b) })).collect::<Vec<_>>(),
    })
}

#[test]
fn vectors_match_the_frozen_files() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let (valid, first) = valid();
    let mut failures = Vec::new();
    for (file, want) in [("valid.json", valid), ("invalid.json", invalid(&first))] {
        let path = dir().join(file);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let frozen: Json = serde_json::from_str(&text).unwrap();
                if frozen != want {
                    failures.push(format!(
                        "{file}：凍結したベクタと違う結果です（並びを変えたなら、並びの版を上げて新しいファイルを足す）"
                    ));
                }
            }
            Err(_) if bless => {
                std::fs::create_dir_all(dir()).unwrap();
                std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
            }
            Err(_) => failures.push(format!("{file} がありません（KURA_BLESS=1 で書けます）")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// 凍結した正しい例は、どれも Rust がそのまま読めて、書き直すと同じバイト列になる。
#[test]
fn frozen_valid_vectors_round_trip_in_rust() {
    let Ok(text) = std::fs::read_to_string(dir().join("valid.json")) else {
        panic!("valid.json がありません（KURA_BLESS=1 で書けます）");
    };
    let frozen: Json = serde_json::from_str(&text).unwrap();
    for case in frozen["cases"].as_array().unwrap() {
        let h = case["hex"].as_str().unwrap();
        let bytes: Vec<u8> = (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect();
        let p = Peaks::from_canonical(&bytes).unwrap();
        assert_eq!(p.to_canonical().unwrap(), bytes, "{}", case["name"]);
        assert_eq!(u64::from(p.channels), case["channels"].as_u64().unwrap());
        assert_eq!(p.frames, case["frames"].as_u64().unwrap());
        assert_eq!(
            p.levels.len(),
            case["levels"].as_array().unwrap().len(),
            "{}",
            case["name"]
        );
    }
}
