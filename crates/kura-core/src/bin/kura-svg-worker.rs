//! フォントなしの SVG の働き手（決定 23。OS によらない）。kura-core のテスト（`tests/svg_worker.rs`）と Linux の CI で、
//! アプリの代わりに起動する。アプリ（src-tauri）と kura-devtools は自分の実行ファイルを働き手にする（システムのフォントつき）。

use std::process::ExitCode;

use kura_core::codec_ext::svg_worker;
use kura_engine::codec::PinnedOptions;

fn main() -> ExitCode {
    if !svg_worker::is_worker_invocation() {
        return ExitCode::from(64);
    }
    svg_worker::serve(&PinnedOptions::default())
}
