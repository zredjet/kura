//! スプライトシートの格子のセルを TS と共有するテストベクタ（M11。docs/design.md §3.1 の格子のシート）。
//!
//! - `tests/vectors/sprite/grid-cells.json`：
//!   - `axis`：1 つの軸に収まるセルの数（`sprite::split_grid::cells_on_axis`）。端の欠けたセルは数えない。u32 の端の値を含む。
//!   - `grids`：画像の大きさと格子から、セルの矩形の列（`cell_rects`。行ごとに左から）。
//! - フロントは `src/lib/asset/timeline.test.ts` が同じファイルを読み、`src/lib/asset/timeline.ts` の `cellsOnAxis`・
//!   `gridCells` と照らす（格子のシートのプレビューと、スプライトのアニメが `sprite.split_grid` と同じセルを切る）。
//! - ベクタは消す・変えるのを認めない（数え方を変えるなら操作の版を上げ、新しいファイルを足す）。足りなければ
//!   `KURA_BLESS=1 cargo test -p kura-engine --test sprite_vectors` で書く。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use kura_engine::ops::sprite::split_grid::{cell_rects, cells_on_axis};
use kura_engine::value::Grid;
use serde_json::{Value as Json, json};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/sprite")
}

fn grid(cell_w: u32, cell_h: u32, offset_x: u32, offset_y: u32, gap_x: u32, gap_y: u32) -> Grid {
    Grid {
        cell_w,
        cell_h,
        offset_x,
        offset_y,
        gap_x,
        gap_y,
    }
}

fn grid_cells() -> Json {
    let max = u32::MAX;
    let axis: Vec<(u64, u32, u32, u32)> = vec![
        (64, 0, 16, 0),
        (63, 0, 16, 0),
        (70, 2, 16, 2),
        (72, 2, 16, 2),
        (10, 0, 0, 0),
        (16, 0, 16, 100),
        (15, 0, 16, 0),
        (100, 90, 16, 0),
        (100, 84, 16, 0),
        (1_000, 1, 3, 4),
        (0, 0, 1, 0),
        (u64::from(max), 0, 1, 0),
        (u64::from(max), 0, max, max),
        (u64::from(max), max, 1, 0),
        (u64::from(max), 1, max - 1, 0),
        (u64::from(max), 7, 1_000, max - 1_000),
    ];
    let axis: Vec<Json> = axis
        .into_iter()
        .map(|(len, offset, cell, gap)| {
            json!({
                "len": len,
                "offset": offset,
                "cell": cell,
                "gap": gap,
                "count": cells_on_axis(len, offset, cell, gap),
            })
        })
        .collect();
    let grids: Vec<(&str, u32, u32, Grid)> = vec![
        ("exact-4x2", 64, 32, grid(16, 16, 0, 0, 0, 0)),
        ("offset-and-gap", 36, 20, grid(16, 8, 2, 1, 2, 2)),
        ("partial-edge-dropped", 50, 20, grid(16, 16, 0, 0, 0, 0)),
        ("one-cell-wide-gap", 20, 20, grid(20, 20, 0, 0, 100, 100)),
        ("tall-strip", 16, 80, grid(16, 16, 0, 0, 0, 0)),
        ("wide-strip-with-gap", 100, 10, grid(10, 10, 0, 0, 5, 0)),
        ("non-square-cells", 48, 30, grid(12, 10, 0, 0, 0, 0)),
        ("offset-beyond-image", 32, 32, grid(16, 16, 20, 0, 0, 0)),
        ("nothing-fits", 10, 10, grid(16, 16, 0, 0, 0, 0)),
        ("zero-cell", 64, 64, grid(0, 16, 0, 0, 0, 0)),
    ];
    let grids: Vec<Json> = grids
        .into_iter()
        .map(|(name, w, h, g)| {
            let cells: Vec<Json> = cell_rects(&g, w, h)
                .iter()
                .map(|r| json!([r.x, r.y, r.w, r.h]))
                .collect();
            json!({
                "name": name,
                "width": w,
                "height": h,
                "grid": {
                    "cellW": g.cell_w,
                    "cellH": g.cell_h,
                    "offsetX": g.offset_x,
                    "offsetY": g.offset_y,
                    "gapX": g.gap_x,
                    "gapY": g.gap_y,
                },
                "cells": cells,
            })
        })
        .collect();
    json!({ "version": 1, "axis": axis, "grids": grids })
}

#[test]
fn vectors_match_the_frozen_files() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let file = "grid-cells.json";
    let want = grid_cells();
    let path = dir().join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let frozen: Json = serde_json::from_str(&text).unwrap();
            assert!(
                frozen == want,
                "{file}：凍結したベクタと違う結果です（数え方を変えたなら、操作の版を上げて新しいファイルを足す）"
            );
        }
        Err(_) if bless => {
            std::fs::create_dir_all(dir()).unwrap();
            std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
        }
        Err(_) => panic!("{file} がありません（KURA_BLESS=1 で書けます）"),
    }
}
