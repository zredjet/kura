//! 範囲と変わった範囲のテスト・ファズの補助（段 10。E18・E31）：ランダムな鎖と編集、範囲の外を壊した値、範囲だけを
//! 有効にした値、前後の値の差が範囲に入るかの照合、範囲の描画と全体の描画の照合。
//!
//! テスト用の補助だが本番のビルドにも入る（`pub mod testing`）ので、ここでも panic しない。

use std::collections::BTreeMap;
use std::sync::Arc;

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

use crate::doc::{Dither, Graph, InputId, QUANTIZE_VERSION, Quantize};
use crate::error::EngineError;
use crate::geom::{IPoint, IRect, Length, Ratio};
use crate::monitor::NoMonitor;
use crate::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use crate::render::Engine;
use crate::render::area::AreaRequest;
use crate::render::cache::{FingerprintedInput, NodeCache};
use crate::render::region::{Area, Rects, Tiles, frames_of, map_rasters_region, tiles_in};
use crate::shape::{Fullness, Shape};
use crate::testgen::InputGen;
use crate::value::{Audio, PixelFormat, Raster, TILE, Value};

/// `0..n` の一様な乱数（n が 0 なら 0）。
pub fn below(r: &mut ChaCha8Rng, n: u64) -> u64 {
    ((u128::from(r.next_u64()) * u128::from(n.max(1))) >> 64) as u64
}

/// 鎖の入力の作り方：画像（いろいろな形式・タイルの境をまたぐ大きさ）・フレームの列・トリムしたフレームの列・シート・音声。
pub fn random_input(r: &mut ChaCha8Rng) -> InputGen {
    let fmt = PixelFormat::ALL[below(r, PixelFormat::ALL.len() as u64) as usize];
    let sizes = [1u32, 7, 64, 255, 256, 257, 300, 520];
    let w = sizes[below(r, sizes.len() as u64) as usize];
    let h = sizes[below(r, sizes.len() as u64) as usize].min(300);
    match below(r, 8) {
        0..=3 => InputGen::Noise {
            w,
            h,
            fmt,
            seed: r.next_u64(),
        },
        4 => InputGen::NoiseFrames {
            w: w.min(40),
            h: h.min(40),
            fmt,
            count: 1 + below(r, 3) as u32,
            seed: r.next_u64(),
        },
        5 => InputGen::TrimmedFrames {
            w: 4 + w.min(36),
            h: 4 + h.min(36),
            fmt,
            count: 1 + below(r, 3) as u32,
            seed: r.next_u64(),
        },
        6 => InputGen::Sheet {
            cols: 1 + below(r, 4) as u32,
            rows: 1 + below(r, 3) as u32,
            cell: 4 + below(r, 20) as u32,
        },
        _ => InputGen::AudioNoise {
            rate: [8_000u32, 22_050, 44_100][below(r, 3) as usize],
            channels: 1 + below(r, 2) as u16,
            frames: 1 + below(r, 6_000),
            seed: r.next_u64(),
        },
    }
}

/// 画像の形の (幅, 高さ, 形式, フレームの数)。音声は None。
fn image_dims(shape: &Shape) -> Option<(u64, u64, PixelFormat, u64)> {
    match *shape {
        Shape::Image { w, h, fmt } => Some((w.max(), h.max(), fmt, 1)),
        Shape::Frames {
            w, h, fmt, count, ..
        } => Some((w.max(), h.max(), fmt, count.max())),
        Shape::Audio { .. } => None,
    }
}

fn color(r: &mut ChaCha8Rng) -> [u8; 4] {
    let a = if below(r, 2) == 0 {
        255
    } else {
        below(r, 256) as u8
    };
    [
        r.next_u32() as u8,
        r.next_u32() as u8,
        r.next_u32() as u8,
        a,
    ]
}

/// ランダムなストローク（フレーム `frames` 枚のどれか。色は形式に合わせる）。
pub fn random_stroke(
    r: &mut ChaCha8Rng,
    w: u64,
    h: u64,
    fmt: PixelFormat,
    frames: u64,
) -> PaintAction {
    let span = |r: &mut ChaCha8Rng, n: u64| below(r, n + 20) as i64 - 10;
    let points: Vec<IPoint> = (0..1 + below(r, 4))
        .map(|_| IPoint {
            x: span(r, w),
            y: span(r, h),
        })
        .collect();
    let indexed = fmt == PixelFormat::Indexed8;
    let alpha = fmt.has_alpha() || indexed;
    let eraser = alpha && below(r, 5) == 0;
    let (tool, color, blend) = if eraser {
        (Tool::Eraser, None, Blend::Replace)
    } else if indexed {
        (
            Tool::Pen,
            Some(PaintColor::Index(below(r, 4) as u8)),
            Blend::Replace,
        )
    } else {
        let c = color(r);
        let gray = matches!(
            fmt,
            PixelFormat::Gray8 | PixelFormat::GrayA8 | PixelFormat::Gray16 | PixelFormat::GrayA16
        );
        let c = if gray { [c[0], c[0], c[0], c[3]] } else { c };
        let (c, blend) = if below(r, 3) == 0 {
            (c, Blend::Over)
        } else if fmt.has_alpha() {
            (c, Blend::Replace)
        } else {
            ([c[0], c[1], c[2], 255], Blend::Replace)
        };
        (Tool::Pen, Some(PaintColor::Rgba(c)), blend)
    };
    PaintAction::Stroke {
        tool,
        color,
        blend,
        brush: Brush {
            shape: if below(r, 2) == 0 {
                BrushShape::Square
            } else {
                BrushShape::Circle
            },
            size: 1 + below(r, 9) as u8,
        },
        pixel_perfect: below(r, 3) == 0,
        frame: below(r, frames.max(1)) as u32,
        points: PointList(points),
    }
}

/// ランダムな塗りつぶし（全体に依る動作）。
fn random_fill(r: &mut ChaCha8Rng, w: u64, h: u64, fmt: PixelFormat, frames: u64) -> PaintAction {
    let color = if fmt == PixelFormat::Indexed8 {
        PaintColor::Index(below(r, 4) as u8)
    } else if fmt.has_alpha() {
        PaintColor::Rgba(color(r))
    } else {
        let c = color(r);
        let gray = matches!(fmt, PixelFormat::Gray8 | PixelFormat::Gray16);
        PaintColor::Rgba(if gray {
            [c[0], c[0], c[0], 255]
        } else {
            [c[0], c[1], c[2], 255]
        })
    };
    PaintAction::Fill {
        at: IPoint {
            x: below(r, w.max(1)) as i64,
            y: below(r, h.max(1)) as i64,
        },
        color,
        tolerance: 0,
        connectivity: crate::paint::Connectivity::Four,
        contiguous: below(r, 2) == 0,
        frame: below(r, frames.max(1)) as u32,
    }
}

/// 形 `shape` の値に当てるランダムな操作 1 つ（操作 ID と引数）。`global` なら全体に依る操作（trim・Indexed8 への convert・
/// 塗りつぶし・正規化）もときどき選ぶ。
pub fn random_op(
    r: &mut ChaCha8Rng,
    shape: &Shape,
    global: bool,
) -> (&'static str, serde_json::Value) {
    let Some((w, h, fmt, frames)) = image_dims(shape) else {
        let frames = match shape {
            Shape::Audio { frames, .. } => frames.max(),
            _ => 0,
        };
        return match below(r, if global { 8 } else { 7 }) {
            0 => (
                "audio.gain",
                json!({ "db": below(r, 200) as f64 / 10.0 - 10.0 }),
            ),
            1 => {
                let start = below(r, frames / 2 + 1);
                ("audio.trim", json!({ "start": start, "end": null }))
            }
            2 | 3 => (
                "audio.fade",
                json!({ "fadeIn": below(r, frames / 2 + 1), "fadeOut": below(r, frames / 2 + 1), "curve": if below(r, 2) == 0 { "linear" } else { "equalPower" } }),
            ),
            4 => ("audio.mono", json!({})),
            5 => {
                let a = below(r, frames.max(2) - 1);
                (
                    "audio.set_loop",
                    json!({ "loop": { "start": a, "end": a + 1 + below(r, frames.max(2) - a - 1) } }),
                )
            }
            6 => {
                let rate = [8_000u32, 11_025, 16_000, 44_100][below(r, 4) as usize];
                ("audio.resample", json!({ "rate": rate }))
            }
            _ => (
                "audio.normalize",
                json!({ "target": -20.0, "peakLimit": null }),
            ),
        };
    };
    let is_image = matches!(shape, Shape::Image { .. });
    let full_frames = matches!(
        shape,
        Shape::Image { .. }
            | Shape::Frames {
                full: Fullness::Full,
                ..
            }
    );
    let rect = |r: &mut ChaCha8Rng| {
        let x = below(r, w) as i64;
        let y = below(r, h) as i64;
        let rw = 1 + below(r, w - x as u64) as u32;
        let rh = 1 + below(r, h - y as u64) as u32;
        IRect::new(x, y, rw, rh)
    };
    loop {
        let pick = below(r, 13);
        match pick {
            0 if full_frames => {
                return ("image.crop", json!({ "rect": rect(r) }));
            }
            1 if full_frames => {
                let axis = if below(r, 2) == 0 {
                    "horizontal"
                } else {
                    "vertical"
                };
                return ("image.flip", json!({ "axis": axis }));
            }
            2 if full_frames => {
                let turn = ["cw", "ccw", "half"][below(r, 3) as usize];
                return ("image.rotate90", json!({ "turn": turn }));
            }
            3 if full_frames => {
                let x = below(r, 60) as i64 - 30;
                let y = below(r, 60) as i64 - 30;
                let rw = 1 + below(r, w + 40) as u32;
                let rh = 1 + below(r, h + 40) as u32;
                let fill = if below(r, 2) == 0 {
                    serde_json::Value::Null
                } else {
                    json!(color(r))
                };
                return (
                    "image.canvas",
                    json!({ "rect": IRect::new(x, y, rw, rh), "fill": fill }),
                );
            }
            4 if full_frames => {
                let len = |r: &mut ChaCha8Rng, n: u64| match below(r, 4) {
                    0 => Length::Auto,
                    1 => Length::Scale(Ratio {
                        num: 1 + below(r, 3) as u32,
                        den: 1 + below(r, 3) as u32,
                    }),
                    _ => Length::Px(1 + below(r, n * 2) as u32),
                };
                let mut width = len(r, w);
                let height = len(r, h);
                if width == Length::Auto && height == Length::Auto {
                    width = Length::Px(1 + below(r, w * 2) as u32);
                }
                let filter = ["nearest", "box", "bilinear", "lanczos3"][below(r, 4) as usize];
                return (
                    "image.resize",
                    json!({ "width": width, "height": height, "filter": filter }),
                );
            }
            5 | 6 => {
                let formats = [
                    "gray8", "grayA8", "rgb8", "rgba8", "gray16", "grayA16", "rgb16", "rgba16",
                ];
                let indexed = global && below(r, 4) == 0;
                let format = if indexed {
                    "indexed8"
                } else {
                    formats[below(r, formats.len() as u64) as usize]
                };
                let dither = ["none", "ordered", "errorDiffusion"][below(r, 3) as usize];
                let matte = [r.next_u32() as u8, 7, 9];
                return (
                    "image.convert",
                    json!({ "format": format, "colors": 2 + below(r, 30), "dither": dither, "matte": matte }),
                );
            }
            7 if fmt == PixelFormat::Indexed8 => {
                let entries: Vec<serde_json::Value> = (0..1 + below(r, 2))
                    .map(|i| json!({ "index": i as u8, "color": color(r) }))
                    .collect();
                return ("palette.replace", json!({ "entries": entries }));
            }
            8 => {
                let cw = 1 + below(r, w) as u32;
                let ch = 1 + below(r, h) as u32;
                return (
                    "meta.set_grid",
                    json!({ "grid": { "cellW": cw, "cellH": ch, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 } }),
                );
            }
            9 => {
                return (
                    "meta.set_pivot",
                    json!({ "pivot": { "x": below(r, w + 1), "y": below(r, h + 1) } }),
                );
            }
            10 | 11 => {
                let mut actions: Vec<PaintAction> = (0..1 + below(r, 3))
                    .map(|_| random_stroke(r, w, h, fmt, frames))
                    .collect();
                if global && below(r, 4) == 0 {
                    actions.push(random_fill(r, w, h, fmt, frames));
                }
                return ("paint.actions", json!({ "actions": actions }));
            }
            12 if global && is_image => {
                return (
                    "image.trim_alpha",
                    json!({ "threshold": below(r, 20), "mode": "union" }),
                );
            }
            _ => continue,
        }
    }
}

/// ランダムな鎖：形 `shape` の入力に、compile が通る操作を `len` 個まで線形につなぐ（通らない操作は 16 回まで選び直す）。
/// 音声の入力では、ときどき出力に量子化（8・16・24 ビット、TPDF・ディザなし）を付ける。
pub fn random_chain(
    engine: &Engine,
    r: &mut ChaCha8Rng,
    shape: &Shape,
    len: usize,
    global: bool,
) -> Graph {
    let mut g = Graph::new_single();
    let shapes = BTreeMap::from([(InputId(0), *shape)]);
    for _ in 0..len {
        for _ in 0..16 {
            let now = engine
                .compile(&g, &shapes)
                .ok()
                .and_then(|p| p.output_shapes().first().map(|o| o.1))
                .unwrap_or(*shape);
            let (op, params) = random_op(r, &now, global);
            let mut next = g.clone();
            if next.push_linear(op, 1, params, r.next_u64()).is_err() {
                continue;
            }
            if engine.compile(&next, &shapes).is_ok() {
                g = next;
                break;
            }
        }
    }
    // 量子化の選びは、ノードの種から作った別の乱数で引く（`r` の並びを変えず、ほかのテストの文書を変えない）
    let mut q = crate::determinism::rng(
        g.nodes
            .iter()
            .fold(len as u64, |a, n| a.rotate_left(7) ^ n.seed),
    );
    if matches!(shape, Shape::Audio { .. })
        && below(&mut q, 3) == 0
        && let Some(out) = g.outputs.first_mut()
    {
        out.quantize = Some(Quantize {
            v: QUANTIZE_VERSION,
            bits: [8u16, 16, 24][below(&mut q, 3) as usize],
            dither: if below(&mut q, 2) == 0 {
                Dither::Tpdf
            } else {
                Dither::None
            },
            seed: q.next_u64(),
        });
    }
    g
}

/// ランダムな編集の種類（テストで、どれも起きたかを数える）。前の 7 つはノード 1 つの引数・追記・有効の切り替え、
/// `fill` は canvas の fill だけの変更、`seed` は種だけの変更、後ろの 4 つは構造の手（置き換え・消す・動かす・足す）。
pub const EDIT_KINDS: &[&str] = &[
    "params", "append", "middle", "toggle", "fade", "loop", "meta", "fill", "seed", "replace",
    "remove", "move", "add",
];

/// [`random_step`] の手の種類（[`EDIT_KINDS`] と、2〜3 手をまとめた `multi`）。
pub const STEP_KINDS: &[&str] = &[
    "params", "append", "middle", "toggle", "fade", "loop", "meta", "fill", "seed", "replace",
    "remove", "move", "add", "multi",
];

/// ランダムな編集：ノード 1 つの引数の引き直し・描画への追記・途中の動作の差し替え・有効の切り替え・フェードの長さ・
/// ループ点・メタデータ・canvas の fill だけ・種だけの変更と、線形の列の構造の手（別の操作への置き換え・消す・動かす・
/// 足す。つなぎは並びの順に直す）。compile が通るものを返す（16 回試して通らなければ None）。返すのは (編集した文書, 種類)。
pub fn random_edit(
    engine: &Engine,
    r: &mut ChaCha8Rng,
    g: &Graph,
    input: &Shape,
) -> Option<(Graph, &'static str)> {
    let shapes = BTreeMap::from([(InputId(0), *input)]);
    let plan = engine.compile(g, &shapes).ok()?;
    // 位置 k（ノード k の前）の値の形
    let shape_at = |k: usize| match k.checked_sub(1).and_then(|p| g.nodes.get(p)) {
        Some(n) => plan.node_shape(n.id),
        None => Some(*input),
    };
    let len = g.nodes.len();
    // 構造の手は 5 回に 1 回（空の列には足すだけ）。ほかはノード 1 つの手（引数・追記・有効の切り替えなどの 7 つ。fill だけ・
    // 種だけは、引数・有効の切り替えの手の一部として引く）。どちらにするかは先に決め、通らなければ同じ側で選び直す
    let structural = len == 0 || below(r, 5) == 0;
    for _ in 0..16 {
        let pick = match (len, structural) {
            (0, _) => 12,
            (_, true) => 9 + below(r, 4),
            (_, false) => below(r, 7),
        };
        let i = below(r, len.max(1) as u64) as usize;
        let mut next = g.clone();
        let kind;
        if pick >= 9 {
            match pick {
                9 => {
                    kind = "replace";
                    let node = next.nodes.get_mut(i)?;
                    let (op, params) = random_op(r, &shape_at(i)?, true);
                    if op == node.op {
                        continue;
                    }
                    (node.op, node.v, node.params) = (op.to_string(), 1, params);
                }
                10 => {
                    kind = "remove";
                    next.nodes.remove(i);
                }
                11 if len >= 2 => {
                    kind = "move";
                    let mut to = below(r, len as u64 - 1) as usize;
                    if to >= i {
                        to += 1;
                    }
                    let n = next.nodes.remove(i);
                    next.nodes.insert(to, n);
                }
                12 => {
                    kind = "add";
                    let at = below(r, len as u64 + 1) as usize;
                    let (op, params) = random_op(r, &shape_at(at)?, true);
                    let id = crate::doc::NodeId(next.next_id);
                    next.next_id = next.next_id.checked_add(1)?;
                    next.nodes.insert(
                        at,
                        crate::doc::Node {
                            id,
                            op: op.to_string(),
                            v: 1,
                            params,
                            inputs: vec![crate::doc::PortRef::Input { id: InputId(0) }],
                            seed: r.next_u64(),
                            enabled: true,
                        },
                    );
                }
                _ => continue,
            }
            relink(&mut next);
        } else {
            let (Some(node), Some(slot)) = (g.nodes.get(i), next.nodes.get_mut(i)) else {
                continue;
            };
            let in_shape = shape_at(i)?;
            let (w, h, fmt, frames) =
                image_dims(&in_shape).unwrap_or((1, 1, PixelFormat::Rgba8, 1));
            match pick {
                0 if node.op == "image.canvas" && below(r, 2) == 0 => {
                    // fill だけを変える（持ち上げた canvas の param_change は、フレームの列の全フレームに広げる）
                    kind = "fill";
                    let mut p = node.params.clone();
                    p["fill"] = if below(r, 3) == 0 {
                        serde_json::Value::Null
                    } else {
                        json!(color(r))
                    };
                    slot.params = p;
                }
                0 => {
                    kind = "params";
                    let (op, params) = random_op(r, &in_shape, true);
                    if op != node.op {
                        continue;
                    }
                    slot.params = params;
                }
                1 | 2 if node.op == "paint.actions" => {
                    let Some(list) = node.params.get("actions").and_then(|a| a.as_array()) else {
                        continue;
                    };
                    let mut list = list.clone();
                    let stroke = serde_json::to_value(random_stroke(r, w, h, fmt, frames)).ok()?;
                    if below(r, 2) == 0 || list.len() < 2 {
                        kind = "append";
                        list.push(stroke);
                    } else {
                        kind = "middle";
                        let k = below(r, list.len() as u64 - 1) as usize;
                        list[k] = stroke;
                    }
                    slot.params = json!({ "actions": list });
                }
                3 if below(r, 3) == 0 => {
                    kind = "seed";
                    slot.seed = r.next_u64();
                }
                3 => {
                    kind = "toggle";
                    slot.enabled = !node.enabled;
                }
                4 if node.op == "audio.fade" => {
                    kind = "fade";
                    let mut p = node.params.clone();
                    let n = match in_shape {
                        Shape::Audio { frames, .. } => frames.max(),
                        _ => continue,
                    };
                    if below(r, 2) == 0 {
                        p["fadeIn"] = json!(below(r, n / 2 + 1));
                    } else {
                        p["fadeOut"] = json!(below(r, n / 2 + 1));
                    }
                    slot.params = p;
                }
                5 if node.op == "audio.set_loop" => {
                    kind = "loop";
                    let n = match in_shape {
                        Shape::Audio { frames, .. } => frames.max(),
                        _ => continue,
                    };
                    let a = below(r, n.max(2) - 1);
                    slot.params = json!({ "loop": { "start": a, "end": a + 1 } });
                }
                6 if node.op.starts_with("meta.") => {
                    kind = "meta";
                    let (op, params) = random_op(r, &in_shape, false);
                    if op != node.op {
                        continue;
                    }
                    slot.params = params;
                }
                _ => continue,
            }
        }
        if engine.compile(&next, &shapes).is_ok() {
            return Some((next, kind));
        }
    }
    None
}

/// ランダムな手：ふつうは [`random_edit`] を 1 回、4 回に 1 回は 2〜3 回まとめて当てる（途中の版を飛ばす。1 手で複数の
/// ノードが変わる。種類は `multi`）。どれも当てられなければ None。
pub fn random_step(
    engine: &Engine,
    r: &mut ChaCha8Rng,
    g: &Graph,
    input: &Shape,
) -> Option<(Graph, &'static str)> {
    if below(r, 4) != 0 {
        return random_edit(engine, r, g, input);
    }
    let mut now = random_edit(engine, r, g, input)?.0;
    for _ in 0..1 + below(r, 2) {
        match random_edit(engine, r, &now, input) {
            Some((next, _)) => now = next,
            None => break,
        }
    }
    Some((now, "multi"))
}

/// テストの次の手：ふつうは [`random_step`]、6 回に 1 回は入力の値だけを替える（`make` の種を替えた同じ形の値。種類は
/// `input`）。返すのは (文書, 種類, 入力の値)。
pub fn random_step_or_input(
    engine: &Engine,
    r: &mut ChaCha8Rng,
    g: &Graph,
    make: &InputGen,
    input: &Value,
) -> Option<(Graph, &'static str, Value)> {
    if below(r, 6) == 0
        && let Some(other) = reseeded(make, r.next_u64()).and_then(|m| m.generate().ok())
        && other.shape() == input.shape()
    {
        return Some((g.clone(), "input", other));
    }
    let (next, kind) = random_step(engine, r, g, &input.shape())?;
    Some((next, kind, input.clone()))
}

/// 線形の列のつなぎを、並びの順に直す（ノード k の入力 0 はノード k − 1 の出力、先頭は入力 0。出力 0 は最後のノード）。
fn relink(g: &mut Graph) {
    let mut from = crate::doc::PortRef::Input { id: InputId(0) };
    for n in &mut g.nodes {
        match n.inputs.first_mut() {
            Some(first) => *first = from,
            None => n.inputs.push(from),
        }
        from = crate::doc::PortRef::Node { id: n.id };
    }
    if let Some(out) = g.outputs.first_mut() {
        out.from = from;
    }
}

/// 同じ作り方で種だけを替えた入力（形は同じで値が違う。入力の値の変更を試すのに使う）。種を持たない作り方は None。
pub fn reseeded(make: &InputGen, seed: u64) -> Option<InputGen> {
    let mut out = make.clone();
    match &mut out {
        InputGen::Noise { seed: s, .. }
        | InputGen::NoiseFrames { seed: s, .. }
        | InputGen::TrimmedFrames { seed: s, .. }
        | InputGen::AudioNoise { seed: s, .. } => *s = seed,
        _ => return None,
    }
    Some(out)
}

/// パレットの変化の決まった場合（段 10 の検証の D2。tests/delta.rs・view.rs）：Indexed8 の上流でパレットだけが変わる
/// （palette.replace の引数の変更・外す・入れる、Indexed8 への convert の引数の変更）文書の前後を、後ろに canvas（広げる。
/// fill なし・あり）・反転・最近傍の拡大をつないで作る。返すのは (説明, 前の文書, 後の文書, 入力)。後ろの出力のパレットも
/// 変わるので、変わった範囲は全体になる。
pub fn palette_change_cases() -> Result<Vec<(String, Graph, Graph, Value)>, EngineError> {
    let indexed = InputGen::Noise {
        w: 300,
        h: 300,
        fmt: PixelFormat::Indexed8,
        seed: 5,
    }
    .generate()?;
    let rgba = InputGen::Noise {
        w: 300,
        h: 260,
        fmt: PixelFormat::Rgba8,
        seed: 6,
    }
    .generate()?;
    let tails: Vec<(&str, serde_json::Value)> = vec![
        (
            "image.canvas",
            json!({ "rect": { "x": -300, "y": -300, "w": 900, "h": 900 }, "fill": null }),
        ),
        (
            "image.canvas",
            json!({ "rect": { "x": -20, "y": -10, "w": 600, "h": 330 }, "fill": [10, 20, 30, 255] }),
        ),
        ("image.flip", json!({ "axis": "horizontal" })),
        (
            "image.resize",
            json!({ "width": { "scale": { "num": 2, "den": 1 } }, "height": "auto", "filter": "nearest" }),
        ),
    ];
    let reds = json!({ "entries": [
        { "index": 0, "color": [255, 0, 255, 255] },
        { "index": 1, "color": [0, 255, 0, 255] },
    ] });
    let blue = json!({ "entries": [{ "index": 0, "color": [1, 2, 3, 255] }] });
    let to_indexed = |colors: u32| json!({ "format": "indexed8", "colors": colors, "dither": "none", "matte": [0, 0, 0] });
    let mut out = Vec::new();
    for (tail, tp) in &tails {
        let doc = |head: &str, hp: &serde_json::Value, enabled: bool| {
            let mut g = Graph::new_single();
            let id = g.push_linear(head, 1, hp.clone(), 1)?;
            g.push_linear(tail, 1, tp.clone(), 2)?;
            if let Some(n) = g.nodes.iter_mut().find(|n| n.id == id) {
                n.enabled = enabled;
            }
            Ok::<_, EngineError>(g)
        };
        let cases = [
            (
                "palette.replace の引数",
                doc("palette.replace", &reds, true)?,
                doc("palette.replace", &blue, true)?,
                &indexed,
            ),
            (
                "palette.replace を外す",
                doc("palette.replace", &reds, true)?,
                doc("palette.replace", &reds, false)?,
                &indexed,
            ),
            (
                "palette.replace を入れる",
                doc("palette.replace", &reds, false)?,
                doc("palette.replace", &reds, true)?,
                &indexed,
            ),
            (
                "Indexed8 への convert の引数",
                doc("image.convert", &to_indexed(8), true)?,
                doc("image.convert", &to_indexed(5), true)?,
                &rgba,
            ),
        ];
        for (what, a, b, input) in cases {
            out.push((format!("{what} → {tail} {tp}"), a, b, input.clone()));
        }
    }
    Ok(out)
}

/// 値 `v` の、範囲 `keep` の外の画素・標本を乱数に替えた値（Indexed8 はパレットの色の数より小さい番号。メタデータ・形は
/// そのまま）。範囲の外の入力を壊しても範囲の中が変わらないことの確かめ（E18）に使う。
pub fn corrupt_outside(v: &Value, keep: &Area, seed: u64) -> Result<Value, EngineError> {
    if keep.is_all() {
        return Ok(v.clone());
    }
    if let Value::Audio(a) = v {
        let mut r = crate::determinism::rng(seed);
        let planes = a
            .planes()
            .iter()
            .map(|p| {
                Arc::new(
                    p.iter()
                        .enumerate()
                        .map(|(i, s)| {
                            if keep.contains_sample(i as u64) {
                                *s
                            } else {
                                (r.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
                            }
                        })
                        .collect(),
                )
            })
            .collect();
        return Ok(Value::Audio(Arc::new(Audio::new(
            a.rate(),
            planes,
            a.meta.clone(),
        )?)));
    }
    let offsets: BTreeMap<u32, IPoint> = frames_of(v).into_iter().map(|(i, _, o)| (i, o)).collect();
    map_rasters_region(v, &BTreeMap::new(), None, |f, px, _, _| {
        let origin = offsets.get(&f).copied().unwrap_or(IPoint { x: 0, y: 0 });
        let colors = px.palette().map_or(256, |p| p.colors.len());
        let bpp = px.bpp();
        let rects = keep.frame_rects(f).unwrap_or_default();
        px.derive(px.width(), px.height(), &NoMonitor, |t, out| {
            px.read_rect(t, out)?;
            let canvas = IRect::new(origin.x + t.x, origin.y + t.y, t.w, t.h);
            // タイルが範囲にまるごと入るなら壊さない
            if rects.is_whole()
                || rects.rects().iter().any(|q| {
                    q.x <= canvas.x
                        && q.y <= canvas.y
                        && q.right() >= canvas.right()
                        && q.bottom() >= canvas.bottom()
                })
            {
                return Ok(());
            }
            let mut noise = vec![0u8; out.len()];
            crate::determinism::rng_stream(
                seed,
                u64::from(f) << 32 | (t.y as u64) << 16 | t.x as u64,
            )
            .fill_bytes(&mut noise);
            if colors < 256 {
                for b in &mut noise {
                    *b = (usize::from(*b) % colors) as u8;
                }
            }
            let touches = rects.intersects(&canvas);
            for (k, (p, n)) in out
                .chunks_exact_mut(bpp)
                .zip(noise.chunks_exact(bpp))
                .enumerate()
            {
                let x = canvas.x + (k as i64 % i64::from(t.w));
                let y = canvas.y + (k as i64 / i64::from(t.w));
                if touches && rects.contains(x, y) {
                    continue;
                }
                p.copy_from_slice(n);
            }
            Ok(())
        })
    })
}

/// 値 `v` の、範囲 `keep` に重なるタイル（値のタイル）だけを有効にした値（ほかは無効。範囲の描画の入力を作る）。
pub fn only_tiles(v: &Value, keep: &Area) -> Result<Value, EngineError> {
    if matches!(v, Value::Audio(_)) || keep.is_all() {
        return Ok(v.clone());
    }
    let need = tiles_in(v, keep, TILE);
    map_rasters_region(v, &need, None, |_, px, pick, _| {
        let mut drop = Tiles::all(px.width(), px.height(), TILE);
        drop.subtract(pick)?;
        Ok(px.invalidate(&drop))
    })
}

/// 前後の値 `a`・`b` の差（画素・標本）が範囲 `area` に入るかを確かめる。形・形式・パレット・フレームの位置が違えば `area` が
/// `All` であること。入らなければ最初の外れの説明。
pub fn check_contains(a: &Value, b: &Value, area: &Area) -> Result<(), String> {
    // 全体か、どのフレームもフレーム全体（`All` を開いたもの）なら、何が変わってもよい
    let whole = |v: &Value| {
        let frames = frames_of(v);
        !frames.is_empty()
            && frames
                .iter()
                .all(|(f, _, _)| area.frame_rects(*f).is_some_and(|r| r.is_whole()))
    };
    if area.is_all() || (whole(a) && whole(b)) {
        return Ok(());
    }
    match (a, b) {
        (Value::Audio(x), Value::Audio(y)) => {
            if x.rate() != y.rate() || x.frames() != y.frames() || x.channels() != y.channels() {
                return Err("音声の形が変わったのに範囲が全体ではありません".into());
            }
            for (p, q) in x.planes().iter().zip(y.planes()) {
                for (i, (s, t)) in p.iter().zip(q.iter()).enumerate() {
                    let same = s.to_bits() == t.to_bits() || (s.is_nan() && t.is_nan());
                    if !same && !area.contains_sample(i as u64) {
                        return Err(format!("標本 {i} が範囲の外で変わりました"));
                    }
                }
            }
            Ok(())
        }
        (Value::Audio(_), _) | (_, Value::Audio(_)) => Err("値の種類が変わりました".into()),
        _ => {
            let (fa, fb) = (frames_of(a), frames_of(b));
            if fa.len() != fb.len() {
                return Err("フレームの数が変わったのに範囲が全体ではありません".into());
            }
            for ((_, p, po), (f, q, qo)) in fa.into_iter().zip(fb) {
                if po != qo
                    || p.width() != q.width()
                    || p.height() != q.height()
                    || p.format() != q.format()
                    || p.palette() != q.palette()
                {
                    return Err(format!(
                        "フレーム {f} の形・形式・パレット・位置が変わったのに範囲が全体ではありません"
                    ));
                }
                if let Some((x, y)) =
                    first_outside(p, q, |x, y| area.contains_pixel(f, qo.x + x, qo.y + y))
                {
                    return Err(format!(
                        "フレーム {f} の画素 ({}, {}) が範囲の外で変わりました",
                        qo.x + x,
                        qo.y + y
                    ));
                }
            }
            Ok(())
        }
    }
}

/// 2 つの値 `a`・`b`（同じ形）が、範囲 `area` の中で同じかを確かめる（画素はキャンバスの座標、音声は区間）。違えば説明。
pub fn check_same_inside(a: &Value, b: &Value, area: &Area) -> Result<(), String> {
    if a.shape() != b.shape() {
        return Err(format!(
            "形が違います（{:?} と {:?}）",
            a.shape(),
            b.shape()
        ));
    }
    match (a, b) {
        (Value::Audio(x), Value::Audio(y)) => {
            for (p, q) in x.planes().iter().zip(y.planes()) {
                for (i, (s, t)) in p.iter().zip(q.iter()).enumerate() {
                    let same = s.to_bits() == t.to_bits() || (s.is_nan() && t.is_nan());
                    if !same && area.contains_sample(i as u64) {
                        return Err(format!("標本 {i} が範囲の中で違います"));
                    }
                }
            }
            Ok(())
        }
        _ => {
            for ((_, p, po), (f, q, qo)) in frames_of(a).into_iter().zip(frames_of(b)) {
                if po != qo || p.palette() != q.palette() {
                    return Err(format!("フレーム {f} の位置・パレットが違います"));
                }
                // 範囲の外の違いは無視して、中の違いを探す
                if let Some((x, y)) =
                    first_outside(p, q, |x, y| !area.contains_pixel(f, qo.x + x, qo.y + y))
                {
                    return Err(format!(
                        "フレーム {f} の画素 ({}, {}) が範囲の中で違います",
                        qo.x + x,
                        qo.y + y
                    ));
                }
            }
            Ok(())
        }
    }
}

/// 2 つの同じ形の画素で、`inside` の外にある違う画素（画像の座標）の最初のもの。
fn first_outside(a: &Raster, b: &Raster, inside: impl Fn(i64, i64) -> bool) -> Option<(i64, i64)> {
    let bpp = a.bpp();
    for ty in 0..a.tiles_y() {
        for tx in 0..a.tiles_x() {
            if a.shares_tile(b, tx, ty) {
                continue;
            }
            let rect = a.tile_rect(tx, ty);
            let (ta, tb) = (a.tile(tx, ty), b.tile(tx, ty));
            for (k, (p, q)) in ta.chunks_exact(bpp).zip(tb.chunks_exact(bpp)).enumerate() {
                if p != q {
                    let x = rect.x + (k as i64 % i64::from(rect.w));
                    let y = rect.y + (k as i64 / i64::from(rect.w));
                    if !inside(x, y) {
                        return Some((x, y));
                    }
                }
            }
        }
    }
    None
}

/// 範囲の描画の値の、範囲 `area` に重なる有効なタイルが、全体を描いた値 `full` の同じタイルと同じかを確かめる（範囲に
/// 重なるタイルがどれも有効であることも）。違えば説明。
pub fn check_region(
    got: &crate::render::region::PartialValue,
    full: &Value,
    area: &Area,
) -> Result<(), String> {
    if matches!(full, Value::Audio(_)) {
        return match got.exact() {
            Some(v) if v == full => Ok(()),
            _ => Err("音声の範囲の描画が全体の描画と違います".into()),
        };
    }
    if got.shape() != full.shape() {
        return Err(format!(
            "範囲の描画の形が違います（{:?} と {:?}）",
            got.shape(),
            full.shape()
        ));
    }
    let need = tiles_in(full, area, TILE);
    for (f, px, _) in frames_of(full) {
        let Some(want) = need.get(&f) else {
            continue;
        };
        let valid = got
            .valid(f)
            .ok_or_else(|| format!("フレーム {f} がありません"))?;
        for (tx, ty) in want.iter() {
            if !valid.contains(tx, ty) {
                return Err(format!(
                    "フレーム {f} のタイル ({tx}, {ty}) が描かれていません"
                ));
            }
            let tile = got.tile(f, tx, ty).map_err(|e| e.to_string())?;
            if tile != px.tile(tx, ty) {
                return Err(format!(
                    "フレーム {f} のタイル ({tx}, {ty}) が全体の描画と違います"
                ));
            }
        }
    }
    Ok(())
}

/// 出力の値の中のランダムな範囲（1 画素・タイルの境をまたぐ矩形・フレーム全体・ときどき全体）。
pub fn random_area(r: &mut ChaCha8Rng, v: &Value) -> Area {
    if let Value::Audio(a) = v {
        let n = a.frames() as u64;
        let s = below(r, n);
        return Area::span(s, s + 1 + below(r, (n - s).min(3_000)));
    }
    let frames = frames_of(v);
    if frames.is_empty() || below(r, 12) == 0 {
        return Area::All;
    }
    let mut out = Area::Empty;
    for _ in 0..1 + below(r, 2) {
        let (f, px, o) = frames[below(r, frames.len() as u64) as usize];
        let (w, h) = (u64::from(px.width()), u64::from(px.height()));
        let rect = match below(r, 4) {
            0 => IRect::new(o.x + below(r, w) as i64, o.y + below(r, h) as i64, 1, 1),
            1 => {
                // タイルの境をまたぐ
                let x = (below(r, w.div_ceil(256)) * 256) as i64 - 3;
                let y = (below(r, h.div_ceil(256)) * 256) as i64 - 2;
                IRect::new(o.x + x, o.y + y, 6, 5)
            }
            2 => {
                out.union_with(&Area::frame(f, Rects::whole()));
                continue;
            }
            _ => {
                let x = below(r, w) as i64;
                let y = below(r, h) as i64;
                IRect::new(
                    o.x + x,
                    o.y + y,
                    1 + below(r, w - x as u64) as u32,
                    1 + below(r, h - y as u64) as u32,
                )
            }
        };
        out.union_with(&Area::rect(f, rect));
    }
    out
}

/// ファズの照合：文書 `g` を入力 `input` に当て、ランダムな範囲の描画が全体の描画と同じか（範囲の描画が依存の外を読んで
/// 全体に落ちていないこと）と、ランダムな編集の変わった範囲の推論が前後の差を覆うかを確かめる。違えば `Internal`。
pub fn check_region_and_delta(
    engine: &Engine,
    g: &Graph,
    input: &Value,
    seed: u64,
) -> Result<(), EngineError> {
    let mut r = crate::determinism::rng(seed);
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    let Ok(plan) = engine.compile(g, &shapes) else {
        return Ok(());
    };
    if plan.cost() > 1 << 22 {
        return Ok(());
    }
    let inputs = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), crate::digest::Digest::default())?,
    )]);
    let plain = BTreeMap::from([(
        InputId(0),
        crate::render::InputValue {
            value: input.clone(),
            source: crate::digest::Digest::default(),
        },
    )]);
    let Ok(full) = engine.render(&plan, &plain, &NoMonitor) else {
        return Ok(());
    };
    let Some(out) = full.outputs.first() else {
        return Ok(());
    };
    let mut cache = NodeCache::new(1 << 24);
    for _ in 0..3 {
        let area = random_area(&mut r, &out.value);
        let req = AreaRequest {
            output: out.id,
            area: area.clone(),
        };
        match engine.render_area(&plan, &inputs, &mut cache, &req, None, &NoMonitor) {
            Ok(got) => {
                if got.stats.fallbacks != 0 {
                    return Err(EngineError::Internal(
                        "範囲の描画が、申告した依存の外（無効なタイル）を読みました".into(),
                    ));
                }
                check_region(&got.value, &out.value, &area).map_err(EngineError::Internal)?;
            }
            Err(EngineError::Internal(m)) => return Err(EngineError::Internal(m)),
            // 範囲の描画は全体の描画が失敗する入力でも通りうるが、逆に全体が描けて範囲が失敗するのは誤り
            Err(e) => {
                return Err(EngineError::Internal(format!(
                    "全体は描けたのに範囲の描画が失敗しました：{e}"
                )));
            }
        }
    }
    // 変わった範囲の推論
    if let Some((next, _)) = random_step(engine, &mut r, g, &input.shape())
        && let Ok(next_plan) = engine.compile(&next, &shapes)
        && next_plan.cost() <= 1 << 22
        && let Ok(after) = engine.render(&next_plan, &plain, &NoMonitor)
        && let Some(b) = after.outputs.first()
    {
        let delta = engine.delta(&plan, &inputs, &next_plan, &inputs)?;
        let area = delta
            .outputs
            .iter()
            .find(|(id, _)| *id == out.id)
            .map_or(Area::All, |(_, a)| a.clone());
        let after = after.outputs.iter().find(|o| o.id == out.id).unwrap_or(b);
        check_contains(&out.value, &after.value, &area).map_err(|m| {
            EngineError::Internal(format!("変わった範囲の推論の外で値が変わりました：{m}"))
        })?;
    }
    Ok(())
}
