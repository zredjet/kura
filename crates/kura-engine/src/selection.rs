//! 選択の値（N → 1。M14 段 6。docs/editing.md §4.5、ADR-197）：一括で選んだ N 件の画像（とフレームの列）を、選んだ順に 1 つの
//! フレームの列にする。スプライトシート（`sprite.pack_grid`）の入力（`InputRole::Selection`）になる。
//!
//! - 1 枚の画像はフレーム 1 つ（表示時間 [`STILL_MS`]）、フレームの列（アニメ）はすべてのフレームを順に（利用者の決定 3）。
//! - キャンバスは入力のキャンバスの幅・高さの最大。入力ごとのキャンバスを 9 点の揃え（[`Anchor`]）で置き、フレームはその中の
//!   位置のまま（割り切れない余りは左・上へ寄せる。`image.fit_canvas` と同じ ⌊余り·係数/2⌋。利用者の決定 2）。
//! - 形式は、すべて同じで Indexed8 でなければそのまま、ほかは 16 ビットの入力があれば Rgba16、なければ Rgba8（パレットは
//!   入力ごとに違いうるので、推論でも描画でも同じ決まりにする。値は変えずに広げるだけ）。シートの `fill` がその形式で表せない
//!   （グレーに灰色でない色、アルファのない形式に不透明でない色）なら、同じく Rgba8・Rgba16 に広げる（ADR-198）。
//! - メタデータは ICC だけを運ぶ：すべての入力の ICC が同じ（バイト列が一致）ならそれを持ち、違う・一部にしかないなら持たず、
//!   sRGB でない ICC があれば警告（`ColorSpaceAssumed`。色の変換はしない。§5.2 の v1 の決まり。利用者の決定）。格子・ピボットは
//!   捨てる（シートの格子は `sprite.pack_grid` が書く）。繰り返しは無限（0）。
//! - 乱数の元（`InputValue::source`）は、選んだ順の中身のハッシュの列のダイジェスト（[`source`]）。
//!
//! 決定性：入力の値と並びと揃えだけで決まる（並列は形式の変換の `convert_raster` の中だけ）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::digest::{Digest, Hasher};
use crate::doc::{FormatChoice, Graph, InputRole};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::{Anchor, IPoint, Size};
use crate::monitor::Monitor;
use crate::ops::image::convert::convert_raster;
use crate::shape::{Dim, Fullness, Shape};
use crate::value::{Frame, Frames, ImageMeta, PixelFormat, Value};

/// 1 枚の画像をフレームにするときの表示時間（ミリ秒）。
pub const STILL_MS: u32 = 100;

/// 乱数の元のダイジェストのドメイン（凍結する）。
const SOURCE_DOMAIN: &str = "kura-selection-v1";

/// シートの間隔の上限（`sprite.pack_grid` の引数と同じ）。
pub const MAX_GAP: u32 = 4_096;
/// 1 行のセルの数の上限（`sprite.pack_grid` の引数と同じ）。
pub const MAX_COLUMNS: u32 = 4_096;

fn default_anchor() -> Anchor {
    Anchor::Center
}

/// スプライトシートの引数（[`sheet_graph`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SheetParams {
    /// 1 行のセルの数（None は ⌈√フレームの数⌉。[`columns_for`]）。
    #[serde(default)]
    pub columns: Option<u32>,
    #[serde(default)]
    pub gap_x: u32,
    #[serde(default)]
    pub gap_y: u32,
    /// 何も置かない部分の色（ストレートの RGBA8）。null は透明。
    #[serde(default)]
    pub fill: Option<[u8; 4]>,
    /// 大きさの違う絵の、セルの中の揃え（既定は中央）。
    #[serde(default = "default_anchor")]
    pub anchor: Anchor,
}

impl SheetParams {
    /// 選択によらない引数の確かめ（間隔は 0〜[`MAX_GAP`]、1 行の数は 1〜[`MAX_COLUMNS`]）。積む前・事前チェックの前に呼ぶ。
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.gap_x > MAX_GAP || self.gap_y > MAX_GAP {
            return Err(invalid(format!(
                "シートの間隔は 0〜{MAX_GAP} です（{}・{}）",
                self.gap_x, self.gap_y
            )));
        }
        if let Some(c) = self.columns
            && !(1..=MAX_COLUMNS).contains(&c)
        {
            return Err(invalid(format!(
                "1 行のセルの数は 1〜{MAX_COLUMNS} です（{c}）"
            )));
        }
        Ok(())
    }
}

impl Default for SheetParams {
    fn default() -> Self {
        Self {
            columns: None,
            gap_x: 0,
            gap_y: 0,
            fill: None,
            anchor: Anchor::Center,
        }
    }
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadInput, message)))
}

/// 選択のフレームの形式（モジュールの説明）。`fill` は何も置かない部分の色（その形式で表せなければ広げる）。
pub fn common_format(
    formats: impl IntoIterator<Item = PixelFormat>,
    fill: Option<[u8; 4]>,
) -> PixelFormat {
    let mut first: Option<PixelFormat> = None;
    let mut same = true;
    let mut wide = false;
    for f in formats {
        wide |= matches!(
            f,
            PixelFormat::Gray16 | PixelFormat::GrayA16 | PixelFormat::Rgb16 | PixelFormat::Rgba16
        );
        match first {
            None => first = Some(f),
            Some(p) if p != f => same = false,
            Some(_) => {}
        }
    }
    let fits = |f: PixelFormat| match fill {
        None => true,
        Some([r, g, b, a]) => {
            (f.has_alpha() || a == 255)
                && (!crate::imaging::pixels::is_gray(f) || (r == g && g == b))
        }
    };
    match first {
        Some(f) if same && f != PixelFormat::Indexed8 && fits(f) => f,
        _ if wide => PixelFormat::Rgba16,
        _ => PixelFormat::Rgba8,
    }
}

/// 大きさの最大（どれかが上限しか分からなければ上限）。
fn dim_max(dims: impl IntoIterator<Item = Dim>) -> Dim {
    let mut exact = true;
    let mut most = 0u64;
    for d in dims {
        exact &= matches!(d, Dim::Exact(_));
        most = most.max(d.max());
    }
    if exact {
        Dim::Exact(most)
    } else {
        Dim::AtMost(most)
    }
}

/// 選択の形：入力の形（選んだ順）とシートの引数 `p`（`fill` が形式に効く）から、フレームの列の形を推論する（描かない）。画像と
/// フレームの列だけで、空・音声は断る。
pub fn shape(inputs: &[Shape], p: &SheetParams) -> Result<Shape, EngineError> {
    if inputs.is_empty() {
        return Err(invalid("選択が空です"));
    }
    let mut count_exact = true;
    let mut count = 0u64;
    let mut ws = Vec::with_capacity(inputs.len());
    let mut hs = Vec::with_capacity(inputs.len());
    let mut fmts = Vec::with_capacity(inputs.len());
    let mut any_unknown = false;
    let mut any_trimmed = false;
    for s in inputs {
        match *s {
            Shape::Image { w, h, fmt } => {
                count = count.saturating_add(1);
                ws.push(w);
                hs.push(h);
                fmts.push(fmt);
            }
            Shape::Frames {
                count: n,
                w,
                h,
                fmt,
                full,
            } => {
                count_exact &= matches!(n, Dim::Exact(_));
                count = count.saturating_add(n.max());
                ws.push(w);
                hs.push(h);
                fmts.push(fmt);
                any_unknown |= full == Fullness::Unknown;
                any_trimmed |= full == Fullness::Trimmed;
            }
            Shape::Audio { .. } => {
                return Err(invalid(
                    "音声はスプライトシートにできません（画像とアニメだけ）",
                ));
            }
        }
    }
    let (w, h) = (dim_max(ws.iter().copied()), dim_max(hs.iter().copied()));
    let sizes_known = ws
        .iter()
        .chain(hs.iter())
        .all(|d| matches!(d, Dim::Exact(_)));
    let same_size = sizes_known
        && ws.iter().all(|d| d.max() == w.max())
        && hs.iter().all(|d| d.max() == h.max());
    let full = if any_unknown || !sizes_known {
        Fullness::Unknown
    } else if any_trimmed || !same_size {
        Fullness::Trimmed
    } else {
        Fullness::Full
    };
    Ok(Shape::Frames {
        count: if count_exact {
            Dim::Exact(count)
        } else {
            Dim::AtMost(count)
        },
        w,
        h,
        fmt: common_format(fmts, p.fill),
        full,
    })
}

/// 入力 1 つのキャンバスとフレーム。
fn parts(v: &Value) -> Result<(Size, Vec<Frame>), EngineError> {
    match v {
        Value::Image(img) => Ok((
            Size {
                w: img.px.width(),
                h: img.px.height(),
            },
            vec![Frame::full(img.px.clone(), STILL_MS)],
        )),
        Value::Frames(fr) => Ok((fr.canvas(), fr.frames().to_vec())),
        Value::Audio(_) => Err(invalid(
            "音声はスプライトシートにできません（画像とアニメだけ）",
        )),
    }
}

/// 選択の値と警告（[`frames`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    pub frames: Frames,
    /// ICC の違う入力を sRGB とみなしたなど。
    pub warnings: Vec<Issue>,
}

/// 入力のメタデータ。
fn meta_of(v: &Value) -> Option<&ImageMeta> {
    match v {
        Value::Image(img) => Some(&img.meta),
        Value::Frames(fr) => Some(&fr.meta),
        Value::Audio(_) => None,
    }
}

/// 選択の値：入力の値（選んだ順）を、シートの引数 `p`（揃えと `fill`）で 1 つのフレームの列にする（モジュールの説明）。
pub fn frames(
    inputs: &[Value],
    p: &SheetParams,
    mon: &dyn Monitor,
) -> Result<Selected, EngineError> {
    let anchor = p.anchor;
    if inputs.is_empty() {
        return Err(invalid("選択が空です"));
    }
    let parts: Vec<(Size, Vec<Frame>)> = inputs.iter().map(parts).collect::<Result<_, _>>()?;
    let canvas = Size {
        w: parts.iter().map(|(s, _)| s.w).max().unwrap_or(0),
        h: parts.iter().map(|(s, _)| s.h).max().unwrap_or(0),
    };
    let fmt = common_format(
        parts
            .iter()
            .flat_map(|(_, fs)| fs.iter().map(|f| f.px.format())),
        p.fill,
    );
    let (fx, fy) = anchor.factors();
    let mut out = Vec::new();
    out.try_reserve_exact(parts.iter().map(|(_, fs)| fs.len()).sum())
        .map_err(|_| EngineError::Limit("フレームの数が多すぎます".into()))?;
    for (size, fs) in parts {
        // 入力のキャンバスを揃えの位置に置く（⌊余り·係数/2⌋。余りは 0 以上）
        let dx = i64::from(canvas.w - size.w) * fx / 2;
        let dy = i64::from(canvas.h - size.h) * fy / 2;
        for f in fs {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let px = if f.px.format() == fmt {
                f.px
            } else {
                convert_raster(&f.px, fmt, [0, 0, 0], mon)?
            };
            let offset = IPoint {
                x: dx
                    .checked_add(f.offset.x)
                    .ok_or_else(|| EngineError::Limit("フレームの位置が範囲の外です".into()))?,
                y: dy
                    .checked_add(f.offset.y)
                    .ok_or_else(|| EngineError::Limit("フレームの位置が範囲の外です".into()))?,
            };
            out.push(Frame {
                px,
                offset,
                duration_ms: f.duration_ms,
            });
        }
    }
    // ICC：すべて同じなら運び、違えば持たない（sRGB でないものがあれば警告）
    let iccs: Vec<Option<&std::sync::Arc<Vec<u8>>>> = inputs
        .iter()
        .map(|v| meta_of(v).and_then(|m| m.icc.as_ref()))
        .collect();
    let same_icc = iccs
        .windows(2)
        .all(|w| w[0].map(|a| a.as_slice()) == w[1].map(|b| b.as_slice()));
    let mut warnings = Vec::new();
    let icc = if same_icc {
        iccs.first().copied().flatten().cloned()
    } else {
        if iccs
            .iter()
            .flatten()
            .any(|icc| !crate::imaging::icc::is_srgb(icc))
        {
            warnings.push(Issue::warning(
                IssueCode::ColorSpaceAssumed,
                "色空間（ICC）の違う絵を並べたので、色を変えずに sRGB とみなしました",
            ));
        }
        None
    };
    let meta = ImageMeta {
        icc,
        ..ImageMeta::default()
    };
    Ok(Selected {
        frames: Frames::new(out, canvas, meta, 0)?,
        warnings,
    })
}

/// 乱数の元：選んだ順の中身のハッシュの列のダイジェスト（並びも数も効く）。
pub fn source(hashes: &[[u8; 32]]) -> Digest {
    let mut h = Hasher::new(SOURCE_DOMAIN);
    h.u64(hashes.len() as u64);
    for hash in hashes {
        h.raw(hash);
    }
    h.finish()
}

/// 1 行のセルの数（`sprite.pack_grid` の引数）：指定があればそれ（1〜4,096）、なければ ⌈√フレームの数⌉（整数で求める）。
/// 実際に 1 行に並ぶ数は [`columns_in_row`]。
pub fn columns_for(count: u64, columns: Option<u32>) -> u32 {
    if let Some(c) = columns {
        return c.clamp(1, MAX_COLUMNS);
    }
    let mut c = 1u64;
    while c.saturating_mul(c) < count {
        c += 1;
    }
    u32::try_from(c.min(u64::from(MAX_COLUMNS))).unwrap_or(MAX_COLUMNS)
}

/// 実際に 1 行に並ぶセルの数（`sprite.pack_grid` は min(1 行の数, フレームの数) 列に並べる）。
pub fn columns_in_row(count: u64, columns: Option<u32>) -> u32 {
    let c = columns_for(count, columns);
    u32::try_from(u64::from(c).min(count.max(1))).unwrap_or(c)
}

/// スプライトシートの文書：選択（入力 0、`Selection`）→ `sprite.pack_grid`（1 行のセルの数は [`columns_for`]）→ PNG。`count` は
/// 選択のフレームの数。
pub fn sheet_graph(p: &SheetParams, count: u64) -> Result<Graph, EngineError> {
    let mut g = Graph::new_single();
    if let Some(slot) = g.inputs.first_mut() {
        slot.role = InputRole::Selection;
    }
    g.push_linear(
        "sprite.pack_grid",
        1,
        json!({
            "columns": columns_for(count, p.columns),
            "gapX": p.gap_x,
            "gapY": p.gap_y,
            "fill": p.fill,
        }),
        0,
    )?;
    if let Some(out) = g.outputs.first_mut() {
        out.format = FormatChoice::To {
            format: "png".into(),
            options: json!({}),
        };
    }
    Ok(g)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::doc::InputId;
    use crate::monitor::NoMonitor;
    use crate::render::{Engine, InputValue};
    use crate::testgen::InputGen;
    use crate::value::{Image, Raster};

    fn sprite(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Value {
        InputGen::PixelSprite { w, h, fmt, seed }
            .generate()
            .unwrap()
    }

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Value {
        let px = Raster::filled(w, h, PixelFormat::Rgba8, None, &rgba).unwrap();
        Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta::default(),
        }))
    }

    /// 形式の決まり：すべて同じで Indexed8 でなければそのまま、ほかは 16 ビットがあれば Rgba16、なければ Rgba8。
    #[test]
    fn the_common_format_widens_only_when_needed() {
        use PixelFormat::*;
        assert_eq!(common_format([Rgb8, Rgb8], None), Rgb8);
        assert_eq!(common_format([Rgba16], None), Rgba16);
        assert_eq!(common_format([Indexed8], None), Rgba8);
        assert_eq!(common_format([Indexed8, Indexed8], None), Rgba8);
        assert_eq!(common_format([Rgb8, Rgba8], None), Rgba8);
        assert_eq!(common_format([Gray8, Rgb16], None), Rgba16);
        // fill がその形式で表せなければ広げる（グレーに色、アルファのない形式に透明）
        assert_eq!(common_format([Rgb8], Some([1, 2, 3, 255])), Rgb8);
        assert_eq!(common_format([Rgb8], Some([0, 0, 0, 0])), Rgba8);
        assert_eq!(common_format([Gray8], Some([7, 7, 7, 255])), Gray8);
        assert_eq!(common_format([Gray8], Some([255, 0, 0, 255])), Rgba8);
        assert_eq!(common_format([Rgb16], Some([0, 0, 0, 128])), Rgba16);
        assert_eq!(common_format([Rgba8], Some([0, 0, 0, 0])), Rgba8);
    }

    /// 揃え：大きさの違う絵は、最大のキャンバスの中で 9 点の位置に置く（余りは左・上へ）。形の推論は描いた値と合う。
    #[test]
    fn frames_are_anchored_in_the_largest_canvas() {
        let a = solid(4, 4, [255, 0, 0, 255]);
        let b = solid(1, 3, [0, 255, 0, 255]);
        for (anchor, want) in [
            (Anchor::TopLeft, (0, 0)),
            (Anchor::Center, (1, 0)),
            (Anchor::BottomRight, (3, 1)),
            (Anchor::Bottom, (1, 1)),
        ] {
            let p = SheetParams {
                anchor,
                ..SheetParams::default()
            };
            let fr = frames(&[a.clone(), b.clone()], &p, &NoMonitor)
                .unwrap()
                .frames;
            assert_eq!(fr.canvas(), Size { w: 4, h: 4 });
            assert_eq!(fr.frames().len(), 2);
            assert_eq!(fr.frames()[0].offset, IPoint { x: 0, y: 0 });
            assert_eq!(
                (fr.frames()[1].offset.x, fr.frames()[1].offset.y),
                want,
                "{anchor:?}"
            );
            let inferred = shape(&[a.shape(), b.shape()], &p).unwrap();
            assert!(inferred.admits(&Value::Frames(Arc::new(fr)).shape()));
            assert!(matches!(
                inferred,
                Shape::Frames {
                    full: Fullness::Trimmed,
                    ..
                }
            ));
        }
    }

    /// アニメ（フレームの列）はすべてのフレームを順に、元の位置と表示時間のまま運ぶ。同じ大きさの画像だけなら Full。
    #[test]
    fn animations_keep_all_frames_in_order() {
        let still = sprite(8, 8, PixelFormat::Rgba8, 1);
        let anim = Value::Frames(Arc::new(
            Frames::full(
                vec![
                    (
                        Raster::filled(8, 8, PixelFormat::Rgba8, None, &[1, 2, 3, 255]).unwrap(),
                        40,
                    ),
                    (
                        Raster::filled(8, 8, PixelFormat::Rgba8, None, &[4, 5, 6, 255]).unwrap(),
                        60,
                    ),
                ],
                ImageMeta::default(),
                0,
            )
            .unwrap(),
        ));
        let p = SheetParams::default();
        let fr = frames(&[anim.clone(), still.clone()], &p, &NoMonitor)
            .unwrap()
            .frames;
        let ms: Vec<u32> = fr.frames().iter().map(|f| f.duration_ms).collect();
        assert_eq!(ms, vec![40, 60, STILL_MS]);
        assert_eq!(fr.frames()[0].px.pixel(0, 0), Some(&[1, 2, 3, 255][..]));
        assert!(fr.is_full());
        let inferred = shape(&[anim.shape(), still.shape()], &p).unwrap();
        assert_eq!(
            inferred,
            Shape::Frames {
                count: Dim::Exact(3),
                w: Dim::Exact(8),
                h: Dim::Exact(8),
                fmt: PixelFormat::Rgba8,
                full: Fullness::Full,
            }
        );
    }

    /// パレットの絵は Rgba8 に広げる（値は変えない）。音声・空は断る。
    #[test]
    fn indexed_inputs_widen_and_audio_is_refused() {
        let pal = sprite(6, 5, PixelFormat::Indexed8, 3);
        let p = SheetParams::default();
        let fr = frames(std::slice::from_ref(&pal), &p, &NoMonitor)
            .unwrap()
            .frames;
        assert_eq!(fr.first().px.format(), PixelFormat::Rgba8);
        let Value::Image(img) = &pal else { panic!() };
        let palette = img.px.palette().unwrap();
        let idx = img.px.pixel(2, 2).unwrap()[0];
        assert_eq!(
            fr.first().px.pixel(2, 2),
            Some(&palette.colors[usize::from(idx)][..])
        );
        assert!(frames(&[], &p, &NoMonitor).is_err());
        let audio = InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: 10,
            seed: 1,
        }
        .generate()
        .unwrap();
        assert!(frames(std::slice::from_ref(&audio), &p, &NoMonitor).is_err());
        assert!(shape(&[audio.shape()], &p).is_err());
        assert!(shape(&[], &p).is_err());
    }

    /// 乱数の元は並びと数で変わる。1 行のセルの数は ⌈√n⌉ か指定（1〜4,096）。
    #[test]
    fn the_source_and_columns_are_fixed() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert_ne!(source(&[a, b]), source(&[b, a]));
        assert_ne!(source(&[a]), source(&[a, a]));
        assert_eq!(source(&[a, b]), source(&[a, b]));
        assert_eq!(
            [1, 2, 4, 5, 9, 10, 100].map(|n| columns_for(n, None)),
            [1, 2, 2, 3, 3, 4, 10]
        );
        assert_eq!(columns_for(0, None), 1);
        assert_eq!(columns_for(7, Some(0)), 1);
        assert_eq!(columns_for(7, Some(9_999)), 4_096);
    }

    /// シートの文書は、選択のフレームの列から格子のシートを描く（セル＝キャンバス、左から順に）。
    #[test]
    fn the_sheet_graph_packs_the_selection() {
        let a = solid(4, 4, [255, 0, 0, 255]);
        let b = solid(2, 2, [0, 0, 255, 255]);
        let fr = Value::Frames(Arc::new(
            frames(
                &[a.clone(), b.clone()],
                &SheetParams {
                    anchor: Anchor::TopLeft,
                    ..SheetParams::default()
                },
                &NoMonitor,
            )
            .unwrap()
            .frames,
        ));
        let g = sheet_graph(
            &SheetParams {
                gap_x: 1,
                ..SheetParams::default()
            },
            2,
        )
        .unwrap();
        assert_eq!(g.inputs[0].role, InputRole::Selection);
        let engine = Engine::new().unwrap();
        let shapes = BTreeMap::from([(InputId(0), fr.shape())]);
        let plan = engine.compile(&g, &shapes).unwrap();
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value: fr,
                source: source(&[[1; 32], [2; 32]]),
            },
        )]);
        let out = engine.render(&plan, &inputs, &NoMonitor).unwrap();
        let Value::Image(sheet) = &out.outputs[0].value else {
            panic!()
        };
        assert_eq!((sheet.px.width(), sheet.px.height()), (9, 4));
        assert_eq!(sheet.px.pixel(0, 0), Some(&[255, 0, 0, 255][..]));
        assert_eq!(sheet.px.pixel(5, 0), Some(&[0, 0, 255, 255][..]));
        assert_eq!(sheet.px.pixel(4, 0), Some(&[0, 0, 0, 0][..]), "間隔は透明");
        assert_eq!(
            sheet.px.pixel(5, 3),
            Some(&[0, 0, 0, 0][..]),
            "小さい絵の外は透明"
        );
        let grid = sheet.meta.grid.unwrap();
        assert_eq!((grid.cell_w, grid.cell_h, grid.gap_x), (4, 4, 1));
    }

    fn with_icc(v: &Value, icc: Option<Vec<u8>>) -> Value {
        let Value::Image(img) = v else { panic!() };
        Value::Image(Arc::new(Image {
            px: img.px.clone(),
            meta: ImageMeta {
                icc: icc.map(Arc::new),
                ..ImageMeta::default()
            },
        }))
    }

    /// ICC：すべて同じなら運び、違えば持たない（sRGB でないものがあれば警告）。
    #[test]
    fn the_icc_is_carried_only_when_all_inputs_agree() {
        let a = solid(2, 2, [1, 2, 3, 255]);
        let p3 = vec![7u8; 64];
        let p = SheetParams::default();
        let same = frames(
            &[
                with_icc(&a, Some(p3.clone())),
                with_icc(&a, Some(p3.clone())),
            ],
            &p,
            &NoMonitor,
        )
        .unwrap();
        assert_eq!(same.frames.meta.icc.as_deref(), Some(&p3));
        assert!(same.warnings.is_empty());
        let mixed = frames(
            &[with_icc(&a, Some(p3.clone())), with_icc(&a, None)],
            &p,
            &NoMonitor,
        )
        .unwrap();
        assert_eq!(mixed.frames.meta.icc, None);
        assert_eq!(mixed.warnings.len(), 1);
        assert_eq!(mixed.warnings[0].code, IssueCode::ColorSpaceAssumed);
        let none = frames(&[a.clone(), a.clone()], &p, &NoMonitor).unwrap();
        assert_eq!(none.frames.meta.icc, None);
        assert!(none.warnings.is_empty());
    }

    /// fill が選択の形式で表せないとき（アルファのない絵に透明・グレーの絵に色）は広げて、シートを描ける。
    #[test]
    fn a_fill_widens_the_selection_format() {
        let rgb = Value::Image(Arc::new(Image {
            px: Raster::filled(3, 2, PixelFormat::Rgb8, None, &[9, 9, 9]).unwrap(),
            meta: ImageMeta::default(),
        }));
        let small = Value::Image(Arc::new(Image {
            px: Raster::filled(1, 1, PixelFormat::Rgb8, None, &[1, 1, 1]).unwrap(),
            meta: ImageMeta::default(),
        }));
        let p = SheetParams {
            fill: Some([0, 0, 0, 0]),
            ..SheetParams::default()
        };
        let sel = frames(&[rgb.clone(), small.clone()], &p, &NoMonitor).unwrap();
        assert_eq!(sel.frames.first().px.format(), PixelFormat::Rgba8);
        let inferred = shape(&[rgb.shape(), small.shape()], &p).unwrap();
        let value = Value::Frames(Arc::new(sel.frames));
        assert!(inferred.admits(&value.shape()));
        let g = sheet_graph(&p, 2).unwrap();
        let engine = Engine::new().unwrap();
        let plan = engine
            .compile(&g, &BTreeMap::from([(InputId(0), value.shape())]))
            .unwrap();
        let out = engine
            .render(
                &plan,
                &BTreeMap::from([(
                    InputId(0),
                    InputValue {
                        value,
                        source: source(&[[1; 32], [2; 32]]),
                    },
                )]),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(sheet) = &out.outputs[0].value else {
            panic!()
        };
        assert_eq!(
            sheet.px.pixel(4, 1),
            Some(&[0, 0, 0, 0][..]),
            "小さい絵の外は透明"
        );
    }

    /// 引数の確かめ（間隔・1 行の数）と、実際に 1 行に並ぶ数（フレームの数で切る）。
    #[test]
    fn params_are_validated_and_rows_are_clipped() {
        assert!(SheetParams::default().validate().is_ok());
        for bad in [
            SheetParams {
                gap_x: MAX_GAP + 1,
                ..SheetParams::default()
            },
            SheetParams {
                gap_y: MAX_GAP + 1,
                ..SheetParams::default()
            },
            SheetParams {
                columns: Some(0),
                ..SheetParams::default()
            },
            SheetParams {
                columns: Some(MAX_COLUMNS + 1),
                ..SheetParams::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert_eq!(columns_in_row(3, Some(8)), 3);
        assert_eq!(columns_in_row(10, Some(8)), 8);
        assert_eq!(columns_in_row(5, None), 3);
    }
}
