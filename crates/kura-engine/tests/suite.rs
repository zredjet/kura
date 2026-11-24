//! kura-engine の結合テストを 1 つのバイナリにまとめる（ADR-209）。
//!
//! - macOS は作り直したばかりの実行ファイルを初めて起動するときに数十秒待たせることがあり（新しい実行ファイルの確かめ）、
//!   ファイルごとのバイナリ（60 個近く）だとエンジンを直すたびにその待ちとリンクが数十回かかった。ここに `mod` で並べた
//!   ファイルは、このバイナリのモジュールとして回る（ファイルの置き場所は `tests/` のまま。中の `#![allow(..)]` は
//!   モジュールに効く）。`pub mod` にするのは、別のバイナリだったときと同じく、ファイルの `pub` の項目を使われていなくても
//!   dead_code にしないため。
//! - 1 つだけ回す：`cargo test -p kura-engine --test suite <ファイル名>::`（例：`--test suite anchor::`）。
//!   ふだん回さないもの（`#[ignore]`）は `cargo test --release -p kura-engine --test suite <ファイル名>:: -- --ignored`。
//! - 別のバイナリのままにするもの（Cargo.toml の `[[test]]`）：CI が回すゴールデン（`golden`・`codec_golden`・
//!   `analysis_golden`・`model_golden`・`replay`）と、`#[global_allocator]` で確保を数えるもの（`model_json_heap`・
//!   `stream_held`）。
//! - テストのファイルを足したら、ここに `pub mod` を足すか、Cargo.toml に `[[test]]` を足す（`autotests = false` なので、
//!   どちらもしないと回らない。下の `every_test_file_is_built` が見張る）。

pub mod analysis_conformance;
pub mod analysis_core;
pub mod analysis_image;
pub mod analysis_needs;
pub mod analysis_ops;
pub mod anchor;
pub mod aseprite_ref;
pub mod audio_analysis;
pub mod audio_chunks;
pub mod audio_props;
pub mod audio_region;
pub mod audio_stream;
pub mod audio_tiers;
pub mod bench_analysis;
pub mod bench_append;
pub mod bench_audio_analysis;
pub mod bench_edit;
pub mod bench_image;
pub mod bench_preview;
pub mod bench_region;
pub mod bench_session;
pub mod budget;
pub mod cache;
pub mod conformance;
pub mod decode_stream;
pub mod delta;
pub mod e29;
pub mod e4;
pub mod encode_stream;
pub mod fixtures;
pub mod fuzz_smoke;
pub mod image_props;
pub mod large;
pub mod long_audio;
pub mod model;
pub mod ogg_vorbis;
pub mod paint_vectors;
pub mod param_origin;
pub mod peaks_patch;
pub mod peaks_vectors;
pub mod preview;
pub mod random_docs;
pub mod recipe;
pub mod region;
pub mod relative_ops;
pub mod rules;
pub mod session;
pub mod sprite_vectors;
pub mod stateful;
pub mod time_chain;
pub mod versions;
pub mod vertical;
pub mod view;

/// `tests/` の `.rs` は、どれもこのバイナリのモジュールか、Cargo.toml の `[[test]]` のどちらか 1 つで回る。
#[test]
fn every_test_file_is_built() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let suite = std::fs::read_to_string(root.join("tests/suite.rs")).unwrap();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let modules: Vec<&str> = suite
        .lines()
        .filter_map(|l| l.strip_prefix("pub mod ")?.strip_suffix(';'))
        .collect();
    let targets: Vec<&str> = manifest
        .lines()
        .filter_map(|l| l.strip_prefix("path = \"tests/")?.strip_suffix(".rs\""))
        .collect();
    let mut missing = Vec::new();
    let mut twice = Vec::new();
    for entry in std::fs::read_dir(root.join("tests")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_owned();
        let built = modules.iter().filter(|m| **m == stem).count()
            + targets.iter().filter(|t| **t == stem).count();
        match built {
            0 => missing.push(stem),
            1 => {}
            _ => twice.push(stem),
        }
    }
    missing.sort();
    twice.sort();
    assert!(
        missing.is_empty(),
        "どのテストのバイナリにも入っていないファイル: {missing:?}"
    );
    assert!(
        twice.is_empty(),
        "2 つのバイナリに入っているファイル: {twice:?}"
    );
}
