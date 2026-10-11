//! 撮影の手順（docs/architecture.md §10 の 1〜4）。DevTools への命令は `Page` を通すので、偽物で確かめられる。
//!
//! ページで動かす JavaScript は決まった形だけで、ルールのセレクタは JSON の文字列として埋め込む
//! （文字列をそのままスクリプトにつながない）。

use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use super::cdp::Page;
use super::rules::{CaptureMode, CaptureRule};
use crate::{CoreError, Result};

/// 表示範囲の高さ（幅はルールで決める）。
pub const VIEWPORT_HEIGHT: i64 = 900;
/// 撮影の解像度の倍率（Retina で文字がにじまないように）。長すぎるページは 1 倍に下げる。
pub const SCALE: f64 = 2.0;
/// 1 枚の画像の高さの上限（px。Chrome が撮れる大きさ）。
pub const MAX_PIXELS: f64 = 16_384.0;
/// 読み込みの完了を待つ時間の上限。
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// 問い合わせの間隔。
const POLL: Duration = Duration::from_millis(200);
/// スクロールのあと、遅延読み込みを待つ時間。
const SCROLL_PAUSE: Duration = Duration::from_millis(800);

/// 撮った画像。
#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub png: Vec<u8>,
    pub title: String,
    /// 高さが上限を超えて途中までになったか。
    pub truncated: bool,
}

/// 撮影の途中で確かめること（止める指示・待ち）。
pub trait Pace {
    /// 止める指示が出ていれば true。
    fn stop(&self) -> bool;
    /// 待つ（テストでは待たない）。
    fn sleep(&self, d: Duration);
}

/// JavaScript の文字列リテラル（JSON の文字列として正しくエスケープする）。
fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// 式を評価して値を返す（例外ならエラー）。
fn evaluate(page: &mut dyn Page, expression: &str) -> Result<Value> {
    let out = page.call(
        "Runtime.evaluate",
        json!({ "expression": expression, "returnByValue": true }),
    )?;
    if let Some(details) = out.get("exceptionDetails") {
        let text = details
            .pointer("/exception/description")
            .or_else(|| details.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("スクリプトのエラー");
        return Err(CoreError::Tool(format!("ページで失敗しました: {text}")));
    }
    Ok(out.pointer("/result/value").cloned().unwrap_or(Value::Null))
}

fn check(pace: &dyn Pace) -> Result<()> {
    if pace.stop() {
        Err(CoreError::Invalid("止めました".into()))
    } else {
        Ok(())
    }
}

/// 隠す要素の CSS セレクタを、見つけた要素ごとに `display: none` にするスクリプト。読めないセレクタは飛ばす。
pub fn hide_script(selectors: &[String]) -> String {
    let list = serde_json::to_string(selectors).unwrap_or_else(|_| "[]".to_string());
    format!(
        "(() => {{ for (const s of {list}) {{ try {{ document.querySelectorAll(s).forEach(e => \
         e.style.setProperty('display', 'none', 'important')); }} catch (e) {{}} }} return true; }})()"
    )
}

/// 要素の位置と大きさ（ページの先頭から）を返すスクリプト。見つからなければ null。
pub fn rect_script(selector: &str) -> String {
    format!(
        "(() => {{ const e = document.querySelector({}); if (!e) return null; \
         const r = e.getBoundingClientRect(); \
         return {{ x: r.left + window.scrollX, y: r.top + window.scrollY, width: r.width, height: r.height }}; }})()",
        js_string(selector)
    )
}

/// URL のページを `rule` のとおりに撮る。
pub fn shoot(page: &mut dyn Page, url: &str, rule: &CaptureRule, pace: &dyn Pace) -> Result<Shot> {
    // 1. ルールの幅で開く
    page.call(
        "Emulation.setDeviceMetricsOverride",
        json!({ "width": rule.viewport_width, "height": VIEWPORT_HEIGHT, "deviceScaleFactor": 1, "mobile": false }),
    )?;
    let nav = page.call("Page.navigate", json!({ "url": url }))?;
    if let Some(error) = nav.get("errorText").and_then(Value::as_str) {
        return Err(CoreError::Tool(format!(
            "ページを開けませんでした（{error}）"
        )));
    }
    let mut waited = Duration::ZERO;
    loop {
        check(pace)?;
        if evaluate(page, "document.readyState")?.as_str() == Some("complete") {
            break;
        }
        if waited >= LOAD_TIMEOUT {
            return Err(CoreError::Tool(
                "ページの読み込みが終わりませんでした".into(),
            ));
        }
        pace.sleep(POLL);
        waited += POLL;
    }

    // 1'. wait_ms 待つ（要素のみなら、その要素が出たらそこまで）
    let wait = Duration::from_millis(u64::try_from(rule.wait_ms).unwrap_or(0));
    match (&rule.mode, &rule.target_selector) {
        (CaptureMode::Element, Some(selector)) => {
            let probe = format!("document.querySelector({}) !== null", js_string(selector));
            let mut waited = Duration::ZERO;
            while evaluate(page, &probe)?.as_bool() != Some(true) && waited < wait {
                check(pace)?;
                pace.sleep(POLL);
                waited += POLL;
            }
        }
        _ => pace.sleep(wait),
    }
    check(pace)?;

    // 2. 隠す
    if !rule.hide_selectors.is_empty() {
        evaluate(page, &hide_script(&rule.hide_selectors))?;
    }
    // 3. スクロールして遅延読み込みを起こし、先頭に戻す
    for _ in 0..rule.scroll_times {
        check(pace)?;
        evaluate(
            page,
            "window.scrollTo(0, (document.scrollingElement || document.body).scrollHeight)",
        )?;
        pace.sleep(SCROLL_PAUSE);
    }
    if rule.scroll_times > 0 {
        evaluate(page, "window.scrollTo(0, 0)")?;
        // 遅延読み込みで出てきた要素も隠す
        if !rule.hide_selectors.is_empty() {
            evaluate(page, &hide_script(&rule.hide_selectors))?;
        }
        pace.sleep(Duration::from_millis(300));
    }
    check(pace)?;

    // 4. 撮る
    let mut area = match rule.mode {
        CaptureMode::Viewport => clip(0.0, 0.0, rule.viewport_width as f64, VIEWPORT_HEIGHT as f64),
        CaptureMode::FullPage => {
            let metrics = page.call("Page.getLayoutMetrics", json!({}))?;
            let size = metrics
                .get("cssContentSize")
                .or_else(|| metrics.get("contentSize"))
                .cloned()
                .unwrap_or(Value::Null);
            let width = size
                .get("width")
                .and_then(Value::as_f64)
                .unwrap_or(rule.viewport_width as f64);
            let height = size
                .get("height")
                .and_then(Value::as_f64)
                .unwrap_or(VIEWPORT_HEIGHT as f64);
            clip(0.0, 0.0, width, height)
        }
        CaptureMode::Element => {
            let selector = rule.target_selector.as_deref().unwrap_or("");
            let rect = evaluate(page, &rect_script(selector))?;
            let (Some(x), Some(y), Some(w), Some(h)) = (
                rect.get("x").and_then(Value::as_f64),
                rect.get("y").and_then(Value::as_f64),
                rect.get("width").and_then(Value::as_f64),
                rect.get("height").and_then(Value::as_f64),
            ) else {
                return Err(CoreError::Tool(format!(
                    "対象の要素（{selector}）が見つかりませんでした"
                )));
            };
            if w < 1.0 || h < 1.0 {
                return Err(CoreError::Tool(format!(
                    "対象の要素（{selector}）の大きさが 0 です"
                )));
            }
            clip(x, y, w, h)
        }
    };
    // 倍率をかけても上限を超えないように。超えるなら 1 倍に、それでも超えるなら途中まで
    let height = area["height"].as_f64().unwrap_or(0.0);
    let scale = if height * SCALE <= MAX_PIXELS {
        SCALE
    } else {
        1.0
    };
    let truncated = height * scale > MAX_PIXELS;
    if truncated {
        area["height"] = json!(MAX_PIXELS / scale);
    }
    area["scale"] = json!(scale);
    let shot = page.call(
        "Page.captureScreenshot",
        json!({ "format": "png", "clip": area, "captureBeyondViewport": true, "fromSurface": true }),
    )?;
    let data = shot
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Tool("撮影の結果がありません".into()))?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| CoreError::Tool(format!("撮影の結果を読めませんでした: {e}")))?;
    let title = evaluate(page, "document.title")?
        .as_str()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    Ok(Shot {
        png,
        title,
        truncated,
    })
}

fn clip(x: f64, y: f64, width: f64, height: f64) -> Value {
    json!({ "x": x.max(0.0), "y": y.max(0.0), "width": width.max(1.0), "height": height.max(1.0), "scale": 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// 送られた命令を記録し、決めた返事を返すページ。
    struct FakePage {
        calls: Vec<(String, Value)>,
        /// Runtime.evaluate の返事（順に）。尽きたら null。
        values: VecDeque<Value>,
        layout_height: f64,
    }

    impl Page for FakePage {
        fn call(&mut self, method: &str, params: Value) -> Result<Value> {
            self.calls.push((method.to_string(), params));
            Ok(match method {
                "Runtime.evaluate" => {
                    json!({ "result": { "value": self.values.pop_front().unwrap_or(Value::Null) } })
                }
                "Page.getLayoutMetrics" => {
                    json!({ "cssContentSize": { "width": 1200.0, "height": self.layout_height } })
                }
                "Page.captureScreenshot" => json!({ "data": "iVBORw0KGgo=" }),
                _ => json!({}),
            })
        }
    }

    struct NoWait {
        slept: RefCell<Duration>,
        stop_after: Option<usize>,
        checks: RefCell<usize>,
    }

    impl Pace for NoWait {
        fn stop(&self) -> bool {
            *self.checks.borrow_mut() += 1;
            self.stop_after.is_some_and(|n| *self.checks.borrow() > n)
        }
        fn sleep(&self, d: Duration) {
            *self.slept.borrow_mut() += d;
        }
    }

    fn pace() -> NoWait {
        NoWait {
            slept: RefCell::new(Duration::ZERO),
            stop_after: None,
            checks: RefCell::new(0),
        }
    }

    fn rule(mode: CaptureMode) -> CaptureRule {
        CaptureRule {
            id: 1,
            name: "r".into(),
            url_pattern: "https://example.com/*".into(),
            mode,
            target_selector: (mode == CaptureMode::Element).then(|| r#"div[data-x="a'b"]"#.into()),
            hide_selectors: vec!["header".into(), ".ad".into()],
            viewport_width: 800,
            scroll_times: 2,
            wait_ms: 1000,
            dest_folder_id: None,
            enabled: true,
            last_run_at: None,
            last_status: None,
        }
    }

    fn methods(page: &FakePage) -> Vec<&str> {
        page.calls.iter().map(|(m, _)| m.as_str()).collect()
    }

    fn capture_params(page: &FakePage) -> &Value {
        &page
            .calls
            .iter()
            .find(|(m, _)| m == "Page.captureScreenshot")
            .unwrap()
            .1
    }

    #[test]
    fn full_page_follows_the_steps() {
        let mut page = FakePage {
            calls: vec![],
            // 読み込み中 → 完了、隠す・スクロール ×2・先頭へ・もう一度隠す、題名
            values: VecDeque::from(vec![
                json!("loading"),
                json!("complete"),
                json!(true),
                json!(null),
                json!(null),
                json!(null),
                json!(true),
                json!(" 記事の題名 "),
            ]),
            layout_height: 3000.0,
        };
        let p = pace();
        let shot = shoot(
            &mut page,
            "https://example.com/a",
            &rule(CaptureMode::FullPage),
            &p,
        )
        .unwrap();
        assert_eq!(shot.title, "記事の題名");
        assert_eq!(
            shot.png,
            base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgo=")
                .unwrap()
        );
        assert!(!shot.truncated);
        assert_eq!(
            methods(&page),
            [
                "Emulation.setDeviceMetricsOverride",
                "Page.navigate",
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Page.getLayoutMetrics",
                "Page.captureScreenshot",
                "Runtime.evaluate",
            ]
        );
        assert_eq!(page.calls[0].1["width"], 800);
        assert_eq!(page.calls[1].1["url"], "https://example.com/a");
        let clip = &capture_params(&page)["clip"];
        assert_eq!(
            (clip["height"].as_f64(), clip["scale"].as_f64()),
            (Some(3000.0), Some(2.0))
        );
        assert_eq!(capture_params(&page)["captureBeyondViewport"], true);
        // wait_ms とスクロールの待ちを待った
        assert!(*p.slept.borrow() >= Duration::from_millis(1000 + 2 * 800));
    }

    #[test]
    fn very_long_pages_drop_the_scale_and_are_cut() {
        let mut page = FakePage {
            calls: vec![],
            values: VecDeque::from(vec![json!("complete")]),
            layout_height: 10_000.0,
        };
        let mut r = rule(CaptureMode::FullPage);
        r.hide_selectors.clear();
        r.scroll_times = 0;
        let shot = shoot(&mut page, "https://example.com/a", &r, &pace()).unwrap();
        assert!(!shot.truncated);
        assert_eq!(capture_params(&page)["clip"]["scale"].as_f64(), Some(1.0));

        let mut page = FakePage {
            calls: vec![],
            values: VecDeque::from(vec![json!("complete")]),
            layout_height: 40_000.0,
        };
        let shot = shoot(&mut page, "https://example.com/a", &r, &pace()).unwrap();
        assert!(shot.truncated);
        assert_eq!(
            capture_params(&page)["clip"]["height"].as_f64(),
            Some(MAX_PIXELS)
        );
    }

    #[test]
    fn element_mode_waits_for_and_clips_the_element() {
        let mut page = FakePage {
            calls: vec![],
            values: VecDeque::from(vec![
                json!("complete"),
                json!(false),
                json!(true),
                json!(true),
                json!(null),
                json!(null),
                json!(null),
                json!(true),
                json!({ "x": 10.5, "y": 200.0, "width": 600.0, "height": 400.0 }),
                json!("投稿"),
            ]),
            layout_height: 0.0,
        };
        let shot = shoot(
            &mut page,
            "https://example.com/u/status/1",
            &rule(CaptureMode::Element),
            &pace(),
        )
        .unwrap();
        assert_eq!(shot.title, "投稿");
        let clip = &capture_params(&page)["clip"];
        assert_eq!(
            (
                clip["x"].as_f64(),
                clip["y"].as_f64(),
                clip["width"].as_f64(),
                clip["height"].as_f64()
            ),
            (Some(10.5), Some(200.0), Some(600.0), Some(400.0))
        );
        // セレクタは JSON の文字列として埋め込む
        let probe = page.calls[3].1["expression"].as_str().unwrap();
        assert_eq!(
            probe,
            r#"document.querySelector("div[data-x=\"a'b\"]") !== null"#
        );
    }

    #[test]
    fn missing_elements_and_navigation_errors_are_explained() {
        let mut page = FakePage {
            calls: vec![],
            values: VecDeque::from(vec![json!("complete")]),
            layout_height: 0.0,
        };
        let mut r = rule(CaptureMode::Element);
        r.wait_ms = 0;
        r.scroll_times = 0;
        r.hide_selectors.clear();
        let err = shoot(&mut page, "https://example.com/a", &r, &pace())
            .unwrap_err()
            .to_string();
        assert!(err.contains("見つかりませんでした"), "{err}");

        struct Broken;
        impl Page for Broken {
            fn call(&mut self, method: &str, _: Value) -> Result<Value> {
                Ok(if method == "Page.navigate" {
                    json!({ "errorText": "net::ERR_NAME_NOT_RESOLVED" })
                } else {
                    json!({})
                })
            }
        }
        let err = shoot(&mut Broken, "https://nowhere.invalid/", &r, &pace())
            .unwrap_err()
            .to_string();
        assert!(err.contains("ERR_NAME_NOT_RESOLVED"), "{err}");
    }

    #[test]
    fn stops_when_asked() {
        let mut page = FakePage {
            calls: vec![],
            values: VecDeque::new(),
            layout_height: 0.0,
        };
        let p = NoWait {
            stop_after: Some(0),
            ..pace()
        };
        assert!(
            shoot(
                &mut page,
                "https://example.com/a",
                &rule(CaptureMode::Viewport),
                &p
            )
            .is_err()
        );
        assert!(!methods(&page).contains(&"Page.captureScreenshot"));
    }

    #[test]
    fn scripts_escape_selectors() {
        let s = hide_script(&["a'); alert(1); ('".into()]);
        assert!(s.contains(r#"["a'); alert(1); ('"]"#), "{s}");
        let r = rect_script("</script>\"x");
        assert!(
            r.contains(r#"document.querySelector("</script>\"x")"#),
            "{r}"
        );
    }
}
