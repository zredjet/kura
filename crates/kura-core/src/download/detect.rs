//! URL から取得方法を決める（docs/architecture.md §9 の「判定」）。ユーザーの規則を先に見て、
//! なければホストの表から選ぶ。表にないホストは yt-dlp の汎用の取得に任せる。

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// 取得に使う外部ツール。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    YtDlp,
    GalleryDl,
}

impl Tool {
    /// コマンドの名前。
    pub fn command(self) -> &'static str {
        match self {
            Self::YtDlp => "yt-dlp",
            Self::GalleryDl => "gallery-dl",
        }
    }
}

/// ユーザーが足す規則（URL の正規表現 → ツールと追加の引数）。上から順に見て、最初に一致したものを使う。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRule {
    pub name: String,
    pub pattern: String,
    pub tool: Tool,
    #[serde(default)]
    pub args: Vec<String>,
}

/// 判定の結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    /// 前後の空白を除いた URL。
    pub url: String,
    /// サイトの識別名（`items.source_site`・`remote_sources.site`）。表にないホストはホスト名。
    pub site: String,
    /// 画面に出すサイト名（「YouTube」など）。
    pub site_label: String,
    pub tool: Tool,
    /// 一致した規則の名前。
    pub rule: Option<String>,
    /// 規則の追加の引数。
    pub extra_args: Vec<String>,
    /// 再生リスト・まとめのページ（中の動画をまとめて取る）。
    pub playlist: bool,
    /// 再生リストの中の 1 本を指す URL（`--no-playlist` でその 1 本だけ取る）。
    pub single_in_list: bool,
}

/// ホストの表：(サイト名, 表示名, ホスト, ツール)。ホストはそのものかサブドメインで一致する。
const SITES: &[(&str, &str, &[&str], Tool)] = &[
    (
        "youtube",
        "YouTube",
        &["youtube.com", "youtu.be", "youtube-nocookie.com"],
        Tool::YtDlp,
    ),
    (
        "niconico",
        "ニコニコ動画",
        &["nicovideo.jp", "nico.ms"],
        Tool::YtDlp,
    ),
    ("soundcloud", "SoundCloud", &["soundcloud.com"], Tool::YtDlp),
    ("vimeo", "Vimeo", &["vimeo.com"], Tool::YtDlp),
    ("twitch", "Twitch", &["twitch.tv"], Tool::YtDlp),
    (
        "bilibili",
        "bilibili",
        &["bilibili.com", "b23.tv"],
        Tool::YtDlp,
    ),
    ("tiktok", "TikTok", &["tiktok.com"], Tool::YtDlp),
    ("bandcamp", "Bandcamp", &["bandcamp.com"], Tool::YtDlp),
    ("pixiv", "pixiv", &["pixiv.net"], Tool::GalleryDl),
    ("fanbox", "pixivFANBOX", &["fanbox.cc"], Tool::GalleryDl),
    ("x", "X", &["x.com", "twitter.com"], Tool::GalleryDl),
    ("bluesky", "Bluesky", &["bsky.app"], Tool::GalleryDl),
    ("misskey", "Misskey", &["misskey.io"], Tool::GalleryDl),
    (
        "instagram",
        "Instagram",
        &["instagram.com"],
        Tool::GalleryDl,
    ),
    ("tumblr", "Tumblr", &["tumblr.com"], Tool::GalleryDl),
    (
        "deviantart",
        "DeviantArt",
        &["deviantart.com"],
        Tool::GalleryDl,
    ),
    (
        "artstation",
        "ArtStation",
        &["artstation.com"],
        Tool::GalleryDl,
    ),
    ("imgur", "Imgur", &["imgur.com"], Tool::GalleryDl),
    (
        "reddit",
        "Reddit",
        &["reddit.com", "redd.it"],
        Tool::GalleryDl,
    ),
    (
        "danbooru",
        "Danbooru",
        &["danbooru.donmai.us"],
        Tool::GalleryDl,
    ),
    ("gelbooru", "Gelbooru", &["gelbooru.com"], Tool::GalleryDl),
    (
        "safebooru",
        "Safebooru",
        &["safebooru.org"],
        Tool::GalleryDl,
    ),
    ("yandere", "yande.re", &["yande.re"], Tool::GalleryDl),
    (
        "konachan",
        "Konachan",
        &["konachan.com", "konachan.net"],
        Tool::GalleryDl,
    ),
];

/// URL の取得方法を決める。`http(s)://` でなければエラー。
pub fn detect(url: &str, rules: &[DownloadRule]) -> Result<Detection> {
    let url = url.trim();
    let host = host_of(url).ok_or_else(|| {
        CoreError::Invalid("http:// か https:// で始まる URL を入れてください".into())
    })?;
    let known = SITES
        .iter()
        .find(|(_, _, hosts, _)| hosts.iter().any(|h| host_matches(&host, h)));
    let (site, site_label, mut tool) = match known {
        Some((site, label, _, tool)) => (site.to_string(), label.to_string(), *tool),
        None => {
            let bare = host.strip_prefix("www.").unwrap_or(&host).to_string();
            (bare.clone(), bare, Tool::YtDlp)
        }
    };
    let mut rule_name = None;
    let mut extra_args = Vec::new();
    if let Some(rule) = rules.iter().find(|r| rule_matches(r, url)) {
        tool = rule.tool;
        rule_name = Some(rule.name.clone());
        extra_args = rule.args.clone();
    }
    let (playlist, single_in_list) = if site == "youtube" {
        let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
        let has = |key: &str| {
            query
                .split(['&', '#'])
                .any(|p| p.split_once('=').is_some_and(|(k, _)| k == key))
        };
        let playlist_page = url_path(url).starts_with("/playlist") && has("list");
        let single = has("list") && !playlist_page;
        (playlist_page, single)
    } else {
        (false, false)
    };
    Ok(Detection {
        url: url.to_string(),
        site,
        site_label,
        tool,
        rule: rule_name,
        extra_args,
        playlist,
        single_in_list,
    })
}

/// 規則が URL に一致するか（正規表現が読めなければ一致しない）。
fn rule_matches(rule: &DownloadRule, url: &str) -> bool {
    Regex::new(&rule.pattern).is_ok_and(|re| re.is_match(url))
}

/// 規則を保存する前に確かめる：名前・正規表現・引数（出力先や実行を変える引数は受け付けない）。
pub fn validate_rule(rule: &DownloadRule) -> Result<()> {
    if rule.name.trim().is_empty() {
        return Err(CoreError::Invalid("規則の名前を入れてください".into()));
    }
    Regex::new(&rule.pattern).map_err(|err| {
        // regex のエラーは位置を示す複数行なので、最後の行（理由）だけにする
        let text = err.to_string();
        let reason = text.lines().last().unwrap_or(&text).trim().to_string();
        CoreError::Invalid(format!("正規表現が読めません（{reason}）"))
    })?;
    if let Some(arg) = rule.args.iter().find(|a| forbidden_arg(rule.tool, a)) {
        return Err(CoreError::Invalid(format!(
            "この引数は使えません（保存先・ファイル名・実行するコマンドはアプリが決めます）: {arg}"
        )));
    }
    Ok(())
}

/// 保存先・ファイル名・取得済みの記録・出力の形式・ほかのコマンドの実行・設定ファイルを変える引数。
fn forbidden_arg(tool: Tool, arg: &str) -> bool {
    let flag = arg.split_once('=').map_or(arg, |(f, _)| f);
    let common = [
        "--download-archive",
        "--no-download-archive",
        "--exec",
        "--print",
        "--print-to-file",
        "--restrict-filenames",
        "--windows-filenames",
        "--config",
        "--input-file",
        "--quiet",
        "-q",
    ];
    if common.contains(&flag) {
        return true;
    }
    match tool {
        Tool::YtDlp => [
            "-o",
            "--output",
            "-P",
            "--paths",
            "-O",
            "--exec-before-download",
            "--no-exec",
            "--progress-template",
            "--newline",
            "--no-progress",
            "--write-info-json",
            "--no-write-info-json",
            "--config-locations",
            "--batch-file",
            "-a",
            "--load-info-json",
            "-s",
            "--simulate",
            "--skip-download",
            "-j",
            "-J",
            "--dump-json",
            "--dump-single-json",
            "--paths-temp",
        ]
        .contains(&flag),
        Tool::GalleryDl => {
            [
                "-d",
                "--destination",
                "-D",
                "--directory",
                "-f",
                "--filename",
                "--exec-after",
                "-N",
                "--Print",
                "--Print-to-file",
                "--write-metadata",
                "-c",
                "--config-json",
                "--config-yaml",
                "--config-toml",
                "--config-ignore",
                "-i",
                "-I",
                "--input-file-comment",
                "-x",
                "--input-file-delete",
                "-g",
                "--get-urls",
                "-G",
                "--resolve-urls",
                "-j",
                "--dump-json",
                "-J",
                "--resolve-json",
                "-s",
                "--simulate",
                "--no-download",
                "--zip",
                "--cbz",
                "--rename",
                "--rename-to",
            ]
            .contains(&flag)
                || option_key_forbidden(arg)
        }
    }
}

/// gallery-dl の `-o KEY=VALUE` で、保存先・ファイル名・記録・後処理を変えるもの。
fn option_key_forbidden(arg: &str) -> bool {
    let key = arg.split_once('=').map_or(arg, |(k, _)| k);
    let key = key.rsplit('.').next().unwrap_or(key);
    [
        "base-directory",
        "directory",
        "filename",
        "archive",
        "postprocessors",
        "skip",
        "path-restrict",
    ]
    .contains(&key)
}

/// `scheme://[user@]host[:port]/...` のホスト（小文字）。http(s) でなければ None。
pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?.trim_end_matches('.');
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// URL のパス（`/watch` など）。
pub(crate) fn url_path(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let start = rest.find('/').unwrap_or(rest.len());
    let path = rest.get(start..).unwrap_or("");
    path.split(['?', '#']).next().unwrap_or(path)
}

pub(crate) fn host_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_tools_by_host() {
        let d = detect(" https://www.youtube.com/watch?v=abc ", &[]).unwrap();
        assert_eq!(d.url, "https://www.youtube.com/watch?v=abc");
        assert_eq!((d.site.as_str(), d.tool), ("youtube", Tool::YtDlp));
        assert_eq!(d.site_label, "YouTube");
        assert!(!d.playlist && !d.single_in_list);

        let d = detect("https://youtu.be/abc", &[]).unwrap();
        assert_eq!(d.site, "youtube");
        let d = detect("https://www.pixiv.net/artworks/147501205", &[]).unwrap();
        assert_eq!((d.site.as_str(), d.tool), ("pixiv", Tool::GalleryDl));
        let d = detect("https://twitter.com/user/status/1", &[]).unwrap();
        assert_eq!((d.site.as_str(), d.tool), ("x", Tool::GalleryDl));
        let d = detect("https://creator.fanbox.cc/posts/1", &[]).unwrap();
        assert_eq!(d.site, "fanbox");
        // サブドメインでない似た名前は一致しない
        let d = detect("https://notyoutube.com/x", &[]).unwrap();
        assert_eq!((d.site.as_str(), d.tool), ("notyoutube.com", Tool::YtDlp));
        // 表にないホストは yt-dlp の汎用
        let d = detect("http://user@Example.ORG:8080/v?a=1", &[]).unwrap();
        assert_eq!(
            (d.site.as_str(), d.site_label.as_str()),
            ("example.org", "example.org")
        );
    }

    #[test]
    fn rejects_non_http_urls() {
        for bad in [
            "",
            "ftp://x.com/a",
            "youtube.com/watch",
            "https://",
            "https:// a/",
        ] {
            assert!(detect(bad, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn tells_playlists_from_videos_in_a_list() {
        let d = detect("https://www.youtube.com/playlist?list=PL1", &[]).unwrap();
        assert!(d.playlist && !d.single_in_list);
        let d = detect("https://www.youtube.com/watch?v=a&list=PL1&index=3", &[]).unwrap();
        assert!(!d.playlist && d.single_in_list);
    }

    #[test]
    fn rules_come_first() {
        let rules = vec![
            DownloadRule {
                name: "壊れた規則".into(),
                pattern: "(".into(),
                tool: Tool::GalleryDl,
                args: vec![],
            },
            DownloadRule {
                name: "YouTube の音声".into(),
                pattern: r"^https://www\.youtube\.com/.*music".into(),
                tool: Tool::YtDlp,
                args: vec!["--embed-metadata".into()],
            },
            DownloadRule {
                name: "画像サイト".into(),
                pattern: r"example\.org/gallery/".into(),
                tool: Tool::GalleryDl,
                args: vec![],
            },
        ];
        let d = detect("https://www.youtube.com/watch?v=music", &rules).unwrap();
        assert_eq!(d.rule.as_deref(), Some("YouTube の音声"));
        assert_eq!(d.extra_args, vec!["--embed-metadata".to_string()]);
        let d = detect("https://example.org/gallery/1", &rules).unwrap();
        assert_eq!((d.tool, d.site.as_str()), (Tool::GalleryDl, "example.org"));
        let d = detect("https://example.org/watch/1", &rules).unwrap();
        assert_eq!((d.tool, d.rule), (Tool::YtDlp, None));
    }

    #[test]
    fn validates_rules() {
        let rule = |tool, args: &[&str]| DownloadRule {
            name: "r".into(),
            pattern: "x".into(),
            tool,
            args: args.iter().map(|s| s.to_string()).collect(),
        };
        assert!(validate_rule(&rule(Tool::YtDlp, &["--embed-metadata", "-f", "bv"])).is_ok());
        assert!(validate_rule(&rule(Tool::GalleryDl, &["-o", "browser=firefox"])).is_ok());
        for args in [
            &["-o", "x"][..],
            &["--output=x"],
            &["--exec", "rm"],
            &["-P", "/tmp"],
            &["--download-archive", "a"],
        ] {
            assert!(validate_rule(&rule(Tool::YtDlp, args)).is_err(), "{args:?}");
        }
        for args in [
            &["-D", "/tmp"][..],
            &["-o", "base-directory=/tmp"],
            &["--exec-after", "x"],
        ] {
            assert!(
                validate_rule(&rule(Tool::GalleryDl, args)).is_err(),
                "{args:?}"
            );
        }
        let mut bad = rule(Tool::YtDlp, &[]);
        bad.pattern = "(".into();
        let message = validate_rule(&bad).unwrap_err().to_string();
        assert!(
            message.ends_with("正規表現が読めません（error: unclosed group）"),
            "{message}"
        );
        bad.pattern = "x".into();
        bad.name = " ".into();
        assert!(validate_rule(&bad).is_err());
    }
}
