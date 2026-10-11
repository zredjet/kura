//! ツールが書いたメタデータ（yt-dlp の `.info.json`、gallery-dl の `--write-metadata`）を、
//! アイテムの題名・URL・投稿日・タグ・ユニットのまとまりにする（docs/architecture.md §9 の「登録」）。

use serde_json::Value;

use crate::tags::TagName;
use crate::text::nfc;
use crate::time::{days_from_civil, parse_iso8601_ms};

/// 自動で付けるタグの選び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagPolicy {
    /// `投稿者:` / `作者:`。
    pub uploader: bool,
    /// サイトのタグ・ハッシュタグ。
    pub site_tags: bool,
}

/// 1 ファイルの登録内容。
#[derive(Debug, Clone, PartialEq)]
pub struct FileMeta {
    pub title: String,
    pub source_url: Option<String>,
    pub site: String,
    /// 動画 ID・作品 ID（`remote_sources.remote_id`）。
    pub remote_id: Option<String>,
    /// 複数ページの作品のページ番号・再生リストの番号。
    pub page_index: Option<i64>,
    /// 投稿日時（UTC ミリ秒）。
    pub captured_at: Option<i64>,
    pub tags: Vec<TagName>,
    /// まとめて 1 つのユニットにするもの（複数ファイルの投稿・再生リスト）。
    pub group: Option<Group>,
}

/// ユニットにするまとまり。
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// まとまりを見分ける名前（`pixiv:147501205`・`youtube:playlist:PL…`）。
    pub key: String,
    pub name: String,
    pub source_url: Option<String>,
    /// ユニットの中の並び。
    pub pos: f64,
}

/// yt-dlp の `.info.json` から。
pub fn from_ytdlp(
    info: &Value,
    site: &str,
    policy: TagPolicy,
    playlist_unit: bool,
    job_url: &str,
) -> FileMeta {
    let title = text(info, "title").unwrap_or_else(|| text(info, "id").unwrap_or_default());
    let mut tags = Vec::new();
    if policy.uploader
        && let Some(name) = text(info, "uploader")
            .or_else(|| text(info, "channel"))
            .or_else(|| text(info, "creator"))
    {
        push_tag(&mut tags, "投稿者", &name);
    }
    if policy.site_tags {
        let mut words = hashtags(&title);
        if let Some(description) = text(info, "description") {
            words.extend(hashtags(&description));
        }
        for word in words {
            push_tag(&mut tags, "", &word);
        }
    }
    let captured_at = info
        .get("timestamp")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t > 0.0)
        .map(|t| (t * 1000.0) as i64)
        .or_else(|| text(info, "upload_date").and_then(|d| yyyymmdd_ms(&d)));
    let playlist_index = info.get("playlist_index").and_then(Value::as_i64);
    let group = match (playlist_unit, text(info, "playlist_id")) {
        (true, Some(list)) => {
            let count = info
                .get("n_entries")
                .or_else(|| info.get("playlist_count"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            (count > 1).then(|| Group {
                key: format!("{site}:playlist:{list}"),
                name: text(info, "playlist_title")
                    .or_else(|| text(info, "playlist"))
                    .unwrap_or_else(|| list.clone()),
                source_url: text(info, "playlist_webpage_url").or(Some(job_url.to_string())),
                pos: playlist_index.unwrap_or(0) as f64,
            })
        }
        _ => None,
    };
    FileMeta {
        title,
        source_url: text(info, "webpage_url").or_else(|| Some(job_url.to_string())),
        site: site.to_string(),
        remote_id: text(info, "id"),
        page_index: playlist_index.filter(|_| group.is_some()),
        captured_at,
        tags,
        group,
    }
}

/// gallery-dl の `--write-metadata` から。`stem` はファイル名（拡張子なし。題名がないときに使う）。
pub fn from_gallerydl(
    meta: &Value,
    fallback_site: &str,
    policy: TagPolicy,
    job_url: &str,
    stem: &str,
) -> FileMeta {
    let category = text(meta, "category").unwrap_or_else(|| fallback_site.to_string());
    let site = match category.as_str() {
        "twitter" => "x".to_string(),
        other => other.to_string(),
    };
    let id = text(meta, "id")
        .or_else(|| text(meta, "tweet_id"))
        .or_else(|| text(meta, "post_id"));
    let num = meta.get("num").and_then(Value::as_i64);
    let count = meta
        .get("count")
        .or_else(|| meta.get("page_count"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    // pixiv の num は 0 から、ほかは 1 から
    let page = num.map(|n| if site == "pixiv" { n + 1 } else { n });

    let author = |key: &str| -> Option<String> {
        let user = meta.get(key)?;
        text(user, "nick").or_else(|| text(user, "name"))
    };
    let (namespace, uploader) = if site == "pixiv" || site == "fanbox" {
        ("作者", author("user").or_else(|| author("creator")))
    } else {
        (
            "投稿者",
            author("author")
                .or_else(|| author("user"))
                .or_else(|| text(meta, "uploader"))
                .or_else(|| text(meta, "artist")),
        )
    };
    let base_title = text(meta, "title")
        .or_else(|| text(meta, "content").map(|c| first_line(&c, 60)))
        .or_else(|| text(meta, "description").map(|c| first_line(&c, 60)))
        .unwrap_or_else(|| stem.to_string());
    let title = match page {
        Some(page) if count > 1 => format!("{base_title}（{page}/{count}）"),
        _ => base_title.clone(),
    };

    let mut tags = Vec::new();
    if policy.uploader
        && let Some(name) = &uploader
    {
        push_tag(&mut tags, namespace, name);
    }
    if policy.site_tags {
        for key in ["tags", "hashtags"] {
            if let Some(list) = meta.get(key).and_then(Value::as_array) {
                for tag in list {
                    let name = tag
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| text(tag, "name"));
                    if let Some(name) = name {
                        push_tag(&mut tags, "", &name);
                    }
                }
            }
        }
    }
    let source_url = match (site.as_str(), &id) {
        ("pixiv", Some(id)) => Some(format!("https://www.pixiv.net/artworks/{id}")),
        ("x", Some(id)) => meta
            .get("author")
            .and_then(|a| text(a, "name"))
            .map(|user| format!("https://x.com/{user}/status/{id}")),
        _ => None,
    }
    .or_else(|| Some(job_url.to_string()));
    let captured_at = text(meta, "date").and_then(|d| parse_iso8601_ms(&d));
    let group = match &id {
        Some(id) if count > 1 => Some(Group {
            key: format!("{site}:{id}"),
            name: base_title,
            source_url: source_url.clone(),
            pos: page.unwrap_or(0) as f64,
        }),
        _ => None,
    };
    FileMeta {
        title,
        source_url,
        site,
        remote_id: id,
        page_index: page,
        captured_at,
        tags,
        group,
    }
}

/// 文中のハッシュタグ（`#yoga` → `yoga`）。重複は除く。
pub fn hashtags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == '、' || c == '。') {
        let Some(tag) = word.strip_prefix('#').or_else(|| word.strip_prefix('＃')) else {
            continue;
        };
        let tag: String = tag
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == 'ー')
            .collect();
        if !tag.is_empty() && !tag.chars().all(|c| c.is_ascii_digit()) && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// 値を文字列で（数の ID も文字列にする）。空なら None。
fn text(v: &Value, key: &str) -> Option<String> {
    let value = v.get(key)?;
    let s = match value {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

/// タグを足す（NFC・前後の空白を除く・重複は除く）。名前空間なしのタグに `:` があっても名前として扱う。
fn push_tag(tags: &mut Vec<TagName>, namespace: &str, name: &str) {
    let name = nfc(name.trim());
    if name.is_empty() || name.chars().count() > 100 {
        return;
    }
    let tag = TagName {
        namespace: namespace.to_string(),
        name,
    };
    if !tags.contains(&tag) {
        tags.push(tag);
    }
}

/// 本文の 1 行目（長ければ `max` 文字で切る）。
fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// `20260421` → その日の 0 時（UTC）。
fn yyyymmdd_ms(s: &str) -> Option<i64> {
    if s.len() != 8 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let y = s.get(0..4)?.parse().ok()?;
    let m = s.get(4..6)?.parse().ok()?;
    let d = s.get(6..8)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: TagPolicy = TagPolicy {
        uploader: true,
        site_tags: true,
    };

    fn tag(ns: &str, name: &str) -> TagName {
        TagName {
            namespace: ns.into(),
            name: name.into(),
        }
    }

    #[test]
    fn youtube_video() {
        let info = json!({
            "id": "9fcbOOzzFWM",
            "title": "Do These 7 Moves #fitness",
            "uploader": "PERFECT WORKOUT",
            "channel": "PERFECT WORKOUT",
            "upload_date": "20260421",
            "timestamp": 1776774932,
            "webpage_url": "https://www.youtube.com/watch?v=9fcbOOzzFWM",
            "description": "朝の運動 #yoga #stretching\n#fitness #2026",
            "tags": ["yoga", "morning exercise"],
        });
        let m = from_ytdlp(&info, "youtube", ALL, true, "https://youtu.be/9fcbOOzzFWM");
        assert_eq!(m.title, "Do These 7 Moves #fitness");
        assert_eq!(
            m.source_url.as_deref(),
            Some("https://www.youtube.com/watch?v=9fcbOOzzFWM")
        );
        assert_eq!(m.remote_id.as_deref(), Some("9fcbOOzzFWM"));
        assert_eq!(m.captured_at, Some(1_776_774_932_000));
        // 動画の tags（検索用の語）は付けず、ハッシュタグだけ
        assert_eq!(
            m.tags,
            vec![
                tag("投稿者", "PERFECT WORKOUT"),
                tag("", "fitness"),
                tag("", "yoga"),
                tag("", "stretching"),
            ]
        );
        assert_eq!((m.group, m.page_index), (None, None));

        let none = TagPolicy {
            uploader: false,
            site_tags: false,
        };
        assert!(
            from_ytdlp(&info, "youtube", none, true, "u")
                .tags
                .is_empty()
        );
    }

    #[test]
    fn youtube_playlist_becomes_a_group() {
        let info = json!({
            "id": "a", "title": "1 本目", "upload_date": "20260102",
            "playlist_id": "PL1", "playlist_title": "作業用", "playlist_index": 3, "n_entries": 12,
        });
        let m = from_ytdlp(
            &info,
            "youtube",
            ALL,
            true,
            "https://www.youtube.com/playlist?list=PL1",
        );
        assert_eq!(
            m.captured_at,
            Some(days_from_civil(2026, 1, 2) * 86_400_000)
        );
        assert_eq!(
            m.group,
            Some(Group {
                key: "youtube:playlist:PL1".into(),
                name: "作業用".into(),
                source_url: Some("https://www.youtube.com/playlist?list=PL1".into()),
                pos: 3.0,
            })
        );
        assert_eq!(m.page_index, Some(3));
        assert_eq!(from_ytdlp(&info, "youtube", ALL, false, "u").group, None);
    }

    #[test]
    fn pixiv_multi_page_work() {
        let meta = json!({
            "category": "pixiv", "subcategory": "work", "id": 147501205, "num": 1,
            "count": 2, "page_count": 2, "title": "ひとやすみ",
            "date": "2026-07-22 08:29:06",
            "tags": ["オリジナル", "男の子", "Re:ゼロ"],
            "user": {"id": 16996441, "name": "KR", "account": "takaaao1121"},
        });
        let m = from_gallerydl(
            &meta,
            "pixiv",
            ALL,
            "https://www.pixiv.net/artworks/147501205",
            "147501205_p1",
        );
        assert_eq!(m.title, "ひとやすみ（2/2）");
        assert_eq!(m.site, "pixiv");
        assert_eq!(m.remote_id.as_deref(), Some("147501205"));
        assert_eq!(m.page_index, Some(2));
        assert_eq!(
            m.source_url.as_deref(),
            Some("https://www.pixiv.net/artworks/147501205")
        );
        assert_eq!(m.captured_at, parse_iso8601_ms("2026-07-22T08:29:06Z"));
        assert_eq!(
            m.tags,
            vec![
                tag("作者", "KR"),
                tag("", "オリジナル"),
                tag("", "男の子"),
                tag("", "Re:ゼロ"),
            ]
        );
        assert_eq!(
            m.group,
            Some(Group {
                key: "pixiv:147501205".into(),
                name: "ひとやすみ".into(),
                source_url: Some("https://www.pixiv.net/artworks/147501205".into()),
                pos: 2.0,
            })
        );
    }

    #[test]
    fn single_page_posts_are_not_grouped() {
        let meta = json!({"category": "pixiv", "id": 1, "num": 0, "count": 1, "title": "一枚"});
        let m = from_gallerydl(&meta, "pixiv", ALL, "u", "1_p0");
        assert_eq!((m.title.as_str(), m.group), ("一枚", None));
    }

    #[test]
    fn x_post_uses_text_and_hashtags() {
        let meta = json!({
            "category": "twitter", "tweet_id": 99, "num": 1, "count": 2,
            "content": "夕焼けがきれい\n二行目", "date": "2026-10-01 10:00:00",
            "author": {"name": "someone", "nick": "だれか"},
            "hashtags": ["夕焼け"],
        });
        let m = from_gallerydl(&meta, "x", ALL, "https://x.com/someone/status/99", "99_1");
        assert_eq!(m.site, "x");
        assert_eq!(m.title, "夕焼けがきれい（1/2）");
        assert_eq!(
            m.source_url.as_deref(),
            Some("https://x.com/someone/status/99")
        );
        assert_eq!(m.tags, vec![tag("投稿者", "だれか"), tag("", "夕焼け")]);
        assert_eq!(m.group.map(|g| g.key), Some("x:99".into()));
    }

    #[test]
    fn falls_back_to_the_file_name_and_job_url() {
        let m = from_gallerydl(
            &json!({}),
            "example.org",
            ALL,
            "https://example.org/a",
            "img01",
        );
        assert_eq!(m.title, "img01");
        assert_eq!(m.site, "example.org");
        assert_eq!(m.source_url.as_deref(), Some("https://example.org/a"));
        assert!(m.tags.is_empty());
    }

    #[test]
    fn finds_hashtags() {
        assert_eq!(
            hashtags("#yoga ＃ストレッチ、#2026 #a_b! #yoga"),
            vec!["yoga", "ストレッチ", "a_b"]
        );
        assert!(hashtags("# 空 #").is_empty());
    }
}
