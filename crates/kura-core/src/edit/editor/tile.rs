//! エディタの表示のタイルを配る（kura:// の `/edit/{session}/{rev}/{level}/{frame}/{tx}/{ty}`。docs/editing.md §12.13、
//! docs/architecture.md §4）。
//!
//! - 応えるのは、エディタが表示した今か直前の表示（`published`）と rev・段が同じで、そのタイルが有効なときだけ（ほかは 404）。
//!   描いている途中の錠を待たない（表示を写して読む。タイルは `Arc` で共有しているので写すのは軽い）。
//! - (session, rev, 段, フレーム, タイル) の中身は変わらない（rev はセッションの中で中身ごとに振る）ので、長く覚えてよい
//!   （`Cache-Control: immutable`。src-tauri が付ける）。
//! - 中身はどの画素の形式でも RGBA8（ストレート。16 ビットは四捨五入、パレットは色に）。非圧縮（行を詰めた `w × h × 4`。
//!   `w`・`h` はタイルの大きさで、右端・下端のタイルは小さい）か PNG（`.png`。速い圧縮）。どちらにするかは段 5 で WebKit で
//!   測って決める。

use std::sync::Arc;

use kura_engine::geom::IRect;
use kura_engine::imaging::convert::for_each_rgba8_key;
use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster};
use kura_engine::{EngineError, NoMonitor, Value};

use super::TILE;
use crate::edit::EngineServices;

/// タイルの中身の形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileFormat {
    /// 行を詰めた RGBA8（非圧縮）。
    Rgba,
    /// PNG（RGBA8・速い圧縮）。
    Png,
}

/// タイルの要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRequest<'a> {
    pub session: &'a str,
    pub rev: u64,
    pub level: u8,
    pub frame: u32,
    pub tx: u32,
    pub ty: u32,
    pub format: TileFormat,
}

/// タイルを返せない理由。
#[derive(Debug, thiserror::Error)]
pub enum TileError {
    /// エディタ・表示・タイルがない（閉じた・古い rev や段・有効でないタイル）。
    #[error("タイルがありません: {0}")]
    NotFound(String),
    /// エンジンを使えない。
    #[error("エンジンを使えません: {0}")]
    Unavailable(String),
    /// 中身を作れない。
    #[error("タイルを作れません: {0}")]
    Failed(String),
}

impl TileError {
    /// HTTP の状態（404・503・500）。
    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::Unavailable(_) => 503,
            Self::Failed(_) => 500,
        }
    }
}

/// 表示の値のタイルを、RGBA8 の行（`w × h × 4`）にする。
pub(super) fn tile_rgba(
    value: &kura_engine::render::region::PartialValue,
    frame: u32,
    tx: u32,
    ty: u32,
) -> Result<(u32, u32, Vec<u8>), TileError> {
    let missing = || TileError::NotFound(format!("フレーム {frame} のタイル {tx},{ty}"));
    let rect = value.frame_rect(frame).ok_or_else(missing)?;
    let valid = value.valid(frame).ok_or_else(missing)?;
    if !valid.contains(tx, ty) {
        return Err(missing());
    }
    let r = tile_rect(rect.w, rect.h, tx, ty).ok_or_else(missing)?;
    let bytes = value
        .tile(frame, tx, ty)
        .map_err(|e| TileError::Failed(e.to_string()))?;
    let fmt = value.format().unwrap_or(PixelFormat::Rgba8);
    let palette = value.palette().map(|p| p.colors.as_slice());
    let n = (r.w as usize)
        .checked_mul(r.h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| TileError::Failed("タイルが大きすぎます".into()))?;
    let mut out = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| TileError::Failed("タイルを確保できません".into()))?;
    for_each_rgba8_key(fmt, palette, bytes, |key| {
        out.extend_from_slice(&key.to_be_bytes());
        true
    })
    .map_err(|e| TileError::Failed(e.to_string()))?;
    if out.len() != n {
        return Err(TileError::Failed(format!(
            "タイルの大きさが合いません（{} バイト。{n} バイトのはず）",
            out.len()
        )));
    }
    Ok((r.w, r.h, out))
}

/// フレームの画像（`w × h`）のタイル `tx, ty` の矩形（範囲の外は None）。
fn tile_rect(w: u32, h: u32, tx: u32, ty: u32) -> Option<IRect> {
    let x = tx.checked_mul(TILE)?;
    let y = ty.checked_mul(TILE)?;
    if x >= w || y >= h {
        return None;
    }
    Some(IRect::new(
        i64::from(x),
        i64::from(y),
        TILE.min(w - x),
        TILE.min(h - y),
    ))
}

/// タイルを返す（モジュールの説明）。PNG はエディタのプレビューのプールで書く。
pub fn serve(engine: &EngineServices, req: &TileRequest<'_>) -> Result<Vec<u8>, TileError> {
    let editor = engine
        .editor_for(req.session)
        .ok_or_else(|| TileError::NotFound(format!("エディタ {}", req.session)))?;
    let value = editor
        .published_value(req.rev, req.level)
        .ok_or_else(|| TileError::NotFound(format!("rev {}・段 {} の表示", req.rev, req.level)))?;
    let (w, h, rgba) = tile_rgba(&value, req.frame, req.tx, req.ty)?;
    match req.format {
        TileFormat::Rgba => Ok(rgba),
        TileFormat::Png => {
            let runtime = engine
                .runtime_if_built()
                .ok_or_else(|| TileError::Unavailable("実行の場がありません".into()))?;
            let codecs = editor.codecs();
            runtime
                .preview(|_| -> Result<Vec<u8>, EngineError> {
                    let px = Raster::from_rows(w, h, PixelFormat::Rgba8, None, &rgba)?;
                    let v = Value::Image(Arc::new(Image {
                        px,
                        meta: ImageMeta::default(),
                    }));
                    Ok(codecs
                        .encode_bytes(
                            &v,
                            "png",
                            &serde_json::json!({ "compression": "fast" }),
                            &NoMonitor,
                        )?
                        .0)
                })
                .map_err(|e| TileError::Failed(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_rects_shrink_at_the_edges() {
        assert_eq!(tile_rect(600, 300, 0, 0), Some(IRect::new(0, 0, 256, 256)));
        assert_eq!(
            tile_rect(600, 300, 2, 1),
            Some(IRect::new(512, 256, 88, 44))
        );
        assert_eq!(tile_rect(600, 300, 3, 0), None);
        assert_eq!(tile_rect(600, 300, 0, 2), None);
        assert_eq!(tile_rect(600, 300, u32::MAX, 0), None);
        assert_eq!(TileError::NotFound(String::new()).status(), 404);
        assert_eq!(TileError::Unavailable(String::new()).status(), 503);
        assert_eq!(TileError::Failed(String::new()).status(), 500);
    }
}
