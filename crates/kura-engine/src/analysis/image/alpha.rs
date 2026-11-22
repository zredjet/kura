//! `image.alpha` v1：透明の種類（none・binary・partial）と、透明・半透明・不透明の画素の数。
//!
//! 定義（凍結する）：
//! - 画素ごとに、アルファが 0 なら**透明**、最大（8 ビットは 255、16 ビットは 65535）なら**不透明**、そのあいだは
//!   **半透明**。16 ビットは元の深さのまま比べる（アルファ 1/65535 は半透明。8 ビットに直した値では数えない）。
//!   アルファのない形式（Gray8・Rgb8・Gray16・Rgb16）は画素を見ずにすべて不透明。Indexed8 は**使われている番号**の
//!   パレットのアルファで数える（使われていない透明な色は数えない）。
//! - フレームの列は各フレームの画素を数えて足し、`uncovered` に「フレームごとの、そのフレームに覆われないキャンバスの
//!   画素の数」の和を入れる（表示の見え方では透明。形式に依らない。画像は 0）。
//! - `kind`：半透明が 1 つでもあれば `partial`、そうでなく透明か `uncovered` が 1 つでもあれば `binary`、ほかは `none`。
//! - 保存の前の警告は `AlphaOutput::loss_in`（形式の `MetaCaps.alpha` と突き合わせる。E26）。
//! - 整数だけ（`Numeric::Exact`）。タイルごとに数えてタイルの順に足す。

use serde::{Deserialize, Serialize};

use crate::analysis::image::{planes, shape_pixels, tile_at, tile_count};
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate, NoParams};
use crate::codec::AlphaCaps;
use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::pixels::can_be_transparent;
use crate::op::Numeric;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Raster, Value};

/// 透明の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AlphaKind {
    /// 透明・半透明の画素がない。
    None,
    /// 透明と不透明だけ（フレームの列の覆われない部分を含む）。
    Binary,
    /// 半透明の画素がある。
    Partial,
}

/// 結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlphaOutput {
    pub kind: AlphaKind,
    /// アルファが 0 の画素（各フレームの画素の中）。
    pub transparent: u64,
    /// アルファが 0 と最大のあいだの画素。
    pub partial: u64,
    /// アルファが最大の画素（アルファのない形式はすべて）。
    pub opaque: u64,
    /// フレームの列で、フレームごとに数えた「そのフレームに覆われないキャンバスの画素」の和。画像は 0。
    pub uncovered: u64,
}
crate::json_output!(AlphaOutput);

/// 書き出しで透明に起きること（保存の前の警告）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AlphaLoss {
    /// 半透明が 2 値（透明か不透明）になる（`AlphaCaps::Binary` の形式に半透明を書く）。
    Binarized,
    /// 透明が失われる（`AlphaCaps::None` の形式に透明・半透明を書く）。
    Removed,
}

impl AlphaOutput {
    /// 透明の能力 `caps` の形式に書くと失うもの（失わなければ None）。中身で決める（形式だけでは決めない）。
    pub fn loss_in(&self, caps: AlphaCaps) -> Option<AlphaLoss> {
        match (caps, self.kind) {
            (AlphaCaps::Full, _) | (_, AlphaKind::None) => None,
            (AlphaCaps::Binary, AlphaKind::Binary) => None,
            (AlphaCaps::Binary, AlphaKind::Partial) => Some(AlphaLoss::Binarized),
            (AlphaCaps::None, _) => Some(AlphaLoss::Removed),
        }
    }
}

/// 透明・半透明・不透明の画素の数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AlphaCounts {
    pub transparent: u64,
    pub partial: u64,
    pub opaque: u64,
}

impl AlphaCounts {
    fn add(self, o: Self) -> Self {
        Self {
            transparent: self.transparent + o.transparent,
            partial: self.partial + o.partial,
            opaque: self.opaque + o.opaque,
        }
    }
}

/// 8・16 ビットのアルファのチャンネルの位置（バイト）と最大。
fn alpha_channel(fmt: PixelFormat) -> Option<(usize, bool)> {
    match fmt {
        PixelFormat::GrayA8 => Some((1, false)),
        PixelFormat::Rgba8 => Some((3, false)),
        PixelFormat::GrayA16 => Some((2, true)),
        PixelFormat::Rgba16 => Some((6, true)),
        _ => None,
    }
}

/// 1 枚の画素の、透明・半透明・不透明の画素の数（タイルごとに並列に数えてタイルの順に足す）。
pub fn count_alpha(px: &Raster, cx: &AnalyzeCx<'_>) -> Result<AlphaCounts, EngineError> {
    let fmt = px.format();
    let all = u64::from(px.width()) * u64::from(px.height());
    if !can_be_transparent(fmt) {
        return Ok(AlphaCounts {
            opaque: all,
            ..AlphaCounts::default()
        });
    }
    let bpp = px.bpp();
    if let Some((at, wide)) = alpha_channel(fmt) {
        let parts = cx.par_blocks(tile_count(px), |i| {
            let (_, tile) = tile_at(px, i);
            // 0 と最大を分岐なしで数え、半透明は残り
            let (mut zero, mut full, mut n) = (0u64, 0u64, 0u64);
            if wide {
                for p in tile.chunks_exact(bpp) {
                    let a = u16::from_le_bytes([p[at], p[at + 1]]);
                    zero += u64::from(a == 0);
                    full += u64::from(a == u16::MAX);
                    n += 1;
                }
            } else {
                for a in tile.iter().skip(at).step_by(bpp) {
                    zero += u64::from(*a == 0);
                    full += u64::from(*a == u8::MAX);
                    n += 1;
                }
            }
            Ok(AlphaCounts {
                transparent: zero,
                partial: n - zero - full,
                opaque: full,
            })
        })?;
        return Ok(parts
            .into_iter()
            .fold(AlphaCounts::default(), AlphaCounts::add));
    }
    // Indexed8：番号ごとの数を数えてから、使われている番号のパレットのアルファで分ける
    let palette = px
        .palette()
        .ok_or_else(|| EngineError::Internal("Indexed8 にパレットがありません".into()))?;
    let parts = cx.par_blocks(tile_count(px), |i| {
        let (_, tile) = tile_at(px, i);
        let mut n = [0u32; 256];
        for b in tile {
            n[usize::from(*b)] += 1;
        }
        Ok(n)
    })?;
    let mut total = [0u64; 256];
    for n in parts {
        for (t, v) in total.iter_mut().zip(n) {
            *t += u64::from(v);
        }
    }
    let mut c = AlphaCounts::default();
    for (i, n) in total.iter().enumerate() {
        if *n == 0 {
            continue;
        }
        let color = palette
            .colors
            .get(i)
            .ok_or_else(|| EngineError::Internal("パレットにない番号の画素があります".into()))?;
        match color[3] {
            0 => c.transparent += n,
            255 => c.opaque += n,
            _ => c.partial += n,
        }
    }
    Ok(c)
}

/// 透明の種類と画素の数。
pub struct Alpha;

impl Analysis for Alpha {
    const ID: &'static str = "image.alpha";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Image, ValueKind::Frames];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = NoParams;
    type Output = AlphaOutput;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<AlphaOutput, EngineError> {
        cx.check()?;
        let (canvas, planes) = planes(v, Self::ID)?;
        let area = u64::from(canvas.w) * u64::from(canvas.h);
        let mut counts = AlphaCounts::default();
        let mut uncovered = 0u64;
        for plane in &planes {
            cx.check()?;
            counts = counts.add(count_alpha(plane.px, cx)?);
            let covered = u64::from(plane.px.width()) * u64::from(plane.px.height());
            uncovered += area.saturating_sub(covered);
        }
        let kind = if counts.partial > 0 {
            AlphaKind::Partial
        } else if counts.transparent > 0 || uncovered > 0 {
            AlphaKind::Binary
        } else {
            AlphaKind::None
        };
        Ok(AlphaOutput {
            kind,
            transparent: counts.transparent,
            partial: counts.partial,
            opaque: counts.opaque,
            uncovered,
        })
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        vec![
            ex(
                "rgba-noise-partial",
                InputGen::Noise {
                    w: 40,
                    h: 30,
                    fmt: PixelFormat::Rgba8,
                    seed: 31,
                },
            ),
            ex(
                "sprite-binary-across-tiles",
                InputGen::Sprite {
                    w: 600,
                    h: 300,
                    rect: IRect::new(250, 40, 300, 200),
                },
            ),
            ex(
                "rgb-none",
                InputGen::Noise {
                    w: 7,
                    h: 5,
                    fmt: PixelFormat::Rgb8,
                    seed: 32,
                },
            ),
            ex(
                "indexed-used-clear-color",
                InputGen::Noise {
                    w: 20,
                    h: 20,
                    fmt: PixelFormat::Indexed8,
                    seed: 33,
                },
            ),
            ex(
                "graya16-deep-alpha",
                InputGen::Noise {
                    w: 33,
                    h: 9,
                    fmt: PixelFormat::GrayA16,
                    seed: 34,
                },
            ),
            ex(
                "checker-rgba16",
                InputGen::Checker {
                    w: 9,
                    h: 9,
                    fmt: PixelFormat::Rgba16,
                    cell: 2,
                },
            ),
            ex(
                "sheet-opaque",
                InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 8,
                },
            ),
            ex(
                "trimmed-frames-uncovered",
                InputGen::TrimmedFrames {
                    w: 40,
                    h: 30,
                    fmt: PixelFormat::Rgba8,
                    count: 4,
                    seed: 35,
                },
            ),
            ex(
                "rgb-trimmed-frames-binary",
                InputGen::TrimmedFrames {
                    w: 16,
                    h: 12,
                    fmt: PixelFormat::Rgb8,
                    count: 3,
                    seed: 36,
                },
            ),
        ]
    }

    fn estimate(_p: &NoParams, input: &Shape) -> Estimate {
        let (pixels, _, _, _) = shape_pixels(input);
        let fmt = match *input {
            Shape::Image { fmt, .. } | Shape::Frames { fmt, .. } => fmt,
            Shape::Audio { .. } => return Estimate::default(),
        };
        Estimate {
            // {"kind":"partial","opaque":…,"partial":…,"transparent":…,"uncovered":…}（数は 20 桁まで）
            bytes: 160,
            // アルファのない形式は画素を見ない
            cost: if can_be_transparent(fmt) { pixels } else { 0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::image::test_util::run;
    use crate::geom::{IPoint, Size};
    use crate::value::{Frame, Frames, Palette};
    use std::sync::Arc;

    fn image(w: u32, h: u32, fmt: PixelFormat, pal: Option<Palette>, data: &[u8]) -> Value {
        Value::Image(Arc::new(crate::value::Image {
            px: Raster::from_rows(w, h, fmt, pal, data).unwrap(),
            meta: Default::default(),
        }))
    }

    /// 値を直接確かめる（ゴールデンに凍結する前に）。
    #[test]
    fn counts_every_kind_of_alpha() {
        // Rgba8：透明 2・半透明 1・不透明 1
        let v = image(
            4,
            1,
            PixelFormat::Rgba8,
            None,
            &[1, 2, 3, 0, 9, 9, 9, 0, 1, 1, 1, 1, 5, 5, 5, 255],
        );
        let out = run::<Alpha>(&NoParams {}, &v);
        assert_eq!(
            out,
            AlphaOutput {
                kind: AlphaKind::Partial,
                transparent: 2,
                partial: 1,
                opaque: 1,
                uncovered: 0,
            }
        );
        // 16 ビットは元の深さで：アルファ 1 は半透明（8 ビットに直すと 0 になる値）、65535 は不透明
        let v = image(
            3,
            1,
            PixelFormat::GrayA16,
            None,
            &[0, 0, 1, 0, 7, 7, 0xff, 0xff, 7, 7, 0, 0],
        );
        let out = run::<Alpha>(&NoParams {}, &v);
        assert_eq!((out.transparent, out.partial, out.opaque), (1, 1, 1));
        // Indexed8：使われていない半透明の色は数えない
        let pal = Palette {
            colors: vec![[0, 0, 0, 0], [1, 1, 1, 255], [2, 2, 2, 128]],
        };
        let v = image(3, 1, PixelFormat::Indexed8, Some(pal), &[0, 1, 1]);
        let out = run::<Alpha>(&NoParams {}, &v);
        assert_eq!(out.kind, AlphaKind::Binary);
        assert_eq!((out.transparent, out.partial, out.opaque), (1, 0, 2));
        // アルファのない形式
        let v = image(2, 2, PixelFormat::Rgb16, None, &[0; 24]);
        let out = run::<Alpha>(&NoParams {}, &v);
        assert_eq!(out.kind, AlphaKind::None);
        assert_eq!(out.opaque, 4);
        // フレームの列：10×8 のキャンバスに 4×3（不透明）と 10×8（不透明）。覆われない部分は 80 − 12
        let px = |w, h| Raster::filled(w, h, PixelFormat::Rgb8, None, &[1, 2, 3]).unwrap();
        let fr = Value::Frames(Arc::new(
            Frames::new(
                vec![
                    Frame {
                        px: px(4, 3),
                        offset: IPoint { x: 2, y: 1 },
                        duration_ms: 10,
                    },
                    Frame::full(px(10, 8), 10),
                ],
                Size { w: 10, h: 8 },
                Default::default(),
                0,
            )
            .unwrap(),
        ));
        let out = run::<Alpha>(&NoParams {}, &fr);
        assert_eq!(
            out,
            AlphaOutput {
                kind: AlphaKind::Binary,
                transparent: 0,
                partial: 0,
                opaque: 92,
                uncovered: 68,
            }
        );
    }

    /// 保存の前の警告：Full は失わない、Binary は半透明だけ、None は透明があれば失う。
    #[test]
    fn losses_follow_the_format_caps() {
        let out = |kind| AlphaOutput {
            kind,
            transparent: 0,
            partial: 0,
            opaque: 0,
            uncovered: 0,
        };
        use AlphaKind::{Binary, None as NoAlpha, Partial};
        for kind in [NoAlpha, Binary, Partial] {
            assert_eq!(out(kind).loss_in(AlphaCaps::Full), None);
        }
        assert_eq!(out(NoAlpha).loss_in(AlphaCaps::Binary), None);
        assert_eq!(out(Binary).loss_in(AlphaCaps::Binary), None);
        assert_eq!(
            out(Partial).loss_in(AlphaCaps::Binary),
            Some(AlphaLoss::Binarized)
        );
        assert_eq!(out(NoAlpha).loss_in(AlphaCaps::None), None);
        assert_eq!(
            out(Binary).loss_in(AlphaCaps::None),
            Some(AlphaLoss::Removed)
        );
        assert_eq!(
            out(Partial).loss_in(AlphaCaps::None),
            Some(AlphaLoss::Removed)
        );
    }
}
