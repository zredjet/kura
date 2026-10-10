//! 開発用ツール。`seed`（ダミーライブラリの生成）は M2 で実装する。

use std::process::ExitCode;

const USAGE: &str = "使い方: kura-devtools seed --items <件数> --out <パス>";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    match std::env::args().nth(1).as_deref() {
        Some("seed") => {
            tracing::error!("seed はまだ実装されていません（M2 で実装）");
            ExitCode::FAILURE
        }
        _ => {
            tracing::info!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
