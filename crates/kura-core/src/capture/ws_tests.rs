//! DevTools の WebSocket のクライアントの試験。照合の相手は tungstenite のサーバー（dev 依存）と、手で組んだバイト列。

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};

use base64::Engine;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use tungstenite::protocol::frame::coding::{Data, OpCode};
use tungstenite::protocol::frame::{Frame, FrameHeader};

use super::*;

/// RFC 6455 §1.3 の例。
#[test]
fn accept_key_matches_the_rfc_example() {
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

fn rng() -> ChaCha8Rng {
    ChaCha8Rng::from_seed([7; 32])
}

/// 手で組んだ流れ：読むと、置いた塊を順に返す（`None` の塊は時間切れ）。書いたものは覚える。
struct Script {
    input: VecDeque<Option<Vec<u8>>>,
    out: Vec<u8>,
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.input.pop_front() {
            None => Ok(0),
            Some(None) => Err(std::io::Error::new(ErrorKind::WouldBlock, "待ち")),
            Some(Some(mut c)) => {
                let n = c.len().min(buf.len());
                buf[..n].copy_from_slice(&c[..n]);
                if n < c.len() {
                    c.drain(..n);
                    self.input.push_front(Some(c));
                }
                Ok(n)
            }
        }
    }
}

impl Write for Script {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 種 `rng()` のクライアントが送る鍵の accept。
fn expected_accept() -> String {
    let mut r = rng();
    let mut raw = [0u8; 16];
    r.fill_bytes(&mut raw);
    accept_key(&base64::engine::general_purpose::STANDARD.encode(raw))
}

fn good_head() -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        expected_accept()
    )
    .into_bytes()
}

/// サーバーのフレーム（マスクなし）。
fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = vec![(u8::from(fin) << 7) | opcode];
    let n = payload.len();
    if n < 126 {
        f.push(n as u8);
    } else if n < 65536 {
        f.push(126);
        f.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        f.push(127);
        f.extend_from_slice(&(n as u64).to_be_bytes());
    }
    f.extend_from_slice(payload);
    f
}

fn connect(chunks: Vec<Option<Vec<u8>>>) -> Result<WebSocket<Script>, WsError> {
    WebSocket::connect(
        Script {
            input: chunks.into(),
            out: Vec::new(),
        },
        "127.0.0.1:9222",
        "/devtools/browser/x",
        rng(),
    )
}

/// クライアントが書いたフレーム（握手のあと）を読み戻す（マスクを外す）。
fn written_frames(out: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let mut b = &out[at..];
    let mut v = Vec::new();
    while !b.is_empty() {
        assert_eq!(b[0] & 0x80, 0x80, "FIN");
        assert_eq!(b[1] & 0x80, 0x80, "クライアントはマスクする");
        let (head, len) = match b[1] & 0x7F {
            126 => (4, u16::from_be_bytes([b[2], b[3]]) as usize),
            127 => (
                10,
                u64::from_be_bytes(b[2..10].try_into().unwrap()) as usize,
            ),
            n => (2, n as usize),
        };
        let mask = [b[head], b[head + 1], b[head + 2], b[head + 3]];
        let data: Vec<u8> = b[head + 4..head + 4 + len]
            .iter()
            .enumerate()
            .map(|(i, &x)| x ^ mask[i % 4])
            .collect();
        v.push((b[0] & 0x0F, data));
        b = &b[head + 4 + len..];
    }
    v
}

/// 握手の要求は RFC 6455 の形（GET・Host・Upgrade・Connection・鍵・版 13）。
#[test]
fn the_handshake_request_has_the_required_fields() {
    let ws = connect(vec![Some(good_head())]).unwrap();
    let req = String::from_utf8(ws.stream.out.clone()).unwrap();
    assert!(
        req.starts_with("GET /devtools/browser/x HTTP/1.1\r\n"),
        "{req}"
    );
    for h in [
        "Host: 127.0.0.1:9222\r\n",
        "Upgrade: websocket\r\n",
        "Connection: Upgrade\r\n",
        "Sec-WebSocket-Version: 13\r\n",
        "Sec-WebSocket-Key: ",
    ] {
        assert!(req.contains(h), "{h} {req}");
    }
    assert!(req.ends_with("\r\n\r\n"));
}

/// 握手の返事が合わなければ断る。
#[test]
fn bad_handshake_responses_are_refused() {
    let accept = expected_accept();
    let cases = [
        format!("HTTP/1.1 200 OK\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"),
        format!("HTTP/1.1 101 x\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"),
        format!("HTTP/1.1 101 x\r\nUpgrade: h2c\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"),
        format!("HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: keep-alive\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"),
        "HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: AAAA\r\n\r\n".to_string(),
        "HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n".to_string(),
        format!("HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: chat\r\n\r\n"),
        format!("HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n"),
        format!("HTTP/1.1 101 x\r\nUpgrade: websocket\r\nbroken\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"),
        "HTTP/1.1 101 x\r\nUpgrade: websocket\r\n".to_string(),
    ];
    for c in cases {
        assert!(connect(vec![Some(c.clone().into_bytes())]).is_err(), "{c}");
    }
    // 頭が長すぎる
    let long = format!("HTTP/1.1 101 x\r\nX: {}\r\n", "a".repeat(20_000));
    assert!(connect(vec![Some(long.into_bytes())]).is_err());
    // 大文字小文字・Connection の列・頭のあとのフレームは受け付ける
    let mut ok = format!(
        "HTTP/1.1 101 x\r\nupgrade: WebSocket\r\nconnection: keep-alive, upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
    )
    .into_bytes();
    ok.extend(frame(true, 1, b"hello"));
    let mut ws = connect(vec![Some(ok)]).unwrap();
    assert_eq!(ws.read().unwrap(), Some(Message::Text("hello".into())));
}

/// 時間切れをはさんで 1 バイトずつ届いても、フレームを持ち越して読む。分割・ping・pong・バイナリ・close。
#[test]
fn reads_frames_across_timeouts_and_fragments() {
    let mut body = Vec::new();
    body.extend(frame(false, 1, "日本".as_bytes()));
    body.extend(frame(true, 9, b"p1"));
    body.extend(frame(false, 0, "語".as_bytes()));
    body.extend(frame(true, 0, b"!"));
    body.extend(frame(true, 2, &[1, 2, 3]));
    body.extend(frame(true, 1, &vec![b'x'; 300]));
    body.extend(frame(true, 1, &vec![b'y'; 70_000]));
    body.extend(frame(true, 10, b"p2"));
    let mut close = 1000u16.to_be_bytes().to_vec();
    close.extend_from_slice("bye".as_bytes());
    body.extend(frame(true, 8, &close));
    let mut chunks = vec![Some(good_head())];
    for b in body.chunks(1) {
        chunks.push(Some(b.to_vec()));
        chunks.push(None);
    }
    let mut ws = connect(chunks).unwrap();
    let mut got = Vec::new();
    for _ in 0..2_000_000 {
        match ws.read() {
            Ok(Some(m)) => {
                let end = matches!(m, Message::Close(_));
                got.push(m);
                if end {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(
        got,
        vec![
            Message::Ping(b"p1".to_vec()),
            Message::Text("日本語!".into()),
            Message::Binary(vec![1, 2, 3]),
            Message::Text("x".repeat(300)),
            Message::Text("y".repeat(70_000)),
            Message::Pong(b"p2".to_vec()),
            Message::Close(Some((1000, "bye".into()))),
        ]
    );
    // pong と close を返した
    let written = written_frames(&ws.stream.out);
    assert_eq!(
        written,
        vec![(0xA, b"p1".to_vec()), (0x8, 1000u16.to_be_bytes().to_vec())]
    );
    // 閉じたあとは読めない・送れない
    assert!(matches!(ws.read(), Err(WsError::Closed)));
    assert!(matches!(ws.send_text("x"), Err(WsError::Closed)));
}

/// 送るフレームの長さの書き方（7・16・64 ビット）とマスク。
#[test]
fn sends_masked_text_frames() {
    let mut ws = connect(vec![Some(good_head())]).unwrap();
    let texts: Vec<String> = [0usize, 1, 125, 126, 65_535, 65_536]
        .iter()
        .map(|&n| "あ".repeat(n / 3) + &"z".repeat(n % 3))
        .collect();
    for t in &texts {
        ws.send_text(t).unwrap();
    }
    let written = written_frames(&ws.stream.out);
    assert_eq!(written.len(), texts.len());
    for ((op, data), t) in written.iter().zip(&texts) {
        assert_eq!(*op, 1);
        assert_eq!(data, t.as_bytes());
    }
}

/// 本文を読ませ、最初の結果を返す（本文のあとは時間切れで止める。EOF にしないので、検査がなければ None か
/// メッセージになる）。
fn first_result(body: Vec<u8>) -> Result<Option<Message>, WsError> {
    let mut chunks = vec![Some(good_head())];
    if !body.is_empty() {
        chunks.push(Some(body));
    }
    chunks.push(None);
    let mut ws = connect(chunks).unwrap();
    ws.read()
}

/// フレームの決まりに反するものは、メッセージを返さずに最初の読みで誤り（文言も照らす）。
#[test]
fn protocol_violations_are_errors() {
    let mut masked = frame(true, 1, b"hi");
    masked[1] |= 0x80;
    masked.splice(2..2, [1, 2, 3, 4]);
    let mut rsv = frame(true, 1, b"hi");
    rsv[0] |= 0x40;
    let mut huge = vec![0x81, 127];
    huge.extend_from_slice(&((MAX_FRAME as u64) + 1).to_be_bytes());
    let mut msb = vec![0x81, 127];
    msb.extend_from_slice(&(1u64 << 63).to_be_bytes());
    // 中身なしで頭だけ：16 MiB を名乗る ping は中身をためずに断る
    let mut long_ping = vec![0x89, 127];
    long_ping.extend_from_slice(&((MAX_FRAME as u64) - 1).to_be_bytes());
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("マスクしたフレーム", masked, "マスク"),
        ("予約のビット", rsv, "予約"),
        ("大きすぎるフレーム", huge, "フレームが大きすぎ"),
        ("長さの最上位のビット", msb, "最上位"),
        (
            "長い制御のフレーム",
            frame(true, 9, &[0; 126]),
            "制御のフレームが長すぎ",
        ),
        ("頭だけの長い ping", long_ping, "制御のフレームが長すぎ"),
        (
            "分割した制御のフレーム",
            frame(false, 9, b"x"),
            "分割した制御",
        ),
        ("始まりのない続き", frame(true, 0, b"x"), "始まりのない続き"),
        (
            "分割の途中の新しいメッセージ",
            {
                let mut b = frame(false, 1, b"a");
                b.extend(frame(true, 1, b"b"));
                b
            },
            "分割の途中",
        ),
        (
            "UTF-8 でないテキスト",
            frame(true, 1, &[0xff, 0xfe]),
            "UTF-8",
        ),
        (
            "分けた UTF-8 の途中で終わる",
            {
                let mut b = frame(false, 1, &"あ".as_bytes()[..2]);
                b.extend(frame(true, 0, b""));
                b
            },
            "UTF-8",
        ),
        ("長さ 1 の close", frame(true, 8, &[3]), "1 バイト"),
        (
            "UTF-8 でない close の理由",
            frame(true, 8, &[3, 232, 0xff]),
            "UTF-8",
        ),
        (
            "知らないデータの種類",
            frame(true, 3, b"x"),
            "知らないデータ",
        ),
        ("知らない制御の種類", frame(true, 11, b"x"), "知らない制御"),
    ];
    for (what, body, word) in cases {
        match first_result(body) {
            Err(WsError::Protocol(m)) => assert!(m.contains(word), "{what}：{m}"),
            r => panic!("{what}：{r:?}"),
        }
    }
    // 正しいものは時間切れで None（検査が誤って断っていないこと）
    assert_eq!(first_result(Vec::new()).unwrap(), None);
    assert_eq!(
        first_result(frame(false, 1, b"a")).unwrap(),
        None,
        "分割の途中"
    );
    // close のないまま切れた：閉じたとして伝える
    let mut ws = connect(vec![Some(good_head())]).unwrap();
    assert!(matches!(ws.read(), Err(WsError::Closed)));
}

/// close の返事：送ってよい番号はそのまま、1005・1006・999 などは 1002。
#[test]
fn close_replies_use_allowed_codes() {
    for (got, want) in [
        (1000u16, 1000u16),
        (1001, 1001),
        (3000, 3000),
        (4999, 4999),
        (1005, 1002),
        (1006, 1002),
        (999, 1002),
        (2000, 1002),
        (5000, 1002),
    ] {
        let mut ws = connect(vec![
            Some(good_head()),
            Some(frame(true, 8, &got.to_be_bytes())),
        ])
        .unwrap();
        assert_eq!(
            ws.read().unwrap(),
            Some(Message::Close(Some((got, String::new()))))
        );
        assert_eq!(
            written_frames(&ws.stream.out),
            vec![(0x8, want.to_be_bytes().to_vec())],
            "{got}"
        );
    }
}

/// 同じ名前の頭が重なれば：Connection はどれかに Upgrade があればよく、Accept はどれも合うこと。
#[test]
fn repeated_headers_are_read_carefully() {
    let accept = expected_accept();
    let ok = format!(
        "HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nConnection: keep-alive\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    assert!(connect(vec![Some(ok.into_bytes())]).is_ok());
    let bad = format!(
        "HTTP/1.1 101 x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: AAAA\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    assert!(connect(vec![Some(bad.into_bytes())]).is_err());
    // Chrome の返事の形
    let chrome = format!(
        "HTTP/1.1 101 WebSocket Protocol Handshake\r\nUpgrade: WebSocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    assert!(connect(vec![Some(chrome.into_bytes())]).is_ok());
}

/// メッセージが 64 MiB を超えれば誤り（分割を継いだ長さで）。メッセージを返す前に断る。
#[test]
fn messages_are_limited() {
    let part = vec![b'a'; MAX_FRAME];
    let mut body = frame(false, 1, &part);
    for _ in 0..3 {
        body.extend(frame(false, 0, &part));
    }
    body.extend(frame(true, 0, b"a"));
    match first_result(body) {
        Err(WsError::Protocol(m)) => assert!(m.contains("メッセージが大きすぎ"), "{m}"),
        r => panic!("{r:?}"),
    }
}

/// 乱数のバイト列を読んでも落ちず、必ず止まる（壊れた入力。ChaCha8）。
#[test]
fn random_bytes_never_panic() {
    let mut r = crate::random::rng(5);
    for _ in 0..3000 {
        let n = (r.next_u32() % 400) as usize;
        let mut body: Vec<u8> = (0..n).map(|_| r.next_u32() as u8).collect();
        if r.next_u32().is_multiple_of(2) && !body.is_empty() {
            // 頭のビットを読める形に寄せる（マスクなし・予約なし）
            body[0] &= 0x8F;
            if body.len() > 1 {
                body[1] &= 0x7F;
            }
        }
        let mut chunks = vec![Some(good_head())];
        for c in body.chunks(1 + (r.next_u32() % 7) as usize) {
            chunks.push(Some(c.to_vec()));
        }
        let mut ws = connect(chunks).unwrap();
        for _ in 0..10_000 {
            match ws.read() {
                Ok(Some(_)) => {}
                _ => break,
            }
        }
    }
}

/// 本物の tungstenite のサーバーと話す：握手、テキストの行き来、サーバーの分割したメッセージ・ping・close。
#[test]
fn talks_to_a_tungstenite_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            match ws.read().unwrap() {
                tungstenite::Message::Text(t) => got.push(t.as_str().to_string()),
                other => panic!("{other:?}"),
            }
        }
        // 分割したテキスト・ping・大きなテキスト
        let raw = |fin: bool, op: OpCode, data: &[u8]| {
            let mut f = Frame::message(data.to_vec(), op, fin);
            *f.header_mut() = FrameHeader {
                is_final: fin,
                opcode: op,
                ..FrameHeader::default()
            };
            tungstenite::Message::Frame(f)
        };
        ws.send(raw(false, OpCode::Data(Data::Text), "分".as_bytes()))
            .unwrap();
        ws.send(raw(true, OpCode::Data(Data::Continue), "割".as_bytes()))
            .unwrap();
        ws.send(tungstenite::Message::Ping(b"hi".to_vec().into()))
            .unwrap();
        ws.send(tungstenite::Message::text("z".repeat(100_000)))
            .unwrap();
        // ping の pong を待つ
        let pong = ws.read().unwrap();
        assert!(
            matches!(pong, tungstenite::Message::Pong(ref p) if p.as_ref() == b"hi"),
            "{pong:?}"
        );
        ws.close(None).unwrap();
        // close の返事
        let end = ws.read();
        (got, format!("{end:?}"))
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut ws = WebSocket::connect(
        stream,
        &format!("127.0.0.1:{port}"),
        "/devtools/browser/t",
        os_seeded_rng().unwrap(),
    )
    .unwrap();
    for t in ["a", &"b".repeat(200), &"c".repeat(70_000)] {
        ws.send_text(t).unwrap();
    }
    let mut got = Vec::new();
    // 5 秒の時間切れの読みを回数で区切る（サーバーが止まっても試験が止まり続けない）
    for _ in 0..20 {
        if let Some(m) = ws.read().unwrap() {
            let end = matches!(m, Message::Close(_));
            got.push(m);
            if end {
                break;
            }
        }
    }
    assert_eq!(
        got,
        vec![
            Message::Text("分割".into()),
            Message::Ping(b"hi".to_vec()),
            Message::Text("z".repeat(100_000)),
            Message::Close(None),
        ]
    );
    let (sent, end) = server.join().unwrap();
    assert_eq!(
        sent,
        vec!["a".to_string(), "b".repeat(200), "c".repeat(70_000)]
    );
    assert!(
        end.contains("ConnectionClosed") || end.contains("Close"),
        "{end}"
    );
}
