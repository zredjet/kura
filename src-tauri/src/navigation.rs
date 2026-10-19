//! main の WebView の移動を、アプリの origin の中に限る（docs/architecture.md §11）。
//!
//! main を kura:// の SVG などへ移すと、そのページのスクリプトから invoke が通ってしまう（M11 の調べで実測）。
//! そこで tauri.conf.json の main の窓は `create: false` にし、setup で設定のまま（titleBarStyle などを落とさず）
//! `on_navigation` を付けて作る。WKWebView は iframe の読み込みにも同じ判定を呼ぶので、枠も開かない。

use kura_core::serve::{AppOrigin, MAIN_WEBVIEW};
use tauri::{App, Url, WebviewWindowBuilder};

/// tauri.conf.json の main の窓を、移動の制限を付けて作る（Tauri が設定の窓を作るときと同じ `from_config`）。
pub fn create_main_window(app: &App, origin: AppOrigin) -> Result<(), Box<dyn std::error::Error>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == MAIN_WEBVIEW)
        .cloned()
        .ok_or("tauri.conf.json に main の窓がありません")?;
    WebviewWindowBuilder::from_config(app.handle(), &config)?
        .on_navigation(move |url| allow(&origin, url))
        .build()?;
    Ok(())
}

/// main の移動を許すか（アプリの origin の中だけ）。止めたものはログに残す。
fn allow(origin: &AppOrigin, url: &Url) -> bool {
    let ok = origin.allows_navigation(url.as_str());
    if !ok {
        // data: の URL などは長いので、先頭だけを残す（Url の文字列は ASCII）
        let shown = url.as_str().get(..200).unwrap_or(url.as_str());
        tracing::warn!(url = shown, "main の WebView の移動を止めました");
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn main_navigates_only_within_the_app() {
        let release = AppOrigin::new(false, Some("http://localhost:1430/"));
        let dev = AppOrigin::new(true, Some("http://localhost:1430/"));
        // WebKit から来る形（tauri::Url に読んだもの）で確かめる
        assert!(allow(&release, &url("tauri://localhost")));
        assert!(allow(&release, &url("tauri://localhost/index.html")));
        assert!(allow(&dev, &url("http://localhost:1430/")));
        assert!(allow(&dev, &url("http://localhost:1430/?t=1#x")));
        assert!(!allow(&release, &url("http://localhost:1430/")));
        assert!(!allow(&dev, &url("tauri://localhost")));
        for app in [&release, &dev] {
            for bad in [
                "kura://localhost/file/0000000000000000000000000000000000000000000000000000000000000000",
                "https://example.com/",
                "http://localhost:1431/",
                "file:///etc/passwd",
                "about:blank",
                "about:srcdoc",
                "data:text/html,<script>1</script>",
                "blob:tauri://localhost/1",
            ] {
                assert!(!allow(app, &url(bad)), "{} → {bad}", app.as_str());
            }
        }
    }

    #[test]
    fn main_window_is_built_with_navigation_guard() {
        // Tauri が読むのと同じ型で、main の窓が自動では作られず（setup で作る）、属性が残っていることを見る
        let config: tauri::utils::config::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let mains: Vec<_> = config
            .app
            .windows
            .iter()
            .filter(|w| w.label == MAIN_WEBVIEW)
            .collect();
        assert_eq!(mains.len(), 1);
        let main = mains[0];
        assert!(
            !main.create,
            "main を Tauri に作らせると on_navigation が付きません"
        );
        assert_eq!(main.title, "Kura");
        assert_eq!(main.title_bar_style, tauri::utils::TitleBarStyle::Overlay);
        assert!(main.hidden_title);
        let position = main.traffic_light_position.as_ref().unwrap();
        assert_eq!((position.x, position.y), (25.0, 35.0));
        assert!(main.background_color.is_some());
        assert_eq!(
            main.background_throttling,
            Some(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
        );
        // 移動の制限を付けて作る窓が 1 つだけで、ほかの窓を設定から自動で作らない
        assert!(config.app.windows.iter().all(|w| !w.create));
    }
}
