//! ⌃⌥⌘C：ログイン用の Chrome を見ながら「このページを撮る」（ADR-112）。
//!
//! キーはログイン用の Chrome を開いているあいだだけ登録する（ふだんはほかのアプリのキーを横取りしない）。
//! 撮るのはログイン用の Chrome が前面のときだけ（ふだん使いの Chrome を見ながら押しても撮らない）。
//! Kura は裏にいるので、撮れたらシャッター音、だめならエラーの音を鳴らし、結果をイベントで画面に送る。

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::commands::capture::login_snapshot;
use crate::dto::LoginShotEvent;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 撮るキー（⌃⌥⌘C）。
pub fn capture_shortcut() -> Shortcut {
    Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER),
        Code::KeyC,
    )
}

/// 登録する（登録済みなら何もしない）。
pub fn register(app: &AppHandle) {
    let shortcuts = app.global_shortcut();
    let key = capture_shortcut();
    if shortcuts.is_registered(key) {
        return;
    }
    let result = shortcuts.on_shortcut(key, |app, _key, event| {
        if event.state == ShortcutState::Pressed {
            let app = app.clone();
            // 撮影は時間がかかるので、キーの処理（メインスレッド）からは離す
            std::thread::spawn(move || shoot(&app));
        }
    });
    if let Err(err) = result {
        tracing::warn!(%err, "撮影のキー（⌃⌥⌘C）を登録できませんでした");
    }
}

/// 外す（登録していなければ何もしない）。
pub fn unregister(app: &AppHandle) {
    let shortcuts = app.global_shortcut();
    let key = capture_shortcut();
    if shortcuts.is_registered(key)
        && let Err(err) = shortcuts.unregister(key)
    {
        tracing::warn!(%err, "撮影のキー（⌃⌥⌘C）を外せませんでした");
    }
}

fn shoot(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let outcome = (|| {
        let slot = state.login_slot();
        let pid = slot.lock().ok().and_then(|mut guard| {
            let browser = guard.as_mut()?;
            browser.alive().then(|| browser.pid())
        });
        let Some(pid) = pid else {
            unregister(app);
            return Err(AppError::new(
                AppErrorCode::NotFound,
                "ログイン用の Chrome が開いていません",
            ));
        };
        if kura_mac::frontmost_pid() != Some(pid) {
            return Err(AppError::new(
                AppErrorCode::Invalid,
                "ログイン用の Chrome を前面にしてから ⌃⌥⌘C を押してください",
            ));
        }
        let session = state.session()?;
        login_snapshot(&session, &slot, false)
    })();
    kura_mac::play_capture_sound(outcome.is_ok());
    let event = match outcome {
        Ok(result) => LoginShotEvent {
            ok: true,
            message: String::new(),
            result: Some(result),
        },
        Err(err) => {
            tracing::info!(message = %err.message, "⌃⌥⌘C で撮れませんでした");
            LoginShotEvent {
                ok: false,
                message: err.message,
                result: None,
            }
        }
    };
    if let Err(err) = tauri_specta::Event::emit(&event, app) {
        tracing::warn!(%err, "撮影の結果を画面に送れませんでした");
    }
}
