//! `image.pixel_scale` v1：最近傍で整数倍に拡大した絵の倍率と位相（軸ごと）。厳密な定義だけ（許容差つきの推定はしない）。
//!
//! 定義（凍結する。横の軸。縦も同じ）：
//! - **変わり目**の位置 x（1 ≤ x < 幅）：ある行 y で、画素 (x − 1, y) と (x, y) の**見た目**が違う。全行の合併を取る。
//!   見た目が同じ ⟺ 生の値（画素のバイト列）が同じか、両方が透明（アルファ 0。Indexed8 はパレットの色が同じか、
//!   両方のパレットの色のアルファが 0）。画像の端は変わり目にしない（端の欠けたブロックは数えない：切り抜いた絵・
//!   トリムしたスプライトでも崩れない）。
//! - 変わり目の集合を E とすると、|E| ≤ 1 なら null（倍率が決まらない：1 色・横縞だけ・左右 2 色に分けただけ など。
//!   変わり目が 1 本だけの軸も null）。そうでなければ
//!   `k = gcd{ e − min E : e ∈ E }`、`phase = min E mod k`、`lines = |E|`。これは「どのブロックも 1 色になる最大の
//!   格子」と同じ（ブロックが 1 色 ⟺ ブロックの内側に横・縦の変わり目がない）。
//! - `lossless`：**生の値**の変わり目の集合 R（⊇ E）が、横・縦とも見た目の格子の上（`x ≡ phase (mod k)`）にある。
//!   null の軸では R = E。真なら、各ブロックの生の値が 1 つで、最近傍で 1/k にして k 倍に戻すとビット単位で戻る
//!   （位相が 0 でなければ、格子に合わせて切り抜いてから。E26）。
//! - フレームの列は、各フレームの中の隣り合う画素だけを比べ、その変わり目（生の値・見た目の両方）を、フレームの
//!   `offset` を足したキャンバスの座標で合併する。フレームの縁と、その外（キャンバスの覆われない部分）の境目は
//!   変わり目にしない（画像の端と同じ扱い。フレームの外を透明とはみなさない）。フレームどうしも比べない。このため、
//!   トリムしたフレームの列と、同じ見え方の 1 枚の画像（フレームの外を透明で描いたもの）では結果が違いうる（一様な
//!   フレーム 1 つの列は null、画像はフレームの縁が変わり目になる）。
//! - 厳密なので、JPEG で保存し直した絵・平滑に拡大した絵・あとから 1 点を描き足した絵では 1 に落ちる。
//! - 整数だけ（`Numeric::Exact`）。タイルごとに列・行の印を作って論理和で畳む。タイルの左端・上端は隣のタイルの
//!   1 列・1 行と比べる。

use serde::{Deserialize, Serialize};

use crate::analysis::image::{filled_vec, planes, raw_key, shape_pixels, tile_at, tile_count};
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Estimate, NoParams};
use crate::error::EngineError;
use crate::geom::IRect;
use crate::op::Numeric;
use crate::shape::{Shape, ValueKind};
use crate::testgen::InputGen;
use crate::value::{PixelFormat, Raster, Value};

/// 1 つの軸の倍率。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AxisScale {
    /// 倍率（1 以上）。
    pub k: u32,
    /// 格子の線の位置 mod k（左上のブロックが欠けていれば 0 でない）。
    pub phase: u32,
    /// 変わり目のある線の数（2 以上。1 本以下の軸は null）。
    pub lines: u32,
}

/// 結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PixelScaleOutput {
    /// 横の倍率（横の変わり目が 1 つ以下なら null）。
    pub x: Option<AxisScale>,
    /// 縦の倍率。
    pub y: Option<AxisScale>,
    /// 生の値の変わり目もすべて同じ格子の上（最近傍で 1/k → k 倍がビット単位で戻る）。
    pub lossless: bool,
}
crate::json_output!(PixelScaleOutput);

/// 印：見た目が変わる。
const VIS: u8 = 1;
/// 印：生の値が変わる。
const RAW: u8 = 2;

/// 隣り合う画素の比べ方（見た目が同じか）。
enum Look {
    /// 透明を持たない形式：生の値で比べる。
    Raw,
    /// アルファ付きの形式：アルファのバイトの位置と 16 ビットか。両方が透明なら見た目は同じ。
    Alpha { at: usize, wide: bool },
    /// Indexed8：番号 → 見た目の組（同じ色の最初の番号。透明な色はすべて同じ組）。
    Indexed(Vec<u16>),
}

impl Look {
    fn of(px: &Raster) -> Result<Self, EngineError> {
        Ok(match px.format() {
            PixelFormat::GrayA8 => Self::Alpha { at: 1, wide: false },
            PixelFormat::Rgba8 => Self::Alpha { at: 3, wide: false },
            PixelFormat::GrayA16 => Self::Alpha { at: 2, wide: true },
            PixelFormat::Rgba16 => Self::Alpha { at: 6, wide: true },
            PixelFormat::Indexed8 => {
                let pal = px.palette().ok_or_else(|| {
                    EngineError::Internal("Indexed8 にパレットがありません".into())
                })?;
                let mut canon = vec![u16::MAX - 1; 256];
                for (i, c) in pal.colors.iter().enumerate() {
                    let first = pal.colors.iter().position(|d| d == c).unwrap_or(i);
                    if let Some(slot) = canon.get_mut(i) {
                        *slot = if c[3] == 0 { u16::MAX } else { first as u16 };
                    }
                }
                Self::Indexed(canon)
            }
            _ => Self::Raw,
        })
    }

    /// 画素を並べた `src` の、生の値の鍵と見た目の鍵（見た目が同じ ⟺ 見た目の鍵が同じ）。透明な画素の見た目の鍵は 0
    /// （透明でない画素はアルファのバイトが 0 でないので、生の値の鍵が 0 にならない）。Indexed8 の見た目の鍵は組の番号。
    fn keys(&self, src: &[u8], bpp: usize) -> (Vec<u64>, Vec<u64>) {
        let n = src.len() / bpp.max(1);
        let (mut raw, mut vis) = (vec![0u64; n], vec![0u64; n]);
        self.keys_into(src, bpp, &mut raw, &mut vis);
        (raw, vis)
    }

    /// `keys` と同じものを、用意した領域に書く（行ごとに使い回す）。
    fn keys_into(&self, src: &[u8], bpp: usize, raw: &mut [u64], vis: &mut [u64]) {
        for (k, p) in raw.iter_mut().zip(src.chunks_exact(bpp)) {
            *k = raw_key(p);
        }
        match self {
            Self::Raw => vis.copy_from_slice(raw),
            Self::Alpha { at, wide } => {
                for ((v, k), p) in vis.iter_mut().zip(raw.iter()).zip(src.chunks_exact(bpp)) {
                    let clear = p[*at] == 0 && (!*wide || p[*at + 1] == 0);
                    *v = if clear { 0 } else { *k };
                }
            }
            Self::Indexed(canon) => {
                for (v, i) in vis.iter_mut().zip(src) {
                    *v = u64::from(canon[usize::from(*i)]);
                }
            }
        }
    }
}

/// 変わり目の印（生の値が違う・見た目が違う）。
fn mark(raw_differs: bool, vis_differs: bool) -> u8 {
    (u8::from(raw_differs) * RAW) | (u8::from(vis_differs) * VIS)
}

/// 1 枚の画素の変わり目の印（列ごと・行ごと）を、キャンバスの印 `cols`・`rows` に論理和で足す（左上 `at`）。
fn mark_edges(
    px: &Raster,
    at: (i64, i64),
    cols: &mut [u8],
    rows: &mut [u8],
    cx: &AnalyzeCx<'_>,
) -> Result<(), EngineError> {
    let look = Look::of(px)?;
    let bpp = px.bpp();
    let parts = cx.par_blocks(tile_count(px), |i| {
        let (r, tile) = tile_at(px, i);
        let (tw, th) = (r.w as usize, r.h as usize);
        // タイルの左の 1 列・上の 1 行の鍵（画像の端なら比べない）
        let left = if r.x > 0 {
            let mut b = vec![0u8; th * bpp];
            px.read_rect(IRect::new(r.x - 1, r.y, 1, r.h), &mut b)?;
            Some(look.keys(&b, bpp))
        } else {
            None
        };
        // 上の行の鍵（タイルの上端は隣のタイルの行。画像の上端なら無い）と、今の行の鍵（行ごとに使い回す）
        let mut above = if r.y > 0 {
            let mut b = vec![0u8; tw * bpp];
            px.read_rect(IRect::new(r.x, r.y - 1, r.w, 1), &mut b)?;
            Some(look.keys(&b, bpp))
        } else {
            None
        };
        let (mut rr, mut rv) = (vec![0u64; tw], vec![0u64; tw]);
        let mut c = vec![0u8; tw];
        let mut rw = vec![0u8; th];
        for (y, row) in tile.chunks_exact(tw * bpp).enumerate() {
            look.keys_into(row, bpp, &mut rr, &mut rv);
            // 横：左の画素と比べる（タイルの左端は隣のタイルの列と）
            if let (Some((lr, lv)), Some(m)) = (&left, c.first_mut())
                && let (Some(a), Some(b), Some(p), Some(q)) =
                    (lr.get(y), lv.get(y), rr.first(), rv.first())
            {
                *m |= mark(p != a, q != b);
            }
            for ((m, (r1, r0)), (v1, v0)) in c
                .iter_mut()
                .skip(1)
                .zip(rr.iter().skip(1).zip(&rr))
                .zip(rv.iter().skip(1).zip(&rv))
            {
                *m |= mark(r1 != r0, v1 != v0);
            }
            // 縦：上の行と比べる（行ごとにまとめて）
            if let Some((ar, av)) = above.as_mut() {
                if let Some(m) = rw.get_mut(y) {
                    *m |= mark(*ar != rr, *av != rv);
                }
                std::mem::swap(ar, &mut rr);
                std::mem::swap(av, &mut rv);
            } else {
                above = Some((rr.clone(), rv.clone()));
            }
        }
        Ok((r, c, rw))
    })?;
    for (r, c, rw) in parts {
        let at_x = at.0.saturating_add(r.x);
        let at_y = at.1.saturating_add(r.y);
        for (k, v) in c.iter().enumerate() {
            if let Some(slot) = usize::try_from(at_x.saturating_add(k as i64))
                .ok()
                .and_then(|i| cols.get_mut(i))
            {
                *slot |= v;
            }
        }
        for (k, v) in rw.iter().enumerate() {
            if let Some(slot) = usize::try_from(at_y.saturating_add(k as i64))
                .ok()
                .and_then(|i| rows.get_mut(i))
            {
                *slot |= v;
            }
        }
    }
    Ok(())
}

fn gcd(a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// 1 つの軸の印から、倍率と「生の値の変わり目も格子の上にあるか」。
pub(crate) fn axis_scale(marks: &[u8]) -> (Option<AxisScale>, bool) {
    let vis = || {
        marks
            .iter()
            .enumerate()
            .filter(|(_, m)| **m & VIS != 0)
            .map(|(i, _)| i as u32)
    };
    let (mut first, mut g, mut lines) = (None, 0u32, 0u32);
    for e in vis() {
        match first {
            None => first = Some(e),
            Some(f) => g = gcd(g, e - f),
        }
        lines += 1;
    }
    let raw_off_grid = |on: &dyn Fn(u32) -> bool| {
        marks
            .iter()
            .enumerate()
            .any(|(i, m)| *m & RAW != 0 && !on(i as u32))
    };
    match first {
        Some(f) if lines >= 2 && g > 0 => {
            let phase = f % g;
            let lossless = !raw_off_grid(&|i| i % g == phase);
            (Some(AxisScale { k: g, phase, lines }), lossless)
        }
        // 倍率が決まらない軸：生の値の変わり目が見た目の変わり目と同じなら失わない
        _ => (
            None,
            !raw_off_grid(&|i| marks.get(i as usize).is_some_and(|m| m & VIS != 0)),
        ),
    }
}

/// 最近傍の拡大の倍率。
pub struct PixelScale;

impl Analysis for PixelScale {
    const ID: &'static str = "image.pixel_scale";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Image, ValueKind::Frames];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = NoParams;
    type Output = PixelScaleOutput;

    fn run(_p: &NoParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<PixelScaleOutput, EngineError> {
        cx.check()?;
        let (canvas, planes) = planes(v, Self::ID)?;
        let mut cols = filled_vec(canvas.w as usize, 0u8)?;
        let mut rows = filled_vec(canvas.h as usize, 0u8)?;
        for plane in &planes {
            cx.check()?;
            mark_edges(plane.px, (plane.x, plane.y), &mut cols, &mut rows, cx)?;
        }
        let (x, lx) = axis_scale(&cols);
        let (y, ly) = axis_scale(&rows);
        Ok(PixelScaleOutput {
            x,
            y,
            lossless: lx && ly,
        })
    }

    fn examples() -> Vec<AnalysisExample<NoParams>> {
        let ex = |name, input| AnalysisExample {
            name,
            params: NoParams {},
            input,
        };
        let noise = |w, h, fmt, seed| Box::new(InputGen::Noise { w, h, fmt, seed });
        vec![
            ex(
                "checker-cell-4",
                InputGen::Checker {
                    w: 24,
                    h: 16,
                    fmt: PixelFormat::Rgba8,
                    cell: 4,
                },
            ),
            ex(
                "sprite-rect-across-tiles",
                InputGen::Sprite {
                    w: 600,
                    h: 300,
                    rect: IRect::new(250, 40, 300, 200),
                },
            ),
            ex(
                "upscaled-noise-3x2",
                InputGen::Upscale {
                    base: noise(12, 10, PixelFormat::Rgba8, 51),
                    kx: 3,
                    ky: 2,
                },
            ),
            ex(
                "cropped-upscale-with-phase",
                InputGen::Crop {
                    base: Box::new(InputGen::Upscale {
                        base: noise(10, 8, PixelFormat::Rgba8, 52),
                        kx: 4,
                        ky: 4,
                    }),
                    rect: IRect::new(3, 1, 30, 25),
                },
            ),
            ex("noise-is-1", *noise(16, 16, PixelFormat::Rgba8, 53)),
            ex(
                "indexed-upscale-5",
                InputGen::Upscale {
                    base: noise(7, 6, PixelFormat::Indexed8, 54),
                    kx: 5,
                    ky: 5,
                },
            ),
            ex(
                "gray16-checker-3",
                InputGen::Checker {
                    w: 13,
                    h: 11,
                    fmt: PixelFormat::Gray16,
                    cell: 3,
                },
            ),
            ex(
                "all-clear-is-null",
                InputGen::Sprite {
                    w: 9,
                    h: 7,
                    rect: IRect::new(20, 20, 1, 1),
                },
            ),
            ex(
                "upscaled-trimmed-frames",
                InputGen::Upscale {
                    base: Box::new(InputGen::TrimmedFrames {
                        w: 12,
                        h: 8,
                        fmt: PixelFormat::Rgba8,
                        count: 3,
                        seed: 55,
                    }),
                    kx: 2,
                    ky: 2,
                },
            ),
            ex(
                "upscaled-sprite-sheet-across-tiles",
                InputGen::Upscale {
                    base: Box::new(InputGen::SpriteSheet {
                        cols: 5,
                        rows: 3,
                        cell_w: 16,
                        cell_h: 16,
                        gap_x: 2,
                        gap_y: 2,
                        margin_x: 1,
                        margin_y: 1,
                        pad_right: 0,
                        pad_bottom: 0,
                        background: None,
                        full: false,
                        empty: vec![],
                        seed: 56,
                    }),
                    kx: 3,
                    ky: 3,
                },
            ),
            // 片方の軸だけ null：横は 3 倍、縦は変わり目がない（どの行も同じ）
            ex(
                "one-axis-null-stripes",
                InputGen::Upscale {
                    base: noise(8, 1, PixelFormat::Rgba8, 57),
                    kx: 3,
                    ky: 5,
                },
            ),
            // 倍率の決まる軸で、透明な画素どうし（RGB が違う）の生の値の変わり目が格子の外：横は k 2・位相 0 だが
            // x = 3 に生の値だけの変わり目があるので lossless は偽。縦は変わり目が 1 本（y = 2）で null
            ex(
                "clear-rgb-off-grid-is-lossy",
                InputGen::Pixels {
                    w: 6,
                    h: 3,
                    colors: vec![
                        [10, 20, 30, 255],
                        [1, 2, 3, 0],
                        [7, 8, 9, 0],
                        [40, 50, 60, 255],
                        [70, 80, 90, 255],
                        [100, 110, 120, 255],
                    ],
                    index: vec![
                        0, 0, 1, 2, 3, 3, //
                        0, 0, 1, 2, 3, 3, //
                        4, 4, 4, 4, 5, 5,
                    ],
                },
            ),
            // null の軸で、生の値の変わり目が見た目の変わり目と違う：横は見た目の変わり目が 1 本（x = 2）で null、
            // 生の値は x = 1 でも変わるので lossless は偽。縦は 2 倍（y = 2・4）
            ex(
                "clear-rgb-on-a-null-axis-is-lossy",
                InputGen::Upscale {
                    base: Box::new(InputGen::Pixels {
                        w: 4,
                        h: 3,
                        colors: vec![
                            [1, 2, 3, 0],
                            [7, 8, 9, 0],
                            [40, 50, 60, 255],
                            [9, 9, 9, 255],
                        ],
                        index: vec![
                            0, 1, 2, 2, //
                            0, 1, 3, 3, //
                            0, 1, 2, 2,
                        ],
                    }),
                    kx: 1,
                    ky: 2,
                },
            ),
        ]
    }

    fn estimate(_p: &NoParams, input: &Shape) -> Estimate {
        let (pixels, _, _, _) = shape_pixels(input);
        Estimate {
            // {"lossless":false,"x":{"k":…,"lines":…,"phase":…},"y":{…}}（数は 10 桁まで）
            bytes: 160,
            cost: pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::image::test_util::run;
    use crate::value::Palette;
    use std::sync::Arc;

    fn image(w: u32, h: u32, fmt: PixelFormat, pal: Option<Palette>, data: &[u8]) -> Value {
        Value::Image(Arc::new(crate::value::Image {
            px: Raster::from_rows(w, h, fmt, pal, data).unwrap(),
            meta: Default::default(),
        }))
    }

    #[test]
    fn axis_from_marks() {
        // 変わり目 4・8 と 6・10（レンガ積み）の合併は 2 の格子
        let mut m = vec![0u8; 12];
        for i in [4, 6, 8, 10] {
            m[i] = VIS | RAW;
        }
        assert_eq!(
            axis_scale(&m),
            (
                Some(AxisScale {
                    k: 2,
                    phase: 0,
                    lines: 4
                }),
                true
            )
        );
        // 位相：3・7・11 は k 4・位相 3。生の値だけの変わり目が 5 にあれば失う
        let mut m = vec![0u8; 12];
        for i in [3, 7, 11] {
            m[i] = VIS | RAW;
        }
        assert_eq!(axis_scale(&m).0.map(|a| (a.k, a.phase)), Some((4, 3)));
        m[5] = RAW;
        assert!(!axis_scale(&m).1);
        // 1 つ以下は null。生の値の変わり目が見た目と同じなら失わない
        let mut m = vec![0u8; 5];
        m[2] = VIS | RAW;
        assert_eq!(axis_scale(&m), (None, true));
        m[3] = RAW;
        assert_eq!(axis_scale(&m), (None, false));
        assert_eq!(axis_scale(&[]), (None, true));
    }

    /// 値を直接確かめる（ゴールデンに凍結する前に）。
    #[test]
    fn scales_of_small_images() {
        // 2×2 のブロックの 4×2（横の変わり目は 2 だけ → null、縦は無い → null）
        let v = image(4, 2, PixelFormat::Gray8, None, &[1, 1, 2, 2, 1, 1, 2, 2]);
        let out = run::<PixelScale>(&NoParams {}, &v);
        assert_eq!((out.x, out.y, out.lossless), (None, None, true));
        // 6 幅で 2 ずつ（変わり目 2・4）→ k 2
        let v = image(6, 1, PixelFormat::Gray8, None, &[1, 1, 2, 2, 3, 3]);
        let out = run::<PixelScale>(&NoParams {}, &v);
        assert_eq!(
            out.x,
            Some(AxisScale {
                k: 2,
                phase: 0,
                lines: 2
            })
        );
        // 透明な画素の RGB が違っても見た目は同じ（生の値は違うので lossless は偽）
        let v = image(
            6,
            1,
            PixelFormat::Rgba8,
            None,
            &[
                1, 1, 1, 255, 1, 1, 1, 255, 0, 0, 0, 0, 9, 9, 9, 0, 5, 5, 5, 255, 5, 5, 5, 255,
            ],
        );
        let out = run::<PixelScale>(&NoParams {}, &v);
        assert_eq!(out.x.map(|a| (a.k, a.phase)), Some((2, 0)));
        assert!(!out.lossless);
        // Indexed8：同じ色の番号は見た目が同じ、透明な色どうしも同じ
        let pal = Palette {
            colors: vec![
                [1, 1, 1, 255],
                [1, 1, 1, 255],
                [0, 0, 0, 0],
                [7, 0, 0, 0],
                [2, 2, 2, 255],
            ],
        };
        let v = image(6, 1, PixelFormat::Indexed8, Some(pal), &[0, 1, 2, 3, 4, 4]);
        let out = run::<PixelScale>(&NoParams {}, &v);
        assert_eq!(out.x.map(|a| (a.k, a.phase)), Some((2, 0)));
        assert!(!out.lossless);
    }

    /// 例の値を直接確かめる（ゴールデンに凍結する前に）：片方の軸だけ null、lossless が偽になる 2 つの枝
    /// （倍率の決まる軸で格子の外・null の軸で見た目と違う）。
    #[test]
    fn null_axes_and_lossy_examples() {
        let ex = |name: &str| {
            let e = PixelScale::examples()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap();
            run::<PixelScale>(&NoParams {}, &e.input.generate().unwrap())
        };
        let axis = |k, phase, lines| Some(AxisScale { k, phase, lines });
        let out = ex("one-axis-null-stripes");
        assert_eq!((out.x, out.y, out.lossless), (axis(3, 0, 7), None, true));
        let out = ex("clear-rgb-off-grid-is-lossy");
        assert_eq!((out.x, out.y, out.lossless), (axis(2, 0, 2), None, false));
        let out = ex("clear-rgb-on-a-null-axis-is-lossy");
        assert_eq!((out.x, out.y, out.lossless), (None, axis(2, 0, 2), false));
    }

    /// フレームの列は各フレームの中だけで比べる：フレームの縁とフレームの外（キャンバスの覆われない部分）の境目は
    /// 変わり目にしない。同じ見え方の 1 枚の画像（外を透明で描いたもの）とは結果が違う。
    #[test]
    fn frame_edges_are_not_compared_with_the_outside() {
        use crate::geom::{IPoint, Size};
        use crate::value::{Frame, Frames};
        // 10×6 のキャンバスの (1, 1) に 4×4 の不透明な赤
        let red = Raster::filled(4, 4, PixelFormat::Rgba8, None, &[255, 0, 0, 255]).unwrap();
        let frames = Value::Frames(Arc::new(
            Frames::new(
                vec![Frame {
                    px: red,
                    offset: IPoint { x: 1, y: 1 },
                    duration_ms: 10,
                }],
                Size { w: 10, h: 6 },
                Default::default(),
                0,
            )
            .unwrap(),
        ));
        let out = run::<PixelScale>(&NoParams {}, &frames);
        assert_eq!((out.x, out.y, out.lossless), (None, None, true));
        let mut data = vec![0u8; 10 * 6 * 4];
        for y in 1..5 {
            for x in 1..5 {
                data[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let flat = image(10, 6, PixelFormat::Rgba8, None, &data);
        let out = run::<PixelScale>(&NoParams {}, &flat);
        assert_eq!(out.x.map(|a| (a.k, a.phase, a.lines)), Some((4, 1, 2)));
        assert_eq!(out.y.map(|a| (a.k, a.phase, a.lines)), Some((4, 1, 2)));
    }
}
