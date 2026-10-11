//! キャプチャ（docs/architecture.md §10）：ルール、撮影のジョブ、ログイン用の Chrome、アプリ内ブラウザと見たままキャプチャ。
//!
//! アプリ内ブラウザ（`capture-browser`）は外部のページを開くので、アプリのコマンドの権限を付けない
//! （capabilities は `main` だけ）。開けるのは http / https だけで、ほかの scheme への移動は止める。

use std::sync::Mutex;
use std::time::Duration;

use kura_core::capture::{
    self, CHROME_TOOL, CaptureInput, CaptureMode, LoginBrowser, capture_meta, register_png,
    start_capture,
};
use kura_core::jobs::JobKind;
use kura_core::library::LibrarySession;
use kura_core::time::now_ms;
use kura_core::tools::find;
use tauri::{AppHandle, Manager, State, Url, WebviewUrl, WebviewWindowBuilder};

use super::blocking;
use crate::dto::{CaptureRuleDto, CaptureRuleInputDto, SnapshotResultDto};
use crate::error::{AppError, AppErrorCode};
use crate::shortcut;
use crate::state::AppState;

/// アプリ内ブラウザのウィンドウのラベル。
const BROWSER: &str = "capture-browser";

/// http / https の URL だけ受け付ける。
fn web_url(url: &str) -> Result<Url, AppError> {
    let parsed: Url = url
        .trim()
        .parse()
        .map_err(|_| AppError::new(AppErrorCode::Invalid, "URL が正しくありません"))?;
    if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
        Ok(parsed)
    } else {
        Err(AppError::new(
            AppErrorCode::Invalid,
            "http:// か https:// の URL を入れてください",
        ))
    }
}

/// ルールを並び順に一覧する。
#[tauri::command]
#[specta::specta]
pub async fn capture_rules_list(
    state: State<'_, AppState>,
) -> Result<Vec<CaptureRuleDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(capture::rules::list)?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// ルールを作る（`id` が None）か変える。
#[tauri::command]
#[specta::specta]
pub async fn capture_rule_save(
    state: State<'_, AppState>,
    id: Option<i64>,
    rule: CaptureRuleInputDto,
) -> Result<CaptureRuleDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let input = rule.into();
        let saved = session.library().db().write(move |tx| match id {
            Some(id) => capture::rules::update(tx, id, input),
            None => capture::rules::create(tx, input),
        })?;
        Ok(saved.into())
    })
    .await
}

/// ルールを消す。
#[tauri::command]
#[specta::specta]
pub async fn capture_rule_delete(state: State<'_, AppState>, id: i64) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| capture::rules::delete(tx, id))?)
    })
    .await
}

/// ルールを並べ替える。
#[tauri::command]
#[specta::specta]
pub async fn capture_rules_reorder(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| capture::rules::reorder(tx, &ids))?)
    })
    .await
}

/// URL をルールのとおりに撮るジョブを積む。
#[tauri::command]
#[specta::specta]
pub async fn capture_run(
    state: State<'_, AppState>,
    url: String,
    rule_id: i64,
) -> Result<i64, AppError> {
    let url = web_url(&url)?.to_string();
    let session = state.session()?;
    let profile_dir = state.capture_profile_dir();
    blocking(move || {
        let rule = session
            .library()
            .db()
            .read(|conn| capture::rules::get(conn, rule_id))?;
        let input = CaptureInput {
            url,
            rule,
            profile_dir,
        };
        Ok(start_capture(session.jobs(), &input)?)
    })
    .await
}

/// ログイン用に、キャプチャと同じプロファイルの Chrome を画面ありで開く（DevTools でつなぐ。ADR-111）。
/// すでに開いていれば、URL を新しいタブで開く。
#[tauri::command]
#[specta::specta]
pub async fn capture_login(
    app: AppHandle,
    state: State<'_, AppState>,
    url: Option<String>,
) -> Result<(), AppError> {
    let url = match url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => Some(web_url(u)?.to_string()),
        None => None,
    };
    let profile = state.capture_profile_dir();
    let slot = state.login_slot();
    blocking(move || {
        let mut guard = slot
            .lock()
            .map_err(|_| AppError::internal("ログイン用の Chrome の状態を読めません"))?;
        if let Some(browser) = guard.as_mut()
            && browser.alive()
        {
            if let Some(url) = &url {
                browser.open_url(url)?;
            }
            return Ok(());
        }
        // 閉じられていたものは片付けてから開き直す
        *guard = None;
        let chrome = find(CHROME_TOOL).ok_or_else(|| {
            AppError::new(AppErrorCode::ToolMissing, "Google Chrome が見つかりません")
        })?;
        *guard = Some(LoginBrowser::launch(&chrome, &profile, url.as_deref())?);
        Ok(())
    })
    .await?;
    // 開いているあいだだけ、Chrome を見ながら撮るキーを登録する
    shortcut::register(&app);
    Ok(())
}

/// ログイン用の Chrome が開いているか（撮影中で手が離せなければ開いているとみなす）。
/// ユーザーが Chrome を閉じていたら片付けて、撮るキーも外す。
#[tauri::command]
#[specta::specta]
pub fn capture_login_status(app: AppHandle, state: State<'_, AppState>) -> bool {
    let slot = state.login_slot();
    let open = match slot.try_lock() {
        Ok(mut guard) => {
            let open = guard.as_mut().is_some_and(LoginBrowser::alive);
            if !open && guard.is_some() {
                *guard = None;
            }
            open
        }
        Err(std::sync::TryLockError::WouldBlock) => true,
        Err(_) => false,
    };
    if !open {
        shortcut::unregister(&app);
    }
    open
}

/// ログイン用の Chrome の前面のタブを、見ている表示のまま撮って入れる。URL がルールに一致すれば、その撮り方
/// （要素のみ・隠す要素）を使う。`full_page` ならページ全体（隠す要素はルールのもの）。
#[tauri::command]
#[specta::specta]
pub async fn capture_login_snapshot(
    state: State<'_, AppState>,
    full_page: bool,
) -> Result<SnapshotResultDto, AppError> {
    let session = state.session()?;
    let slot = state.login_slot();
    blocking(move || login_snapshot(&session, &slot, full_page)).await
}

/// ログイン用の Chrome の前面のタブを撮って登録する（ボタンと ⌃⌥⌘C で共通）。
pub fn login_snapshot(
    session: &LibrarySession,
    slot: &Mutex<Option<LoginBrowser>>,
    full_page: bool,
) -> Result<SnapshotResultDto, AppError> {
    let mut guard = slot
        .lock()
        .map_err(|_| AppError::internal("ログイン用の Chrome の状態を読めません"))?;
    let closed = || {
        AppError::new(
            AppErrorCode::NotFound,
            "ログイン用の Chrome が開いていません",
        )
    };
    let browser = guard.as_mut().ok_or_else(closed)?;
    if !browser.alive() {
        return Err(closed());
    }
    let page = browser.front_page()?;
    let rule = session
        .library()
        .db()
        .read(|conn| capture::rules::find_for_url(conn, &page.url))?;
    let (mode, target, hide) = match (&rule, full_page) {
        (Some(r), false) => (r.mode, r.target_selector.clone(), r.hide_selectors.clone()),
        (Some(r), true) => (CaptureMode::FullPage, None, r.hide_selectors.clone()),
        (None, true) => (CaptureMode::FullPage, None, Vec::new()),
        (None, false) => (CaptureMode::Viewport, None, Vec::new()),
    };
    let shot = browser.shoot(&page, mode, target.as_deref(), &hide)?;
    drop(guard);
    if capture::job::host_of(&shot.url).is_none() {
        return Err(AppError::new(
            AppErrorCode::Invalid,
            "http / https のページだけ撮れます",
        ));
    }
    let staging = session.library().layout().staging_dir();
    std::fs::create_dir_all(&staging).map_err(|e| AppError::internal(e.to_string()))?;
    let path = staging.join(format!("login-{}.png", now_ms()));
    std::fs::write(&path, &shot.png).map_err(|e| AppError::internal(e.to_string()))?;
    let backend = kura_mac::media_backend();
    let result = register_png(
        session.library(),
        backend.as_ref(),
        &path,
        capture_meta(&shot.url, &shot.title, now_ms()),
        rule.as_ref().and_then(|r| r.dest_folder_id),
        rule.as_ref().map(|r| r.id),
        None,
    );
    let _ = std::fs::remove_file(&path);
    let result = result?;
    session.jobs().ensure_queued(JobKind::Thumbnail, "{}")?;
    Ok(SnapshotResultDto {
        title: result.title.unwrap_or_default(),
        url: shot.url,
        item_ids: result.item_ids,
        added: result.added,
        rule_name: rule.map(|r| r.name),
        truncated: shot.truncated,
    })
}

/// ログイン用の Chrome を閉じる。
#[tauri::command]
#[specta::specta]
pub async fn capture_login_close(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    shortcut::unregister(&app);
    let slot = state.login_slot();
    blocking(move || {
        let browser = slot.lock().ok().and_then(|mut guard| guard.take());
        drop(browser);
        Ok(())
    })
    .await
}

/// アプリ内ブラウザで URL を開く（開いていればそのウィンドウで移動する）。
#[tauri::command]
#[specta::specta]
pub async fn browser_open(app: AppHandle, url: String) -> Result<(), AppError> {
    let url = web_url(&url)?;
    if let Some(window) = app.get_webview_window(BROWSER) {
        window.navigate(url)?;
        window.set_focus()?;
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, BROWSER, WebviewUrl::External(url))
        .title("見たままキャプチャ")
        .inner_size(1200.0, 900.0)
        // 外部のページから file: や独自の scheme へは移らない
        .on_navigation(|url| matches!(url.scheme(), "http" | "https"))
        .build()?;
    Ok(())
}

/// アプリ内ブラウザで開いているページの URL（開いていなければ None）。
#[tauri::command]
#[specta::specta]
pub fn browser_status(app: AppHandle) -> Option<String> {
    let window = app.get_webview_window(BROWSER)?;
    window.url().ok().map(|u| u.to_string())
}

/// アプリ内ブラウザを閉じる。
#[tauri::command]
#[specta::specta]
pub fn browser_close(app: AppHandle) -> Result<(), AppError> {
    if let Some(window) = app.get_webview_window(BROWSER) {
        window.close()?;
    }
    Ok(())
}

/// アプリ内ブラウザの表示範囲を撮って、キャプチャとしてライブラリに入れる（見たままキャプチャ）。
#[tauri::command]
#[specta::specta]
pub async fn browser_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SnapshotResultDto, AppError> {
    let session = state.session()?;
    let window = app.get_webview_window(BROWSER).ok_or_else(|| {
        AppError::new(
            AppErrorCode::NotFound,
            "見たままキャプチャのウィンドウが開いていません",
        )
    })?;
    let staging = session.library().layout().staging_dir();
    std::fs::create_dir_all(&staging).map_err(|e| AppError::internal(e.to_string()))?;
    let path = staging.join(format!("snapshot-{}.png", now_ms()));
    let (tx, rx) = std::sync::mpsc::channel();
    let out = path.clone();
    window.with_webview(move |webview| {
        #[cfg(target_os = "macos")]
        {
            let done: kura_mac::SnapshotDone = Box::new(move |result| {
                let _ = tx.send(result);
            });
            // SAFETY: with_webview はメインスレッドで、生きている WKWebView を渡す
            unsafe { kura_mac::snapshot_webview(webview.inner(), &out, done) };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (webview, &out);
            let _ = tx.send(Err("この OS では撮れません".to_string()));
        }
    })?;
    blocking(move || {
        let outcome = rx
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| AppError::internal("撮影が終わりませんでした"))
            .and_then(|r| r.map_err(AppError::internal));
        let registered = outcome.and_then(|info| {
            if capture::job::host_of(&info.url).is_none() {
                return Err(AppError::new(
                    AppErrorCode::Invalid,
                    "http / https のページだけ撮れます",
                ));
            }
            let meta = capture_meta(&info.url, &info.title, now_ms());
            let backend = kura_mac::media_backend();
            let result = register_png(
                session.library(),
                backend.as_ref(),
                &path,
                meta,
                None,
                None,
                None,
            )?;
            session.jobs().ensure_queued(JobKind::Thumbnail, "{}")?;
            Ok(SnapshotResultDto {
                title: result.title.unwrap_or_default(),
                url: info.url,
                item_ids: result.item_ids,
                added: result.added,
                rule_name: None,
                truncated: false,
            })
        });
        let _ = std::fs::remove_file(&path);
        registered
    })
    .await
}
