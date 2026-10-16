//! 大きな画像（16384²）を上限の中で読み書きできること（M10 段 4 の完了条件。docs/editing.md §13）。
//! 重いので通常のテストでは回さない：`cargo test --release -p kura-engine --test large -- --ignored`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::sync::Arc;

use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster};
use kura_engine::{EngineError, Limits, NoMonitor, Value};
use serde_json::json;

#[test]
#[ignore]
fn png_16384_square_round_trips() {
    let side = 16_384;
    let px = Raster::filled(side, side, PixelFormat::Rgba8, None, &[10, 20, 30, 255]).unwrap();
    let v = Value::Image(Arc::new(Image {
        px,
        meta: ImageMeta::default(),
    }));
    let set = CodecSet::pinned();
    let (bytes, _) = set
        .encode_bytes(&v, "png", &json!({ "compression": "fast" }), &NoMonitor)
        .unwrap();
    let (back, _) = set
        .decode(
            &mut Cursor::new(bytes.clone()),
            "png",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(back.digest().unwrap(), v.digest().unwrap());
    // 1 辺の上限を 16383 にすると、読む前に断る
    let small = Limits {
        max_dim: 16_383,
        ..Limits::default()
    };
    assert!(matches!(
        set.decode(
            &mut Cursor::new(bytes),
            "png",
            &DecodeOptions::default(),
            &small,
            &NoMonitor
        ),
        Err(EngineError::Limit(_))
    ));
}
