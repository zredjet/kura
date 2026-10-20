//! `kura://localhost/...` のカスタムプロトコル（docs/architecture.md §4）。
//! サムネイル・ジャケット・元ファイル・再生用ファイル・波形の段・アニメの帯を返す。Range は 1 区間・1 回 4MB まで。
//! 試聴の PCM（`/pcm`）はその場で WAV にして返す（kura-core の `asset::pcm`。ジョブにしない。決定 8）。
//!
//! 受け付けの判定（WebView のラベル・Origin・メソッド）と応答のヘッダは kura-core の `serve::access`
//! （§4.1。CI で回す純粋な関数）が決め、ここは HTTP の型に写すだけ。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use kura_core::asset::pcm;
use kura_core::edit::EngineServices;
use kura_core::library::Layout;
use kura_core::serve::{
    ALLOWED_METHODS, Admit, AppOrigin, RangeRequest, Route, admit, content_type, parse_range,
    parse_route, pcm_source, preflight_headers, resolve, response_headers,
};
use tauri::http::{HeaderName, HeaderValue, Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// 受け付けの判定の結果。
pub enum Gate {
    /// 読んで返す（呼ぶ側で spawn_blocking して [`respond`] を呼ぶ）。
    Serve { head: bool },
    /// この応答で終える（断る・事前確認）。ファイルには触れていない。
    Done(Response<Vec<u8>>),
}

/// 受け付けの判定（ファイルに触る前。spawn_blocking の前に呼ぶ）。`label` は要求を出した WebView。
pub fn gate(label: &str, request: &Request<Vec<u8>>, origin: &AppOrigin) -> Gate {
    let method = request.method().as_str();
    // 読めない Origin（ASCII でないなど）はアプリの origin と違うものとして断る
    let request_origin = request
        .headers()
        .get(header::ORIGIN)
        .map(|v| v.to_str().unwrap_or("<invalid>"));
    let response = match admit(label, method, request_origin, origin) {
        Admit::Serve { head } => return Gate::Serve { head },
        Admit::Preflight => {
            let mut response = empty(StatusCode::NO_CONTENT);
            for (name, value) in preflight_headers() {
                insert(&mut response, name, value);
            }
            decorate(&mut response, origin);
            response
        }
        Admit::Reject { status, cors } => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
            let mut response = empty(code);
            if code == StatusCode::METHOD_NOT_ALLOWED {
                insert(&mut response, "Allow", ALLOWED_METHODS);
            }
            if cors {
                decorate(&mut response, origin);
            }
            response
        }
    };
    tracing::debug!(
        label,
        method,
        uri = %request.uri(),
        origin = request_origin.unwrap_or("-"),
        status = response.status().as_u16(),
        acao = acao_of(&response),
        "kura://（受け付けで答えました）"
    );
    Gate::Done(response)
}

/// ログに出す ACAO（なければ `-`）。
fn acao_of(response: &Response<Vec<u8>>) -> &str {
    response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
}

/// 受け付けた要求（main から）に答える（ファイルを開くのでブロックする。呼ぶ側で spawn_blocking する）。
pub fn respond(
    app: &AppHandle,
    request: &Request<Vec<u8>>,
    origin: &AppOrigin,
    head: bool,
) -> Response<Vec<u8>> {
    let session = app.try_state::<AppState>().and_then(|s| s.current());
    let served = session.as_ref().map(|s| Served {
        layout: s.library().layout(),
        engine: s.engine(),
    });
    let response = reply(served.as_ref(), request, origin, head);
    tracing::debug!(
        label = kura_core::serve::MAIN_WEBVIEW,
        method = request.method().as_str(),
        uri = %request.uri(),
        origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-"),
        range = range_of(request).unwrap_or("-"),
        status = response.status().as_u16(),
        bytes = response.body().len(),
        acao = acao_of(&response),
        "kura://"
    );
    response
}

/// 開いているライブラリのうち、kura:// が使うもの（ファイルの置き場所と、試聴の PCM を読むエンジン）。
struct Served<'a> {
    layout: &'a Layout,
    engine: Option<&'a Arc<EngineServices>>,
}

/// ライブラリ（開いていなければ None で 503）から答え、main への応答のヘッダを付ける。
fn reply(
    served: Option<&Served<'_>>,
    request: &Request<Vec<u8>>,
    origin: &AppOrigin,
    head: bool,
) -> Response<Vec<u8>> {
    let mut response = answer(served, request.uri().path(), range_of(request), head);
    decorate(&mut response, origin);
    response
}

fn range_of(request: &Request<Vec<u8>>) -> Option<&str> {
    request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
}

/// パスの中身を返す（ヘッダの飾りは呼ぶ側）。
fn answer(
    served: Option<&Served<'_>>,
    path: &str,
    range: Option<&str>,
    head: bool,
) -> Response<Vec<u8>> {
    let Some(served) = served else {
        return empty(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(route) = parse_route(path) else {
        return empty(StatusCode::NOT_FOUND);
    };
    if let Route::Pcm { hash, start, end } = &route {
        return pcm_response(served, hash, *start, *end, route.cache_control(), head);
    }
    let Some(file) = resolve(served.layout, &route) else {
        return empty(StatusCode::NOT_FOUND);
    };
    serve_file(&file, range, route.cache_control(), head).unwrap_or_else(|err| {
        tracing::warn!(%err, path = %file.display(), "kura:// のファイルを読めませんでした");
        empty(StatusCode::INTERNAL_SERVER_ERROR)
    })
}

/// 試聴の PCM（WAV）をその場で作って返す（Range は見ない。範囲はパスのフレームで決まる）。音声がなければ 404、エンジンが
/// なければ・予算が空いていなければ 503、範囲が音声の外・長すぎれば 416。
fn pcm_response(
    served: &Served<'_>,
    hash: &str,
    start: u64,
    end: u64,
    cache_control: Option<&str>,
    head: bool,
) -> Response<Vec<u8>> {
    let Some(source) = pcm_source(served.layout, hash) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let Some(engine) = served.engine else {
        return empty(StatusCode::SERVICE_UNAVAILABLE);
    };
    // 読んだ結果は、そのライブラリの置き場に覚える（閉じると手放す。M11 の検証 E2）
    let body = match pcm::serve(engine, served.layout.root(), hash, &source, start, end) {
        Ok(body) => body,
        Err(err) => {
            tracing::debug!(%err, hash, start, end, "kura:// の試聴の PCM を返せません");
            let code =
                StatusCode::from_u16(err.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return empty(code);
        }
    };
    let len = body.len();
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::CONTENT_LENGTH, len);
    if let Some(cache_control) = cache_control {
        builder = builder.header(header::CACHE_CONTROL, cache_control);
    }
    builder
        .body(if head { Vec::new() } else { body })
        .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
}

/// main への応答すべて（エラーを含む）に、ACAO・ACEH・`CSP: sandbox`・`nosniff` を付ける。
fn decorate(response: &mut Response<Vec<u8>>, origin: &AppOrigin) {
    for (name, value) in response_headers(origin) {
        insert(response, name, value);
    }
}

fn insert(response: &mut Response<Vec<u8>>, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        response.headers_mut().insert(name, value);
    }
}

fn serve_file(
    path: &Path,
    range: Option<&str>,
    cache_control: Option<&str>,
    head: bool,
) -> std::io::Result<Response<Vec<u8>>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let (code, start, count, content_range) = match parse_range(range, len) {
        RangeRequest::Full => (StatusCode::OK, 0, len, None),
        RangeRequest::Partial { start, end } => (
            StatusCode::PARTIAL_CONTENT,
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{len}")),
        ),
        RangeRequest::Unsatisfiable => {
            let response = Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Vec::new());
            return Ok(response.unwrap_or_else(|_| empty(StatusCode::RANGE_NOT_SATISFIABLE)));
        }
    };
    // HEAD は長さだけを返し、本体を読まない
    let body = if head {
        Vec::new()
    } else {
        let mut body = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        file.seek(SeekFrom::Start(start))?;
        file.take(count).read_to_end(&mut body)?;
        if body.len() as u64 != count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "読んでいるあいだにファイルが短くなりました",
            ));
        }
        body
    };
    let mut builder = Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, content_type(path))
        .header(header::CONTENT_LENGTH, count)
        .header(header::ACCEPT_RANGES, "bytes");
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    if let Some(cache_control) = cache_control {
        builder = builder.header(header::CACHE_CONTROL, cache_control);
    }
    Ok(builder
        .body(body)
        .unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR)))
}

fn empty(code: StatusCode) -> Response<Vec<u8>> {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = code;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_core::serve::MAIN_WEBVIEW;

    const HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    const IMMUTABLE: Option<&str> = Some("public, max-age=31536000, immutable");

    fn header_of(r: &Response<Vec<u8>>, name: &str) -> Option<String> {
        r.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    fn release() -> AppOrigin {
        AppOrigin::new(false, Some("http://localhost:1430/"))
    }

    fn request(method: &str, path: &str, headers: &[(&str, &str)]) -> Request<Vec<u8>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("kura://localhost{path}"));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Vec::new()).unwrap()
    }

    /// 応答の場合：名前・ライブラリ・要求・期待する状態。
    type Case<'a> = (
        &'static str,
        Option<&'a Layout>,
        Request<Vec<u8>>,
        StatusCode,
    );

    /// main への応答に付くヘッダを確かめる。
    fn assert_decorated(r: &Response<Vec<u8>>, origin: &AppOrigin, what: &str) {
        assert_eq!(
            header_of(r, "access-control-allow-origin").as_deref(),
            Some(origin.as_str()),
            "{what}"
        );
        assert_eq!(
            header_of(r, "access-control-expose-headers").as_deref(),
            Some("Content-Range, Accept-Ranges, Content-Length"),
            "{what}"
        );
        assert_eq!(
            header_of(r, "content-security-policy").as_deref(),
            Some("sandbox"),
            "{what}"
        );
        assert_eq!(
            header_of(r, "x-content-type-options").as_deref(),
            Some("nosniff"),
            "{what}"
        );
    }

    /// ライブラリの形のフォルダに、中身 `0123456789` の mp4 と小さいサムネイルを置く。
    fn library() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let hash = kura_core::library::parse_hex32(HASH).unwrap();
        let file = layout.content_path(&hash, "mp4");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"0123456789").unwrap();
        let thumb = layout.thumb_path(HASH, kura_core::library::ThumbSize::Small);
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(&thumb, b"jpg").unwrap();
        (dir, layout)
    }

    #[test]
    fn serves_full_partial_and_unsatisfiable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mp4");
        std::fs::write(&path, b"0123456789").unwrap();

        let full = serve_file(&path, None, None, false).unwrap();
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.body(), b"0123456789");
        assert_eq!(
            header_of(&full, "content-type").as_deref(),
            Some("video/mp4")
        );
        assert_eq!(header_of(&full, "accept-ranges").as_deref(), Some("bytes"));
        assert_eq!(header_of(&full, "cache-control"), None);

        let part = serve_file(&path, Some("bytes=2-5"), None, false).unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.body(), b"2345");
        assert_eq!(
            header_of(&part, "content-range").as_deref(),
            Some("bytes 2-5/10")
        );
        assert_eq!(header_of(&part, "content-length").as_deref(), Some("4"));

        let tail = serve_file(&path, Some("bytes=7-"), None, false).unwrap();
        assert_eq!(tail.body(), b"789");

        let bad = serve_file(&path, Some("bytes=20-"), None, false).unwrap();
        assert_eq!(bad.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            header_of(&bad, "content-range").as_deref(),
            Some("bytes */10")
        );
    }

    #[test]
    fn head_returns_the_length_without_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        std::fs::write(&path, b"0123456789").unwrap();
        let head = serve_file(&path, None, None, true).unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head.body().is_empty());
        assert_eq!(header_of(&head, "content-length").as_deref(), Some("10"));
        let part = serve_file(&path, Some("bytes=-3"), None, true).unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert!(part.body().is_empty());
        assert_eq!(header_of(&part, "content-length").as_deref(), Some("3"));
        assert_eq!(
            header_of(&part, "content-range").as_deref(),
            Some("bytes 7-9/10")
        );
    }

    #[test]
    fn thumbnails_are_cached_forever() {
        let (_dir, layout) = library();
        let served = Served {
            layout: &layout,
            engine: None,
        };
        let r = answer(Some(&served), &format!("/thumb/{HASH}/s"), None, false);
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(header_of(&r, "content-type").as_deref(), Some("image/jpeg"));
        assert_eq!(header_of(&r, "cache-control").as_deref(), IMMUTABLE);
    }

    #[test]
    fn every_main_response_carries_cors_and_sandbox() {
        let (_dir, layout) = library();
        let origin = release();
        let file = format!("/file/{HASH}");
        let cases: Vec<Case<'_>> = vec![
            (
                "200",
                Some(&layout),
                request("GET", &file, &[]),
                StatusCode::OK,
            ),
            (
                "206",
                Some(&layout),
                request("GET", &file, &[("Range", "bytes=0-1")]),
                StatusCode::PARTIAL_CONTENT,
            ),
            (
                "416",
                Some(&layout),
                request("GET", &file, &[("Range", "bytes=99-")]),
                StatusCode::RANGE_NOT_SATISFIABLE,
            ),
            (
                "404（ない中身）",
                Some(&layout),
                request("GET", &format!("/file/{}", "0".repeat(64)), &[]),
                StatusCode::NOT_FOUND,
            ),
            (
                "404（経路の外）",
                Some(&layout),
                request("GET", "/nope", &[]),
                StatusCode::NOT_FOUND,
            ),
            (
                "404（波形はまだ）",
                Some(&layout),
                request("GET", &format!("/peaks/{HASH}"), &[]),
                StatusCode::NOT_FOUND,
            ),
            (
                "503",
                None,
                request("GET", &file, &[]),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (what, layout, req, status) in cases {
            let served = layout.map(|layout| Served {
                layout,
                engine: None,
            });
            let r = reply(served.as_ref(), &req, &origin, false);
            assert_eq!(r.status(), status, "{what}");
            assert_decorated(&r, &origin, what);
        }

        // 500：読めないファイル（権限を外す）
        use std::os::unix::fs::PermissionsExt;
        let hash = kura_core::library::parse_hex32(HASH).unwrap();
        let path = layout.content_path(&hash, "mp4");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let served = Served {
            layout: &layout,
            engine: None,
        };
        let r = reply(Some(&served), &request("GET", &file, &[]), &origin, false);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_decorated(&r, &origin, "500");
    }

    /// 予算を決められる偽のファクトリ（固定の段だけのコーデックの組）。
    struct Factory;

    impl kura_core::edit::EngineFactory for Factory {
        fn runtime_config(&self) -> kura_core::edit::RuntimeConfig {
            kura_core::edit::RuntimeConfig {
                cores: Some(2),
                budget: Some(64 << 20),
                ..kura_core::edit::RuntimeConfig::default()
            }
        }

        fn codecs(&self, _work_dir: &std::path::Path) -> kura_core::edit::CodecSet {
            kura_core::edit::CodecSet::pinned()
        }
    }

    /// files/ に 1,000 フレーム・8kHz・モノラルの WAV を置く（ハッシュ `cd…`）。
    fn place_wav(layout: &Layout) -> String {
        use kura_core::asset::pcm::{KuraChunk, SampleFormat, write_wav};
        let hash = "cd".repeat(32);
        let samples: Vec<f32> = (0..1_000).map(|i| (i % 100) as f32 / 32_768.0).collect();
        let bytes = write_wav(
            &[&samples],
            8_000,
            SampleFormat::I16,
            &KuraChunk {
                start: 0,
                total: 1_000,
                loop_range: None,
                decoder: "",
            },
        )
        .unwrap();
        let path = layout.content_path(&kura_core::library::parse_hex32(&hash).unwrap(), "wav");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        hash
    }

    /// 試聴の PCM はその場で WAV にして返す（no-store）。範囲の外は 416、エンジンがなければ 503、音声がなければ 404。
    /// 波形の段と帯は作ってあれば immutable で返す。どの応答にも 4 つのヘッダ。
    #[test]
    fn pcm_peaks_and_anim_are_served_and_errors_carry_the_headers() {
        let (_dir, layout) = library();
        let origin = release();
        let engine = Arc::new(kura_core::edit::EngineServices::new(
            Arc::new(Factory),
            layout.staging_dir(),
        ));
        let with = Served {
            layout: &layout,
            engine: Some(&engine),
        };
        let without = Served {
            layout: &layout,
            engine: None,
        };
        let audio = place_wav(&layout);
        let get = |served: &Served<'_>, path: String| {
            reply(Some(served), &request("GET", &path, &[]), &origin, false)
        };

        let ok = get(&with, format!("/pcm/{audio}/10-20"));
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(header_of(&ok, "content-type").as_deref(), Some("audio/wav"));
        assert_eq!(header_of(&ok, "cache-control").as_deref(), Some("no-store"));
        assert_eq!(
            header_of(&ok, "content-length"),
            Some(ok.body().len().to_string())
        );
        assert_eq!(&ok.body()[0..4], b"RIFF");
        assert_decorated(&ok, &origin, "pcm 200");
        let head = reply(
            Some(&with),
            &request("HEAD", &format!("/pcm/{audio}/10-20"), &[]),
            &origin,
            true,
        );
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head.body().is_empty());
        assert_eq!(
            header_of(&head, "content-length"),
            Some(ok.body().len().to_string())
        );

        for (what, served, path, status) in [
            (
                "416（音声の外）",
                &with,
                format!("/pcm/{audio}/990-1001"),
                StatusCode::RANGE_NOT_SATISFIABLE,
            ),
            (
                "503（エンジンなし）",
                &without,
                format!("/pcm/{audio}/0-1"),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                "404（音声でない）",
                &with,
                format!("/pcm/{HASH}/0-1"),
                StatusCode::NOT_FOUND,
            ),
            (
                "404（波形はまだ）",
                &with,
                format!("/peaks/{HASH}"),
                StatusCode::NOT_FOUND,
            ),
            (
                "404（帯はまだ）",
                &with,
                format!("/anim/{HASH}/1"),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let r = get(served, path);
            assert_eq!(r.status(), status, "{what}");
            assert_decorated(&r, &origin, what);
        }

        for (path, file) in [
            (format!("/peaks/{HASH}"), layout.peaks_path(HASH)),
            (format!("/anim/{HASH}/1"), layout.anim_path(HASH)),
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, b"made").unwrap();
            let r = get(&with, path.clone());
            assert_eq!(r.status(), StatusCode::OK, "{path}");
            assert_eq!(r.body(), b"made");
            assert_eq!(
                header_of(&r, "cache-control").as_deref(),
                IMMUTABLE,
                "{path}"
            );
            assert_decorated(&r, &origin, &path);
        }
    }

    #[test]
    fn rejects_other_webviews_before_touching_files() {
        let origin = release();
        for label in ["capture-browser", "Main", ""] {
            let req = request("GET", &format!("/file/{HASH}"), &[]);
            let Gate::Done(r) = gate(label, &req, &origin) else {
                panic!("{label} を受け付けました");
            };
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "{label}");
            // ほかの WebView には CORS のヘッダを返さない
            assert_eq!(header_of(&r, "access-control-allow-origin"), None);
            assert!(r.body().is_empty());
        }
    }

    #[test]
    fn answers_only_get_head_and_options() {
        let origin = release();
        let path = format!("/file/{HASH}");
        let with_origin = [("Origin", origin.as_str())];
        assert!(matches!(
            gate(MAIN_WEBVIEW, &request("GET", &path, &with_origin), &origin),
            Gate::Serve { head: false }
        ));
        assert!(matches!(
            gate(MAIN_WEBVIEW, &request("HEAD", &path, &[]), &origin),
            Gate::Serve { head: true }
        ));

        let Gate::Done(pre) = gate(
            MAIN_WEBVIEW,
            &request("OPTIONS", &path, &with_origin),
            &origin,
        ) else {
            panic!("OPTIONS を読もうとしました");
        };
        assert_eq!(pre.status(), StatusCode::NO_CONTENT);
        assert_decorated(&pre, &origin, "204");
        assert_eq!(
            header_of(&pre, "access-control-allow-methods").as_deref(),
            Some("GET, HEAD, OPTIONS")
        );
        assert_eq!(
            header_of(&pre, "access-control-allow-headers").as_deref(),
            Some("Range")
        );

        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let Gate::Done(r) = gate(MAIN_WEBVIEW, &request(method, &path, &with_origin), &origin)
            else {
                panic!("{method} を受け付けました");
            };
            assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(
                header_of(&r, "allow").as_deref(),
                Some("GET, HEAD, OPTIONS")
            );
            // 状態を fetch で読めるように CORS のヘッダを付ける
            assert_decorated(&r, &origin, method);
        }
    }

    #[test]
    fn rejects_foreign_origin_from_main() {
        let origin = AppOrigin::new(true, Some("http://localhost:1430/"));
        let path = format!("/file/{HASH}");
        // Origin なし（crossorigin のない <img>・<audio>）は受ける
        assert!(matches!(
            gate(MAIN_WEBVIEW, &request("GET", &path, &[]), &origin),
            Gate::Serve { .. }
        ));
        let mut odd = request("GET", &path, &[]);
        odd.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_bytes(b"http://localhost:1430\xff").unwrap(),
        );
        for req in [
            request("GET", &path, &[("Origin", "null")]),
            request("GET", &path, &[("Origin", "tauri://localhost")]),
            request("GET", &path, &[("Origin", "https://example.com")]),
            odd,
        ] {
            let Gate::Done(r) = gate(MAIN_WEBVIEW, &req, &origin) else {
                panic!("よその Origin を受け付けました");
            };
            assert_eq!(r.status(), StatusCode::FORBIDDEN);
            assert_decorated(&r, &origin, "403");
        }
    }

    #[test]
    fn acao_is_never_wildcard() {
        let (_dir, layout) = library();
        let served = Served {
            layout: &layout,
            engine: None,
        };
        for origin in [
            release(),
            AppOrigin::new(true, Some("http://localhost:1430/")),
            AppOrigin::new(true, Some("*")),
        ] {
            let r = reply(
                Some(&served),
                &request("GET", &format!("/file/{HASH}"), &[]),
                &origin,
                false,
            );
            let acao = header_of(&r, "access-control-allow-origin").unwrap();
            assert!(!acao.contains('*'), "{acao}");
            assert!(acao == "tauri://localhost" || acao == "http://localhost:1430");
        }
    }

    #[test]
    fn csp_allows_kura_for_fetch_and_fonts() {
        // Tauri が読むのと同じ型で読む（文字列の CSP がそのまま使われること）
        let config: tauri::utils::config::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let csp = config
            .app
            .security
            .csp
            .expect("CSP がありません")
            .to_string();
        assert_eq!(
            kura_core::serve::csp_problems(&csp),
            Vec::<String>::new(),
            "{csp}"
        );
        for needed in [
            "connect-src 'self' ipc: http://ipc.localhost kura:",
            "font-src 'self' kura:",
            "object-src 'none'",
            "frame-src 'none'",
        ] {
            assert!(csp.contains(needed), "{needed}: {csp}");
        }
    }
}
