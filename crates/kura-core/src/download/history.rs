//! 前に同じものをダウンロードしたか（URL の欄で「再ダウンロード」を選べるようにする。ADR-132）。
//!
//! 取得済みの記録（ADR-95）の中身はツールの ID なので、取得する前に URL から引けない。代わりに
//! ダウンロードのジョブの履歴から、同じものを指す URL を探す。

use rusqlite::Connection;

use super::detect::{host_matches, host_of, url_path};
use super::job::{DownloadInput, DownloadResult};
use crate::Result;
use crate::jobs::{JobId, JobKind};

/// 前に取得したダウンロード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousDownload {
    pub job_id: JobId,
    /// 取得したものの題名。
    pub title: Option<String>,
    /// 取得した日時（終わっていなければジョブを作った日時）。
    pub at: i64,
    /// そのときライブラリに入れたアイテムの数（取得済みとして飛ばしただけなら 0）。
    pub items: u64,
    /// そのうち今もライブラリにある数（ゴミ箱を除く）。
    pub in_library: u64,
}

/// 同じものを指す URL かを比べる鍵。YouTube は動画・再生リストの ID、ほかはホスト（`www.`・`m.` を除く）・
/// パス（末尾の `/` を除く）・追跡用を除いたクエリ（並べ替える）。http(s) でなければ None。
pub fn target_key(url: &str) -> Option<String> {
    let url = url.trim();
    let host = host_of(url)?;
    let host = ["www.", "m.", "mobile."]
        .iter()
        .find_map(|p| host.strip_prefix(p))
        .unwrap_or(&host);
    let host = if host == "twitter.com" { "x.com" } else { host };
    let path = url_path(url).trim_end_matches('/');
    let query = url
        .split('#')
        .next()
        .and_then(|u| u.split_once('?'))
        .map_or("", |(_, q)| q);
    let params: Vec<(&str, &str)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .collect();
    let param = |key: &str| {
        params
            .iter()
            .find(|(k, v)| *k == key && !v.is_empty())
            .map(|(_, v)| *v)
    };

    let youtube = ["youtube.com", "youtube-nocookie.com"]
        .iter()
        .any(|d| host_matches(host, d));
    if host == "youtu.be" || youtube {
        let segment = |prefix: &str| {
            path.strip_prefix(prefix)
                .and_then(|rest| rest.split('/').next())
                .filter(|id| !id.is_empty())
        };
        let video = if host == "youtu.be" {
            segment("/")
        } else if path == "/watch" {
            param("v")
        } else {
            ["/shorts/", "/live/", "/embed/", "/v/"]
                .iter()
                .find_map(|p| segment(p))
        };
        if let Some(id) = video {
            return Some(format!("youtube:video:{id}"));
        }
        if path == "/playlist"
            && let Some(id) = param("list")
        {
            return Some(format!("youtube:list:{id}"));
        }
    }

    // X の投稿の URL のクエリは共有のときに付くものだけ
    let mut kept: Vec<String> = if host == "x.com" {
        Vec::new()
    } else {
        params
            .iter()
            .filter(|(k, _)| !is_tracking(k))
            .map(|(k, v)| format!("{k}={v}"))
            .collect()
    };
    kept.sort();
    let query = if kept.is_empty() {
        String::new()
    } else {
        format!("?{}", kept.join("&"))
    };
    Some(format!("{host}{path}{query}"))
}

/// 共有・広告の追跡に付くクエリ（中身は変わらない）。
fn is_tracking(key: &str) -> bool {
    key.starts_with("utm_")
        || [
            "si", "feature", "pp", "fbclid", "gclid", "igsh", "igshid", "ref_src", "ref_url",
        ]
        .contains(&key)
}

/// `url` と同じものを前に取得したか。ライブラリに入れたことのあるジョブのいちばん新しいもの、
/// なければ取得済みとして飛ばしたジョブのいちばん新しいもの。
pub fn find_previous(conn: &Connection, url: &str) -> Result<Option<PreviousDownload>> {
    let Some(key) = target_key(url) else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT id, input_json, result_json, created_at, finished_at FROM jobs
         WHERE kind = ?1 AND result_json IS NOT NULL ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([JobKind::Download.as_str()], |r| {
        Ok((
            r.get::<_, JobId>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    let mut skipped_only: Option<PreviousDownload> = None;
    for row in rows {
        let (job_id, input, result, created_at, finished_at) = row?;
        let Ok(input) = serde_json::from_str::<DownloadInput>(&input) else {
            continue;
        };
        if target_key(&input.detection.url).as_deref() != Some(key.as_str()) {
            continue;
        }
        let Ok(result) = serde_json::from_str::<DownloadResult>(&result) else {
            continue;
        };
        let previous = PreviousDownload {
            job_id,
            title: result.title.clone(),
            at: finished_at.unwrap_or(created_at),
            items: result.item_ids.len() as u64,
            in_library: 0,
        };
        if result.files > 0 {
            return Ok(Some(PreviousDownload {
                in_library: count_in_library(conn, &result.item_ids)?,
                ..previous
            }));
        }
        if result.skipped > 0 && skipped_only.is_none() {
            skipped_only = Some(previous);
        }
    }
    Ok(skipped_only)
}

/// ライブラリにある（ゴミ箱を除く）アイテムの数。
fn count_in_library(conn: &Connection, ids: &[i64]) -> Result<u64> {
    let mut stmt =
        conn.prepare_cached("SELECT 1 FROM items WHERE id = ?1 AND trashed_at IS NULL")?;
    let mut count = 0;
    for id in ids {
        if stmt.exists([id])? {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use crate::download::{CookieSource, DownloadOptions, detect};

    fn key(url: &str) -> String {
        target_key(url).unwrap()
    }

    #[test]
    fn youtube_urls_compare_by_video_or_list() {
        let video = "youtube:video:dQw4w9WgXcQ";
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ&si=abc&t=42s",
            "https://m.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RD1",
            "https://youtu.be/dQw4w9WgXcQ?si=xyz",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(key(url), video, "{url}");
        }
        assert_eq!(
            key("https://www.youtube.com/playlist?list=PL123&si=a"),
            "youtube:list:PL123"
        );
        assert_ne!(
            key("https://www.youtube.com/watch?v=other"),
            video,
            "別の動画"
        );
        // ID のないページはふつうの比べ方
        assert_eq!(
            key("https://www.youtube.com/@channel/videos/"),
            "youtube.com/@channel/videos"
        );
    }

    #[test]
    fn other_urls_ignore_www_slash_tracking_and_order() {
        assert_eq!(
            key("https://www.pixiv.net/artworks/100/"),
            key("http://pixiv.net/artworks/100#top")
        );
        assert_eq!(
            key("https://twitter.com/user/status/1?s=20&t=abc"),
            "x.com/user/status/1"
        );
        assert_eq!(key("https://x.com/user/status/1"), "x.com/user/status/1");
        assert_eq!(
            key("https://danbooru.donmai.us/posts?tags=a&page=2&utm_source=x"),
            "danbooru.donmai.us/posts?page=2&tags=a"
        );
        assert_ne!(
            key("https://danbooru.donmai.us/posts?tags=a"),
            key("https://danbooru.donmai.us/posts?tags=b")
        );
        assert_ne!(
            key("https://www.pixiv.net/artworks/100"),
            key("https://www.pixiv.net/artworks/1000")
        );
        assert_eq!(target_key("ftp://example.com/a"), None);
    }

    /// ダウンロードのジョブを入れる（`result` は結果）。
    fn job(conn: &Connection, url: &str, result: &DownloadResult, finished_at: i64) -> JobId {
        let input = DownloadInput {
            detection: detect(url, &[]).unwrap(),
            options: DownloadOptions::default(),
            cookies: CookieSource::None,
            redownload: false,
        };
        conn.execute(
            "INSERT INTO jobs (kind, state, input_json, result_json, created_at, finished_at)
             VALUES ('download', 'done', ?1, ?2, 1, ?3)",
            rusqlite::params![
                serde_json::to_string(&input).unwrap(),
                serde_json::to_string(result).unwrap(),
                finished_at
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn item(conn: &Connection, id: i64, trashed_at: Option<i64>) -> i64 {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at,
                                updated_at, trashed_at)
             VALUES (?1, ?2, 'mp4', 'video', 1, 't', 't', 0, 0, ?3)",
            rusqlite::params![id, vec![id as u8; 32], trashed_at],
        )
        .unwrap();
        id
    }

    #[test]
    fn finds_the_latest_download_that_added_files() {
        let conn = test_conn();
        let url = "https://www.youtube.com/watch?v=vid1";
        assert_eq!(find_previous(&conn, url).unwrap(), None);

        let kept = item(&conn, 1, None);
        let trashed = item(&conn, 2, Some(5));
        let fetched = DownloadResult {
            title: Some("動画".into()),
            files: 3,
            added: 3,
            // 3 つめは削除済み（ゴミ箱を空にした）
            item_ids: vec![kept, trashed, 999],
            ..DownloadResult::default()
        };
        let first = job(&conn, url, &fetched, 100);
        // ほかの URL・取得済みで飛ばしただけのジョブはあとでも選ばない
        job(
            &conn,
            "https://www.youtube.com/watch?v=other",
            &fetched,
            200,
        );
        let skipped = DownloadResult {
            skipped: 1,
            ..DownloadResult::default()
        };
        job(&conn, "https://youtu.be/vid1", &skipped, 300);

        let found = find_previous(&conn, "https://youtu.be/vid1?si=share")
            .unwrap()
            .unwrap();
        assert_eq!(
            found,
            PreviousDownload {
                job_id: first,
                title: Some("動画".into()),
                at: 100,
                items: 3,
                in_library: 1,
            }
        );
    }

    #[test]
    fn falls_back_to_a_skipped_download() {
        let conn = test_conn();
        let url = "https://www.pixiv.net/artworks/100";
        // 何も取れなかったジョブは数えない
        job(&conn, url, &DownloadResult::default(), 100);
        assert_eq!(find_previous(&conn, url).unwrap(), None);

        let skipped = DownloadResult {
            title: Some("作品".into()),
            skipped: 2,
            ..DownloadResult::default()
        };
        let id = job(&conn, url, &skipped, 200);
        let found = find_previous(&conn, url).unwrap().unwrap();
        assert_eq!((found.job_id, found.at), (id, 200));
        assert_eq!((found.items, found.in_library), (0, 0));
    }
}
