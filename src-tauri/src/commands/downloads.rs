//! ダウンロードと外部ツール（docs/architecture.md §9）。

use std::path::PathBuf;
use std::time::Duration;

use kura_core::capture;
use kura_core::download::{self, DownloadInput, DownloadOptions};
use kura_core::jobs::{JobFilter, JobKind};
use kura_core::settings;
use kura_core::tools::{KNOWN_TOOLS, find, inspect, run_with_timeout};
use tauri::State;

use super::blocking;
use crate::dto::{
    CaptureMatchDto, DetectionDto, DownloadJobDto, DownloadOptionsDto, DownloadPrefsDto,
    ToolInfoDto, ToolUpdateOutputDto, ToolsUpdateDto,
};
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// URL の取得方法を判定する（キャプチャのルール → 取得の規則 → サイトの表）。
/// 前に同じものをダウンロードしていれば、そのときのことも返す（ADR-132）。
#[tauri::command]
#[specta::specta]
pub async fn download_detect(
    state: State<'_, AppState>,
    url: String,
) -> Result<DetectionDto, AppError> {
    let rules = state.download_prefs().rules;
    let session = state.current();
    blocking(move || {
        let d = download::detect(&url, &rules)?;
        // キャプチャのルールに一致すれば、ダウンロードより先に撮る（ADR）
        let (capture, previous) = match &session {
            Some(session) => session.library().db().read(|conn| {
                let capture =
                    capture::rules::find_for_url(conn, &d.url)?.map(|rule| CaptureMatchDto {
                        rule_id: rule.id,
                        rule_name: rule.name,
                        mode: rule.mode.into(),
                        chrome_found: find(capture::CHROME_TOOL).is_some(),
                    });
                let previous = match capture {
                    Some(_) => None,
                    None => download::history::find_previous(conn, &d.url)?.map(Into::into),
                };
                Ok((capture, previous))
            })?,
            None => (None, None),
        };
        Ok(DetectionDto {
            capture,
            previous,
            tool_found: find(d.tool.command()).is_some(),
            url: d.url,
            site: d.site,
            site_label: d.site_label,
            tool: d.tool.into(),
            rule: d.rule,
            playlist: d.playlist,
        })
    })
    .await
}

/// 前回の選択（ライブラリごと。なければ既定値）。
#[tauri::command]
#[specta::specta]
pub async fn download_defaults(state: State<'_, AppState>) -> Result<DownloadOptionsDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let options: Option<DownloadOptions> = session
            .library()
            .db()
            .read(|conn| settings::get(conn, settings::DOWNLOAD_DEFAULTS))?;
        Ok(options.unwrap_or_default().into())
    })
    .await
}

/// ダウンロードを積む。選んだ内容は次回の既定にする。`redownload` なら取得済みの記録にあるものも取り直す。
#[tauri::command]
#[specta::specta]
pub async fn download_enqueue(
    state: State<'_, AppState>,
    url: String,
    options: DownloadOptionsDto,
    redownload: bool,
) -> Result<i64, AppError> {
    let session = state.session()?;
    let prefs = state.download_prefs();
    blocking(move || {
        let detection = download::detect(&url, &prefs.rules)?;
        let options: DownloadOptions = options.into();
        let defaults = options.clone();
        session
            .library()
            .db()
            .write(move |tx| settings::set(tx, settings::DOWNLOAD_DEFAULTS, &defaults))?;
        let input = DownloadInput {
            detection,
            options,
            cookies: prefs.cookies,
            redownload,
        };
        Ok(download::start_download(session.jobs(), &input)?)
    })
    .await
}

/// ダウンロードとキャプチャのジョブを新しい順に一覧する。
#[tauri::command]
#[specta::specta]
pub async fn downloads_list(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<DownloadJobDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        let filter = JobFilter {
            kinds: vec![JobKind::Download, JobKind::Capture],
            states: Vec::new(),
            limit: Some(limit.unwrap_or(300)),
        };
        Ok(session
            .jobs()
            .list(&filter)?
            .into_iter()
            .filter_map(DownloadJobDto::from_record)
            .collect())
    })
    .await
}

/// ダウンロードの設定（Cookie の読み込み元・規則）。
#[tauri::command]
#[specta::specta]
pub fn download_prefs_get(state: State<'_, AppState>) -> DownloadPrefsDto {
    state.download_prefs().into()
}

/// ダウンロードの設定を保存する（規則の正規表現・引数を確かめる）。
#[tauri::command]
#[specta::specta]
pub fn download_prefs_set(
    state: State<'_, AppState>,
    prefs: DownloadPrefsDto,
) -> Result<DownloadPrefsDto, AppError> {
    Ok(state.set_download_prefs(prefs.into())?.into())
}

/// 外部ツールの状態（場所・版・Homebrew 版か）。
#[tauri::command]
#[specta::specta]
pub async fn tools_status() -> Result<Vec<ToolInfoDto>, AppError> {
    blocking(|| {
        // 版を読むのに 1 つ数百ミリ秒かかるので並べて調べる
        let infos = std::thread::scope(|scope| {
            let handles: Vec<_> = KNOWN_TOOLS
                .iter()
                .map(|name| scope.spawn(move || inspect(name)))
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .map(ToolInfoDto::from)
                .collect()
        });
        Ok(infos)
    })
    .await
}

/// 外部ツールの場所を指定する（`path` が None なら指定をやめて自動で探す）。
#[tauri::command]
#[specta::specta]
pub async fn tools_set_path(
    state: State<'_, AppState>,
    tool: String,
    path: Option<String>,
) -> Result<ToolInfoDto, AppError> {
    if !KNOWN_TOOLS.contains(&tool.as_str()) {
        return Err(AppError::new(
            AppErrorCode::Invalid,
            format!("知らないツールです: {tool}"),
        ));
    }
    let path = path.map(PathBuf::from);
    if let Some(p) = &path
        && !p.is_file()
    {
        return Err(AppError::new(
            AppErrorCode::NotFound,
            format!("ファイルが見つかりません: {}", p.display()),
        ));
    }
    state.set_tool_path(&tool, path)?;
    blocking(move || Ok(inspect(&tool).into())).await
}

/// 「更新を確認」：Homebrew 版は `brew upgrade` のコマンドを返し、単体版の yt-dlp は自分で更新する。
#[tauri::command]
#[specta::specta]
pub async fn tools_update() -> Result<ToolsUpdateDto, AppError> {
    blocking(|| {
        let mut brew = Vec::new();
        let mut outputs = Vec::new();
        for name in ["yt-dlp", "gallery-dl", "deno", "ffmpeg", "mpv"] {
            let info = inspect(name);
            let Some(path) = info.path else {
                continue;
            };
            if info.homebrew {
                brew.push(name);
                continue;
            }
            // 単体版：yt-dlp は自分で更新できる。gallery-dl は確認だけ
            let flag = match name {
                "yt-dlp" => "-U",
                "gallery-dl" => "--update-check",
                _ => continue,
            };
            let result = run_with_timeout(
                std::process::Command::new(&path).arg(flag),
                Duration::from_secs(120),
            );
            let (ok, text) = match result {
                Ok(out) => {
                    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&out.stderr));
                    (out.status.success(), text.trim().to_string())
                }
                Err(err) => (false, err.to_string()),
            };
            outputs.push(ToolUpdateOutputDto {
                tool: name.to_string(),
                ok,
                text,
            });
        }
        Ok(ToolsUpdateDto {
            brew_command: (!brew.is_empty()).then(|| format!("brew upgrade {}", brew.join(" "))),
            outputs,
        })
    })
    .await
}

/// 取得済みの記録の状態（yt-dlp・gallery-dl。設定画面）。
#[tauri::command]
#[specta::specta]
pub async fn download_archives(
    state: State<'_, AppState>,
) -> Result<Vec<crate::dto::ArchiveStatusDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(
            kura_core::download::archive::status(session.library().layout())
                .into_iter()
                .map(Into::into)
                .collect(),
        )
    })
    .await
}

/// 取得済みの記録を消す（そのツールで同じものを取り直せるようになる）。
#[tauri::command]
#[specta::specta]
pub async fn download_archive_clear(
    state: State<'_, AppState>,
    tool: crate::dto::ToolDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    blocking(move || {
        kura_core::download::archive::clear(session.library().layout(), tool.into())?;
        Ok(())
    })
    .await
}
