//! アプリの実行ファイル（`main.rs`）が SVG の働き手として動く（決定 23）：`--kura-svg-worker` で起動すると Tauri を始めずに、
//! アプリと同じ固定の段の SVG のデコーダ（システムのフォント）で読み、プロセスの中で読んだのと同じ値・読み取りの記録・
//! エラーを返す。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::time::Duration;

use kura_core::codec_ext::svg_worker::{self, Deadlines, WorkerCommand};
use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::{Limits, NoMonitor};

/// 文字（macOS ではシステムのフォントで描く。ほかの OS はフォントがなく、どちらも Unsupported）・図形・壊れた XML。
const CASES: [&str; 3] = [
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="220" height="48"><text x="4" y="34" font-family="sans-serif" font-size="28" fill="#246">Kura 蔵 ✓</text></svg>"##,
    r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30"><circle cx="20" cy="15" r="12" fill="#e60" opacity="0.6"/></svg>"##,
    "<svg",
];

#[test]
fn the_app_binary_reads_svg_like_the_in_process_decoder() {
    let opts = kura_mac::pinned_options();
    let local = CodecSet::pinned_with(&opts);
    let mut remote = CodecSet::pinned_with(&opts);
    // ビルドしたばかりの実行ファイルの最初の起動は、重い環境で 1 秒を超えることがある。値の一致を確かめるので、時間で
    // 止まらないように上限を長くする（時間の上限は kura-core の tests/svg_worker.rs が確かめる）
    let long = Duration::from_secs(120);
    svg_worker::install_with(
        &mut remote,
        Ok(WorkerCommand::new(
            env!("CARGO_BIN_EXE_kura-app"),
            [svg_worker::WORKER_ARG],
        )),
        Deadlines {
            shape: long,
            decode: long,
        },
    )
    .unwrap();
    for (i, svg) in CASES.iter().enumerate() {
        let read = |set: &CodecSet| {
            set.decode(
                &mut Cursor::new(svg.as_bytes().to_vec()),
                "svg",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
        };
        match (read(&local), read(&remote)) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{i}"),
            (Err(a), Err(b)) => {
                assert!(
                    !(cfg!(target_os = "macos") && i == 0),
                    "文字が読めない: {a}"
                );
                assert_eq!(a.to_string(), b.to_string(), "{i}");
            }
            (a, b) => panic!("{i}: {a:?} と {b:?}"),
        }
    }
}
