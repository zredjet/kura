//! アプリ全体の設定（`~/Library/Application Support/<bundle id>/app.json`）。ライブラリごとの設定は DB の settings に置く。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kura_core::download::{CookieSource, DownloadRule};
use serde::{Deserialize, Serialize};

/// アプリ全体の設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    /// 前回開いたライブラリ。
    pub last_library: Option<PathBuf>,
    /// 再生の設定。
    pub player: PlayerPrefs,
    /// 外部ツールの場所の指定（ツール名 → 実行ファイル）。指定がなければ Homebrew・PATH から探す。
    pub tools: BTreeMap<String, PathBuf>,
    /// ダウンロードの設定（この Mac で共通）。
    pub download: DownloadPrefs,
}

/// ダウンロードの設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadPrefs {
    /// ログインが要るサイトの Cookie を読むブラウザ。
    pub cookies: CookieSource,
    /// URL の規則（上から順に見る）。
    pub rules: Vec<DownloadRule>,
}

/// 再生の設定（docs/playlists.md §5.3・§6.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerPrefs {
    /// 音量（0〜100）。
    pub volume: u32,
    /// 画像の間も直前の音楽を流す。
    pub bgm_through_images: bool,
    /// 画像の間に音楽が終わったら、その曲をループする。
    pub bgm_loop: bool,
    /// 画像の表示時間の既定値。
    pub image_duration_ms: i64,
}

/// 画像の表示時間として受け付ける範囲（1〜60 秒）。
pub const IMAGE_DURATION_RANGE: std::ops::RangeInclusive<i64> = 1000..=60_000;

impl Default for PlayerPrefs {
    fn default() -> Self {
        Self {
            volume: 80,
            bgm_through_images: true,
            bgm_loop: false,
            image_duration_ms: kura_core::unit::DEFAULT_IMAGE_DURATION_MS,
        }
    }
}

impl PlayerPrefs {
    /// 範囲外の値を丸める。
    pub fn clamped(mut self) -> Self {
        self.volume = self.volume.min(100);
        self.image_duration_ms = self
            .image_duration_ms
            .clamp(*IMAGE_DURATION_RANGE.start(), *IMAGE_DURATION_RANGE.end());
        self
    }
}

/// 読む。なければ、または壊れていれば既定値。
pub fn load(path: &Path) -> AppConfig {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(%err, "アプリの設定を読めなかったので既定値を使います");
            AppConfig::default()
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
        Err(err) => {
            tracing::warn!(%err, "アプリの設定を読めなかったので既定値を使います");
            AppConfig::default()
        }
    }
}

/// 書く（一時ファイルに書いてから置き換える）。
pub fn save(path: &Path, config: &AppConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/app.json");
        assert_eq!(load(&path), AppConfig::default());

        let config = AppConfig {
            last_library: Some(PathBuf::from("/Users/me/Pictures/Kura Library.kuralib")),
            player: PlayerPrefs {
                volume: 40,
                bgm_loop: true,
                ..PlayerPrefs::default()
            },
            tools: BTreeMap::from([("yt-dlp".into(), PathBuf::from("/opt/tools/yt-dlp"))]),
            download: DownloadPrefs {
                cookies: CookieSource::Firefox,
                rules: vec![],
            },
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path), config);

        // M4 までの設定（player がない）も読める
        std::fs::write(&path, r#"{"lastLibrary": "/a.kuralib"}"#).unwrap();
        assert_eq!(load(&path).player, PlayerPrefs::default());

        std::fs::write(&path, "{ broken").unwrap();
        assert_eq!(load(&path), AppConfig::default());
    }

    #[test]
    fn clamps_player_prefs() {
        let prefs = PlayerPrefs {
            volume: 300,
            image_duration_ms: 0,
            ..PlayerPrefs::default()
        }
        .clamped();
        assert_eq!((prefs.volume, prefs.image_duration_ms), (100, 1000));
    }
}
