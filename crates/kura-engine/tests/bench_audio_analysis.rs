//! 段 8 の音声の解析のベンチ（docs/editing.md §16 の「5 分の曲の解析（波形・ラウドネス・トゥルーピーク）2 秒」。NFR-15）。
//! ふだんは回さない：`cargo test --release -p kura-engine --test bench_audio_analysis -- --ignored --nocapture`
//!
//! 時間は `Engine::analyze_dyn`（引数の確かめ・上限・解析・正規の形）と `analyze_many`（4 つの解析）。デコード（WAV・FLAC の
//! 16 ビット）を含めた時間も測る。3 回の最小。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;

use kura_engine::analysis::AnalysisRequest;
use kura_engine::codec::CodecSet;
use kura_engine::value::{Audio, AudioMeta};
use kura_engine::{Engine, Limits, NoMonitor, Value};
use rand_chacha::rand_core::Rng;
use serde_json::json;

fn time<T>(label: &str, f: impl Fn() -> T) -> T {
    let mut best = f64::INFINITY;
    let mut out = None;
    for _ in 0..3 {
        let t = Instant::now();
        out = Some(f());
        best = best.min(t.elapsed().as_secs_f64());
    }
    eprintln!("{label}: {:.1} ms（3 回の最小）", best * 1e3);
    out.unwrap()
}

/// 音楽らしい信号：低域に寄せた雑音と正弦を混ぜ、1 秒ごとに大きさ（−40〜−6 dBFS）を変える。前後に 1 秒の無音。
fn program(rate: u32, secs: usize) -> Value {
    let frames = rate as usize * secs;
    let mut env = kura_engine::determinism::rng(1);
    let levels: Vec<f64> = (0..secs)
        .map(|_| {
            let u = f64::from(env.next_u32()) / f64::from(u32::MAX);
            kura_engine::math::db_to_gain(-40.0 + 34.0 * u)
        })
        .collect();
    let planes = (0..2u64)
        .map(|c| {
            let mut r = kura_engine::determinism::rng_stream(2, c);
            let mut lp = 0f64;
            let step = 2.0 * std::f64::consts::PI * 220.0 / f64::from(rate);
            Arc::new(
                (0..frames)
                    .map(|i| {
                        let sec = i / rate as usize;
                        if sec == 0 || sec + 1 == secs {
                            return 0.0;
                        }
                        let u = f64::from(r.next_u32()) / f64::from(u32::MAX) * 2.0 - 1.0;
                        lp = 0.7 * lp + 0.3 * u;
                        (levels[sec] * (0.6 * lp + 0.4 * kura_engine::math::sin(step * i as f64)))
                            as f32
                    })
                    .collect::<Vec<f32>>(),
            )
        })
        .collect();
    Value::Audio(Arc::new(
        Audio::new(rate, planes, AudioMeta::default()).unwrap(),
    ))
}

fn requests() -> Vec<AnalysisRequest> {
    [
        "audio.peaks",
        "audio.loudness",
        "audio.silence",
        "audio.loop",
    ]
    .into_iter()
    .map(AnalysisRequest::new)
    .collect()
}

#[test]
#[ignore]
fn bench_audio_analyses() {
    let engine = Engine::new().unwrap();
    for rate in [44_100u32, 48_000] {
        let v = program(rate, 300);
        eprintln!("== 5 分 {rate} Hz ステレオ ==");
        for req in requests() {
            let n = time(&req.id, || {
                engine
                    .analyze_dyn(&req, &v, &NoMonitor)
                    .unwrap()
                    .output
                    .len()
            });
            eprintln!("  正規の形 {n} バイト");
        }
        let reqs = requests();
        time("4 つの解析（analyze_many）", || {
            let outs = engine.analyze_many(&v, &reqs, &NoMonitor);
            assert!(outs.iter().all(Result::is_ok));
        });
        let set = CodecSet::pinned();
        for format in ["wav", "flac"] {
            let (bytes, _) = set
                .encode_bytes(&v, format, &json!({ "bits": 16 }), &NoMonitor)
                .unwrap();
            time(
                &format!("{format} 16 ビットのデコード＋4 つの解析"),
                || {
                    let (decoded, _) = set
                        .decode(
                            &mut Cursor::new(bytes.as_slice()),
                            format,
                            &Default::default(),
                            &Limits::default(),
                            &NoMonitor,
                        )
                        .unwrap();
                    let outs = engine.analyze_many(&decoded, &reqs, &NoMonitor);
                    assert!(outs.iter().all(Result::is_ok));
                },
            );
        }
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        one.install(|| {
            time("4 つの解析（1 スレッド）", || {
                let outs = engine.analyze_many(&v, &reqs, &NoMonitor);
                assert!(outs.iter().all(Result::is_ok));
            });
        });
    }
}
