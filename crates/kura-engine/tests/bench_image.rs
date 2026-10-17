//! 段 5 の画像の操作と描画のベンチ（docs/editing.md §16）。ふだんは回さない：
//! `cargo test --release -p kura-engine --test bench_image -- --ignored --nocapture`
//!
//! 筆 1,000 本の文書の大きさも測る（文書の大きさの上限を決めるため。ADR-147）。
//! 時間は操作そのもの（`apply`）で、文書の計画・出力のダイジェストを含まない。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::disallowed_methods
)]

use std::time::Instant;

use kura_engine::geom::{IPoint, IRect};
use kura_engine::op::ApplyCx;
use kura_engine::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{Limits, NoMonitor, Value};
use serde_json::json;

/// 操作そのものを当てる（文書の計画・メタデータの写し・出力のダイジェストを除く。ダイジェストは 64 MB で 20 ms ほどかかる）。
fn render_value(
    id: &str,
    v: u16,
    params: serde_json::Value,
    input: Value,
) -> Result<Value, kura_engine::EngineError> {
    let engine = kura_engine::Engine::new().unwrap();
    let op = match engine.registry().resolve(id, v, &params).unwrap() {
        kura_engine::op::Resolved::Direct(op, _) | kura_engine::op::Resolved::Migrated(op, _) => op,
    };
    let p = op.parse(&params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
    Ok(op.apply(p.as_ref(), &[&input], &mut cx)?.value)
}

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    eprintln!("{label}: {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    out
}

fn strokes(n: usize, points: usize, size: u8) -> Vec<PaintAction> {
    (0..n)
        .map(|i| {
            let pts: Vec<IPoint> = (0..points)
                .map(|k| IPoint {
                    x: ((i * 37 + k * 5) % 1000) as i64,
                    y: ((i * 13 + k * 3 + (k * k) % 7) % 1000) as i64,
                })
                .collect();
            PaintAction::Stroke {
                tool: Tool::Pen,
                color: Some(PaintColor::Rgba([(i % 256) as u8, 40, 200, 255])),
                blend: Blend::Replace,
                brush: Brush {
                    shape: BrushShape::Circle,
                    size,
                },
                pixel_perfect: size == 1,
                frame: 0,
                points: PointList(pts),
            }
        })
        .collect()
}

#[test]
#[ignore]
fn bench_image_ops() {
    let big = InputGen::Noise {
        w: 4096,
        h: 4096,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    time("resize lanczos3 4096² → 1024²", || {
        render_value(
            "image.resize",
            1,
            json!({ "width": { "px": 1024 }, "height": { "px": 1024 }, "filter": "lanczos3" }),
            big.clone(),
        )
        .unwrap()
    });
    time("resize bilinear 4096² → 4500²（拡大）", || {
        render_value(
            "image.resize",
            1,
            json!({ "width": { "px": 4500 }, "height": { "px": 4500 }, "filter": "bilinear" }),
            big.clone(),
        )
        .unwrap()
    });
    let small = InputGen::Noise {
        w: 512,
        h: 512,
        fmt: PixelFormat::Indexed8,
        seed: 2,
    }
    .generate()
    .unwrap();
    time("resize nearest 512² → 2048²（Indexed8）", || {
        render_value(
            "image.resize",
            1,
            json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
            small.clone(),
        )
        .unwrap()
    });
    let sprite = InputGen::Sprite {
        w: 4096,
        h: 4096,
        rect: IRect::new(1000, 900, 1500, 2000),
    }
    .generate()
    .unwrap();
    time("trim_alpha 4096²", || {
        render_value(
            "image.trim_alpha",
            1,
            json!({ "threshold": 0, "mode": "union" }),
            sprite.clone(),
        )
        .unwrap()
    });
    time("rotate90 4096²", || {
        render_value("image.rotate90", 1, json!({ "turn": "cw" }), big.clone()).unwrap()
    });
    let photo = InputGen::Noise {
        w: 1024,
        h: 1024,
        fmt: PixelFormat::Rgb8,
        seed: 3,
    }
    .generate()
    .unwrap();
    for dither in ["none", "ordered", "errorDiffusion"] {
        time(&format!("convert 1024² → 256 色（{dither}）"), || {
            render_value(
                "image.convert",
                1,
                json!({ "format": "indexed8", "colors": 256, "dither": dither, "matte": [255, 255, 255] }),
                photo.clone(),
            )
            .unwrap()
        });
    }
}

#[test]
#[ignore]
fn bench_paint() {
    let canvas = InputGen::Noise {
        w: 1024,
        h: 1024,
        fmt: PixelFormat::Rgba8,
        seed: 4,
    }
    .generate()
    .unwrap();
    for (size, label) in [(1u8, "1px・ピクセルパーフェクト"), (8, "円 8px")] {
        let actions = strokes(1_000, 50, size);
        let params = json!({ "actions": actions });
        let doc_bytes = serde_json::to_vec(&params).unwrap().len();
        eprintln!("筆 1,000 本（各 50 点、{label}）の引数の JSON：{doc_bytes} バイト");
        time(&format!("paint 1,000 本 全体（{label}）"), || {
            render_value("paint.actions", 1, params.clone(), canvas.clone()).unwrap()
        });
        // 差分の適用：999 本の結果に 1 本だけ足す
        let prev = kura_engine::ops::paint::actions::PaintParams {
            actions: actions[..999].to_vec(),
        };
        let all = kura_engine::ops::paint::actions::PaintParams {
            actions: actions.clone(),
        };
        let limits = Limits::default();
        let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
        use kura_engine::op::Op;
        let prev_out = kura_engine::ops::paint::actions::Actions::apply(&prev, &[&canvas], &mut cx)
            .unwrap()
            .value;
        time(&format!("paint 差分の 1 本（{label}）"), || {
            kura_engine::ops::paint::actions::Actions::apply_appended(
                &prev, &prev_out, &all, &mut cx,
            )
            .unwrap()
            .unwrap()
        });
    }
    // 全面がつながった色の塗りつぶし（目標は 4096² で 30ms。docs/editing.md §16）
    for size in [2048u32, 4096] {
        let flat = InputGen::Checker {
            w: size,
            h: size,
            fmt: PixelFormat::Rgba8,
            cell: 8192,
        }
        .generate()
        .unwrap();
        for (contiguous, label) in [(true, "連結"), (false, "全体")] {
            let fill = PaintAction::Fill {
                at: IPoint { x: 10, y: 10 },
                color: PaintColor::Rgba([1, 2, 3, 255]),
                tolerance: 0,
                connectivity: kura_engine::paint::Connectivity::Four,
                contiguous,
                frame: 0,
            };
            time(&format!("fill {size}²（全面・{label}）"), || {
                render_value(
                    "paint.actions",
                    1,
                    json!({ "actions": [fill] }),
                    flat.clone(),
                )
                .unwrap()
            });
        }
    }
    // 迷路のような形（区間が多い）の塗りつぶし
    let maze = InputGen::Checker {
        w: 2048,
        h: 2048,
        fmt: PixelFormat::Rgba8,
        cell: 1,
    }
    .generate()
    .unwrap();
    let fill = PaintAction::Fill {
        at: IPoint { x: 0, y: 0 },
        color: PaintColor::Rgba([1, 2, 3, 255]),
        tolerance: 0,
        connectivity: kura_engine::paint::Connectivity::Eight,
        contiguous: true,
        frame: 0,
    };
    time(
        "fill 2048²（市松模様・8 連結。区間が 1 画素ずつ）",
        || {
            render_value(
                "paint.actions",
                1,
                json!({ "actions": [fill] }),
                maze.clone(),
            )
            .unwrap()
        },
    );
}
