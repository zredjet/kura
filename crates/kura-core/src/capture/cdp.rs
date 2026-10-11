//! Chrome DevTools Protocol の小さなクライアント（docs/decisions.md の ADR：chromiumoxide の代わり）。
//!
//! - 接続先は Chrome が自分のプロファイルに書いた `DevToolsActivePort` の 127.0.0.1 のポートだけ。
//! - 1 本の WebSocket で、ブラウザに新しいページを作り（`Target.createTarget`）、そのページに
//!   `flatten` で付いたセッションへ命令を送る。イベントは読み捨てる（読み込みの完了などは問い合わせで確かめる）。
//! - 同期の WebSocket（tungstenite）なので、ジョブのスレッドからそのまま使える。

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::protocol::WebSocket;
use tungstenite::{Message, client};

use crate::{CoreError, Result};

/// 1 つの命令の返事を待つ時間の上限。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// ページへの命令（テストでは偽物に差し替える）。
pub trait Page {
    /// 命令を送り、結果（`result`）を返す。
    fn call(&mut self, method: &str, params: Value) -> Result<Value>;
}

/// `DevToolsActivePort`（1 行目にポート、2 行目にパス）から接続先を作る。127.0.0.1 だけ。
pub fn endpoint_from_port_file(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    if port == 0 || !path.starts_with("/devtools/browser/") || path.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

/// 接続先が 127.0.0.1 の ws:// か確かめ、ポートを返す。
fn local_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("ws://127.0.0.1:")?;
    let port = rest.split('/').next()?;
    port.parse().ok()
}

/// DevTools への接続。
pub struct Cdp {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    session: Option<String>,
    timeout: Duration,
}

impl Cdp {
    /// `ws://127.0.0.1:<port>/devtools/browser/<id>` に接続する。
    pub fn connect(url: &str, timeout: Duration) -> Result<Self> {
        let port = local_port(url).ok_or_else(|| {
            CoreError::Tool(format!("DevTools の接続先が正しくありません: {url}"))
        })?;
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let (socket, _) = client(url, stream)
            .map_err(|e| CoreError::Tool(format!("DevTools に接続できませんでした: {e}")))?;
        // 返事を待つあいだも区切りよく時間を確かめられるよう、読み取りは短く区切る
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(250)))?;
        Ok(Self {
            socket,
            next_id: 1,
            session: None,
            timeout,
        })
    }

    /// `DevToolsActivePort` が書かれるのを待って接続する。
    pub fn connect_from_profile(
        profile: &Path,
        wait: Duration,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Self> {
        let file = profile.join("DevToolsActivePort");
        let deadline = Instant::now() + wait;
        loop {
            if let Some(url) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| endpoint_from_port_file(&t))
            {
                return Self::connect(&url, CALL_TIMEOUT);
            }
            if !alive() {
                return Err(CoreError::Tool("Chrome が途中で終了しました".into()));
            }
            if Instant::now() >= deadline {
                return Err(CoreError::Tool(
                    "Chrome が起動しませんでした（DevTools の準備ができません）".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 新しい空のページを作って、以後の命令の送り先にする。
    pub fn open_page(&mut self) -> Result<()> {
        let created = self.send(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
        let target = created
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Tool("ページを作れませんでした".into()))?
            .to_string();
        let attached = self.send(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )?;
        let session = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Tool("ページに接続できませんでした".into()))?;
        self.session = Some(session.to_string());
        Ok(())
    }

    /// 命令を送り、同じ ID の返事を待つ（その間のイベントは読み捨てる）。
    fn send(&mut self, session: Option<String>, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = Value::String(session);
        }
        self.socket
            .send(Message::text(message.to_string()))
            .map_err(|e| CoreError::Tool(format!("DevTools に送れませんでした: {e}")))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    if let Some(result) = response_for(id, method, text.as_str())? {
                        return Ok(result);
                    }
                }
                Ok(Message::Close(_)) => {
                    return Err(CoreError::Tool("DevTools の接続が切れました".into()));
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => {
                    return Err(CoreError::Tool(format!(
                        "DevTools から読めませんでした: {e}"
                    )));
                }
            }
            if Instant::now() >= deadline {
                return Err(CoreError::Tool(format!("{method} の返事がありません")));
            }
        }
    }
}

impl Page for Cdp {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let session = self.session.clone();
        self.send(session, method, params)
    }
}

/// 届いた文が `id` の返事なら結果を返す（エラーならエラー）。イベント・ほかの返事は None。
fn response_for(id: u64, method: &str, text: &str) -> Result<Option<Value>> {
    let value: Value = serde_json::from_str(text)?;
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Ok(None);
    }
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("不明なエラー");
        return Err(CoreError::Tool(format!("{method}: {message}")));
    }
    Ok(Some(value.get("result").cloned().unwrap_or(Value::Null)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn reads_the_port_file() {
        assert_eq!(
            endpoint_from_port_file("53412\n/devtools/browser/abc-123\n").as_deref(),
            Some("ws://127.0.0.1:53412/devtools/browser/abc-123")
        );
        assert_eq!(endpoint_from_port_file("0\n/devtools/browser/x"), None);
        assert_eq!(endpoint_from_port_file("1\n/other/x"), None);
        assert_eq!(endpoint_from_port_file("x\n/devtools/browser/x"), None);
        assert_eq!(endpoint_from_port_file("5"), None);
    }

    #[test]
    fn only_connects_to_localhost() {
        assert_eq!(
            local_port("ws://127.0.0.1:9222/devtools/browser/x"),
            Some(9222)
        );
        assert_eq!(local_port("ws://example.com:9222/devtools/browser/x"), None);
        assert_eq!(local_port("ws://127.0.0.1.evil:9222/x"), None);
        assert!(Cdp::connect("ws://10.0.0.1:9222/devtools/browser/x", CALL_TIMEOUT).is_err());
    }

    #[test]
    fn matches_responses_and_errors() {
        assert_eq!(
            response_for(3, "m", r#"{"method":"Page.loadEventFired"}"#).unwrap(),
            None
        );
        assert_eq!(
            response_for(3, "m", r#"{"id":2,"result":{}}"#).unwrap(),
            None
        );
        assert_eq!(
            response_for(3, "m", r#"{"id":3,"result":{"a":1}}"#).unwrap(),
            Some(json!({"a":1}))
        );
        let err = response_for(3, "Page.navigate", r#"{"id":3,"error":{"message":"bad"}}"#)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "Page.navigate: bad");
    }

    /// 本物の WebSocket で、イベントをはさんだ返事・セッションを確かめる（Chrome の代わりの小さなサーバ）。
    #[test]
    fn talks_to_a_devtools_like_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut seen = Vec::new();
            loop {
                let Ok(Message::Text(text)) = ws.read() else {
                    break;
                };
                let msg: Value = serde_json::from_str(text.as_str()).unwrap();
                let id = msg["id"].as_u64().unwrap();
                seen.push((
                    msg["method"].as_str().unwrap().to_string(),
                    msg.get("sessionId").cloned(),
                ));
                let result = match msg["method"].as_str().unwrap() {
                    "Target.createTarget" => json!({"targetId": "T1"}),
                    "Target.attachToTarget" => json!({"sessionId": "S1"}),
                    _ => json!({"result": {"value": "done"}}),
                };
                ws.send(Message::text(json!({"method": "Some.event"}).to_string()))
                    .unwrap();
                ws.send(Message::text(
                    json!({"id": id, "result": result}).to_string(),
                ))
                .unwrap();
                if seen.len() == 3 {
                    break;
                }
            }
            seen
        });
        let mut cdp = Cdp::connect(
            &format!("ws://127.0.0.1:{port}/devtools/browser/test"),
            Duration::from_secs(5),
        )
        .unwrap();
        cdp.open_page().unwrap();
        let out = cdp
            .call("Runtime.evaluate", json!({"expression": "1"}))
            .unwrap();
        assert_eq!(out["result"]["value"], "done");
        let seen = server.join().unwrap();
        assert_eq!(
            seen,
            vec![
                ("Target.createTarget".to_string(), None),
                ("Target.attachToTarget".to_string(), None),
                ("Runtime.evaluate".to_string(), Some(json!("S1"))),
            ]
        );
    }
}
