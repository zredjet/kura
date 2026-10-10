//! 計測の結果をログに残す（`VITE_KURA_BENCH=1` でビルドしたフロントだけが呼ぶ）。

use crate::error::AppError;

/// フロントで測った結果（JSON の文字列）をログに書く。
#[tauri::command]
#[specta::specta]
pub fn bench_report(name: String, result: String) -> Result<(), AppError> {
    tracing::info!(
        name,
        result,
        since_start_ms = crate::since_start_ms(),
        "計測"
    );
    Ok(())
}
