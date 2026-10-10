//! ログ（tracing）の初期化。標準エラー出力と、日ごとのログファイルに書く。

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{InitError, RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// 既定のログレベル。環境変数 `RUST_LOG` で上書きできる。
const DEFAULT_FILTER: &str = if cfg!(debug_assertions) {
    "info,kura_app_lib=debug,kura_core=debug,kura_mac=debug"
} else {
    "info"
};

/// 残しておくログファイルの数（1 日 1 ファイル）。
const MAX_LOG_FILES: usize = 7;

/// アプリのログフォルダ（`~/Library/Logs/<bundle id>`。Tauri の `app_log_dir` と同じ場所）。
pub fn log_dir(identifier: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Logs").join(identifier))
}

/// ログを初期化する。戻り値はファイル書き込みスレッドのガードで、アプリの終了まで保持する。
pub fn init(dir: Option<&Path>) -> Option<WorkerGuard> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let stderr = fmt::layer().with_writer(std::io::stderr);

    let (file, guard, file_error) = match dir.map(open_appender) {
        Some(Ok(appender)) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer().with_ansi(false).with_writer(writer);
            (Some(layer), Some(guard), None)
        }
        Some(Err(err)) => (None, None, Some(err)),
        None => (None, None, None),
    };

    // すでに初期化済み（テストなど）なら何もしない。
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .try_init();

    if let Some(err) = file_error {
        tracing::warn!(%err, "ログファイルを開けませんでした");
    }
    guard
}

fn open_appender(dir: &Path) -> Result<RollingFileAppender, OpenError> {
    // フォルダがないと古いログの掃除が失敗するので先に作る
    std::fs::create_dir_all(dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("kura")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(dir)?;
    Ok(appender)
}

#[derive(Debug, thiserror::Error)]
enum OpenError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Init(#[from] InitError),
}

#[cfg(test)]
mod tests {
    #[test]
    fn log_dir_is_under_library_logs() {
        let dir = super::log_dir("dev.kura.Kura").unwrap();
        assert!(dir.ends_with("Library/Logs/dev.kura.Kura"));
    }
}
