//! 画像の解析の一覧（画像・フレームの列を受けるもの）。解析を足すときは、ファイルを 1 つ作り、ここの `mod` と
//! `register_analyses!` の `current` に 1 行ずつ足す（音声の一覧 `analysis/audio/mod.rs` とは別のファイル）。
//!
//! 画像の解析で共有する決めごと（docs/editing.md §11）：
//! - **走査**：画素はタイル（`Raster` の 256² のタイル）ごとに `par_blocks` で並列に見て、タイルごとの小さな結果
//!   （数・集合・列ごとの印）をタイルの順に整数で畳む。畳み方は和・論理和・並べた集合の和集合だけなので、タイルの
//!   分け方・スレッド数に依らない。隣の画素と比べるもの（変わり目・8 連結）は、タイルの外の 1 列・1 行を `read_rect`
//!   で読んで比べる。浮動小数・乱数を使わない（`Numeric::Exact`。ゴールデンはすべてのターゲットで照らす）。
//! - **透明**：アルファが 0（16 ビットは 0、Indexed8 はパレットのアルファが 0）。**不透明**：アルファが最大
//!   （255・65535。アルファのない形式はすべて）。
//! - **8 ビットの色**：`imaging::convert::for_each_rgba8_key`（`image.convert` が Indexed8 へ減色するときと同じ色）。
//! - **フレームの列**：各フレームをキャンバスの `offset` に置いて、キャンバスの座標で畳む。フレームの外（キャンバスの
//!   覆われない部分）の扱いは解析ごとに決めて、各ファイルの冒頭に書く：`alpha` は透明として `uncovered` に数える
//!   （形式に依らない）。`colors` は数えない。`opaque_bounds` は透明とみなす（アルファのない形式だけは `union` を
//!   キャンバス全体にする）。`pixel_scale` はフレームの縁とその外を比べない（変わり目にしない）。`sprite_grid` は
//!   フレームの列を受けない。

pub mod alpha;
pub mod colors;
pub mod opaque_bounds;
pub mod pixel_scale;
pub mod sprite_grid;

use crate::error::EngineError;
use crate::geom::{IRect, Size};
use crate::shape::Shape;
use crate::value::{Raster, Value};

crate::analysis::register_analyses! {
    current: [
        alpha::Alpha,
        colors::Colors,
        opaque_bounds::OpaqueBounds,
        pixel_scale::PixelScale,
        sprite_grid::SpriteGrid,
    ],
    legacy: [],
    dropped: [],
}

/// 画素の並び 1 つ（画像は 1 枚、フレームの列はフレームごと）と、キャンバスの中の左上。
pub(crate) struct Plane<'a> {
    pub px: &'a Raster,
    pub x: i64,
    pub y: i64,
}

/// 値の画素の並び（フレームの順）とキャンバスの大きさ（画像は画像の大きさ）。音声は内部の誤り
/// （受け付ける種類は呼ぶ側が確かめている）。
pub(crate) fn planes<'a>(v: &'a Value, id: &str) -> Result<(Size, Vec<Plane<'a>>), EngineError> {
    match v {
        Value::Image(img) => Ok((
            Size {
                w: img.px.width(),
                h: img.px.height(),
            },
            vec![Plane {
                px: &img.px,
                x: 0,
                y: 0,
            }],
        )),
        Value::Frames(fr) => Ok((
            fr.canvas(),
            fr.frames()
                .iter()
                .map(|f| Plane {
                    px: &f.px,
                    x: f.offset.x,
                    y: f.offset.y,
                })
                .collect(),
        )),
        Value::Audio(_) => Err(EngineError::Internal(format!("{id} の入力が音声です"))),
    }
}

/// タイルの数。
pub(crate) fn tile_count(px: &Raster) -> usize {
    (px.tiles_x() as usize) * (px.tiles_y() as usize)
}

/// 番号 `i`（行の順）のタイルの矩形と中身。
pub(crate) fn tile_at(px: &Raster, i: usize) -> (IRect, &[u8]) {
    let tx_n = px.tiles_x().max(1) as usize;
    let (tx, ty) = ((i % tx_n) as u32, (i / tx_n) as u32);
    (px.tile_rect(tx, ty), px.tile(tx, ty))
}

/// 入力の形の画素の数（フレームの列は全フレームの上限の和）と、フレームの数・キャンバスの大きさ。
pub(crate) fn shape_pixels(input: &Shape) -> (u64, u64, u64, u64) {
    match *input {
        Shape::Image { w, h, .. } => (w.max().saturating_mul(h.max()), 1, w.max(), h.max()),
        Shape::Frames { count, w, h, .. } => (
            w.max().saturating_mul(h.max()).saturating_mul(count.max()),
            count.max(),
            w.max(),
            h.max(),
        ),
        Shape::Audio { .. } => (0, 0, 0, 0),
    }
}

/// 1 画素の生の値（1〜8 バイト）を、リトルエンディアンで並べた数にする（比べるための鍵。値が同じ ⟺ 鍵が同じ）。
pub(crate) fn raw_key(p: &[u8]) -> u64 {
    match *p {
        [a] => u64::from(a),
        [a, b] => u64::from(u16::from_le_bytes([a, b])),
        [a, b, c] => u64::from(u32::from_le_bytes([a, b, c, 0])),
        [a, b, c, d] => u64::from(u32::from_le_bytes([a, b, c, d])),
        [a, b, c, d, e, f] => u64::from_le_bytes([a, b, c, d, e, f, 0, 0]),
        [a, b, c, d, e, f, g, h] => u64::from_le_bytes([a, b, c, d, e, f, g, h]),
        _ => p
            .iter()
            .rev()
            .fold(0u64, |acc, b| acc.wrapping_shl(8) | u64::from(*b)),
    }
}

/// 作業領域の `Vec` を確保して `fill` で埋める（足りなければ `Limit`）。
pub(crate) fn filled_vec<T: Clone>(n: usize, fill: T) -> Result<Vec<T>, EngineError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| EngineError::Limit("解析の作業領域を確保できません".into()))?;
    v.resize(n, fill);
    Ok(v)
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::analysis::{Analysis, AnalyzeCx};
    use crate::limits::Limits;
    use crate::monitor::NoMonitor;
    use crate::value::Value;

    /// 既定の上限・中止なしで解く（単体のテスト用）。
    pub fn run<A: Analysis>(p: &A::Params, v: &Value) -> A::Output {
        let limits = Limits::default();
        A::run(
            p,
            v,
            &AnalyzeCx {
                mon: &NoMonitor,
                limits: &limits,
            },
        )
        .unwrap()
    }
}
