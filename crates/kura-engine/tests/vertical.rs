//! 縦の通し（M10 段 2。docs/editing.md §17 の E2 の一部）：ファイルを読み、文書を当て、書き、読み直す。
//! 固定のコーデックだけの経路では、何度作り直しても値のダイジェスト（R1）と出力のバイト列（R2）が一致する。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use kura_engine::codec::{CodecSet, Tier};
use kura_engine::doc::{DecodeOptions, Graph, InputId};
use kura_engine::render::{Engine, InputValue};
use kura_engine::testgen::InputGen;
use kura_engine::value::{AudioMeta, LoopRange, Marker, PixelFormat, Value};
use kura_engine::{Digest, Limits, NoMonitor};
use serde_json::json;

/// ファイル（バイト列）を読み、文書を当てて、同じ形式で書く。返すのは書いたバイト列・値のダイジェスト・読み取りの記録の値のダイジェスト。
fn pipeline(
    bytes: &[u8],
    format: &'static str,
    graph: &Graph,
    opts: serde_json::Value,
) -> (Vec<u8>, Digest, Digest) {
    let codecs = CodecSet::pinned();
    let engine = Engine::new().unwrap();
    let (value, stamp) = codecs
        .decode(
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(stamp.tier, Tier::Pinned);
    assert_eq!(stamp.value_digest, value.digest().unwrap());
    let plan = engine
        .compile(graph, &BTreeMap::from([(InputId(0), value.shape())]))
        .unwrap();
    // 元のファイルの中身のハッシュを種に使う（ライブラリの hash の代わり）
    let source = Digest(*blake3::hash(bytes).as_bytes());
    let rendered = engine
        .render(
            &plan,
            &BTreeMap::from([(InputId(0), InputValue { value, source })]),
            &NoMonitor,
        )
        .unwrap();
    let out = &rendered.outputs[0];
    let (encoded, estamp) = codecs
        .encode_bytes(&out.value, format, &opts, &NoMonitor)
        .unwrap();
    assert_eq!(estamp.bytes as usize, encoded.len());
    (encoded, out.digest, stamp.value_digest)
}

#[test]
fn png_crop_png_is_reproducible_and_reads_back() {
    let source = InputGen::Sheet {
        cols: 4,
        rows: 3,
        cell: 16,
    }
    .generate()
    .unwrap();
    let Value::Image(img) = &source else { panic!() };
    let mut img = Arc::unwrap_or_clone(img.clone());
    img.meta.dpi = Some((72, 72));
    img.meta.text.insert("Author".into(), "kura".into());
    let codecs = CodecSet::pinned();
    let (file, _) = codecs
        .encode_bytes(&Value::Image(Arc::new(img)), "png", &json!({}), &NoMonitor)
        .unwrap();

    let mut g = Graph::new_single();
    g.push_linear(
        "image.crop",
        1,
        json!({ "rect": { "x": 16, "y": 16, "w": 32, "h": 32 } }),
        0,
    )
    .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();

    let (a, digest_a, _) = pipeline(&file, "png", &g, json!({}));
    let (b, digest_b, _) = pipeline(&file, "png", &g, json!({}));
    assert_eq!(digest_a, digest_b, "R1");
    assert_eq!(a, b, "R2");

    // 書いたものを読み直すと、描いた値と同じ（メタデータも写ったまま）
    let (back, stamp) = codecs
        .decode(
            &mut Cursor::new(&a),
            "png",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(stamp.value_digest, digest_a);
    let Value::Image(back) = back else { panic!() };
    assert_eq!(
        (back.px.width(), back.px.height(), back.px.format()),
        (32, 32, PixelFormat::Rgba8)
    );
    assert_eq!(
        back.meta.text.get("Author").map(String::as_str),
        Some("kura")
    );
    assert_eq!(back.meta.dpi, Some((72, 72)));
    // PNG は格子を持てない（MetaCaps）。元のファイルにも書かれていないので、ないまま
    assert_eq!(back.meta.grid, None);
}

#[test]
fn wav_gain_wav_is_reproducible_and_keeps_loop_points() {
    let Value::Audio(a) = InputGen::Sine {
        rate: 44_100,
        channels: 2,
        frames: 30_000,
        freq: 220,
        loop_range: None,
    }
    .generate()
    .unwrap() else {
        panic!()
    };
    let mut a = Arc::unwrap_or_clone(a);
    a.meta = AudioMeta {
        loop_range: Some(LoopRange {
            start: 1_000,
            end: 29_000,
        }),
        markers: vec![Marker {
            pos: 500,
            name: "hit".into(),
        }],
        tags: [("title".to_string(), "loop".to_string())].into(),
        ..AudioMeta::default()
    };
    let codecs = CodecSet::pinned();
    let (file, _) = codecs
        .encode_bytes(
            &Value::Audio(Arc::new(a)),
            "wav",
            &json!({ "bits": 24 }),
            &NoMonitor,
        )
        .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -6.0 }), 0)
        .unwrap();

    let (x, dx, _) = pipeline(&file, "wav", &g, json!({ "bits": 24 }));
    let (y, dy, _) = pipeline(&file, "wav", &g, json!({ "bits": 24 }));
    assert_eq!((dx, &x), (dy, &y), "R1・R2");
    let (back, _) = codecs
        .decode(
            &mut Cursor::new(&x),
            "wav",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    let Value::Audio(back) = back else { panic!() };
    assert_eq!(
        back.meta.loop_range,
        Some(LoopRange {
            start: 1_000,
            end: 29_000
        })
    );
    assert_eq!(
        back.meta.markers,
        vec![Marker {
            pos: 500,
            name: "hit".into()
        }]
    );
    assert_eq!(
        back.meta.tags.get("title").map(String::as_str),
        Some("loop")
    );
    assert_eq!(back.meta.source.map(|s| s.bits), Some(24));
}

/// 文書を JSON にして読み直しても、同じ出力になる（保存した文書の作り直し）。
#[test]
fn saved_documents_render_the_same() {
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 4_000,
        seed: 2,
    }
    .generate()
    .unwrap();
    let codecs = CodecSet::pinned();
    let (file, _) = codecs
        .encode_bytes(&input, "wav", &json!({ "bits": 16 }), &NoMonitor)
        .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": 31.447_295_999_999_99 }), 0)
        .unwrap();
    let engine = Engine::new().unwrap();
    let reloaded = engine.load(&g.to_canonical_json().unwrap()).unwrap();
    assert_eq!(reloaded, g);
    let (a, da, _) = pipeline(&file, "wav", &g, json!({}));
    let (b, db, _) = pipeline(&file, "wav", &reloaded, json!({}));
    assert_eq!((da, a), (db, b));
}
