//! ドラッグアウト（M15 段 5。ADR-202・207）：アプリの中のドラッグが窓の外へ出たら、名前と形式を整えたファイルを一時置き場に
//! 作り始めて OS のドラッグに切り替え、落とす先にファイルの URL で渡す。持ち出し先のプロジェクト（名前と形式の既定）を
//! 読み書きする。ロジックは `kura_core::export::dragout`、OS のドラッグは `kura_mac::begin_drag`。

use std::sync::Arc;
use std::time::Duration;

use kura_core::export::{DragSession, dragout};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State, WebviewWindow};

use super::blocking;
use crate::error::AppError;
use crate::state::AppState;

/// OS のドラッグを始めるのを待つ長さ（メインスレッドが答えなければ始めなかったことにする）。
const BEGIN_TIMEOUT: Duration = Duration::from_secs(5);

/// ドラッグアウトを始めた結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DragOutStartDto {
    /// OS のドラッグを始めた（マウスのボタンが離れていれば false）。
    pub started: bool,
    /// 一時置き場のこの回のフォルダ（Kura 自身に落としたファイルを見分ける）。
    pub dir: String,
    /// 飛ばしたアイテムの数（ゴミ箱の中・見つからない）。
    pub skipped: u32,
}

/// ドラッグアウトが終わった知らせ（渡した項目と、落とす先が求めたのに渡さなかった項目の数）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "dragout://ended")]
pub struct DragOutEnded {
    /// 一時置き場のこの回のフォルダ（`dragout_start` の `dir`）。
    pub dir: String,
    pub delivered: u32,
    /// 落とす先が求めたのに渡さなかった項目（作れなかった・待ちきれなかった）。
    pub missed: u32,
}

/// 持ち出し先のプロジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DragOutTargetDto {
    /// 選んだプロジェクト（選んでいなければ null）。
    pub chosen: Option<i64>,
    /// 名前と形式に使うプロジェクト（選んでいなければ、登録が 1 つならそれ）。null なら名前も形式もそのまま。
    pub effective: Option<i64>,
}

fn count(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// 1 回のドラッグの受け手（AppKit から、メインスレッドで呼ばれる）。
struct Handler {
    session: Arc<DragSession>,
    app: AppHandle,
}

impl kura_mac::DragHandler for Handler {
    fn wait(&self, index: usize) -> bool {
        self.session.wait(index)
    }

    fn ended(&self, dropped: bool) {
        let outcome = self.session.end();
        tracing::info!(
            session = self.session.id(),
            dropped,
            delivered = outcome.delivered,
            failed = outcome.failed,
            late = outcome.late,
            missed = outcome.missed,
            "ドラッグアウトが終わりました"
        );
        let event = DragOutEnded {
            dir: self.session.dir().display().to_string(),
            delivered: count(outcome.delivered),
            missed: count(outcome.missed),
        };
        if let Err(err) = tauri_specta::Event::emit(&event, &self.app) {
            tracing::warn!(%err, "ドラッグアウトの終わりを送れませんでした");
        }
    }

    fn retired(&self) -> bool {
        self.session.retired()
    }
}

/// ドラッグアウトを始める：選んだもの（一覧の ID。ユニットは `LIST_FLAG | unit_id`。選んだ順）の名前と形式を持ち出し先の
/// プロジェクトの既定で決め、一時置き場に作り始めて、OS のドラッグに切り替える。マウスのボタンが離れていれば始めない
/// （`started` が false。作りかけは片付ける）。作るものがない・多すぎるときは `Invalid`。
#[tauri::command]
#[specta::specta]
pub async fn dragout_start(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    entries: Vec<i64>,
) -> Result<DragOutStartDto, AppError> {
    let session = state.session()?;
    let drag = blocking(move || Ok(session.dragout().begin(&entries)?)).await?;
    let files: Vec<kura_mac::DragFile> = drag
        .entries()
        .iter()
        .map(|e| kura_mac::DragFile {
            path: e.path.clone(),
            folder: e.folder,
            image: e.image.clone(),
        })
        .collect();
    let handler: Arc<dyn kura_mac::DragHandler> = Arc::new(Handler {
        session: drag.clone(),
        app,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let sent = window.with_webview(move |webview| {
        #[cfg(target_os = "macos")]
        // SAFETY: with_webview はメインスレッドで、生きている WKWebView を渡す
        let started = unsafe { kura_mac::begin_drag(webview.inner(), &files, handler) };
        #[cfg(not(target_os = "macos"))]
        let started = {
            let _ = (webview, files, handler);
            false
        };
        let _ = tx.send(started);
    });
    let started = match sent {
        Ok(()) => blocking(move || Ok(rx.recv_timeout(BEGIN_TIMEOUT).unwrap_or(false))).await?,
        Err(err) => {
            drag.abort();
            return Err(err.into());
        }
    };
    if !started {
        drag.abort();
    }
    Ok(DragOutStartDto {
        started,
        dir: drag.dir().display().to_string(),
        skipped: count(drag.skipped()),
    })
}

/// 持ち出し先のプロジェクト（選んだものと、名前と形式に使うもの）。
#[tauri::command]
#[specta::specta]
pub async fn dragout_target(state: State<'_, AppState>) -> Result<DragOutTargetDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let t = session.library().db().read(dragout::target)?;
        Ok(DragOutTargetDto {
            chosen: t.chosen,
            effective: t.effective,
        })
    })
    .await
}

/// 持ち出し先のプロジェクトを選ぶ（null で選ばない。ないプロジェクトは `NotFound`）。
#[tauri::command]
#[specta::specta]
pub async fn dragout_target_set(
    state: State<'_, AppState>,
    project: Option<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| dragout::set_target(tx, project))?)
    })
    .await
}
