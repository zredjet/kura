//! 操作の共通の補助。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::geom::Size;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::{Audio, Frame, Frames, Grid, Image, Raster, Value};

/// 格子の引数（`sprite.split_grid`・`meta.set_grid`）。JSON の形はメタデータの格子と同じだが、型を分けておく
/// （メタデータの格子に項目を足しても、引数の形と版が変わらないように）。知らない項目は拒む。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GridParam {
    pub cell_w: u32,
    pub cell_h: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub gap_x: u32,
    pub gap_y: u32,
}

impl From<GridParam> for Grid {
    fn from(g: GridParam) -> Self {
        Grid {
            cell_w: g.cell_w,
            cell_h: g.cell_h,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            gap_x: g.gap_x,
            gap_y: g.gap_y,
        }
    }
}

impl From<Grid> for GridParam {
    fn from(g: Grid) -> Self {
        GridParam {
            cell_w: g.cell_w,
            cell_h: g.cell_h,
            offset_x: g.offset_x,
            offset_y: g.offset_y,
            gap_x: g.gap_x,
            gap_y: g.gap_y,
        }
    }
}

/// 引数の誤り（種類を指定する。上限は `IssueCode::TooLarge`）。
pub fn param_issue(key: &str, code: IssueCode, message: impl Into<String>) -> Issues {
    Issues::one(Issue::error(code, message).param(key))
}

/// 引数の誤り（その引数の名前付き）。
pub fn bad_param(key: &str, message: impl Into<String>) -> Issues {
    Issues::one(Issue::error(IssueCode::OutOfRange, message).param(key))
}

/// 入力の誤り。
pub fn bad_input(message: impl Into<String>) -> Issues {
    Issues::one(Issue::error(IssueCode::BadInput, message))
}

/// 入力が画像であること（違えば内部の誤り。推論で種類を確かめてあるので、ここには来ない）。
pub fn image_of<'a>(v: &'a Value, op: &str) -> Result<&'a Arc<Image>, EngineError> {
    match v {
        Value::Image(img) => Ok(img),
        _ => Err(EngineError::Internal(format!(
            "{op} の入力が画像ではありません"
        ))),
    }
}

/// 入力が音声であること（違えば内部の誤り。推論で種類を確かめてあるので、ここには来ない）。
pub fn audio_of<'a>(v: &'a Value, op: &str) -> Result<&'a Arc<Audio>, EngineError> {
    match v {
        Value::Audio(a) => Ok(a),
        _ => Err(EngineError::Internal(format!(
            "{op} の入力が音声ではありません"
        ))),
    }
}

/// 音声の形 (レート, チャンネル数, 長さ)。音声でなければ None。
pub fn audio_shape(s: &Shape) -> Option<(u32, u16, Dim)> {
    match *s {
        Shape::Audio {
            rate,
            channels,
            frames,
        } => Some((rate, channels, frames)),
        _ => None,
    }
}

/// フレームの数の引数の上限（2^40。読み込みで断り、後の計算であふれないようにする）。
pub const MAX_FRAME_PARAM: i64 = 1 << 40;

/// 決まっている大きさ（どちらかが `AtMost` なら None）。
pub fn exact_size(w: Dim, h: Dim) -> Option<Size> {
    Some(Size {
        w: u32::try_from(w.exact()?).ok()?,
        h: u32::try_from(h.exact()?).ok()?,
    })
}

/// フレームの列の各フレームの画素を `f` で作り直す（位置・表示時間・キャンバス・メタデータ・繰り返しはそのまま）。
/// 作り直した画素は元と同じ大きさで、形式・パレットはどのフレームもそろっていること。
pub fn map_frames(
    fr: &Frames,
    mon: &dyn Monitor,
    mut f: impl FnMut(usize, &Frame) -> Result<Raster, EngineError>,
) -> Result<Frames, EngineError> {
    let mut out = Vec::with_capacity(fr.frames().len());
    for (i, frame) in fr.frames().iter().enumerate() {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        out.push(Frame {
            px: f(i, frame)?,
            offset: frame.offset,
            duration_ms: frame.duration_ms,
        });
    }
    Frames::new(out, fr.canvas(), fr.meta.clone(), fr.loop_count)
}

/// 画像かフレームの列の画素を作り直す（`f` は (フレームの番号, 画素) を受け取る。画像は番号 0）。
pub fn map_rasters(
    v: &Value,
    mon: &dyn Monitor,
    mut f: impl FnMut(usize, &Raster) -> Result<Raster, EngineError>,
) -> Result<Value, EngineError> {
    match v {
        Value::Image(img) => Ok(Value::Image(Arc::new(Image {
            px: f(0, &img.px)?,
            meta: img.meta.clone(),
        }))),
        Value::Frames(fr) => Ok(Value::Frames(Arc::new(map_frames(fr, mon, |i, frame| {
            f(i, &frame.px)
        })?))),
        Value::Audio(_) => Err(EngineError::Internal(
            "音声には画素の操作を当てられません".into(),
        )),
    }
}
