//! ライブラリごとの設定（`settings` テーブル。値は JSON）。

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;

/// ダウンロードの既定の選択（保存先・画質など。`DownloadOptions`）。
pub const DOWNLOAD_DEFAULTS: &str = "download.defaults";

/// `key` の値。なければ、または読めなければ None。
pub fn get<T: DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
    let value: Option<String> = stmt.query_row([key], |row| row.get(0)).optional()?;
    Ok(value.and_then(|v| serde_json::from_str(&v).ok()))
}

/// `key` に値を保存する。
pub fn set<T: Serialize>(conn: &Connection, key: &str, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    stmt.execute(params![key, json])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use crate::download::{DownloadOptions, Quality};

    #[test]
    fn round_trips_json_values() {
        let conn = test_conn();
        assert_eq!(
            get::<DownloadOptions>(&conn, DOWNLOAD_DEFAULTS).unwrap(),
            None
        );
        let options = DownloadOptions {
            quality: Quality::P720,
            dest_folder_id: Some(3),
            ..DownloadOptions::default()
        };
        set(&conn, DOWNLOAD_DEFAULTS, &options).unwrap();
        set(&conn, DOWNLOAD_DEFAULTS, &options).unwrap();
        assert_eq!(get(&conn, DOWNLOAD_DEFAULTS).unwrap(), Some(options));
        conn.execute(
            "UPDATE settings SET value = 'x' WHERE key = ?1",
            [DOWNLOAD_DEFAULTS],
        )
        .unwrap();
        assert_eq!(
            get::<DownloadOptions>(&conn, DOWNLOAD_DEFAULTS).unwrap(),
            None
        );
    }
}
