//! 外部ツールに渡す引数を組み立てる（引数は yt-dlp 2026.08・gallery-dl 1.32 の `--help` で確かめた）。
//!
//! 進捗とできたファイルは、`KURA` で始まるタブ区切りの行として標準出力に出させ、`progress` で読む。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::detect::{Detection, Tool};

/// 画質。「最高」は最高の解像度で、同じ解像度なら mp4（H.264 / AAC）を優先する。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[default]
    Best,
    #[serde(rename = "1080")]
    P1080,
    #[serde(rename = "720")]
    P720,
    #[serde(rename = "480")]
    P480,
}

/// Cookie を読むブラウザ（ログインが要るサイト用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CookieSource {
    #[default]
    None,
    Firefox,
    Safari,
    Chrome,
}

impl CookieSource {
    fn browser(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Firefox => Some("firefox"),
            Self::Safari => Some("safari"),
            Self::Chrome => Some("chrome"),
        }
    }
}

/// 1 件のダウンロードの選択（画面の「画質」「音声のみ」「保存先」「自動タグ」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadOptions {
    pub quality: Quality,
    pub audio_only: bool,
    /// 入れる仮想フォルダ。
    pub dest_folder_id: Option<i64>,
    /// 投稿者・作者のタグ（`投稿者:` / `作者:`）。
    pub tag_uploader: bool,
    /// サイトのタグ・ハッシュタグ。
    pub tag_site_tags: bool,
    /// 再生リストを 1 つのユニットにする。
    pub playlist_unit: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            quality: Quality::Best,
            audio_only: false,
            dest_folder_id: None,
            tag_uploader: true,
            tag_site_tags: true,
            playlist_unit: true,
        }
    }
}

/// 実行に使う場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPaths {
    /// 保存先（`staging/dl-<job>/`）。
    pub work_dir: PathBuf,
    /// 取得済みの記録（`archives/` の中）。
    pub archive: PathBuf,
    pub ffmpeg: Option<PathBuf>,
    pub deno: Option<PathBuf>,
}

/// 進捗の行の印（タブ区切りの先頭）。
pub const MARK: &str = "KURA";

/// yt-dlp の引数（ツールのパスは含めない）。
pub fn ytdlp_args(
    detection: &Detection,
    options: &DownloadOptions,
    cookies: CookieSource,
    paths: &RunPaths,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |s: &str| args.push(s.into());
    for s in [
        "--newline",
        "--color",
        "never",
        "--progress",
        "--progress-delta",
        "0.5",
        "--no-mtime",
        "--write-info-json",
        "--progress-template",
        &format!(
            "download:{MARK}\tdl\t%(progress.downloaded_bytes)s\t%(progress.total_bytes)s\t\
             %(progress.total_bytes_estimate)s\t%(progress.speed)s\t%(progress.eta)s\t%(info.format_id)s"
        ),
        "--print",
        &format!(
            "before_dl:{MARK}\tplan\t%(id)s\t%(playlist_index)s\t%(n_entries)s\t\
             %(requested_formats.:.filesize)j\t%(requested_formats.:.filesize_approx)j\t\
             %(filesize,filesize_approx)j\t%(title)j"
        ),
        "--print",
        &format!("after_move:{MARK}\tfile\t%(filepath)s"),
        // --print は表示を止めるので戻す（取得済みで飛ばした行「has already been recorded in the archive」を読むため）
        "--no-quiet",
        "-o",
        "%(id)s.%(ext)s",
    ] {
        push(s);
    }
    args.push("-P".into());
    args.push(paths.work_dir.as_os_str().to_owned());
    args.push("--download-archive".into());
    args.push(paths.archive.as_os_str().to_owned());
    if let Some(ffmpeg) = &paths.ffmpeg {
        args.push("--ffmpeg-location".into());
        args.push(ffmpeg.as_os_str().to_owned());
    }
    if let Some(deno) = &paths.deno {
        // GUI アプリは PATH に Homebrew がないので、YouTube 用の JavaScript の実行環境を場所で渡す
        let mut value = OsString::from("deno:");
        value.push(deno.as_os_str());
        args.push("--js-runtimes".into());
        args.push(value);
    }
    if detection.single_in_list {
        args.push("--no-playlist".into());
    }
    if options.audio_only {
        for s in ["-f", "ba[ext=m4a]/ba/b", "-x"] {
            args.push(s.into());
        }
    } else {
        let res = match options.quality {
            Quality::Best => "res".to_string(),
            Quality::P1080 => "res:1080".to_string(),
            Quality::P720 => "res:720".to_string(),
            Quality::P480 => "res:480".to_string(),
        };
        args.push("-S".into());
        args.push(format!("{res},ext:mp4:m4a").into());
    }
    if let Some(browser) = cookies.browser() {
        args.push("--cookies-from-browser".into());
        args.push(browser.into());
    }
    args.extend(detection.extra_args.iter().map(OsString::from));
    // URL の前に `--` を置き、`-` で始まる URL を引数として読ませない
    args.push("--".into());
    args.push(detection.url.clone().into());
    args
}

/// gallery-dl の引数（ツールのパスは含めない）。
pub fn gallerydl_args(
    detection: &Detection,
    cookies: CookieSource,
    paths: &RunPaths,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--no-colors".into(),
        "--write-metadata".into(),
        "--no-mtime".into(),
        "--ugoira".into(),
        "mp4".into(),
        "-D".into(),
        paths.work_dir.as_os_str().to_owned(),
        "--download-archive".into(),
        paths.archive.as_os_str().to_owned(),
        "--Print".into(),
        format!("prepare:{MARK}\tprepare\t{{category}}\t{{id}}\t{{num}}\t{{count}}\t{{title!j}}")
            .into(),
        "--Print".into(),
        format!("after:{MARK}\tfile\t{{_path}}").into(),
        "--Print".into(),
        format!("skip:{MARK}\tskip\t{{_path}}").into(),
    ];
    if let Some(browser) = cookies.browser() {
        args.push("--cookies-from-browser".into());
        args.push(browser.into());
    }
    args.extend(detection.extra_args.iter().map(OsString::from));
    args.push("--".into());
    args.push(detection.url.clone().into());
    args
}

/// 取得済みの記録のファイル名（ツールごとに 1 つ）。
pub fn archive_file(archives_dir: &Path, tool: Tool) -> PathBuf {
    archives_dir.join(match tool {
        Tool::YtDlp => "yt-dlp.txt",
        Tool::GalleryDl => "gallery-dl.sqlite3",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::detect::detect;

    fn paths() -> RunPaths {
        RunPaths {
            work_dir: "/lib/staging/dl-7".into(),
            archive: "/lib/archives/yt-dlp.txt".into(),
            ffmpeg: Some("/opt/homebrew/bin/ffmpeg".into()),
            deno: Some("/opt/homebrew/bin/deno".into()),
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).map(String::as_str)
    }

    #[test]
    fn ytdlp_saves_into_the_work_dir_with_metadata() {
        let d = detect("https://www.youtube.com/watch?v=a&list=PL", &[]).unwrap();
        let args = strings(&ytdlp_args(
            &d,
            &DownloadOptions::default(),
            CookieSource::Firefox,
            &paths(),
        ));
        assert_eq!(after(&args, "-P"), Some("/lib/staging/dl-7"));
        assert_eq!(after(&args, "-o"), Some("%(id)s.%(ext)s"));
        assert_eq!(
            after(&args, "--download-archive"),
            Some("/lib/archives/yt-dlp.txt")
        );
        assert_eq!(
            after(&args, "--js-runtimes"),
            Some("deno:/opt/homebrew/bin/deno")
        );
        assert_eq!(after(&args, "-S"), Some("res,ext:mp4:m4a"));
        assert_eq!(after(&args, "--cookies-from-browser"), Some("firefox"));
        assert!(args.contains(&"--write-info-json".to_string()));
        assert!(args.contains(&"--no-quiet".to_string()));
        assert!(args.contains(&"--no-playlist".to_string()));
        assert_eq!(
            &args[args.len() - 2..],
            ["--", "https://www.youtube.com/watch?v=a&list=PL"]
        );
    }

    #[test]
    fn ytdlp_quality_and_audio() {
        let d = detect("https://soundcloud.com/a/b", &[]).unwrap();
        let options = DownloadOptions {
            quality: Quality::P720,
            ..DownloadOptions::default()
        };
        let args = strings(&ytdlp_args(&d, &options, CookieSource::None, &paths()));
        assert_eq!(after(&args, "-S"), Some("res:720,ext:mp4:m4a"));
        assert!(!args.contains(&"--no-playlist".to_string()));
        assert!(!args.contains(&"--cookies-from-browser".to_string()));

        let options = DownloadOptions {
            audio_only: true,
            ..DownloadOptions::default()
        };
        let args = strings(&ytdlp_args(&d, &options, CookieSource::None, &paths()));
        assert_eq!(after(&args, "-f"), Some("ba[ext=m4a]/ba/b"));
        assert!(args.contains(&"-x".to_string()));
        assert!(!args.contains(&"-S".to_string()));
    }

    #[test]
    fn gallerydl_prints_events_and_appends_rule_args() {
        let mut d = detect("https://www.pixiv.net/artworks/1", &[]).unwrap();
        d.extra_args = vec!["--range".into(), "1-3".into()];
        let mut p = paths();
        p.archive = "/lib/archives/gallery-dl.sqlite3".into();
        let args = strings(&gallerydl_args(&d, CookieSource::None, &p));
        assert_eq!(after(&args, "-D"), Some("/lib/staging/dl-7"));
        assert_eq!(
            after(&args, "--download-archive"),
            Some("/lib/archives/gallery-dl.sqlite3")
        );
        assert!(args.contains(&"--write-metadata".to_string()));
        assert_eq!(args.iter().filter(|a| a.as_str() == "--Print").count(), 3);
        assert_eq!(
            &args[args.len() - 4..],
            ["--range", "1-3", "--", "https://www.pixiv.net/artworks/1"]
        );
    }

    #[test]
    fn options_have_defaults() {
        let o: DownloadOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(o, DownloadOptions::default());
        assert!(o.tag_uploader && o.tag_site_tags && o.playlist_unit);
        let o: DownloadOptions = serde_json::from_str(r#"{"quality":"720"}"#).unwrap();
        assert_eq!(o.quality, Quality::P720);
    }
}
