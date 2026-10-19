//! `kura://localhost/...` で返すものを決める（docs/architecture.md §4）。HTTP の組み立ては src-tauri が行う。
//!
//! | パス | 返すもの |
//! |---|---|
//! | `/thumb/{hash}/s`・`/thumb/{hash}/l` | サムネイル（長辺 512 / 1600。png、jpg の順に探す） |
//! | `/art/{hash}` | 音楽のジャケット |
//! | `/file/{hash}` | 元ファイル |
//! | `/play/{hash}` | 再生用：プロキシがあればプロキシ、なければ元ファイル |
//! | `/peaks/{hash}` | 波形の段（`peaks/ab/<hash>.peaks`。解析のジョブが書く） |
//! | `/pcm/{hash}/{start}-{end}` | 試聴用の PCM（WAV。その場で作る。[`crate::asset::pcm`]。元は [`pcm_source`]） |
//! | `/anim/{hash}/{版}` | アニメの帯（`anim/ab/<hash>-<版>.png`。`item_anim_get` が書く。[`crate::asset::anim`]） |
//!
//! 受け付け（WebView のラベル・Origin・メソッド）と応答のヘッダ・CSP の確かめは [`access`]。

pub mod access;

pub use access::{
    ALLOWED_METHODS, Admit, AppOrigin, MAIN_WEBVIEW, RELEASE_ORIGIN, admit, csp_problems,
    origin_of, preflight_headers, response_headers,
};

use std::path::{Path, PathBuf};

use crate::import::{MediaKind, classify, format_id, mime_for};
use crate::library::{ANIM_V, Layout, ThumbFormat, ThumbSize};

/// 1 回の応答で返す最大の大きさ（Range のとき）。
pub const MAX_CHUNK: u64 = 4 * 1024 * 1024;

/// パスの種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Thumb {
        hash: String,
        size: ThumbSize,
    },
    Art {
        hash: String,
    },
    File {
        hash: String,
    },
    Play {
        hash: String,
    },
    /// 波形の段（`kura-peaks`）。
    Peaks {
        hash: String,
    },
    /// 試聴用の PCM。フレーム `[start, end)`（`start < end`）。
    Pcm {
        hash: String,
        start: u64,
        end: u64,
    },
    /// アニメの帯の PNG。`version` は帯の並べ方の版（1 から）。
    Anim {
        hash: String,
        version: u32,
    },
}

/// 中身が変わらないものの `Cache-Control`。
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

impl Route {
    /// 中身が変わらないもの（ハッシュ名のサムネイル・ジャケット・波形の段・アニメの帯）。長くキャッシュしてよい。
    pub fn immutable(&self) -> bool {
        matches!(
            self,
            Self::Thumb { .. } | Self::Art { .. } | Self::Peaks { .. } | Self::Anim { .. }
        )
    }

    /// `Cache-Control` の値（付けないなら None）。PCM はその場で作るので残させない。
    pub fn cache_control(&self) -> Option<&'static str> {
        match self {
            Self::Thumb { .. } | Self::Art { .. } | Self::Peaks { .. } | Self::Anim { .. } => {
                Some(IMMUTABLE)
            }
            Self::Pcm { .. } => Some("no-store"),
            Self::File { .. } | Self::Play { .. } => None,
        }
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
        ("peaks", None) => Some(Route::Peaks { hash }),
        ("pcm", Some(span)) => {
            let (start, end) = span.split_once('-')?;
            let (start, end) = (parse_decimal(start)?, parse_decimal(end)?);
            (start < end).then_some(Route::Pcm { hash, start, end })
        }
        ("anim", Some(version)) => {
            let version = u32::try_from(parse_decimal(version)?).ok()?;
            (version >= 1).then_some(Route::Anim { hash, version })
        }
        _ => None,
    }
}

/// 符号・空白のない 10 進（u64 にあふれたら None）。
fn parse_decimal(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn is_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// パスに対応するファイル（なければ None）。PCM はファイルではなくその場で作るので None（元は [`pcm_source`]）。
pub fn resolve(layout: &Layout, route: &Route) -> Option<PathBuf> {
    let path = match route {
        // 透明のあるものは PNG、ないものは JPEG（作り直して形式が変わったら、サムネイルのジョブが古い形式を消す）。
        // 消し損ねが残っても、新しい PNG を先に出す
        Route::Thumb { hash, size } => [ThumbFormat::Png, ThumbFormat::Jpeg]
            .into_iter()
            .map(|format| layout.thumb_path_as(hash, *size, format))
            .find(|path| path.is_file())?,
        Route::Art { hash } => layout.art_path(hash),
        Route::File { hash } => find_content(layout, hash)?,
        // 波形の段（解析のジョブが書く）。解析していなければ 404
        Route::Peaks { hash } => layout.peaks_path(hash),
        // アニメの帯（item_anim_get が書く）。並べ方の版が今のものだけ（古い版は残さない）
        Route::Anim { hash, version } if *version == ANIM_V => layout.anim_path(hash),
        Route::Anim { .. } | Route::Pcm { .. } => return None,
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

/// 試聴の PCM の元（files/ の中の音声のファイルと、コーデックの形式の ID）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmSource {
    pub path: PathBuf,
    pub format: &'static str,
}

/// `/pcm/{hash}/…` の元を探す（files/ になければ、音声の形式でなければ None）。
pub fn pcm_source(layout: &Layout, hash: &str) -> Option<PcmSource> {
    let path = find_content(layout, hash)?;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if classify(&ext) != Some(MediaKind::Audio) {
        return None;
    }
    Some(PcmSource {
        format: format_id(&ext)?,
        path,
    })
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

/// 拡張子から Content-Type を決める。応答に `nosniff` を付けるので、`<img>` で出す SVG は `image/svg+xml` にする。
pub fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // 形式の表（kura-engine の FORMATS）の MIME。表にない派生のファイル（`.peaks` など）はバイト列として配る
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
    fn parses_peaks_pcm_anim_routes() {
        assert_eq!(
            parse_route(&format!("/peaks/{HASH}")),
            Some(Route::Peaks { hash: HASH.into() })
        );
        assert_eq!(
            parse_route(&format!("/pcm/{HASH}/0-100")),
            Some(Route::Pcm {
                hash: HASH.into(),
                start: 0,
                end: 100
            })
        );
        assert_eq!(
            parse_route(&format!("/pcm/{HASH}/44100-{}", u64::MAX)),
            Some(Route::Pcm {
                hash: HASH.into(),
                start: 44100,
                end: u64::MAX
            })
        );
        assert_eq!(
            parse_route(&format!("/anim/{HASH}/1")),
            Some(Route::Anim {
                hash: HASH.into(),
                version: 1
            })
        );
        // 逆の範囲・空の範囲・片側だけ・あふれ・符号・空白・余分な区切り
        for bad in [
            format!("/pcm/{HASH}/100-0"),
            format!("/pcm/{HASH}/5-5"),
            format!("/pcm/{HASH}/0-"),
            format!("/pcm/{HASH}/-100"),
            format!("/pcm/{HASH}/0-18446744073709551616"),
            format!("/pcm/{HASH}/+1-2"),
            format!("/pcm/{HASH}/1 -2"),
            format!("/pcm/{HASH}/1-2-3"),
            format!("/pcm/{HASH}"),
            format!("/pcm/{HASH}/0-100/x"),
            format!("/peaks/{HASH}/x"),
            format!("/anim/{HASH}"),
            format!("/anim/{HASH}/0"),
            format!("/anim/{HASH}/4294967296"),
            format!("/anim/{HASH}/x"),
            "/peaks/abc".to_string(),
        ] {
            assert_eq!(parse_route(&bad), None, "{bad}");
        }
    }

    #[test]
    fn cache_control_by_route() {
        let immutable = Some("public, max-age=31536000, immutable");
        for (path, expected) in [
            (format!("/thumb/{HASH}/s"), immutable),
            (format!("/art/{HASH}"), immutable),
            (format!("/peaks/{HASH}"), immutable),
            (format!("/anim/{HASH}/1"), immutable),
            (format!("/pcm/{HASH}/0-1"), Some("no-store")),
            (format!("/file/{HASH}"), None),
            (format!("/play/{HASH}"), None),
        ] {
            let route = parse_route(&path).unwrap();
            assert_eq!(route.cache_control(), expected, "{path}");
            assert_eq!(route.immutable(), expected == immutable, "{path}");
        }
    }

    fn put(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// 波形の段と帯は、作ってあれば派生のファイルを返す（作るのは解析のジョブと item_anim_get）。帯は今の版だけ。
    /// PCM はファイルではない（その場で作る。[`pcm_source`]）。
    #[test]
    fn peaks_and_anim_resolve_to_their_derived_files() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let peaks = parse_route(&format!("/peaks/{HASH}")).unwrap();
        let anim = parse_route(&format!("/anim/{HASH}/1")).unwrap();
        let pcm = parse_route(&format!("/pcm/{HASH}/0-1")).unwrap();
        // まだ作っていない
        for route in [&peaks, &anim, &pcm] {
            assert_eq!(resolve(&layout, route), None, "{route:?}");
        }
        put(&layout.peaks_path(HASH), b"kurapeak");
        put(&layout.anim_path(HASH), b"png");
        assert_eq!(resolve(&layout, &peaks), Some(layout.peaks_path(HASH)));
        assert_eq!(resolve(&layout, &anim), Some(layout.anim_path(HASH)));
        assert_eq!(resolve(&layout, &pcm), None, "PCM はファイルから返さない");
        // 帯の版が違えば（古い・新しい並べ方）404
        let other = parse_route(&format!("/anim/{HASH}/2")).unwrap();
        assert_eq!(resolve(&layout, &other), None);
        assert_eq!(
            content_type(&layout.anim_path(HASH)),
            "image/png",
            "帯は PNG"
        );
    }

    /// サムネイルは png、jpg の順に探す（透明のあるものは PNG、ないものは JPEG。作り直して形式が変わっても古い形式を
    /// 出さない。書いたら別の形式の古いファイルを消すのはサムネイルのジョブ）。
    #[test]
    fn resolve_prefers_png_thumb_then_jpg() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        for size in [ThumbSize::Small, ThumbSize::Large] {
            let route = Route::Thumb {
                hash: HASH.into(),
                size,
            };
            let jpg = layout.thumb_path_as(HASH, size, ThumbFormat::Jpeg);
            let png = layout.thumb_path_as(HASH, size, ThumbFormat::Png);
            assert_eq!(resolve(&layout, &route), None);
            put(&jpg, b"jpg");
            assert_eq!(resolve(&layout, &route), Some(jpg.clone()));
            assert_eq!(content_type(&jpg), "image/jpeg");
            put(&png, b"png");
            assert_eq!(resolve(&layout, &route), Some(png.clone()), "PNG が先");
            assert_eq!(content_type(&png), "image/png");
            std::fs::remove_file(&jpg).unwrap();
            assert_eq!(resolve(&layout, &route), Some(png));
        }
    }

    /// 試聴の PCM の元：files/ の中の音声のファイルだけ（画像・動画・ないものは None）。
    #[test]
    fn pcm_source_is_an_audio_file_in_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let hash: [u8; 32] = crate::library::parse_hex32(HASH).unwrap();
        assert_eq!(pcm_source(&layout, HASH), None);
        let png = layout.content_path(&hash, "png");
        put(&png, b"png");
        assert_eq!(pcm_source(&layout, HASH), None, "画像は試聴しない");
        std::fs::remove_file(&png).unwrap();
        let ogg = layout.content_path(&hash, "ogg");
        put(&ogg, b"ogg");
        assert_eq!(
            pcm_source(&layout, HASH),
            Some(PcmSource {
                path: ogg,
                format: "ogg"
            })
        );
    }

    #[test]
    fn content_types_for_new_kinds() {
        for (name, mime) in [
            ("x.svg", "image/svg+xml"),
            ("x.SVG", "image/svg+xml"),
            ("x.ttf", "font/ttf"),
            ("x.otf", "font/otf"),
            ("x.woff", "font/woff"),
            ("x.woff2", "font/woff2"),
            ("x.ttc", "font/collection"),
            ("x.glb", "model/gltf-binary"),
            ("x.psd", "image/vnd.adobe.photoshop"),
            ("x.psb", "image/vnd.adobe.photoshop"),
            ("x.peaks", "application/octet-stream"),
            // 形式の表の MIME（WebKit は描かないが、nosniff と CSP: sandbox で文書としても開かない）
            ("x.aseprite", "image/x-aseprite"),
            ("x.ase", "image/x-aseprite"),
            ("x.png", "image/png"),
            ("x.wav", "audio/wav"),
            ("x", "application/octet-stream"),
        ] {
            assert_eq!(content_type(Path::new(name)), mime, "{name}");
        }
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
