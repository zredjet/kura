//! `kura://localhost/...` で返すものを決める（docs/architecture.md §4）。HTTP の組み立ては src-tauri が行う。
//!
//! | パス | 返すもの |
//! |---|---|
//! | `/thumb/{hash}/s`・`/thumb/{hash}/l` | サムネイル（長辺 512 / 1600） |
//! | `/art/{hash}` | 音楽のジャケット |
//! | `/file/{hash}` | 元ファイル |
//! | `/play/{hash}` | 再生用：プロキシがあればプロキシ、なければ元ファイル |

use std::path::{Path, PathBuf};

use crate::import::mime_for;
use crate::library::{Layout, ThumbSize};

/// 1 回の応答で返す最大の大きさ（Range のとき）。
pub const MAX_CHUNK: u64 = 4 * 1024 * 1024;

/// パスの種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Thumb { hash: String, size: ThumbSize },
    Art { hash: String },
    File { hash: String },
    Play { hash: String },
}

impl Route {
    /// 中身が変わらないもの（ハッシュ名のサムネイル・ジャケット）。長くキャッシュしてよい。
    pub fn immutable(&self) -> bool {
        matches!(self, Self::Thumb { .. } | Self::Art { .. })
    }
}

/// `/thumb/<hash>/s` などを読む。ハッシュは 64 桁の小文字の 16 進のみ。
pub fn parse_route(path: &str) -> Option<Route> {
    let mut parts = path.trim_start_matches('/').split('/');
    let kind = parts.next()?;
    let hash = parts.next().filter(|h| is_hash(h))?.to_string();
    let rest = parts.next();
    if parts.next().is_some() {
        return None;
    }
    match (kind, rest) {
        ("thumb", Some(size)) => Some(Route::Thumb {
            hash,
            size: ThumbSize::parse(size)?,
        }),
        ("art", None) => Some(Route::Art { hash }),
        ("file", None) => Some(Route::File { hash }),
        ("play", None) => Some(Route::Play { hash }),
        _ => None,
    }
}

fn is_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// パスに対応するファイル（なければ None）。
pub fn resolve(layout: &Layout, route: &Route) -> Option<PathBuf> {
    let path = match route {
        Route::Thumb { hash, size } => layout.thumb_path(hash, *size),
        Route::Art { hash } => layout.art_path(hash),
        Route::File { hash } => find_content(layout, hash)?,
        Route::Play { hash } => {
            let proxy = layout.proxy_path(hash);
            if proxy.is_file() {
                proxy
            } else {
                find_content(layout, hash)?
            }
        }
    };
    path.is_file().then_some(path)
}

/// `files/ab/cd/<hash>.<ext>` を探す（拡張子は URL に含めないので、フォルダの中から探す）。
fn find_content(layout: &Layout, hash: &str) -> Option<PathBuf> {
    let prefix = format!("{hash}.");
    std::fs::read_dir(layout.content_dir(hash)?)
        .ok()?
        .filter_map(|entry| entry.ok())
        .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
}

/// 拡張子から Content-Type を決める。
pub fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    mime_for(&ext).unwrap_or("application/octet-stream")
}

/// Range ヘッダの解釈。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRequest {
    /// Range なし（全体を返す）。
    Full,
    /// `start`〜`end`（両端を含む）を返す。返す量は `MAX_CHUNK` まで。
    Partial { start: u64, end: u64 },
    /// 返せない（416）。
    Unsatisfiable,
}

/// `bytes=start-end`・`bytes=start-`・`bytes=-suffix` を読む（1 区間だけ。複数指定は最初の区間）。
pub fn parse_range(header: Option<&str>, len: u64) -> RangeRequest {
    let Some(header) = header else {
        return RangeRequest::Full;
    };
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeRequest::Unsatisfiable;
    };
    let first = spec.split(',').next().unwrap_or("").trim();
    let Some((a, b)) = first.split_once('-') else {
        return RangeRequest::Unsatisfiable;
    };
    if len == 0 {
        return RangeRequest::Unsatisfiable;
    }
    let (start, end) = match (a.trim(), b.trim()) {
        ("", "") => return RangeRequest::Unsatisfiable,
        ("", suffix) => match suffix.parse::<u64>() {
            Ok(n) if n > 0 => (len - n.min(len), len - 1),
            _ => return RangeRequest::Unsatisfiable,
        },
        (start, "") => match start.parse::<u64>() {
            Ok(s) => (s, len - 1),
            Err(_) => return RangeRequest::Unsatisfiable,
        },
        (start, end) => match (start.parse::<u64>(), end.parse::<u64>()) {
            (Ok(s), Ok(e)) if s <= e => (s, e.min(len - 1)),
            _ => return RangeRequest::Unsatisfiable,
        },
    };
    if start >= len {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Partial {
        start,
        end: end.min(start + MAX_CHUNK - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[test]
    fn parses_routes() {
        assert_eq!(
            parse_route(&format!("/thumb/{HASH}/s")),
            Some(Route::Thumb {
                hash: HASH.into(),
                size: ThumbSize::Small
            })
        );
        assert_eq!(
            parse_route(&format!("/art/{HASH}")),
            Some(Route::Art { hash: HASH.into() })
        );
        assert_eq!(
            parse_route(&format!("/file/{HASH}")),
            Some(Route::File { hash: HASH.into() })
        );
        assert_eq!(
            parse_route(&format!("/play/{HASH}")),
            Some(Route::Play { hash: HASH.into() })
        );
        assert_eq!(parse_route(&format!("/thumb/{HASH}/x")), None);
        assert_eq!(parse_route(&format!("/thumb/{HASH}")), None);
        assert_eq!(parse_route("/file/../../etc/passwd"), None);
        assert_eq!(parse_route(&format!("/file/{}", HASH.to_uppercase())), None);
        assert_eq!(parse_route(&format!("/file/{HASH}/extra")), None);
        assert!(
            parse_route(&format!("/thumb/{HASH}/l"))
                .unwrap()
                .immutable()
        );
        assert!(!parse_route(&format!("/play/{HASH}")).unwrap().immutable());
    }

    #[test]
    fn ranges() {
        use RangeRequest::*;
        assert_eq!(parse_range(None, 100), Full);
        assert_eq!(
            parse_range(Some("bytes=0-1"), 100),
            Partial { start: 0, end: 1 }
        );
        assert_eq!(
            parse_range(Some("bytes=10-"), 100),
            Partial { start: 10, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=90-200"), 100),
            Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-500"), 100),
            Partial { start: 0, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=0-1, 5-6"), 100),
            Partial { start: 0, end: 1 }
        );
        assert_eq!(parse_range(Some("bytes=100-"), 100), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=5-1"), 100), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=abc"), 100), Unsatisfiable);
        assert_eq!(parse_range(Some("items=0-1"), 100), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-"), 0), Unsatisfiable);
    }

    #[test]
    fn large_ranges_are_cut_to_chunks() {
        let len = 10 * MAX_CHUNK;
        assert_eq!(
            parse_range(Some("bytes=0-"), len),
            RangeRequest::Partial {
                start: 0,
                end: MAX_CHUNK - 1
            }
        );
        assert_eq!(
            parse_range(Some(&format!("bytes={}-", 9 * MAX_CHUNK + 5)), len),
            RangeRequest::Partial {
                start: 9 * MAX_CHUNK + 5,
                end: len - 1
            }
        );
    }

    #[test]
    fn resolves_files_thumbs_and_play() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let hash: [u8; 32] = crate::library::parse_hex32(HASH).unwrap();
        let file = layout.content_path(&hash, "mov");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"movie").unwrap();

        let play = Route::Play { hash: HASH.into() };
        assert_eq!(resolve(&layout, &play), Some(file.clone()));
        assert_eq!(
            resolve(&layout, &Route::File { hash: HASH.into() }),
            Some(file.clone())
        );
        assert_eq!(content_type(&file), "video/quicktime");

        // プロキシがあればそちら
        let proxy = layout.proxy_path(HASH);
        std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
        std::fs::write(&proxy, b"proxy").unwrap();
        assert_eq!(resolve(&layout, &play), Some(proxy));

        // サムネイルはできるまで見つからない
        let thumb = Route::Thumb {
            hash: HASH.into(),
            size: ThumbSize::Small,
        };
        assert_eq!(resolve(&layout, &thumb), None);
        let path = layout.thumb_path(HASH, ThumbSize::Small);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"jpg").unwrap();
        assert_eq!(resolve(&layout, &thumb), Some(path.clone()));
        assert_eq!(content_type(&path), "image/jpeg");
    }
}
