//! Kura の Tauri アプリ本体。コマンドは薄く保ち、ロジックは `kura-core` / `kura-mac` に置く。

mod commands;
mod config;
mod dto;
mod error;
mod logging;
mod protocol;
mod state;

pub use error::{AppError, AppErrorCode};

use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

#[cfg(any(debug_assertions, test))]
use specta_typescript::Typescript;
use tauri::Manager;
use tauri_specta::{Builder, collect_commands, collect_events};

use crate::state::AppState;

/// 起動した時刻（起動時間の計測に使う）。
static STARTED: OnceLock<Instant> = OnceLock::new();

/// 起動からの経過ミリ秒。
pub(crate) fn since_start_ms() -> u64 {
    STARTED
        .get()
        .map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 型生成を通さずに登録するコマンド（生バイトを返すもの）。
const RAW_COMMANDS: [&str; 1] = ["items_query_ids"];

/// 型生成を通さないコマンドの受け口。
fn raw_handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![commands::items::items_query_ids]
}

/// フロント用の型定義の出力先（`src/ipc/bindings.ts`）。
#[cfg(any(debug_assertions, test))]
const BINDINGS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/ipc/bindings.ts");

/// コマンドとイベントを登録した tauri-specta のビルダー。コマンドを増やしたらここに足す。
fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::appearance::appearance_get,
            commands::bench::bench_report,
            commands::library::library_check_location,
            commands::library::library_create,
            commands::library::library_open,
            commands::library::library_restore,
            commands::library::library_info,
            commands::import::import_preview,
            commands::import::import_start,
            commands::items::items_get,
            commands::items::item_detail,
            commands::items::library_counts,
            commands::items::items_selection_info,
            commands::items::items_update,
            commands::items::items_trash,
            commands::items::items_restore,
            commands::items::trash_empty,
            commands::jobs::jobs_list,
            commands::jobs::job_cancel,
            commands::jobs::job_pause,
            commands::jobs::job_resume,
            commands::jobs::job_retry,
            commands::jobs::job_prioritize,
            commands::jobs::job_log,
            commands::downloads::download_detect,
            commands::downloads::download_defaults,
            commands::downloads::download_enqueue,
            commands::downloads::downloads_list,
            commands::downloads::download_prefs_get,
            commands::downloads::download_prefs_set,
            commands::downloads::tools_status,
            commands::downloads::tools_set_path,
            commands::downloads::tools_update,
            commands::folders::folders_tree,
            commands::folders::folder_create,
            commands::folders::folder_update,
            commands::folders::folder_move,
            commands::folders::folder_delete,
            commands::folders::items_set_folders,
            commands::tags::tags_suggest,
            commands::tags::tags_apply,
            commands::tags::tag_detail,
            commands::tags::tag_update,
            commands::tags::tag_merge,
            commands::tags::tag_delete,
            commands::units::units_get,
            commands::units::unit_detail,
            commands::units::units_list,
            commands::units::unit_create_set,
            commands::units::unit_create_folder,
            commands::units::unit_update,
            commands::units::unit_add_items,
            commands::units::unit_remove_items,
            commands::units::unit_reorder,
            commands::units::unit_set_folders,
            commands::units::unit_tags_apply,
            commands::units::unit_delete,
            commands::search::query_parse,
            commands::search::query_suggest,
            commands::search::smart_folders_list,
            commands::search::smart_folder_save,
            commands::search::smart_folder_update,
            commands::search::smart_folder_delete,
            commands::player::playback_support_set,
            commands::player::play_queue,
            commands::player::playback_report,
            commands::player::proxy_request,
            commands::player::open_external,
            commands::player::player_prefs_get,
            commands::player::player_prefs_set,
            commands::playlists::playlists_list,
            commands::playlists::playlist_get,
            commands::playlists::playlist_create,
            commands::playlists::playlist_update,
            commands::playlists::playlist_delete,
            commands::playlists::playlist_set_granularity,
            commands::playlists::playlist_contents,
            commands::playlists::playlist_add,
            commands::playlists::playlist_move,
            commands::playlists::playlist_remove,
            commands::playlists::playlist_snapshot_to_files,
            commands::playlists::playlist_restore_entries,
            commands::playlists::playlist_regenerate,
            commands::playlists::playlist_save_state,
        ])
        .events(collect_events![dto::JobProgress])
        // ジョブの結果（resultJson）を読むための型
        .typ::<dto::ImportResultDto>()
        // ID・ミリ秒・バイト数は 2^53 を超えないので number として扱う
        .dangerously_cast_bigints_to_number()
}

#[cfg(any(debug_assertions, test))]
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

#[cfg(any(debug_assertions, test))]
#[derive(Debug, thiserror::Error)]
enum BindingsError {
    #[error(transparent)]
    Export(#[from] specta_typescript::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// アプリを起動し、終了まで待つ。
pub fn run() -> ExitCode {
    let _ = STARTED.set(Instant::now());
    let context = tauri::generate_context!();
    let _log_guard = logging::init(logging::log_dir(&context.config().identifier).as_deref());

    let builder = specta_builder();

    #[cfg(debug_assertions)]
    if let Err(err) = export_bindings(&builder) {
        tracing::warn!(%err, "型定義（bindings.ts）の書き出しに失敗しました");
    }

    let typed = builder.invoke_handler();
    let raw = raw_handler();
    let app = tauri::Builder::<tauri::Wry>::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(move |invoke: tauri::ipc::Invoke<tauri::Wry>| {
            if RAW_COMMANDS.contains(&invoke.message.command()) {
                raw(invoke)
            } else {
                typed(invoke)
            }
        })
        .register_asynchronous_uri_scheme_protocol("kura", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                responder.respond(protocol::respond(&app, &request));
            });
        })
        .setup(move |app| {
            builder.mount_events(app);
            let config_path = app.path().app_config_dir()?.join("app.json");
            app.manage(AppState::new(config_path));
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "Kura を起動しました");
            Ok(())
        })
        .build(context);

    match app {
        Ok(app) => {
            app.run(|handle, event| {
                if let tauri::RunEvent::Exit = event {
                    // 実行中のジョブを止めて DB を閉じる（取り込みは次回の起動で続きから）
                    if let Some(state) = handle.try_state::<AppState>() {
                        state.close();
                    }
                }
            });
            ExitCode::SUCCESS
        }
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
