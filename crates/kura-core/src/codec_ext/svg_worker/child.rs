//! 働き手の本体（子の側。OS によらない）：標準入力の頼みを 1 つ読み、渡されたデコーダ（固定の段の SVG）で形か値を読み、
//! 標準出力に返事を 1 つ書く。頼みのビルドの印が自分のもの（`stamp`）と違えば、読まずに専用のエラーを返す。

use std::io::{Cursor, Read, Write};

use kura_engine::codec::{Decoder, Stage, contain};
use kura_engine::{EngineError, NoMonitor};

use super::wire::{self, Answer, Op, Request, RequestError, Source};

/// 頼みを 1 つ読み、`decoder` で処理して返事を書く（`stamp` は子のビルドの印）。頼みが壊れていれば内部の誤りの返事、別の
/// ビルドの親からなら専用のエラーの返事を書く。返すエラーは書けなかったときだけ（親がもういないなど）。バイト列の入力は
/// `max_bytes` まで受け取る。
pub fn serve_with(
    decoder: &dyn Decoder,
    stamp: &str,
    input: &mut dyn Read,
    output: &mut dyn Write,
    max_bytes: u64,
) -> std::io::Result<()> {
    serve_request(
        decoder,
        stamp,
        wire::read_request(input, max_bytes, stamp),
        output,
    )
}

/// 読んだ頼み（読めなかったら理由）に答えて返事を書く（[`serve_with`] の後半。頼みの時間の上限を見てから答えたい
/// 呼び手（`serve`）が使う）。
pub fn serve_request(
    decoder: &dyn Decoder,
    stamp: &str,
    request: Result<Request, RequestError>,
    output: &mut dyn Write,
) -> std::io::Result<()> {
    let request = match request {
        Ok(r) => r,
        Err(RequestError::Foreign(message)) => {
            return wire::write_foreign(output, stamp, &message);
        }
        Err(RequestError::Broken(message)) => {
            return wire::write_error(
                output,
                stamp,
                &EngineError::Internal(format!("SVG の働き手への頼みが壊れています: {message}")),
            );
        }
    };
    let result = answer(decoder, &request);
    wire::write_reply(output, stamp, &result)
}

/// 頼みに答える（第三者のコードの panic は受け止めて読み取りのエラーにする）。中止は親が子を止めるので、見張りは渡さない。
fn answer(decoder: &dyn Decoder, req: &Request) -> Result<Answer, EngineError> {
    let (format, opts, limits) = (req.format.as_str(), &req.opts, &req.limits);
    contain(decoder.id(), Stage::Decode, || {
        match (req.op, &req.source) {
            (Op::Shape, Source::Path(p)) => decoder
                .shape_file_monitored(p, format, opts, limits, &NoMonitor)
                .map(Answer::Shape),
            (Op::Shape, Source::Bytes(b)) => decoder
                .shape_monitored(
                    &mut Cursor::new(b.as_slice()),
                    format,
                    opts,
                    limits,
                    &NoMonitor,
                )
                .map(Answer::Shape),
            (Op::Decode, Source::Path(p)) => decoder
                .decode_file(p, format, opts, limits, &NoMonitor)
                .map(Answer::Value),
            (Op::Decode, Source::Bytes(b)) => decoder
                .decode(
                    &mut Cursor::new(b.as_slice()),
                    format,
                    opts,
                    limits,
                    &NoMonitor,
                )
                .map(Answer::Value),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::Limits;
    use kura_engine::codec::svg::{self, SvgCodec};
    use kura_engine::doc::DecodeOptions;
    use std::path::Path;

    const STAMP: &str = r#"{"decoder":"svg-resvg-dec/1"}"#;

    fn request(stamp: &str, op: Op, input: wire::Input<'_>) -> Vec<u8> {
        let mut req = Vec::new();
        wire::write_request(
            &mut req,
            stamp,
            op,
            input,
            "svg",
            &DecodeOptions::default(),
            &Limits::default(),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        req
    }

    fn ask(op: Op, input: wire::Input<'_>) -> wire::Reply {
        let req = request(STAMP, op, input);
        let mut out = Vec::new();
        serve_with(
            &SvgCodec::default(),
            STAMP,
            &mut req.as_slice(),
            &mut out,
            svg::MAX_SOURCE,
        )
        .unwrap();
        let mut r = wire::ReplyReader::new(op, &Limits::default(), STAMP);
        r.push(&out).unwrap();
        r.finish().unwrap()
    }

    /// 子の本体は、プロセスの中の固定の段と同じ形・値・エラーを返す（パスでもバイト列でも）。
    #[test]
    fn answers_like_the_in_process_decoder() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="7" height="5"><circle cx="3" cy="2" r="2" fill="#0a0" opacity="0.5"/></svg>"##;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.svg");
        std::fs::write(&path, svg).unwrap();
        let c = SvgCodec::default();
        let (opts, limits) = (DecodeOptions::default(), Limits::default());
        let want_shape = c
            .shape(&mut Cursor::new(&svg[..]), "svg", &opts, &limits)
            .unwrap();
        let want = c
            .decode(
                &mut Cursor::new(&svg[..]),
                "svg",
                &opts,
                &limits,
                &NoMonitor,
            )
            .unwrap();
        for input in [wire::Input::Path(&path), wire::Input::Bytes(svg)] {
            match ask(Op::Shape, input) {
                wire::Reply::Shape(s) => assert_eq!(s, want_shape),
                other => panic!("{other:?}"),
            }
            match ask(Op::Decode, input) {
                wire::Reply::Value(v) => assert_eq!(v, want),
                other => panic!("{other:?}"),
            }
        }
        // 文字（フォントなし）は Unsupported、ないファイルは入出力のエラー
        let text =
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><text>a</text></svg>"#;
        assert!(matches!(
            ask(Op::Decode, wire::Input::Bytes(text)),
            wire::Reply::Error(EngineError::Unsupported(_))
        ));
        match ask(
            Op::Shape,
            wire::Input::Path(Path::new("/nonexistent/kura/x.svg")),
        ) {
            wire::Reply::Error(EngineError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound)
            }
            other => panic!("{other:?}"),
        }
        // 壊れた頼みは内部の誤りの返事
        let mut out = Vec::new();
        serve_with(&c, STAMP, &mut &b"junk"[..], &mut out, 10).unwrap();
        let mut r = wire::ReplyReader::new(Op::Decode, &limits, STAMP);
        r.push(&out).unwrap();
        assert!(matches!(
            r.finish().unwrap(),
            wire::Reply::Error(EngineError::Internal(_))
        ));
    }

    /// 別のビルドの親からの頼み（ビルドの印が違う）は読まずに、自分の印で専用のエラーを返す。親は返事の印で別のビルドと
    /// 分かる（M11 の 3 回目の確認 W2）。
    #[test]
    fn requests_from_another_build_are_refused() {
        let other = r#"{"decoder":"svg-resvg-dec/1","fonts":{}}"#;
        let req = request(other, Op::Decode, wire::Input::Bytes(b"<svg/>"));
        let mut out = Vec::new();
        serve_with(
            &SvgCodec::default(),
            STAMP,
            &mut req.as_slice(),
            &mut out,
            svg::MAX_SOURCE,
        )
        .unwrap();
        // 頼んだ親（印 other）から見ると別のビルド
        let mut r = wire::ReplyReader::new(Op::Decode, &Limits::default(), other);
        let err = r.push(&out).unwrap_err();
        assert!(err.contains("別のビルド"), "{err}");
        assert_eq!(r.foreign(), Some(err.as_str()));
        // 返事の中身は専用のエラー（一時的な誤り）
        let mut r = wire::ReplyReader::new(Op::Decode, &Limits::default(), STAMP);
        r.push(&out).unwrap();
        match r.finish().unwrap() {
            wire::Reply::Error(e) => assert!(super::super::is_unavailable(&e), "{e}"),
            other => panic!("{other:?}"),
        }
    }
}
