//! `image.opaque_bounds` v1：アルファが `threshold` より大きい画素（不透明とみなす画素）を囲む矩形。
//!
//! - 計算は `imaging::bounds::opaque_bounds`（1 枚の画像）と `frames_opaque_bounds`（フレームの列）。透明な余白の
//!   トリム（`image.trim_alpha`）はこの解析を申告して、結果の矩形で切る（E26）。16 ビットは `threshold × 257` と比べ、
//!   Indexed8 はパレットのアルファで見る。
//! - 結果はキャンバスの座標（1 枚の画像なら画像の座標）：`canvas`（キャンバスの大きさ）、`union`（すべての不透明な画素を
//!   囲む矩形。全部が透明なら null）、`frames`（フレームごとの矩形。全部が透明なフレームは null。1 枚の画像は 1 要素）。
//!   トリムの「全部が透明なら左上の 1×1」は操作の規則（ADR-147）で、解析には入れない。
//! - フレームの列（透明を表せる形式）は表示の見え方で数える：フレームの外（キャンバスの覆われない部分）は透明と
//!   みなす。`union` は `frames` の合併。
//! - 透明を表せない形式（アルファのない形式）は画素を見ない：1 枚の画像は全体。フレームの列は、`frames` が各フレームの
//!   矩形そのもの、`union` は覆われない部分があっても**キャンバス全体**（`frames` の合併ではない。トリム
//!   （`image.trim_alpha`）がこの形式では何もしないことと合わせる）。この形式に限り、フレームの外を透明とみなさない
//!   （同じ値で `image.alpha` は覆われない部分を `uncovered` に数えて `binary` と答える）。
//! - 整数だけ（`Numeric::Exact`）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate};
use crate::error::EngineError;
use crate::geom::{IRect, Size};
use crate::imaging::bounds::{frames_opaque_bounds, opaque_bounds};
use crate::imaging::pixels::can_be_transparent;
use crate::op::{Numeric, OpParams, ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Value};

/// 引数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpaqueBoundsParams {
    /// このアルファ以下の画素を透明とみなす（`image.trim_alpha` と同じ範囲 0〜254。16 ビットは 257 倍と比べる）。
    pub threshold: u8,
}

impl OpParams for OpaqueBoundsParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "threshold",
                ParamKind::Int {
                    min: 0,
                    max: 254,
                    step: 1,
                },
                Unit::None,
                json!(0),
            )],
        }
    }
}

/// 結果。座標はキャンバス（1 枚の画像なら画像）の座標。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpaqueBoundsOutput {
    /// キャンバスの大きさ（1 枚の画像なら画像の大きさ）。
    pub canvas: Size,
    /// すべての不透明な画素を囲む矩形。全部が透明なら null。透明を表せない形式はキャンバス全体。
    pub union: Option<IRect>,
    /// フレームごとの矩形（フレームの順。全部が透明なフレームは null）。1 枚の画像は 1 要素。
    pub frames: Vec<Option<IRect>>,
}
crate::json_output!(OpaqueBoundsOutput);

/// 不透明な範囲。
pub struct OpaqueBounds;

/// 矩形 1 つの正規の JSON の長さの上限（`{"h":…,"w":…,"x":…,"y":…}` と区切り。座標は i64・大きさは u32）。
const RECT_BYTES: u64 = 96;

impl Analysis for OpaqueBounds {
    const ID: &'static str = "image.opaque_bounds";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Image, ValueKind::Frames];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = OpaqueBoundsParams;
    type Output = OpaqueBoundsOutput;

    fn run(
        p: &OpaqueBoundsParams,
        v: &Value,
        cx: &AnalyzeCx<'_>,
    ) -> Result<OpaqueBoundsOutput, EngineError> {
        cx.check()?;
        match v {
            Value::Image(img) => {
                let b = opaque_bounds(&img.px, p.threshold, cx.mon)?;
                Ok(OpaqueBoundsOutput {
                    canvas: Size {
                        w: img.px.width(),
                        h: img.px.height(),
                    },
                    union: b,
                    frames: vec![b],
                })
            }
            Value::Frames(fr) => {
                let canvas = fr.canvas();
                let (frames, union) = frames_opaque_bounds(fr, p.threshold, cx.mon)?;
                // 透明を表せない形式は、キャンバスの覆われない部分も不透明に見える（トリムは何もしない）
                let union = if can_be_transparent(fr.first().px.format()) {
                    union
                } else {
                    Some(IRect::new(0, 0, canvas.w, canvas.h))
                };
                Ok(OpaqueBoundsOutput {
                    canvas,
                    union,
                    frames,
                })
            }
            Value::Audio(_) => Err(EngineError::Internal(format!(
                "{} の入力が音声です",
                Self::ID
            ))),
        }
    }

    fn examples() -> Vec<AnalysisExample<OpaqueBoundsParams>> {
        let t = |threshold| OpaqueBoundsParams { threshold };
        vec![
            AnalysisExample {
                name: "sprite-across-tiles",
                params: t(0),
                input: InputGen::Sprite {
                    w: 600,
                    h: 300,
                    rect: IRect::new(250, 40, 300, 200),
                },
            },
            AnalysisExample {
                name: "all-clear",
                params: t(0),
                input: InputGen::Sprite {
                    w: 9,
                    h: 7,
                    rect: IRect::new(20, 20, 1, 1),
                },
            },
            AnalysisExample {
                name: "noise-gray16-threshold",
                params: t(200),
                input: InputGen::Noise {
                    w: 40,
                    h: 30,
                    fmt: PixelFormat::GrayA16,
                    seed: 21,
                },
            },
            AnalysisExample {
                name: "indexed-clear-color",
                params: t(0),
                input: InputGen::Noise {
                    w: 20,
                    h: 20,
                    fmt: PixelFormat::Indexed8,
                    seed: 23,
                },
            },
            AnalysisExample {
                name: "rgb-is-whole",
                params: t(0),
                input: InputGen::Noise {
                    w: 7,
                    h: 5,
                    fmt: PixelFormat::Rgb8,
                    seed: 22,
                },
            },
            AnalysisExample {
                name: "frames-noise",
                params: t(128),
                input: InputGen::NoiseFrames {
                    w: 12,
                    h: 10,
                    fmt: PixelFormat::Rgba8,
                    count: 3,
                    seed: 25,
                },
            },
            AnalysisExample {
                name: "trimmed-frames",
                params: t(7),
                input: InputGen::TrimmedFrames {
                    w: 40,
                    h: 30,
                    fmt: PixelFormat::Rgba8,
                    count: 4,
                    seed: 3,
                },
            },
            AnalysisExample {
                name: "rgb-trimmed-frames-cover-the-canvas",
                params: t(0),
                input: InputGen::TrimmedFrames {
                    w: 16,
                    h: 12,
                    fmt: PixelFormat::Rgb8,
                    count: 3,
                    seed: 4,
                },
            },
            // 一部のフレームだけが全部透明（null）：しきい値 254 ではアルファ 255 の画素だけが不透明。`union` は null で
            // ないフレームの合併（フレーム 0 の 1×1 と、フレーム 3 の矩形）
            AnalysisExample {
                name: "frames-some-all-clear",
                params: t(254),
                input: InputGen::TrimmedFrames {
                    w: 32,
                    h: 16,
                    fmt: PixelFormat::Rgba8,
                    count: 4,
                    seed: 4,
                },
            },
        ]
    }

    fn estimate(_p: &OpaqueBoundsParams, input: &Shape) -> Estimate {
        let (count, pixels, fmt) = match *input {
            Shape::Image { w, h, fmt } => (1, w.max().saturating_mul(h.max()), fmt),
            Shape::Frames {
                count, w, h, fmt, ..
            } => (
                count.max(),
                w.max().saturating_mul(h.max()).saturating_mul(count.max()),
                fmt,
            ),
            Shape::Audio { .. } => return Estimate::default(),
        };
        Estimate {
            // {"canvas":…,"frames":[…],"union":…}
            bytes: 128u64.saturating_add(count.saturating_add(1).saturating_mul(RECT_BYTES)),
            // アルファのない形式は画素を見ない
            cost: if can_be_transparent(fmt) { pixels } else { 0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::value::{Frame, Frames, Raster};
    use std::sync::Arc;

    fn run(v: &Value, threshold: u8) -> OpaqueBoundsOutput {
        let limits = crate::Limits::default();
        OpaqueBounds::run(
            &OpaqueBoundsParams { threshold },
            v,
            &AnalyzeCx {
                mon: &NoMonitor,
                limits: &limits,
            },
        )
        .unwrap()
    }

    /// 値を直接確かめる（ゴールデンに凍結する前に）：スプライト・全部が透明・フレームの列（覆われない部分は透明）・
    /// アルファのない形式。
    #[test]
    fn bounds_of_images_and_frames() {
        let sprite = InputGen::Sprite {
            w: 600,
            h: 300,
            rect: IRect::new(250, 40, 300, 200),
        }
        .generate()
        .unwrap();
        let out = run(&sprite, 0);
        assert_eq!(out.canvas, Size { w: 600, h: 300 });
        assert_eq!(out.union, Some(IRect::new(250, 40, 300, 200)));
        assert_eq!(out.frames, vec![out.union]);
        let clear = InputGen::Sprite {
            w: 9,
            h: 7,
            rect: IRect::new(20, 20, 1, 1),
        }
        .generate()
        .unwrap();
        assert_eq!(run(&clear, 0).union, None);
        assert_eq!(run(&clear, 0).frames, vec![None]);

        // 10×8 のキャンバスに 2 つのフレーム：(1,1) の 4×3（不透明な点は (2,1)）と、(5,4) の 3×3（全部が透明）
        let point = |w: u32, h: u32, at: Option<(u32, u32)>| {
            let mut data = vec![0u8; (w * h * 4) as usize];
            if let Some((x, y)) = at {
                let i = ((y * w + x) * 4) as usize;
                data[i..i + 4].copy_from_slice(&[1, 2, 3, 255]);
            }
            Raster::from_rows(w, h, PixelFormat::Rgba8, None, &data).unwrap()
        };
        let frames = Value::Frames(Arc::new(
            Frames::new(
                vec![
                    Frame {
                        px: point(4, 3, Some((2, 1))),
                        offset: crate::geom::IPoint { x: 1, y: 1 },
                        duration_ms: 10,
                    },
                    Frame {
                        px: point(3, 3, None),
                        offset: crate::geom::IPoint { x: 5, y: 4 },
                        duration_ms: 10,
                    },
                ],
                Size { w: 10, h: 8 },
                Default::default(),
                0,
            )
            .unwrap(),
        ));
        let out = run(&frames, 0);
        assert_eq!(out.canvas, Size { w: 10, h: 8 });
        assert_eq!(out.frames, vec![Some(IRect::new(3, 2, 1, 1)), None]);
        assert_eq!(out.union, Some(IRect::new(3, 2, 1, 1)));

        // アルファのない形式のトリムしたフレーム：各フレームは全体、合併はキャンバス全体
        let rgb = InputGen::TrimmedFrames {
            w: 16,
            h: 12,
            fmt: PixelFormat::Rgb8,
            count: 3,
            seed: 4,
        }
        .generate()
        .unwrap();
        let Value::Frames(fr) = &rgb else { panic!() };
        let out = run(&rgb, 0);
        assert_eq!(out.union, Some(IRect::new(0, 0, 16, 12)));
        for (f, b) in fr.frames().iter().zip(&out.frames) {
            assert_eq!(
                *b,
                Some(IRect::new(
                    f.offset.x,
                    f.offset.y,
                    f.px.width(),
                    f.px.height()
                ))
            );
        }
    }

    /// 例の値を直接確かめる（ゴールデンに凍結する前に）：一部のフレームだけが null の列。フレームごとの矩形は
    /// アルファが 254 を超える画素を総当たりで囲んだもの、`union` は null でないものの合併。
    #[test]
    fn some_frames_are_all_clear() {
        let ex = OpaqueBounds::examples()
            .into_iter()
            .find(|e| e.name == "frames-some-all-clear")
            .unwrap();
        let v = ex.input.generate().unwrap();
        let Value::Frames(fr) = &v else { panic!() };
        let out = run(&v, ex.params.threshold);
        let mut want = Vec::new();
        for f in fr.frames() {
            let (w, rows) = (f.px.width() as usize, f.px.to_rows().unwrap());
            let mut b: Option<(i64, i64, i64, i64)> = None;
            for (i, p) in rows.as_chunks::<4>().0.iter().enumerate() {
                if p[3] > 254 {
                    let (x, y) = (f.offset.x + (i % w) as i64, f.offset.y + (i / w) as i64);
                    b = Some(match b {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
            want.push(b.map(|(x0, y0, x1, y1)| {
                IRect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32)
            }));
        }
        assert_eq!(out.frames, want);
        assert_eq!(
            out.frames,
            vec![
                Some(IRect::new(0, 3, 1, 1)),
                None,
                None,
                Some(IRect::new(4, 3, 16, 7))
            ]
        );
        assert_eq!(out.union, Some(IRect::new(0, 3, 20, 7)));
    }
}
