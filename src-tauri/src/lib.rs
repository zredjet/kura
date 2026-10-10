//! Kura の Tauri アプリ本体。コマンドは薄く保ち、ロジックは `kura-core` / `kura-mac` に置く。

mod commands;
mod error;
mod logging;

pub use error::{AppError, AppErrorCode};

use std::process::ExitCode;

use specta_typescript::Typescript;
use tauri_specta::{Builder, collect_commands};

/// フロント用の型定義の出力先（`src/ipc/bindings.ts`）。
const BINDINGS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/ipc/bindings.ts");

/// コマンドを登録した tauri-specta のビルダー。コマンドを増やしたらここに足す。
fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![commands::appearance::appearance_get,])
}

/// 型定義を `src/ipc/bindings.ts` に書き出す。内容が変わったときだけ書き換える
/// （毎回書くと開発サーバーがページを再読み込みしてしまう）。
fn export_bindings(builder: &Builder<tauri::Wry>) -> Result<(), BindingsError> {
    let temp = std::env::temp_dir().join(format!("kura-bindings-{}.ts", std::process::id()));
    builder.export(Typescript::default(), &temp)?;
    let rendered = std::fs::read_to_string(&temp)?;
    std::fs::remove_file(&temp)?;

    let current = std::fs::read_to_string(BINDINGS_PATH).unwrap_or_default();
    if current != rendered {
        std::fs::write(BINDINGS_PATH, rendered)?;
        tracing::info!("型定義（bindings.ts）を更新しました");
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum BindingsError {
    #[error(transparent)]
    Export(#[from] specta_typescript::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// アプリを起動し、終了まで待つ。
pub fn run() -> ExitCode {
    let context = tauri::generate_context!();
    let _log_guard = logging::init(logging::log_dir(&context.config().identifier).as_deref());

    let builder = specta_builder();

    #[cfg(debug_assertions)]
    if let Err(err) = export_bindings(&builder) {
        tracing::warn!(%err, "型定義（bindings.ts）の書き出しに失敗しました");
    }

    let result = tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "Kura を起動しました");
            Ok(())
        })
        .run(context);

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "アプリの実行に失敗しました");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    /// `cargo test` でも型定義を書き出す（GUI を起動せずに bindings.ts を更新できる）。
    #[test]
    fn export_bindings() {
        super::export_bindings(&super::specta_builder()).unwrap();
    }
}
