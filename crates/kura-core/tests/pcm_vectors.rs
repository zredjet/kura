//! 試聴の PCM（kura:// の `/pcm/{hash}/{start}-{end}` が返す WAV）の書き方を TS と共有するテストベクタ（M11）。
//!
//! 並び（リトルエンディアン。チャンクはこの順で、ほかのチャンクは書かない）：
//!
//! ```text
//! RIFF <u32 全体 − 8> WAVE
//! "fmt " 16：u16 形式（1 = 16 ビット整数、3 = 32 ビット浮動小数）・u16 チャンネル数・u32 レート・u32 バイト毎秒・
//!            u16 ブロック（チャンネル数 × 標本のバイト数）・u16 ビット（16 か 32）
//! "kura" 40 + L（L が奇数なら 1 バイトの詰め物。RIFF の決まり）：
//!   0  u16 並びの版 = 1
//!   2  u16 デコーダの識別子のバイト数 L（0〜255）
//!   4  u32 印：bit 0 = ループ点あり（ほかのビットは 0）
//!   8  u64 開始フレーム（応答の先頭が、元の音声の何フレーム目か）
//!   16 u64 元の音声のフレーム数（同じ読み取りで数えたもの）
//!   24 u64 ループの始まり（印がなければ 0）
//!   32 u64 ループの終わり（含まない。印がなければ 0）
//!   40 [L] デコーダの識別子（UTF-8。`Decoder::id()`。例 "wav-kura-dec/1"）
//! "data" フレーム × ブロック：インターリーブ。整数は `codec::pcm::quantize(x, 16)`（x·32768 の偶数への丸め・範囲で止める・
//!        NaN は 0）、浮動小数は f32 のビットそのまま（NaN の中身も）
//! ```
//!
//! - ループ点・フレーム数・デコーダの識別子は、この PCM を読んだのと同じ読み取りの値を入れる（試聴は DB の値ではなく
//!   これを使う。解析のときとデコーダが変わっていても、継ぎ目が PCM とずれない）。
//! - 元が 16 ビット以下の整数なら 16 ビット（値は同じ）、ほかは 32 ビット浮動小数で書く。
//! - `tests/vectors/pcm/wav.json`：書く前の標本（`input`。f32 のビット）・読んで得る標本（`decoded`。f32 のビット）・
//!   kura チャンクの値・書いたバイト列（16 進）。
//!   - ここではテストの中の写し（`reference_wav`）で書き、エンジンの WAV のデコーダ（`wav-kura-dec/1`）が同じ標本・レート・
//!     チャンネルに読むことを確かめる（kura チャンクは知らないチャンクとして読み飛ばされる）。
//!   - `asset::pcm` の書き手を作ったら、そのテストでもこのファイルのバイト列と照らす（段 3。書き手はこの写しと同じバイト列を
//!     書く）。
//!   - フロントは `src/lib/asset/wav.test.ts` が同じファイルを読み、`src/lib/asset/wav.ts` の `writePcmWav`・
//!     `readPcmWav` と照らす。
//! - ベクタは消す・変えるのを認めない（並びを変えるなら kura チャンクの版を上げ、新しいファイルを足す）。足りなければ
//!   `KURA_BLESS=1 cargo test -p kura-core --test pcm_vectors` で書く。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;

use kura_engine::codec::CodecSet;
use kura_engine::codec::pcm::quantize;
use kura_engine::doc::DecodeOptions;
use kura_engine::{Limits, NoMonitor, Value};
use serde_json::{Value as Json, json};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/pcm")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Format {
    I16,
    F32,
}

struct Case {
    name: &'static str,
    format: Format,
    rate: u32,
    start: u64,
    total: u64,
    looped: Option<(u64, u64)>,
    decoder: &'static str,
    /// チャンネルごとの書く前の標本。
    planes: Vec<Vec<f32>>,
}

/// 並びの写し（モジュールの説明のとおり）。
fn reference_wav(c: &Case) -> Vec<u8> {
    let channels = c.planes.len() as u16;
    let frames = c.planes[0].len();
    let (tag, bits): (u16, u16) = match c.format {
        Format::I16 => (1, 16),
        Format::F32 => (3, 32),
    };
    let block = channels * (bits / 8);
    let mut kura = Vec::new();
    kura.extend_from_slice(&1u16.to_le_bytes());
    kura.extend_from_slice(&(c.decoder.len() as u16).to_le_bytes());
    kura.extend_from_slice(&u32::from(c.looped.is_some()).to_le_bytes());
    kura.extend_from_slice(&c.start.to_le_bytes());
    kura.extend_from_slice(&c.total.to_le_bytes());
    let (ls, le) = c.looped.unwrap_or((0, 0));
    kura.extend_from_slice(&ls.to_le_bytes());
    kura.extend_from_slice(&le.to_le_bytes());
    kura.extend_from_slice(c.decoder.as_bytes());
    let mut data = Vec::new();
    for f in 0..frames {
        for p in &c.planes {
            match c.format {
                Format::I16 => data.extend_from_slice(&(quantize(p[f], 16) as i16).to_le_bytes()),
                Format::F32 => data.extend_from_slice(&p[f].to_le_bytes()),
            }
        }
    }
    let pad = kura.len() & 1;
    let riff = 4 + (8 + 16) + (8 + kura.len() + pad) + (8 + data.len());
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&c.rate.to_le_bytes());
    out.extend_from_slice(&(c.rate * u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"kura");
    out.extend_from_slice(&(kura.len() as u32).to_le_bytes());
    out.extend_from_slice(&kura);
    if pad == 1 {
        out.push(0);
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// 読んで得る標本（整数は 16 ビットの格子に写した値 / 32768）。
fn decoded(c: &Case) -> Vec<Vec<f32>> {
    c.planes
        .iter()
        .map(|p| {
            p.iter()
                .map(|x| match c.format {
                    Format::I16 => quantize(*x, 16) as f32 / 32_768.0,
                    Format::F32 => *x,
                })
                .collect()
        })
        .collect()
}

fn bits(planes: &[Vec<f32>]) -> Json {
    Json::Array(
        planes
            .iter()
            .map(|p| Json::Array(p.iter().map(|x| json!(x.to_bits())).collect()))
            .collect(),
    )
}

fn cases() -> Vec<Case> {
    let ramp = |n: usize, k: f32| -> Vec<f32> { (0..n).map(|i| (i as f32 - 3.0) * k).collect() };
    vec![
        Case {
            name: "i16-stereo-loop",
            format: Format::I16,
            rate: 44_100,
            start: 0,
            total: 10,
            looped: Some((2, 7)),
            decoder: "wav-kura-dec/1",
            planes: vec![
                ramp(10, 1.0 / 32_768.0),
                (0..10).map(|i| -(i as f32) * 1_000.0 / 32_768.0).collect(),
            ],
        },
        // 16 ビットの格子への写し：偶数への丸め（2.5 → 2、3.5 → 4、−2.5 → −2）、端で止める、NaN は 0、±∞ は端
        Case {
            name: "i16-quantize-edges",
            format: Format::I16,
            rate: 48_000,
            start: 5,
            total: 100,
            looped: None,
            decoder: "symphonia-vorbis/1",
            planes: vec![vec![
                2.5 / 32_768.0,
                3.5 / 32_768.0,
                -2.5 / 32_768.0,
                1.0,
                -1.0,
                1.5,
                -1.5,
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                0.1,
                -0.0,
            ]],
        },
        // 浮動小数：ビットそのまま（−0・非正規化数・NaN・±∞・1 を超える値）。デコーダの識別子は奇数の長さ（詰め物）
        Case {
            name: "f32-mono-specials-odd-decoder",
            format: Format::F32,
            rate: 96_000,
            start: 1_000_000,
            total: 1_000_008,
            looped: Some((1_000_002, 1_000_008)),
            decoder: "flac-kura-dec/12",
            planes: vec![vec![
                0.0,
                -0.0,
                1e-40,
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                1.5,
                -0.333_333_34,
            ]],
        },
        // 3 チャンネル（拡張の fmt にしない）・受け付けるレートの下端・デコーダの識別子が空
        Case {
            name: "f32-three-channels-low-rate",
            format: Format::F32,
            rate: 3_000,
            start: 0,
            total: 4,
            looped: None,
            decoder: "",
            planes: vec![ramp(4, 0.25), ramp(4, -0.125), vec![0.5; 4]],
        },
        // 受け付けるレートの上端・長さ 0 の応答
        Case {
            name: "i16-empty-high-rate",
            format: Format::I16,
            rate: 384_000,
            start: 7,
            total: 7,
            looped: None,
            decoder: "wav-kura-dec/1",
            planes: vec![vec![], vec![]],
        },
        // 開始フレームが u32 を超える・ループの終わりが元の終わり
        Case {
            name: "f32-stereo-large-start",
            format: Format::F32,
            rate: 22_050,
            start: 5_000_000_000,
            total: 5_000_000_003,
            looped: Some((4_999_999_000, 5_000_000_003)),
            decoder: "wav-kura-dec/1",
            planes: vec![vec![0.25, 0.5, 0.75], vec![-0.25, -0.5, -0.75]],
        },
    ]
}

fn vectors() -> Json {
    let cases: Vec<Json> = cases()
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "format": match c.format { Format::I16 => "i16", Format::F32 => "f32" },
                "rate": c.rate,
                "channels": c.planes.len(),
                "frames": c.planes[0].len(),
                "start": c.start,
                "total": c.total,
                "loop": c.looped.map(|(a, b)| json!([a, b])),
                "decoder": c.decoder,
                "input": bits(&c.planes),
                "decoded": bits(&decoded(c)),
                "hex": hex(&reference_wav(c)),
            })
        })
        .collect();
    json!({ "version": 1, "cases": cases })
}

#[test]
fn vectors_match_the_frozen_files() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let file = "wav.json";
    let want = vectors();
    let path = dir().join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let frozen: Json = serde_json::from_str(&text).unwrap();
            assert!(
                frozen == want,
                "{file}：凍結したベクタと違う結果です（並びを変えたなら、kura チャンクの版を上げて新しいファイルを足す）"
            );
        }
        Err(_) if bless => {
            std::fs::create_dir_all(dir()).unwrap();
            std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
        }
        Err(_) => panic!("{file} がありません（KURA_BLESS=1 で書けます）"),
    }
}

/// 凍結したバイト列を、エンジンの WAV のデコーダが同じレート・チャンネル・標本に読む（kura チャンクは読み飛ばす）。
#[test]
fn engine_wav_decoder_reads_the_frozen_bytes() {
    let Ok(text) = std::fs::read_to_string(dir().join("wav.json")) else {
        panic!("wav.json がありません（KURA_BLESS=1 で書けます）");
    };
    let frozen: Json = serde_json::from_str(&text).unwrap();
    let codecs = CodecSet::pinned();
    for case in frozen["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let bytes = unhex(case["hex"].as_str().unwrap());
        let (v, stamp) = codecs
            .decode(
                &mut Cursor::new(bytes),
                "wav",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(stamp.codec, "wav-kura-dec/1", "{name}");
        let Value::Audio(a) = v else {
            panic!("{name}: 音声ではありません")
        };
        assert_eq!(
            u64::from(a.rate()),
            case["rate"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            a.channels() as u64,
            case["channels"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            a.frames() as u64,
            case["frames"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            a.meta.loop_range, None,
            "{name}: kura チャンクはループ点にしない"
        );
        let want = case["decoded"].as_array().unwrap();
        for (ch, plane) in a.planes().iter().enumerate() {
            let got: Vec<u64> = plane.iter().map(|x| u64::from(x.to_bits())).collect();
            let want: Vec<u64> = want[ch]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap())
                .collect();
            assert_eq!(got, want, "{name}: チャンネル {ch}");
        }
    }
}
