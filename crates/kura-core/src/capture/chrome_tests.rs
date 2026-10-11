//! 本物の Google Chrome で撮るテスト（`cargo test -p kura-core capture -- --ignored`）。
//! ページは 127.0.0.1 の小さな HTTP サーバから出すので、外部には通信しない。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

use super::cdp::Cdp;
use super::chrome::ChromeProcess;
use super::rules::{CaptureMode, CaptureRule};
use super::shoot::{Pace, shoot};

const PAGE: &str = r#"<!doctype html><html><head><title>テストのページ</title>
<style>body{margin:0;font:16px sans-serif} .ad{height:300px;background:red} #target{width:400px;height:250px;margin:40px;background:#36c}</style>
</head><body><div class="ad">広告</div><div id="target">対象</div><div style="height:3000px"></div></body></html>"#;

/// 何度でも同じページを返す HTTP サーバ。
fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = PAGE.as_bytes();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    port
}

fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    (w, h)
}

struct RealPace;
impl Pace for RealPace {
    fn stop(&self) -> bool {
        false
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

fn rule(mode: CaptureMode) -> CaptureRule {
    CaptureRule {
        id: 1,
        name: "テスト".into(),
        url_pattern: "http://127.0.0.1*".into(),
        mode,
        target_selector: (mode == CaptureMode::Element).then(|| "#target".into()),
        hide_selectors: vec![".ad".into()],
        viewport_width: 1000,
        scroll_times: 1,
        wait_ms: 200,
        dest_folder_id: None,
        enabled: true,
        last_run_at: None,
        last_status: None,
    }
}

#[test]
#[ignore = "Google Chrome が要る"]
fn captures_element_full_page_and_viewport_with_real_chrome() {
    let chrome = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    assert!(chrome.is_file(), "Google Chrome がありません");
    let profile = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:{}/page", serve());
    let mut process = ChromeProcess::launch_headless(&chrome, profile.path()).unwrap();
    let mut cdp = Cdp::connect_from_profile(profile.path(), Duration::from_secs(20), &mut || {
        process.alive()
    })
    .unwrap();
    cdp.open_page().unwrap();

    let element = shoot(&mut cdp, &url, &rule(CaptureMode::Element), &RealPace).unwrap();
    assert_eq!(element.title, "テストのページ");
    // 2 倍で撮る。広告を隠したので要素は上に詰まる
    assert_eq!(png_size(&element.png), (800, 500));

    let full = shoot(&mut cdp, &url, &rule(CaptureMode::FullPage), &RealPace).unwrap();
    let (w, h) = png_size(&full.png);
    assert_eq!(w, 2000);
    assert!(h >= 2 * 3000, "{h}");

    let viewport = shoot(&mut cdp, &url, &rule(CaptureMode::Viewport), &RealPace).unwrap();
    assert_eq!(png_size(&viewport.png), (2000, 1800));
    drop(cdp);
    drop(process);
}
