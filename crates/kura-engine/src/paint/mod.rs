//! 描画の道具（docs/editing.md §10）：ペン・消しゴム・塗りつぶしの動作の型と当て方、スポイト（問い合わせ）。
//!
//! 動作は `paint.actions` の引数に追記だけで溜まる。各動作は「今の画素」と「動作」だけで決まるので、
//! 前の出力に新しい動作だけを当てても、全体を当て直しても同じ結果になる（E21）。
//!
//! - ペン（置き換え）：筆の画素に色を置く。ペン（重ねる）：色を `imaging::composite::over` で 1 回だけ重ねる
//!   （アルファのない形式は、アルファ付きにして重ねてから戻す。下が不透明なので結果も不透明）。
//! - 消しゴム：アルファを 0 にする（色はそのまま）。Indexed8 はパレットの透明な色（アルファ 0 の最初の色）の番号にする。
//! - 塗りつぶし：`fill::fill_mask` の画素に色を置く（置き換え）。
//! - 色の引数：Indexed8 は番号（`{"index": n}`。重ねる・許容差は使えない）、ほかは RGBA8（`{"rgba": [...]}`。
//!   グレーの形式は灰色、置き換えでアルファのない形式は不透明な色だけ）。形式が分かる推論の段階で確かめる。
//! - 座標はキャンバスの座標。トリムしたフレームでは、フレームの外の画素は描かない。

pub mod fill;
pub mod list;
pub mod mask;
pub mod points;
pub mod stroke;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::geom::{IPoint, IRect, coord_ok};
use crate::imaging::composite::over;
use crate::imaging::convert::convert_pixel;
use crate::imaging::pixels::{
    can_be_transparent, channel_max, color_bytes, down16, palette_clear_index, read_rgba,
    with_alpha,
};
use crate::monitor::Monitor;
use crate::value::{PixelFormat, Raster, Value};

pub use fill::Connectivity;
pub use list::ActionList;
pub use mask::Mask;
pub use points::PointList;
pub use stroke::{Brush, BrushShape};

/// 道具。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tool {
    Pen,
    Eraser,
}

/// 色の置き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Blend {
    Replace,
    Over,
}

/// 色（RGBA8 か、パレットの番号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PaintColor {
    Rgba([u8; 4]),
    Index(u8),
}

/// 描画の動作 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PaintAction {
    Stroke {
        tool: Tool,
        /// ペンの色（消しゴムは null）。
        color: Option<PaintColor>,
        blend: Blend,
        brush: Brush,
        pixel_perfect: bool,
        /// フレームの番号（画像は 0）。
        frame: u32,
        points: PointList,
    },
    Fill {
        at: IPoint,
        color: PaintColor,
        tolerance: u8,
        connectivity: Connectivity,
        contiguous: bool,
        frame: u32,
    },
}

impl PaintAction {
    /// 動作のフレームの番号。
    pub fn frame(&self) -> u32 {
        match self {
            Self::Stroke { frame, .. } | Self::Fill { frame, .. } => *frame,
        }
    }

    /// 手間の見積もり。`w`・`h` は描く画像（フレームの列はキャンバス）の大きさの上限。
    /// - ストローク：道の点の数の上限 × 筆の画素の数 ＋ 画素の集合の大きさ（外接矩形と画像の重なり ÷ 64 語）＋ タイルの数。
    /// - 塗りつぶし：画像の画素の数（同じ色の画素を画像全体で照らす）。
    pub fn work(&self, w: u64, h: u64) -> u64 {
        match self {
            Self::Stroke { brush, points, .. } => {
                let stamp =
                    stroke::path_len_bound(&points.0).saturating_mul(stroke::footprint_len(*brush));
                let clip = IRect::new(
                    0,
                    0,
                    u32::try_from(w).unwrap_or(u32::MAX),
                    u32::try_from(h).unwrap_or(u32::MAX),
                );
                let area = stroke::stroke_bounds(&points.0, *brush, clip)
                    .map_or(0, |r| u64::from(r.w) * u64::from(r.h));
                let tiles = w.div_ceil(256).saturating_mul(h.div_ceil(256));
                stamp.saturating_add(area / 64).saturating_add(tiles)
            }
            Self::Fill { .. } => w.saturating_mul(h).max(1),
        }
    }

    /// 動作 1 つを当てる間の作業領域の見積もり（値を除く。`w`・`h` は描く画像（フレームの列はキャンバス）の大きさの上限）。
    /// - ストローク：画素の集合（外接矩形と画像の重なり。上限の画像全体で数える）＋ 筆の形の画素の列 ＋ 点の写し。
    /// - 塗りつぶし：[`fill::fill_work`]（集合と、連結なら区間の待ち行列の上限）。
    pub fn work_bytes(&self, w: u64, h: u64) -> u64 {
        // 筆の形・点は (i64, i64) と IPoint（どちらも 16 バイト）
        const POINT: u64 = 16;
        match self {
            Self::Stroke { brush, points, .. } => mask::mask_bytes(w, h)
                .saturating_add(stroke::footprint_len(*brush).saturating_mul(POINT))
                .saturating_add((points.0.len() as u64).saturating_mul(POINT)),
            Self::Fill { contiguous, .. } => fill::fill_work(w, h, *contiguous),
        }
    }

    /// 形式に対して正しい動作か（推論の段階で確かめる）。誤りなら (引数の名前, 理由)。
    pub fn check(&self, fmt: PixelFormat) -> Result<(), String> {
        let indexed = fmt == PixelFormat::Indexed8;
        let color_ok = |c: &PaintColor, blend: Blend| -> Result<(), String> {
            match (c, indexed) {
                (PaintColor::Index(_), true) if blend == Blend::Replace => Ok(()),
                (PaintColor::Index(_), true) => {
                    Err("パレットの画像では色を重ねられません（置き換えだけ）".into())
                }
                (PaintColor::Index(_), false) => {
                    Err("パレットの画像でないので、色は RGBA で指定します".into())
                }
                (PaintColor::Rgba(_), true) => Err("パレットの画像には番号で色を指定します".into()),
                (PaintColor::Rgba(c), false) => {
                    let target = if blend == Blend::Over {
                        with_alpha(fmt)
                    } else {
                        fmt
                    };
                    color_bytes(target, *c).map(|_| ())
                }
            }
        };
        match self {
            Self::Stroke {
                tool,
                color,
                blend,
                brush,
                points,
                ..
            } => {
                if brush.size == 0 {
                    return Err("筆の大きさは 1 以上にします".into());
                }
                if points.0.is_empty() {
                    return Err("ストロークに点がありません".into());
                }
                match (tool, color) {
                    (Tool::Pen, Some(c)) => color_ok(c, *blend),
                    (Tool::Pen, None) => Err("ペンには色が要ります".into()),
                    (Tool::Eraser, Some(_)) => Err("消しゴムに色は指定しません".into()),
                    (Tool::Eraser, None) if *blend != Blend::Replace => {
                        Err("消しゴムは置き換えだけです".into())
                    }
                    (Tool::Eraser, None) if !can_be_transparent(fmt) => Err(
                        "アルファのない画像では消しゴムを使えません（先にアルファ付きに変換）"
                            .into(),
                    ),
                    (Tool::Eraser, None) => Ok(()),
                }
            }
            Self::Fill {
                at,
                color,
                tolerance,
                ..
            } => {
                if !coord_ok(at.x) || !coord_ok(at.y) {
                    return Err("塗りつぶしの起点が範囲の外です（座標は ±2^31 まで）".into());
                }
                if indexed && *tolerance != 0 {
                    return Err(
                        "パレットの画像の塗りつぶしは番号で比べるので、許容差は 0 にします".into(),
                    );
                }
                color_ok(color, Blend::Replace)
            }
        }
    }
}

/// 1 画素への置き方。
enum Ink {
    /// この画素にする。
    Set(Vec<u8>),
    /// この画素（アルファ付きの形式の画素）を重ねる。`promote` はアルファのない形式で、重ねる前後に形式を変える。
    Over { src: Vec<u8>, promote: bool },
    /// アルファを 0 にする。
    ClearAlpha,
}

fn ink_for(
    fmt: PixelFormat,
    palette: Option<&[[u8; 4]]>,
    tool: Tool,
    color: Option<PaintColor>,
    blend: Blend,
) -> Result<Ink, EngineError> {
    let invalid = |m: String| EngineError::Invalid(crate::ops::util::bad_param("actions", m));
    match (tool, color) {
        (Tool::Eraser, _) => {
            if fmt == PixelFormat::Indexed8 {
                let p =
                    palette.ok_or_else(|| EngineError::Internal("パレットがありません".into()))?;
                let i = palette_clear_index(&crate::value::Palette { colors: p.to_vec() })
                    .ok_or_else(|| {
                        EngineError::Unsupported(
                            "パレットに透明な色がないので、消しゴムを使えません".into(),
                        )
                    })?;
                Ok(Ink::Set(vec![i]))
            } else {
                Ok(Ink::ClearAlpha)
            }
        }
        (Tool::Pen, Some(PaintColor::Index(i))) => {
            let n = palette.map_or(0, <[_]>::len);
            if usize::from(i) >= n {
                return Err(invalid(format!(
                    "番号 {i} はパレットにありません（{n} 色）"
                )));
            }
            Ok(Ink::Set(vec![i]))
        }
        (Tool::Pen, Some(PaintColor::Rgba(c))) => match blend {
            Blend::Replace => Ok(Ink::Set(color_bytes(fmt, c).map_err(invalid)?)),
            Blend::Over => Ok(Ink::Over {
                src: color_bytes(with_alpha(fmt), c).map_err(invalid)?,
                promote: !fmt.has_alpha(),
            }),
        },
        (Tool::Pen, None) => Err(invalid("ペンには色が要ります".into())),
    }
}

/// 1 画素に色を置く。
fn apply_ink(fmt: PixelFormat, dst: &mut [u8], ink: &Ink) -> Result<(), EngineError> {
    match ink {
        Ink::Set(v) => dst.copy_from_slice(v),
        Ink::ClearAlpha => match fmt {
            PixelFormat::GrayA8 => dst[1] = 0,
            PixelFormat::Rgba8 => dst[3] = 0,
            PixelFormat::GrayA16 => dst[2..4].fill(0),
            PixelFormat::Rgba16 => dst[6..8].fill(0),
            _ => {
                return Err(EngineError::Internal(
                    "アルファのない形式に消しゴムを当てようとしました".into(),
                ));
            }
        },
        Ink::Over { src, promote } => {
            if *promote {
                let af = with_alpha(fmt);
                let mut wide = vec![0u8; af.bytes_per_pixel()];
                convert_pixel(fmt, None, dst, af, [0, 0, 0], &mut wide)?;
                over(af, &mut wide, src)?;
                convert_pixel(af, None, &wide, fmt, [0, 0, 0], dst)?;
            } else {
                over(fmt, dst, src)?;
            }
        }
    }
    Ok(())
}

/// 1 つのタイルの中の集合の画素に色を置く。
fn paint_tile(
    fmt: PixelFormat,
    bpp: usize,
    mask: &Mask,
    ink: &Ink,
    tr: IRect,
    tile: &mut [u8],
) -> Result<(), EngineError> {
    let stride = tr.w as usize * bpp;
    for r in 0..i64::from(tr.h) {
        mask.for_each_in_row(tr.y + r, tr.x, tr.right(), |x| {
            let i = r as usize * stride + (x - tr.x) as usize * bpp;
            apply_ink(fmt, &mut tile[i..i + bpp], ink)
        })?;
    }
    Ok(())
}

/// 集合の画素に色を置く。触れたタイルだけを複製する（CoW）。触れるタイルが多いとき（塗りつぶし）はタイルごとに並列に、
/// 少ないとき（ストローク）は順に書く。どちらも各画素に同じ色を置くだけなので、結果は同じ。
fn paint_mask(
    px: &mut Raster,
    mask: &Mask,
    ink: &Ink,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    const PARALLEL_TILES: usize = 8;
    let fmt = px.format();
    let bpp = px.bpp();
    let mut touched = Vec::new();
    for ty in 0..px.tiles_y() {
        for tx in 0..px.tiles_x() {
            let tr = px.tile_rect(tx, ty);
            if mask.any_in(tr) {
                touched.push((tx, ty, tr));
            }
        }
    }
    if touched.len() > PARALLEL_TILES {
        return px.update_tiles(
            mon,
            |tr| mask.any_in(tr),
            |tr, tile| paint_tile(fmt, bpp, mask, ink, tr, tile),
        );
    }
    for (tx, ty, tr) in touched {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        paint_tile(fmt, bpp, mask, ink, tr, px.tile_mut(tx, ty))?;
    }
    Ok(())
}

/// 動作 1 つを画素に当てる。`offset` はフレームのキャンバスの中の位置。
fn apply_one(
    px: &mut Raster,
    offset: IPoint,
    action: &PaintAction,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let palette: Option<Vec<[u8; 4]>> = px.palette().map(|p| p.colors.clone());
    let fmt = px.format();
    let clip = IRect::new(0, 0, px.width(), px.height());
    let (mask, ink) = match action {
        PaintAction::Stroke {
            tool,
            color,
            blend,
            brush,
            pixel_perfect,
            points,
            ..
        } => {
            // 点は ±2^31、位置は 0 以上でキャンバスの中なので、引いてもあふれない（念のため止める）
            let local: Vec<IPoint> = points
                .0
                .iter()
                .map(|p| IPoint {
                    x: p.x.saturating_sub(offset.x),
                    y: p.y.saturating_sub(offset.y),
                })
                .collect();
            let ink = ink_for(fmt, palette.as_deref(), *tool, *color, *blend)?;
            (
                stroke::stroke_mask(&local, *brush, *pixel_perfect, clip, mon)?,
                ink,
            )
        }
        PaintAction::Fill {
            at,
            color,
            tolerance,
            connectivity,
            contiguous,
            ..
        } => {
            let ink = ink_for(
                fmt,
                palette.as_deref(),
                Tool::Pen,
                Some(*color),
                Blend::Replace,
            )?;
            // 起点がフレームの外（あふれる場合を含む）なら何もしない
            let mask = match (at.x.checked_sub(offset.x), at.y.checked_sub(offset.y)) {
                (Some(x), Some(y)) => {
                    fill::fill_mask(px, (x, y), *tolerance, *connectivity, *contiguous, mon)?
                }
                _ => None,
            };
            (mask, ink)
        }
    };
    match mask {
        Some(mask) => paint_mask(px, &mask, &ink, mon),
        None => Ok(()),
    }
}

/// 動作の列を値（画像かフレームの列）に順に当てる。
pub fn apply_actions<'a>(
    v: &Value,
    actions: impl IntoIterator<Item = &'a PaintAction>,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    apply_actions_from(v, actions, 0, mon)
}

/// 実行でしか分からない誤り（パレットの番号・透明な色・フレームの数）に、何番目の動作かを付ける（文言の頭に「動作 i：」）。
fn at_action(i: usize, e: EngineError) -> EngineError {
    match e {
        EngineError::Invalid(mut issues) => {
            for issue in &mut issues.0 {
                issue.message = format!("動作 {i}：{}", issue.message);
            }
            EngineError::Invalid(issues)
        }
        EngineError::Unsupported(m) => EngineError::Unsupported(format!("動作 {i}：{m}")),
        other => other,
    }
}

/// 動作の列を当てる。`first` は最初の動作の番号（差分の適用で、引数の中の番号を誤りに付けるため）。
pub fn apply_actions_from<'a>(
    v: &Value,
    actions: impl IntoIterator<Item = &'a PaintAction>,
    first: usize,
    mon: &dyn Monitor,
) -> Result<Value, EngineError> {
    match v {
        Value::Image(img) => {
            let mut px = img.px.clone();
            for (k, a) in actions.into_iter().enumerate() {
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
                if a.frame() != 0 {
                    return Err(at_action(first + k, frame_error(a.frame(), 1)));
                }
                apply_one(&mut px, IPoint { x: 0, y: 0 }, a, mon)
                    .map_err(|e| at_action(first + k, e))?;
            }
            let mut img = (**img).clone();
            img.px = px;
            Ok(Value::Image(Arc::new(img)))
        }
        Value::Frames(fr) => {
            let mut frames = fr.frames().to_vec();
            for (k, a) in actions.into_iter().enumerate() {
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
                let n = frames.len();
                let f = frames
                    .get_mut(a.frame() as usize)
                    .ok_or_else(|| at_action(first + k, frame_error(a.frame(), n)))?;
                apply_one(&mut f.px, f.offset, a, mon).map_err(|e| at_action(first + k, e))?;
            }
            Ok(Value::Frames(Arc::new(crate::value::Frames::new(
                frames,
                fr.canvas(),
                fr.meta.clone(),
                fr.loop_count,
            )?)))
        }
        Value::Audio(_) => Err(EngineError::Internal("音声には描けません".into())),
    }
}

fn frame_error(frame: u32, count: usize) -> EngineError {
    EngineError::Invalid(crate::ops::util::bad_param(
        "actions",
        format!("フレーム {frame} はありません（{count} 枚）"),
    ))
}

/// スポイトで拾った色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// ストレートの RGBA8（16 ビットは四捨五入で 8 ビットに）。
    pub rgba: [u8; 4],
    /// パレットの番号（Indexed8 のとき）。
    pub index: Option<u8>,
    /// その形式の画素のバイト列。
    pub raw: Vec<u8>,
}

/// スポイト：描画結果の 1 画素を返す（操作ではなく問い合わせ。履歴に入らない。E23）。
/// 座標はキャンバスの座標。画像（フレーム）の外なら None。
pub fn sample(v: &Value, frame: u32, x: i64, y: i64) -> Option<Sample> {
    let (px, offset) = match v {
        Value::Image(img) if frame == 0 => (&img.px, IPoint { x: 0, y: 0 }),
        Value::Frames(fr) => {
            let f = fr.frames().get(frame as usize)?;
            (&f.px, f.offset)
        }
        _ => return None,
    };
    let (lx, ly) = (x.checked_sub(offset.x)?, y.checked_sub(offset.y)?);
    let raw = px.pixel(u32::try_from(lx).ok()?, u32::try_from(ly).ok()?)?;
    let fmt = px.format();
    let c = read_rgba(fmt, px.palette().map(|p| p.colors.as_slice()), raw).ok()?;
    let narrow = |v: u32| {
        if channel_max(fmt) == 255 {
            v as u8
        } else {
            down16(v) as u8
        }
    };
    Some(Sample {
        rgba: [narrow(c[0]), narrow(c[1]), narrow(c[2]), narrow(c[3])],
        index: (fmt == PixelFormat::Indexed8).then(|| raw[0]),
        raw: raw.to_vec(),
    })
}
