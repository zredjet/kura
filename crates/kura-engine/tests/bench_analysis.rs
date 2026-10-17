//! 段 8 の解析の枠組みのベンチ（docs/editing.md §16）。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_analysis -- --ignored --nocapture`
//!
//! 時間は `Engine::analyze_dyn`（引数の確かめ・上限・解析・正規の形）と、申告した解析を先に解く正規化の描画全体。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::time::Instant;

use kura_engine::analysis::AnalysisRequest;
use kura_engine::geom::IRect;
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use kura_engine::testing::render_value;
use kura_engine::value::PixelFormat;
use kura_engine::{NoMonitor, Value};
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

fn analyze(engine: &Engine, id: &str, params: serde_json::Value, v: &Value) -> usize {
    let req = AnalysisRequest {
        id: id.into(),
        v: None,
        params,
    };
    engine
        .analyze_dyn(&req, v, &NoMonitor)
        .unwrap()
        .output
        .len()
}

#[test]
#[ignore]
fn bench_analyses() {
    let engine = Engine::new().unwrap();
    let audio = InputGen::AudioNoise {
        rate: 48_000,
        channels: 2,
        frames: 48_000 * 300,
        seed: 1,
    }
    .generate()
    .unwrap();
    let n = time("audio.loudness_gating 5 分 48 kHz ステレオ", || {
        analyze(&engine, "audio.loudness_gating", json!({}), &audio)
    });
    eprintln!("  正規の形 {n} バイト");
    time(
        "audio.normalize の描画（計測を先に解く＋利得）5 分",
        || {
            render_value(
                "audio.normalize",
                1,
                json!({ "target": -16.0, "peakLimit": -1.0 }),
                audio.clone(),
            )
            .unwrap()
        },
    );
    let sprite = InputGen::Sprite {
        w: 8_192,
        h: 8_192,
        rect: IRect::new(1_000, 2_000, 3_000, 4_000),
    }
    .generate()
    .unwrap();
    time("image.opaque_bounds 8192² Rgba8", || {
        analyze(&engine, "image.opaque_bounds", json!({}), &sprite)
    });
    time(
        "image.trim_alpha の描画（範囲を先に解く＋切る）8192²",
        || {
            render_value(
                "image.trim_alpha",
                1,
                json!({ "threshold": 0, "mode": "union" }),
                sprite.clone(),
            )
            .unwrap()
        },
    );
    let frames = InputGen::TrimmedFrames {
        w: 1_024,
        h: 1_024,
        fmt: PixelFormat::Rgba8,
        count: 64,
        seed: 2,
    }
    .generate()
    .unwrap();
    let n = time(
        "image.opaque_bounds 64 フレーム（1024² のキャンバス）",
        || {
            analyze(
                &engine,
                "image.opaque_bounds",
                json!({ "threshold": 7 }),
                &frames,
            )
        },
    );
    eprintln!("  正規の形 {n} バイト");
}
