//! DevTools の WebSocket のクライアント（RFC 6455。M16 段 9。ADR-226。前は tungstenite）。接続先は 127.0.0.1 の Chrome だけ
//! （`cdp`）なので、使う形だけを持つ：
//!
//! - 握手：`GET <path>` に `Upgrade: websocket`・`Connection: Upgrade`・`Sec-WebSocket-Key`（16 バイトの乱数の base64）・
//!   `Sec-WebSocket-Version: 13` を付けて送り、返事は 101 で、`Upgrade` が websocket、`Connection` に Upgrade があり、
//!   `Sec-WebSocket-Accept` が鍵と GUID の SHA-1 の base64 と同じこと。頼んでいないサブプロトコル・拡張の返事は断る。
//!   頭は 16 KiB・124 行まで。頭のあとに届いたバイトはフレームとして読む。
//! - 送る：テキストのフレーム 1 つ（FIN つき）。マスクの鍵は ChaCha8（種は `/dev/urandom` の 32 バイト。`random` の決まり
//!   どおり ChaCha8 だけを使う）。
//! - 読む：テキスト・バイナリ・続き（分割）・ping（すぐ pong を返す）・pong・close（close を返して閉じる）。予約のビット・
//!   マスクしたサーバーのフレーム・125 バイトを超えるか分割した制御のフレーム・知らない種類・順の合わない続き・壊れた
//!   UTF-8・長さ 1 の close は誤り（種類と制御の長さは頭で断る）。フレームは 16 MiB、メッセージは 64 MiB まで（前の
//!   tungstenite の既定と同じ。長さの頭で断り、確保しない）。close の返事は同じ番号（送ってはならない番号は 1002）。読み取りの
//!   時間切れ（`WouldBlock`・`TimedOut`）は「まだない」で、途中まで届いたフレームは持ち越す。close のないまま切れたら閉じた
//!   （`WsError::Closed`）として伝える。

use std::io::{ErrorKind, Read, Write};

use base64::Engine;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};

use super::sha1::sha1;

/// 握手の GUID（RFC 6455 §1.3）。
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// 1 つのフレームの中身の上限。
pub const MAX_FRAME: usize = 16 << 20;
/// 1 つのメッセージの上限。
pub const MAX_MESSAGE: usize = 64 << 20;
/// 握手の返事の頭の上限（バイト・行）。
const MAX_HEAD: usize = 16 * 1024;
const MAX_HEAD_LINES: usize = 124;

/// WebSocket の誤り。
#[derive(Debug)]
pub enum WsError {
    Io(std::io::Error),
    /// 握手の返事が合わない。
    Handshake(String),
    /// フレームの決まりに反する。
    Protocol(String),
    /// 閉じた（相手が閉じた・もう読めない）。
    Closed,
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Handshake(m) => write!(f, "握手の返事が正しくありません：{m}"),
            Self::Protocol(m) => write!(f, "WebSocket の決まりに反します：{m}"),
            Self::Closed => write!(f, "接続が閉じています"),
        }
    }
}

impl From<std::io::Error> for WsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// 届いたメッセージ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    /// 相手が閉じた（状態の番号と理由）。
    Close(Option<(u16, String)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    /// 相手の close を受けて返した。
    Closed,
}

/// クライアントの WebSocket。
pub struct WebSocket<S: Read + Write> {
    stream: S,
    rbuf: Vec<u8>,
    /// `rbuf` の読み終えた位置。
    start: usize,
    rng: ChaCha8Rng,
    /// 分割の途中のメッセージ（テキストか、中身）。
    incomplete: Option<(bool, Vec<u8>)>,
    state: State,
}

/// `/dev/urandom` の 32 バイトを種にした ChaCha8。
pub fn os_seeded_rng() -> std::io::Result<ChaCha8Rng> {
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
    Ok(ChaCha8Rng::from_seed(seed))
}

/// 鍵に対する `Sec-WebSocket-Accept`。
pub fn accept_key(key: &str) -> String {
    let mut s = String::with_capacity(key.len() + GUID.len());
    s.push_str(key);
    s.push_str(GUID);
    base64::engine::general_purpose::STANDARD.encode(sha1(s.as_bytes()))
}

impl<S: Read + Write> WebSocket<S> {
    /// 握手する。`host` は `Host` の値（例 `127.0.0.1:9222`）、`path` は `/devtools/browser/…`。
    pub fn connect(
        mut stream: S,
        host: &str,
        path: &str,
        mut rng: ChaCha8Rng,
    ) -> Result<Self, WsError> {
        if !path.starts_with('/') || path.bytes().any(|b| b <= b' ' || b == 0x7F) {
            return Err(WsError::Handshake(format!(
                "接続先のパスが正しくありません：{path}"
            )));
        }
        let mut raw = [0u8; 16];
        rng.fill_bytes(&mut raw);
        let key = base64::engine::general_purpose::STANDARD.encode(raw);
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        // 返事の頭（空行まで）を読む
        let mut buf: Vec<u8> = Vec::new();
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if buf.len() > MAX_HEAD {
                return Err(WsError::Handshake("返事の頭が長すぎます".into()));
            }
            let mut chunk = [0u8; 1024];
            let n = match stream.read(&mut chunk) {
                Ok(0) => return Err(WsError::Handshake("返事の途中で接続が切れました".into())),
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(WsError::Io(e)),
            };
            buf.extend_from_slice(&chunk[..n]);
        };
        if head_end > MAX_HEAD {
            return Err(WsError::Handshake("返事の頭が長すぎます".into()));
        }
        check_response(&buf[..head_end - 4], &accept_key(&key))?;
        Ok(WebSocket {
            stream,
            rbuf: buf[head_end..].to_vec(),
            start: 0,
            rng,
            incomplete: None,
            state: State::Open,
        })
    }

    /// 下の流れ（時間切れの設定などに使う）。
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// テキストを 1 つのフレームで送る。
    pub fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        if self.state != State::Open {
            return Err(WsError::Closed);
        }
        self.send_frame(0x1, text.as_bytes())
    }

    /// マスクしたフレーム（FIN つき）を送る。
    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
        let mut f = Vec::with_capacity(payload.len() + 14);
        f.push(0x80 | opcode);
        let n = payload.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if let Ok(n16) = u16::try_from(n) {
            f.push(0x80 | 126);
            f.extend_from_slice(&n16.to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mut mask = [0u8; 4];
        self.rng.fill_bytes(&mut mask);
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, &b)| b ^ mask[i % 4]));
        self.stream.write_all(&f)?;
        self.stream.flush()?;
        Ok(())
    }

    /// 次のメッセージを読む。時間切れで何もなければ None（途中まで届いたフレームは次へ持ち越す）。ping には pong を、
    /// close には close を返す。
    pub fn read(&mut self) -> Result<Option<Message>, WsError> {
        loop {
            if let Some(m) = self.next_message()? {
                return Ok(Some(m));
            }
            if self.state != State::Open {
                return Err(WsError::Closed);
            }
            let mut chunk = [0u8; 64 * 1024];
            match self.stream.read(&mut chunk) {
                // close のないまま切れた（Chrome が終わった・落ちた）も、閉じたとして伝える
                Ok(0) => {
                    self.state = State::Closed;
                    return Err(WsError::Closed);
                }
                Ok(n) => {
                    if self.start > 0 && self.start >= self.rbuf.len() / 2 {
                        self.rbuf.drain(..self.start);
                        self.start = 0;
                    }
                    self.rbuf.extend_from_slice(&chunk[..n]);
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Ok(None);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(WsError::Io(e)),
            }
        }
    }

    /// 置き場にそろったフレームを読み、メッセージになれば返す（足りなければ None）。
    fn next_message(&mut self) -> Result<Option<Message>, WsError> {
        while let Some((fin, opcode, payload)) = self.next_frame()? {
            if self.state != State::Open {
                return Err(WsError::Protocol(
                    "close のあとにフレームが届きました".into(),
                ));
            }
            if opcode >= 0x8 {
                if !fin {
                    return Err(WsError::Protocol("分割した制御のフレーム".into()));
                }
                if payload.len() > 125 {
                    return Err(WsError::Protocol("制御のフレームが長すぎます".into()));
                }
                match opcode {
                    0x8 => {
                        let close = match payload.len() {
                            0 => None,
                            1 => {
                                return Err(WsError::Protocol(
                                    "close の中身が 1 バイトです".into(),
                                ));
                            }
                            _ => {
                                let code = u16::from_be_bytes([payload[0], payload[1]]);
                                let reason =
                                    String::from_utf8(payload[2..].to_vec()).map_err(|_| {
                                        WsError::Protocol(
                                            "close の理由が UTF-8 ではありません".into(),
                                        )
                                    })?;
                                Some((code, reason))
                            }
                        };
                        // 同じ番号で返して閉じる
                        // 送ってよい番号ならそのまま、ほか（1005・1006 など）は 1002（決まりに反する）で返す
                        let reply: Vec<u8> = close
                            .as_ref()
                            .map(|&(c, _)| {
                                let ok = matches!(c, 1000..=1003 | 1007..=1014 | 3000..=4999);
                                (if ok { c } else { 1002 }).to_be_bytes().to_vec()
                            })
                            .unwrap_or_default();
                        self.state = State::Closed;
                        // 返せなくても閉じたことは伝える
                        let _ = self.send_frame(0x8, &reply);
                        return Ok(Some(Message::Close(close)));
                    }
                    0x9 => {
                        self.send_frame(0xA, &payload)?;
                        return Ok(Some(Message::Ping(payload)));
                    }
                    0xA => return Ok(Some(Message::Pong(payload))),
                    _ => {
                        return Err(WsError::Protocol(format!(
                            "知らない制御のフレーム（{opcode}）"
                        )));
                    }
                }
            }
            let (text, mut data) = match (opcode, self.incomplete.take()) {
                (0x0, None) => {
                    return Err(WsError::Protocol("始まりのない続きのフレーム".into()));
                }
                (0x0, Some((text, mut data))) => {
                    if data.len() + payload.len() > MAX_MESSAGE {
                        return Err(WsError::Protocol("メッセージが大きすぎます".into()));
                    }
                    data.extend_from_slice(&payload);
                    (text, data)
                }
                (0x1 | 0x2, Some(_)) => {
                    return Err(WsError::Protocol("分割の途中に新しいメッセージ".into()));
                }
                (0x1 | 0x2, None) => (opcode == 0x1, payload),
                _ => {
                    return Err(WsError::Protocol(format!(
                        "知らないデータのフレーム（{opcode}）"
                    )));
                }
            };
            if !fin {
                self.incomplete = Some((text, data));
                continue;
            }
            if text {
                let s = String::from_utf8(std::mem::take(&mut data))
                    .map_err(|_| WsError::Protocol("テキストが UTF-8 ではありません".into()))?;
                return Ok(Some(Message::Text(s)));
            }
            return Ok(Some(Message::Binary(data)));
        }
        Ok(None)
    }

    /// 置き場から 1 つのフレーム（FIN・種類・中身）を取り出す。そろっていなければ None。
    fn next_frame(&mut self) -> Result<Option<(bool, u8, Vec<u8>)>, WsError> {
        let b = &self.rbuf[self.start..];
        if b.len() < 2 {
            return Ok(None);
        }
        let (b0, b1) = (b[0], b[1]);
        if b0 & 0x70 != 0 {
            return Err(WsError::Protocol("予約のビットが立っています".into()));
        }
        if b1 & 0x80 != 0 {
            return Err(WsError::Protocol(
                "サーバーのフレームがマスクされています".into(),
            ));
        }
        let (head, len) = match b1 & 0x7F {
            126 => {
                if b.len() < 4 {
                    return Ok(None);
                }
                (4, u64::from(u16::from_be_bytes([b[2], b[3]])))
            }
            127 => {
                if b.len() < 10 {
                    return Ok(None);
                }
                let mut l = [0u8; 8];
                l.copy_from_slice(&b[2..10]);
                let len = u64::from_be_bytes(l);
                if len >> 63 != 0 {
                    return Err(WsError::Protocol(
                        "フレームの長さの最上位のビットが立っています".into(),
                    ));
                }
                (10, len)
            }
            n => (2, u64::from(n)),
        };
        if len > MAX_FRAME as u64 {
            return Err(WsError::Protocol("フレームが大きすぎます".into()));
        }
        // 種類と制御の長さは頭で断る（中身をためない）
        let opcode = b0 & 0x0F;
        match opcode {
            0x0..=0x2 => {}
            0x8..=0xA => {
                if b0 & 0x80 == 0 {
                    return Err(WsError::Protocol("分割した制御のフレーム".into()));
                }
                if len > 125 {
                    return Err(WsError::Protocol("制御のフレームが長すぎます".into()));
                }
            }
            0x3..=0x7 => {
                return Err(WsError::Protocol(format!(
                    "知らないデータのフレーム（{opcode}）"
                )));
            }
            _ => {
                return Err(WsError::Protocol(format!(
                    "知らない制御のフレーム（{opcode}）"
                )));
            }
        }
        let len = len as usize;
        if b.len() < head + len {
            return Ok(None);
        }
        let payload = b[head..head + len].to_vec();
        self.start += head + len;
        if self.start == self.rbuf.len() {
            self.rbuf.clear();
            self.start = 0;
            // 大きなフレームのあとは置き場を小さく戻す（長く続く接続で持ち続けない）
            if self.rbuf.capacity() > 1 << 20 {
                self.rbuf.shrink_to(64 * 1024);
            }
        }
        Ok(Some((b0 & 0x80 != 0, b0 & 0x0F, payload)))
    }
}

/// 握手の返事の頭（空行を除く）を確かめる。
fn check_response(head: &[u8], accept: &str) -> Result<(), WsError> {
    let text = std::str::from_utf8(head)
        .map_err(|_| WsError::Handshake("返事の頭が UTF-8 ではありません".into()))?;
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let mut parts = status.split(' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") || code != "101" {
        return Err(WsError::Handshake(format!("101 ではありません：{status}")));
    }
    let mut upgrade = false;
    let mut connection = false;
    let mut accepted: Option<bool> = None;
    for (i, line) in lines.enumerate() {
        if i >= MAX_HEAD_LINES {
            return Err(WsError::Handshake("返事の頭の行が多すぎます".into()));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(WsError::Handshake(format!(
                "頭の行が正しくありません：{line}"
            )));
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            // 同じ名前の頭が重なれば：Upgrade・Connection はどれかが合えばよく、Accept はどれも合うこと
            "upgrade" => upgrade |= value.eq_ignore_ascii_case("websocket"),
            "connection" => {
                connection |= value
                    .split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
            }
            "sec-websocket-accept" => {
                accepted = Some(accepted.unwrap_or(true) && value == accept);
            }
            "sec-websocket-protocol" => {
                return Err(WsError::Handshake(
                    "頼んでいないサブプロトコルの返事".into(),
                ));
            }
            "sec-websocket-extensions" => {
                return Err(WsError::Handshake("頼んでいない拡張の返事".into()));
            }
            _ => {}
        }
    }
    if !upgrade {
        return Err(WsError::Handshake("Upgrade: websocket がありません".into()));
    }
    if !connection {
        return Err(WsError::Handshake(
            "Connection: Upgrade がありません".into(),
        ));
    }
    if accepted != Some(true) {
        return Err(WsError::Handshake(
            "Sec-WebSocket-Accept が合いません".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;
