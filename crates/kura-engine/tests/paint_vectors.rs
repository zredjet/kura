//! 描画の規則を TS と共有するテストベクタ（docs/editing.md §10、E21）。
//!
//! - `tests/vectors/paint/strokes.json`：点の列 → 詰めた文字列・道（ピクセルパーフェクト）・筆を押した画素（画像の中だけ）。
//! - `tests/vectors/paint/over.json`：色を重ねる式（`imaging::composite::over`）の入出力。
//! - `tests/vectors/paint/points-invalid.json`：読めない点の列（Rust も TS も受け付けない）。
//! - `tests/vectors/paint/points-valid.json`：読める点の列の境界（±2^31 の端など。Rust と TS で同じ点に読む）。
//! - フロントは `src/lib/paint.test.ts` が同じファイルを読んで、`src/lib/paint.ts` の結果と照らす。
//! - ベクタは消す・変えるのを認めない（規則を変えるなら操作の版を上げ、新しいファイルを足す）。足りなければ
//!   `KURA_BLESS=1 cargo test -p kura-engine --test paint_vectors` で書く。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use kura_engine::geom::{IPoint, IRect};
use kura_engine::imaging::composite::over;
use kura_engine::paint::stroke::{path, stroke_mask};
use kura_engine::paint::{Brush, BrushShape, PointList};
use kura_engine::value::PixelFormat;
use serde_json::{Value as Json, json};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/paint")
}

fn pts(v: &[(i64, i64)]) -> Vec<IPoint> {
    v.iter().map(|(x, y)| IPoint { x: *x, y: *y }).collect()
}

fn xy(v: &[IPoint]) -> Json {
    Json::Array(v.iter().map(|p| json!([p.x, p.y])).collect())
}

fn stroke_case(
    name: &str,
    w: u32,
    h: u32,
    shape: BrushShape,
    size: u8,
    pixel_perfect: bool,
    points: &[(i64, i64)],
) -> Json {
    let points = pts(points);
    let brush = Brush { shape, size };
    let mut pixels = Vec::new();
    if let Some(m) = stroke_mask(
        &points,
        brush,
        pixel_perfect,
        IRect::new(0, 0, w, h),
        &kura_engine::NoMonitor,
    )
    .unwrap()
    {
        m.for_each(|x, y| {
            pixels.push(IPoint { x, y });
            Ok(())
        })
        .unwrap();
    }
    json!({
        "name": name,
        "width": w,
        "height": h,
        "brush": brush,
        "pixelPerfect": pixel_perfect,
        "points": xy(&points),
        "packed": PointList(points.clone()).pack(),
        "path": xy(&path(&points, pixel_perfect)),
        "pixels": xy(&pixels),
    })
}

fn strokes() -> Json {
    use BrushShape::{Circle, Square};
    let cases = vec![
        stroke_case("dot", 5, 5, Square, 1, false, &[(2, 2)]),
        stroke_case(
            "octants",
            21,
            21,
            Square,
            1,
            false,
            &[
                (10, 10),
                (20, 13),
                (13, 20),
                (10, 10),
                (7, 20),
                (0, 13),
                (10, 10),
                (0, 7),
                (7, 0),
                (10, 10),
                (13, 0),
                (20, 7),
            ],
        ),
        stroke_case(
            "reverse-direction",
            12,
            6,
            Square,
            1,
            false,
            &[(11, 5), (0, 0)],
        ),
        stroke_case(
            "pixel-perfect-stairs",
            10,
            10,
            Square,
            1,
            true,
            &[
                (0, 0),
                (1, 0),
                (1, 1),
                (2, 1),
                (2, 2),
                (3, 2),
                (3, 3),
                (6, 3),
                (6, 6),
            ],
        ),
        stroke_case(
            "pixel-perfect-curve",
            16,
            16,
            Square,
            1,
            true,
            &[(1, 1), (4, 2), (7, 4), (9, 7), (10, 10), (10, 14)],
        ),
        stroke_case(
            "duplicate-points",
            8,
            8,
            Square,
            1,
            true,
            &[(1, 1), (1, 1), (4, 1), (4, 1), (4, 5)],
        ),
        stroke_case("circle-sizes", 40, 12, Circle, 3, false, &[(3, 5), (36, 5)]),
        stroke_case("circle-4", 12, 12, Circle, 4, false, &[(5, 5)]),
        stroke_case("circle-5", 12, 12, Circle, 5, false, &[(5, 5)]),
        stroke_case("circle-8", 14, 14, Circle, 8, false, &[(6, 6)]),
        stroke_case(
            "square-2-diagonal",
            10,
            10,
            Square,
            2,
            false,
            &[(0, 0), (8, 6)],
        ),
        stroke_case(
            "clipped-at-edges",
            6,
            4,
            Circle,
            5,
            false,
            &[(-3, -2), (8, 5)],
        ),
        stroke_case("outside", 6, 4, Square, 3, false, &[(20, 20), (30, 25)]),
        stroke_case(
            "negative-and-far",
            32,
            32,
            Square,
            1,
            false,
            &[(-100, 3), (40, 3), (5, -7), (5, 200)],
        ),
    ];
    json!({ "version": 1, "cases": cases })
}

fn over_cases() -> Json {
    let mut cases = Vec::new();
    let rgba = [
        ([10, 20, 30, 255], [200, 100, 50, 128]),
        ([10, 20, 30, 0], [200, 100, 50, 128]),
        ([255, 255, 255, 200], [0, 0, 0, 1]),
        ([0, 0, 0, 255], [255, 255, 255, 254]),
        ([90, 80, 70, 60], [50, 40, 30, 20]),
        ([1, 2, 3, 4], [5, 6, 7, 0]),
        ([1, 2, 3, 4], [5, 6, 7, 255]),
    ];
    for (dst, src) in rgba {
        let mut out = dst;
        over(PixelFormat::Rgba8, &mut out, &src).unwrap();
        cases.push(json!({ "fmt": "rgba8", "dst": dst, "src": src, "out": out }));
    }
    for (dst, src) in [([100u8, 255u8], [200u8, 64u8]), ([7, 9], [250, 3])] {
        let mut out = dst;
        over(PixelFormat::GrayA8, &mut out, &src).unwrap();
        cases.push(json!({ "fmt": "grayA8", "dst": dst, "src": src, "out": out }));
    }
    json!({ "version": 1, "cases": cases })
}

/// 読めない点の列（Rust も TS も受け付けない）。
fn invalid_points() -> Json {
    let b64 = |bytes: &[u8]| {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let v = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..=c.len() {
                out.push(char::from(A[((v >> (18 - 6 * i)) & 0x3f) as usize]));
            }
        }
        out
    };
    let mut long = vec![0x80u8; 200];
    long.extend_from_slice(&[0, 0]);
    let cases = vec![
        ("length-mod-4-is-1", "A".to_string()),
        ("leftover-bits", "AAB".to_string()),
        ("truncated-varint", "gA".to_string()),
        ("bad-character", "A*A".to_string()),
        ("padding", "AA==".to_string()),
        ("odd-number-of-values", b64(&[0])),
        ("non-canonical-zero", b64(&[0x80, 0x00, 0x00])),
        ("varint-longer-than-10-bytes", b64(&long)),
        ("x-out-of-range", PointList(pts(&[(1 << 32, 0)])).pack()),
        (
            "y-out-of-range",
            PointList(pts(&[(0, -(1 << 31) - 1)])).pack(),
        ),
        (
            "delta-leaves-range",
            PointList(pts(&[(1 << 31, 0), ((1 << 31) + 1, 0)])).pack(),
        ),
    ];
    for (name, text) in &cases {
        assert!(
            PointList::unpack(text).is_err(),
            "Rust が読んでしまう: {name}"
        );
    }
    json!({
        "version": 1,
        "cases": cases.iter().map(|(name, text)| json!({ "name": name, "packed": text })).collect::<Vec<_>>(),
    })
}

/// 読める点の列の境界（座標の端 ±2^31・端から端への差・大きな値の続き）。Rust と TS で同じ点に読む。
fn valid_points() -> Json {
    let l = 1i64 << 31;
    let cases: Vec<(&str, Vec<(i64, i64)>)> = vec![
        ("corners", vec![(l, -l), (-l, l), (l, l), (-l, -l)]),
        ("zero-and-one", vec![(0, 0), (1, -1), (-1, 1)]),
        (
            "varint-edges",
            vec![(63, -64), (64, -65), (8191, -8192), (8192, -8193)],
        ),
        (
            "large-steps",
            vec![(123_456_789, -987_654_321), (-2_000_000_000, 2_000_000_000)],
        ),
    ];
    for (name, v) in &cases {
        let list = PointList(pts(v));
        assert_eq!(PointList::unpack(&list.pack()).unwrap(), list, "{name}");
    }
    json!({
        "version": 1,
        "cases": cases.iter().map(|(name, v)| json!({
            "name": name,
            "points": xy(&pts(v)),
            "packed": PointList(pts(v)).pack(),
        })).collect::<Vec<_>>(),
    })
}

#[test]
fn vectors_match_the_frozen_files() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let mut failures = Vec::new();
    for (file, want) in [
        ("strokes.json", strokes()),
        ("over.json", over_cases()),
        ("points-invalid.json", invalid_points()),
        ("points-valid.json", valid_points()),
    ] {
        let path = dir().join(file);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let frozen: Json = serde_json::from_str(&text).unwrap();
                if frozen != want {
                    failures.push(format!(
                        "{file}：凍結したベクタと違う結果です（描画の規則を変えたなら、操作の版を上げて新しいファイルを足す）"
                    ));
                }
            }
            Err(_) if bless => {
                std::fs::create_dir_all(dir()).unwrap();
                std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
            }
            Err(_) => failures.push(format!("{file} がありません（KURA_BLESS=1 で書けます）")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
