//! アプリ全体の設定（`~/Library/Application Support/<bundle id>/app.json`）。ライブラリごとの設定は DB の settings に置く。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// アプリ全体の設定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    /// 前回開いたライブラリ。
    pub last_library: Option<PathBuf>,
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
        };
        save(&path, &config).unwrap();
        assert_eq!(load(&path), config);

        std::fs::write(&path, "{ broken").unwrap();
        assert_eq!(load(&path), AppConfig::default());
    }
}
