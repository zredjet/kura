//! 取得済みの記録（download-archive。ADR-95）の状態と消去（設定画面。M9）。
//!
//! 記録にあるものは取り直さないので、取り直したいときは記録を消す。消すのは `archives/` の中の
//! ツールごとの 1 ファイルだけ（ライブラリのアイテムには触らない）。

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::Tool;
use super::args::archive_file;
use crate::Result;
use crate::library::Layout;

/// 1 つのツールの記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveStatus {
    pub tool: Tool,
    /// 記録の件数（ファイルがなければ 0、読めなければ None）。
    pub entries: Option<u64>,
    pub size_bytes: u64,
}

/// yt-dlp・gallery-dl の記録の状態。
pub fn status(layout: &Layout) -> Vec<ArchiveStatus> {
    [Tool::YtDlp, Tool::GalleryDl]
        .into_iter()
        .map(|tool| {
            let path = archive_file(&layout.archives_dir(), tool);
            let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let entries = if !path.exists() {
                Some(0)
            } else {
                match tool {
                    Tool::YtDlp => count_lines(&path),
                    Tool::GalleryDl => count_rows(&path),
                }
            };
            ArchiveStatus {
                tool,
                entries,
                size_bytes,
            }
        })
        .collect()
}

/// 記録を消す（なければ何もしない）。消したら true。
pub fn clear(layout: &Layout, tool: Tool) -> Result<bool> {
    let path = archive_file(&layout.archives_dir(), tool);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "取得済みの記録を消しました");
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// yt-dlp の記録（1 行 1 件、「youtube abc123」）。
fn count_lines(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.lines().filter(|l| !l.trim().is_empty()).count() as u64)
}

/// gallery-dl の記録（SQLite の `archive` テーブル）。
fn count_rows(path: &Path) -> Option<u64> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let count: i64 = conn
        .query_row("SELECT count(*) FROM archive", [], |r| r.get(0))
        .ok()?;
    u64::try_from(count).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;

    #[test]
    fn counts_and_clears_each_tool_separately() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let layout = lib.layout();
        assert_eq!(
            status(layout),
            [
                ArchiveStatus {
                    tool: Tool::YtDlp,
                    entries: Some(0),
                    size_bytes: 0
                },
                ArchiveStatus {
                    tool: Tool::GalleryDl,
                    entries: Some(0),
                    size_bytes: 0
                },
            ]
        );

        let yt = archive_file(&layout.archives_dir(), Tool::YtDlp);
        std::fs::write(&yt, "youtube aaa\nyoutube bbb\n\n").unwrap();
        let gd = archive_file(&layout.archives_dir(), Tool::GalleryDl);
        let conn = Connection::open(&gd).unwrap();
        conn.execute_batch(
            "CREATE TABLE archive (entry TEXT PRIMARY KEY) WITHOUT ROWID;
             INSERT INTO archive VALUES ('pixiv1'), ('pixiv2'), ('pixiv3');",
        )
        .unwrap();
        drop(conn);
        let now = status(layout);
        assert_eq!(now[0].entries, Some(2));
        assert_eq!(now[1].entries, Some(3));

        assert!(clear(layout, Tool::YtDlp).unwrap());
        assert!(!yt.exists());
        assert!(gd.exists(), "ほかのツールの記録は残す");
        assert!(!clear(layout, Tool::YtDlp).unwrap());

        // 読めない記録は件数なし
        std::fs::write(&gd, "壊れた").unwrap();
        assert_eq!(status(layout)[1].entries, None);
    }
}
