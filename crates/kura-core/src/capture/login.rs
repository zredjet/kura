//! ログイン用の Chrome（画面あり）。開いているタブを、見ている表示のまま撮れる（ADR-111）。
//!
//! キャプチャと同じ専用プロファイルで起動し、DevTools は 127.0.0.1 の空いているポートで開く。アプリは
//! プロファイルに書かれた `DevToolsActivePort` にだけつなぐ。開いているあいだは同じユーザーのほかのプロセスも
//! そのポートにつなげるので、アプリを閉じるとき・「閉じる」で、この Chrome も閉じる。

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::cdp::{Cdp, Page};
use super::chrome::{login_args, profile_in_use};
use super::rules::CaptureMode;
use super::shoot::{Shot, shoot_live};
use crate::tools::{self, terminate_group};
use crate::{CoreError, Result};

/// 起動して DevTools の準備ができるまで待つ時間。
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// 開いているタブ（撮る候補）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPage {
    pub target_id: String,
    pub url: String,
}

/// アプリが開いて、DevTools でつないでいるログイン用の Chrome。手放すと閉じる。
pub struct LoginBrowser {
    child: Child,
    cdp: Cdp,
}

impl LoginBrowser {
    /// 専用プロファイルで画面ありの Chrome を開き、DevTools につなぐ。
    pub fn launch(chrome: &Path, profile: &Path, url: Option<&str>) -> Result<Self> {
        Self::start(chrome, profile, login_args(profile, url))
    }

    fn start(chrome: &Path, profile: &Path, args: Vec<std::ffi::OsString>) -> Result<Self> {
        if profile_in_use(profile) {
            return Err(CoreError::Tool(
                "同じプロファイルの Chrome がすでに開いています。閉じてからもう一度開いてください"
                    .into(),
            ));
        }
        std::fs::create_dir_all(profile)?;
        let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
        let mut child = tools::spawn(
            Command::new(chrome)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
        .map_err(|e| CoreError::Tool(format!("Chrome を起動できませんでした: {e}")))?;
        let connected = Cdp::connect_from_profile(profile, START_TIMEOUT, &mut || {
            matches!(child.try_wait(), Ok(None))
        });
        match connected {
            Ok(cdp) => Ok(Self { child, cdp }),
            Err(err) => {
                terminate_group(&mut child);
                Err(err)
            }
        }
    }

    /// Chrome のプロセス ID（前面のアプリかどうかを確かめるのに使う）。
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// まだ開いているか（ユーザーが閉じたら false）。
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 新しいタブで URL を開く。
    pub fn open_url(&mut self, url: &str) -> Result<()> {
        self.cdp
            .browser_call("Target.createTarget", json!({ "url": url }))?;
        Ok(())
    }

    /// 撮る候補のタブ（http / https のページ）。
    fn pages(&mut self) -> Result<Vec<OpenPage>> {
        let targets = self.cdp.browser_call("Target.getTargets", json!({}))?;
        Ok(targets
            .get("targetInfos")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                    .filter_map(|t| {
                        let url = t.get("url").and_then(Value::as_str)?;
                        let id = t.get("targetId").and_then(Value::as_str)?;
                        (url.starts_with("https://") || url.starts_with("http://")).then(|| {
                            OpenPage {
                                target_id: id.to_string(),
                                url: url.to_string(),
                            }
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 前面のタブ（表示されていて、フォーカスがあるもの。なければ表示されているもの、なければ最初）。
    pub fn front_page(&mut self) -> Result<OpenPage> {
        let pages = self.pages()?;
        let mut best: Option<(u8, OpenPage)> = None;
        for page in pages {
            self.cdp.attach(&page.target_id)?;
            let state = self.cdp.call(
                "Runtime.evaluate",
                json!({ "expression": "[document.visibilityState === 'visible', document.hasFocus()]", "returnByValue": true }),
            );
            self.cdp.detach()?;
            let value = state
                .ok()
                .and_then(|v| v.pointer("/result/value").cloned())
                .unwrap_or(Value::Null);
            let visible = value.get(0).and_then(Value::as_bool) == Some(true);
            let focused = value.get(1).and_then(Value::as_bool) == Some(true);
            let score = u8::from(visible) * 2 + u8::from(visible && focused);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, page));
            }
        }
        best.map(|(_, page)| page).ok_or_else(|| {
            CoreError::Tool(
                "ログイン用の Chrome に撮れるページがありません（http / https のページを開いてください）".into(),
            )
        })
    }

    /// タブを、見ている表示のまま撮る。
    pub fn shoot(
        &mut self,
        page: &OpenPage,
        mode: CaptureMode,
        target: Option<&str>,
        hide: &[String],
    ) -> Result<Shot> {
        self.cdp.attach(&page.target_id)?;
        let shot = shoot_live(&mut self.cdp, mode, target, hide);
        let _ = self.cdp.detach();
        shot
    }

    /// 閉じる（Chrome に閉じてもらい、5 秒たっても終わらなければ止める）。
    pub fn close(&mut self) {
        if !self.alive() {
            return;
        }
        let _ = self.cdp.browser_call("Browser.close", json!({}));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !self.alive() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        terminate_group(&mut self.child);
    }
}

impl Drop for LoginBrowser {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 何度でも同じページを返す 127.0.0.1 の HTTP サーバ。
    fn serve(page: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
            }
        });
        port
    }

    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        (
            u32::from_be_bytes(png[16..20].try_into().unwrap()),
            u32::from_be_bytes(png[20..24].try_into().unwrap()),
        )
    }

    #[test]
    #[ignore = "Google Chrome が要る"]
    fn shoots_the_open_tab_and_closes() {
        const PAGE: &str = "<!doctype html><title>開いているページ</title><body style=margin:0>\
            <div class=ad style=height:100px>広告</div><div id=t style=width:300px;height:120px>対象</div>\
            <div style=height:2000px></div></body>";
        let chrome = std::path::PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        );
        assert!(chrome.is_file());
        let profile = tempfile::tempdir().unwrap();
        let url = format!("http://127.0.0.1:{}/page", serve(PAGE));
        // 画面を出さないよう、テストではヘッドレスで
        let mut args = login_args(profile.path(), None);
        args.insert(0, "--headless".into());
        args.push("--window-size=800,600".into());
        let mut browser = LoginBrowser::start(&chrome, profile.path(), args).unwrap();
        assert!(profile_in_use(profile.path()));
        browser.open_url(&url).unwrap();
        // 読み込まれるまで待つ
        let deadline = Instant::now() + Duration::from_secs(20);
        let page = loop {
            if let Ok(page) = browser.front_page()
                && page.url == url
            {
                break page;
            }
            assert!(Instant::now() < deadline, "ページが開きません");
            std::thread::sleep(Duration::from_millis(200));
        };
        std::thread::sleep(Duration::from_millis(500));

        let shot = browser
            .shoot(&page, CaptureMode::Element, Some("#t"), &[".ad".into()])
            .unwrap();
        assert_eq!(
            (shot.title.as_str(), shot.url.as_str()),
            ("開いているページ", url.as_str())
        );
        assert_eq!(png_size(&shot.png), (300, 120));
        let full = browser
            .shoot(&page, CaptureMode::FullPage, None, &[])
            .unwrap();
        assert!(png_size(&full.png).1 >= 2220);
        let view = browser
            .shoot(&page, CaptureMode::Viewport, None, &[])
            .unwrap();
        assert_eq!(png_size(&view.png).0, 800);

        browser.close();
        assert!(!browser.alive());
    }
}
