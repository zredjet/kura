//! ランダムな文書（docs/editing.md §13 の安全）。ChaCha8 で文書と入力を作り、落ちないこと・同じ結果になることを確かめる。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::doc::{Graph, InputId};
use kura_engine::op::{ParamKind, ParamSchema};
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use kura_engine::value::PixelFormat;
use kura_engine::{NoMonitor, determinism};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::{Value as J, json};

fn below(r: &mut ChaCha8Rng, n: u64) -> u64 {
    ((u128::from(r.next_u64()) * u128::from(n.max(1))) >> 64) as u64
}

fn random_params(r: &mut ChaCha8Rng, schema: &ParamSchema) -> J {
    let mut map = serde_json::Map::new();
    for spec in &schema.params {
        let v = match &spec.kind {
            // ときどき範囲の外も混ぜる
            ParamKind::Int { min, max, .. } => {
                json!(*min - 2 + below(r, (*max - *min + 5) as u64) as i64)
            }
            ParamKind::Float { min, max, .. } => {
                json!(*min - 1.0 + (max - min + 2.0) * (below(r, 1_000_001) as f64 / 1_000_000.0))
            }
            ParamKind::Bool => json!(below(r, 2) == 1),
            ParamKind::Enum { values } => {
                if below(r, 10) == 0 {
                    json!("nonexistent")
                } else {
                    json!(values[below(r, values.len() as u64) as usize])
                }
            }
            // ときどき端の値（i64・u32 の最大・最小）も混ぜる
            ParamKind::Rect => {
                let edge = |r: &mut ChaCha8Rng, small: i64| match below(r, 12) {
                    0 => i64::MAX,
                    1 => i64::MIN,
                    2 => i64::from(u32::MAX),
                    _ => small,
                };
                let (x, y) = (below(r, 40) as i64 - 2, below(r, 40) as i64 - 2);
                let (x, y) = (edge(r, x), edge(r, y));
                let w = if below(r, 12) == 0 {
                    u32::MAX
                } else {
                    below(r, 40) as u32
                };
                json!({ "x": x, "y": y, "w": w, "h": below(r, 40) })
            }
            ParamKind::Length => match below(r, 6) {
                0 => json!("auto"),
                1 => json!({ "scale": { "num": below(r, 6), "den": below(r, 6) } }),
                2 => json!({ "px": u32::MAX }),
                _ => json!({ "px": below(r, 400) }),
            },
            ParamKind::Grid => {
                if below(r, 8) == 0 {
                    J::Null
                } else {
                    let edge = |r: &mut ChaCha8Rng, n: u64| {
                        if below(r, 16) == 0 {
                            u64::from(u32::MAX)
                        } else {
                            below(r, n)
                        }
                    };
                    json!({
                        "cellW": edge(r, 20), "cellH": edge(r, 20),
                        "offsetX": edge(r, 10), "offsetY": edge(r, 10),
                        "gapX": edge(r, 4), "gapY": edge(r, 4),
                    })
                }
            }
            ParamKind::PaletteEntries => J::Array(
                (0..below(r, 4))
                    .map(|_| json!({ "index": below(r, 24), "color": random_color(r, true) }))
                    .collect(),
            ),
            ParamKind::Color { alpha } => {
                if *alpha && below(r, 3) == 0 {
                    J::Null
                } else {
                    random_color(r, *alpha)
                }
            }
            ParamKind::Point => {
                if below(r, 4) == 0 {
                    J::Null
                } else {
                    json!({ "x": below(r, 70) as i64 - 10, "y": below(r, 70) as i64 - 10 })
                }
            }
            ParamKind::Strokes => J::Array((0..below(r, 4)).map(|_| random_action(r)).collect()),
            _ => spec.default.clone(),
        };
        map.insert(spec.key.to_string(), v);
    }
    if below(r, 20) == 0 {
        map.insert("unexpected".into(), json!(1));
    }
    J::Object(map)
}

fn random_color(r: &mut ChaCha8Rng, alpha: bool) -> J {
    // 灰色・不透明もそれなりに出す（形式の制約を通るものを混ぜる）
    let gray = below(r, 3) == 0;
    let v = below(r, 256);
    let c: Vec<u64> = (0..3)
        .map(|_| if gray { v } else { below(r, 256) })
        .collect();
    if alpha {
        let a = if below(r, 2) == 0 { 255 } else { below(r, 256) };
        json!([c[0], c[1], c[2], a])
    } else {
        json!([c[0], c[1], c[2]])
    }
}

fn random_action(r: &mut ChaCha8Rng) -> J {
    let color = match below(r, 4) {
        0 => J::Null,
        1 => json!({ "index": below(r, 20) }),
        _ => json!({ "rgba": random_color(r, true) }),
    };
    let frame = below(r, 3);
    if below(r, 3) == 0 {
        return json!({
            "type": "fill",
            "at": { "x": below(r, 60) as i64 - 5, "y": below(r, 60) as i64 - 5 },
            "color": if color.is_null() { json!({ "rgba": [0, 0, 0, 0] }) } else { color },
            "tolerance": below(r, 50),
            "connectivity": if below(r, 2) == 0 { "four" } else { "eight" },
            "contiguous": below(r, 2) == 0,
            "frame": frame,
        });
    }
    let points: Vec<kura_engine::geom::IPoint> = (0..1 + below(r, 6))
        .map(|_| kura_engine::geom::IPoint {
            x: below(r, 80) as i64 - 10,
            y: below(r, 80) as i64 - 10,
        })
        .collect();
    json!({
        "type": "stroke",
        "tool": if below(r, 4) == 0 { "eraser" } else { "pen" },
        "color": color,
        "blend": if below(r, 3) == 0 { "over" } else { "replace" },
        "brush": { "shape": if below(r, 2) == 0 { "square" } else { "circle" }, "size": below(r, 12) },
        "pixelPerfect": below(r, 2) == 0,
        "frame": frame,
        "points": kura_engine::paint::PointList(points).pack(),
    })
}

fn random_input(r: &mut ChaCha8Rng) -> InputGen {
    let fmt = PixelFormat::ALL[below(r, PixelFormat::ALL.len() as u64) as usize];
    let (w, h) = (1 + below(r, 300) as u32, 1 + below(r, 300) as u32);
    match below(r, 4) {
        0 => InputGen::Noise {
            w,
            h,
            fmt,
            seed: r.next_u64(),
        },
        1 => InputGen::NoiseFrames {
            w: w.min(40),
            h: h.min(40),
            fmt,
            count: 1 + below(r, 3) as u32,
            seed: r.next_u64(),
        },
        2 => InputGen::Sheet {
            cols: 1 + below(r, 4) as u32,
            rows: 1 + below(r, 3) as u32,
            cell: 1 + below(r, 20) as u32,
        },
        _ => InputGen::AudioNoise {
            rate: 8_000 + below(r, 40_000) as u32,
            channels: 1 + below(r, 3) as u16,
            frames: below(r, 5_000),
            seed: r.next_u64(),
        },
    }
}

#[test]
fn random_documents_never_panic_and_repeat() {
    let engine = Engine::new().unwrap();
    let catalog = engine.registry().catalog();
    let mut r = determinism::rng(0x6b75_7261);
    let (mut rendered, mut rejected) = (0, 0);
    for _ in 0..400 {
        let source = random_input(&mut r);
        let Ok(input) = source.generate() else {
            continue;
        };
        let mut g = Graph::new_single();
        for _ in 0..1 + below(&mut r, 4) {
            let op = &catalog[below(&mut r, catalog.len() as u64) as usize];
            let params = random_params(&mut r, &op.schema);
            g.push_linear(op.id, op.version, params, r.next_u64())
                .unwrap();
            if below(&mut r, 8) == 0 {
                let last = g.nodes.len() - 1;
                g.nodes[last].enabled = false;
            }
        }
        // 文書は JSON を往復しても同じ
        let back = engine.load(&g.to_canonical_json().unwrap()).unwrap();
        assert_eq!(back, g);
        let shapes = BTreeMap::from([(InputId(0), input.shape())]);
        let Ok(_plan) = engine.compile(&g, &shapes) else {
            rejected += 1;
            continue;
        };
        let a = kura_engine::testing::render_with(
            &engine,
            &g,
            input.clone(),
            Default::default(),
            &NoMonitor,
        );
        let b = kura_engine::testing::render_with(
            &engine,
            &g,
            input.clone(),
            Default::default(),
            &NoMonitor,
        );
        match (a, b) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a.digest().unwrap(), b.digest().unwrap());
                rendered += 1;
            }
            (Err(a), Err(b)) => {
                // 推論で通っても実行で範囲を確かめる操作がある（大きさの決まらない入力）。内部の誤りは出ないこと
                assert!(!matches!(a, kura_engine::EngineError::Internal(_)), "{a}");
                assert_eq!(a.to_string(), b.to_string());
            }
            (a, b) => panic!("2 回の結果が違う: {a:?} / {b:?}"),
        }
    }
    // 偏りすぎていないこと（どちらもそれなりに起きる）
    assert!(
        rendered > 40 && rejected > 40,
        "rendered {rendered}, rejected {rejected}"
    );
}

#[test]
fn garbage_documents_are_errors_not_panics() {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(7);
    let base = Graph::new_single().to_canonical_json().unwrap();
    for _ in 0..2_000 {
        let mut bytes = base.clone();
        for _ in 0..1 + below(&mut r, 6) {
            let i = below(&mut r, bytes.len() as u64) as usize;
            bytes[i] = r.next_u32() as u8;
        }
        let _ = engine.load(&bytes);
    }
}
