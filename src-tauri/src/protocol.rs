//! `kura://localhost/...` のカスタムプロトコル（docs/architecture.md §4）。
//! サムネイル・ジャケット・元ファイル・再生用ファイルを返す。Range は 1 区間・1 回 4MB まで。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use kura_core::serve::{RangeRequest, content_type, parse_range, parse_route, resolve};
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// 1 件の要求に答える（ファイルを開くのでブロックする。呼ぶ側で spawn_blocking する）。
pub fn respond(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(session) = app.try_state::<AppState>().and_then(|s| s.current()) else {
        return empty(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(route) = parse_route(request.uri().path()) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let Some(path) = resolve(session.library().layout(), &route) else {
        return empty(StatusCode::NOT_FOUND);
    };
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let result = serve_file(&path, range, route.immutable());
    let response = result.unwrap_or_else(|err| {
        tracing::warn!(%err, path = %path.display(), "kura:// のファイルを読めませんでした");
        empty(StatusCode::INTERNAL_SERVER_ERROR)
    });
    tracing::debug!(
        uri = %request.uri(),
        range = range.unwrap_or("-"),
        status = response.status().as_u16(),
        bytes = response.body().len(),
        "kura://"
    );
    response
}

fn serve_file(
    path: &Path,
    range: Option<&str>,
    immutable: bool,
) -> std::io::Result<Response<Vec<u8>>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let (code, body, content_range) = match parse_range(range, len) {
        RangeRequest::Full => {
            let mut body = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            file.read_to_end(&mut body)?;
            (StatusCode::OK, body, None)
        }
        RangeRequest::Partial { start, end } => {
            file.seek(SeekFrom::Start(start))?;
            let mut body = vec![0u8; usize::try_from(end - start + 1).unwrap_or(0)];
            file.read_exact(&mut body)?;
            (
                StatusCode::PARTIAL_CONTENT,
                body,
                Some(format!("bytes {start}-{end}/{len}")),
            )
        }
        RangeRequest::Unsatisfiable => {
            let response = Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Vec::new());
            return Ok(response.unwrap_or_else(|_| empty(StatusCode::RANGE_NOT_SATISFIABLE)));
        }
    };
    let mut builder = Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, content_type(path))
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::ACCEPT_RANGES, "bytes");
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    if immutable {
        builder = builder.header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
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

    fn header_of(r: &Response<Vec<u8>>, name: header::HeaderName) -> Option<&str> {
        r.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[test]
    fn serves_full_partial_and_unsatisfiable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mp4");
        std::fs::write(&path, b"0123456789").unwrap();

        let full = serve_file(&path, None, false).unwrap();
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.body(), b"0123456789");
        assert_eq!(header_of(&full, header::CONTENT_TYPE), Some("video/mp4"));
        assert_eq!(header_of(&full, header::ACCEPT_RANGES), Some("bytes"));
        assert_eq!(header_of(&full, header::CACHE_CONTROL), None);

        let part = serve_file(&path, Some("bytes=2-5"), false).unwrap();
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.body(), b"2345");
        assert_eq!(
            header_of(&part, header::CONTENT_RANGE),
            Some("bytes 2-5/10")
        );
        assert_eq!(header_of(&part, header::CONTENT_LENGTH), Some("4"));

        let tail = serve_file(&path, Some("bytes=7-"), false).unwrap();
        assert_eq!(tail.body(), b"789");

        let bad = serve_file(&path, Some("bytes=20-"), false).unwrap();
        assert_eq!(bad.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header_of(&bad, header::CONTENT_RANGE), Some("bytes */10"));
    }

    #[test]
    fn thumbnails_are_cached_forever() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x_s.jpg");
        std::fs::write(&path, b"jpg").unwrap();
        let r = serve_file(&path, None, true).unwrap();
        assert_eq!(header_of(&r, header::CONTENT_TYPE), Some("image/jpeg"));
        assert_eq!(
            header_of(&r, header::CACHE_CONTROL),
            Some("public, max-age=31536000, immutable")
        );
    }
}
