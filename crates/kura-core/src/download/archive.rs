//! 取得済みの記録（download-archive。ADR-95）の状態と消去（設定画面。M9）、再ダウンロードの記録の合流。
//!
//! 記録にあるものは取り直さないので、取り直したいときは記録を消すか、再ダウンロードする。消すのは
//! `archives/` の中のツールごとの 1 ファイルだけ（ライブラリのアイテムには触らない）。再ダウンロードは
//! 空の記録で取得し、終わったら取得したものをふだんの記録に足す（ADR-132）。

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

/// 再ダウンロードで書いた記録（`from`）を、ふだんの記録（`into`）に足す。足した件数を返す。
/// すでにあるものは足さない。`from` がなければ何もしない。
pub fn merge(from: &Path, into: &Path, tool: Tool) -> Result<u64> {
    if !from.exists() {
        return Ok(0);
    }
    if let Some(dir) = into.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match tool {
        Tool::YtDlp => merge_lines(from, into),
        Tool::GalleryDl => merge_rows(from, into),
    }
}

fn merge_lines(from: &Path, into: &Path) -> Result<u64> {
    use std::collections::HashSet;
    use std::io::Write;

    let current = match std::fs::read_to_string(into) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    let mut seen: HashSet<&str> = current.lines().map(str::trim).collect();
    let added = std::fs::read_to_string(from)?;
    let mut text = String::new();
    // 最後の行が改行で終わっていなければ（書きかけで止まったとき）改行を補う
    if !current.is_empty() && !current.ends_with('\n') {
        text.push('\n');
    }
    let mut count = 0;
    for line in added.lines().map(str::trim) {
        if !line.is_empty() && seen.insert(line) {
            text.push_str(line);
            text.push('\n');
            count += 1;
        }
    }
    if count > 0 {
        // ほかのダウンロードの yt-dlp も同じファイルに足していくので、1 回の追記で書く
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(into)?;
        file.write_all(text.as_bytes())?;
    }
    Ok(count)
}

fn merge_rows(from: &Path, into: &Path) -> Result<u64> {
    let entries: Vec<String> = {
        let conn = Connection::open_with_flags(
            from,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut stmt = conn.prepare("SELECT entry FROM archive")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    if entries.is_empty() {
        return Ok(0);
    }
    let mut conn = Connection::open(into)?;
    // ほかのダウンロードの gallery-dl が書いているあいだは待つ
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    let tx = conn.transaction()?;
    // gallery-dl が作るのと同じ形
    tx.execute_batch("CREATE TABLE IF NOT EXISTS archive (entry TEXT PRIMARY KEY) WITHOUT ROWID")?;
    let mut count = 0;
    {
        let mut stmt = tx.prepare_cached("INSERT OR IGNORE INTO archive (entry) VALUES (?1)")?;
        for entry in &entries {
            count += stmt.execute([entry])? as u64;
        }
    }
    tx.commit()?;
    Ok(count)
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

    #[test]
    fn merges_redownloaded_lines_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("work/.kura-archive.txt");
        let into = dir.path().join("archives/yt-dlp.txt");
        std::fs::create_dir_all(from.parent().unwrap()).unwrap();

        // 再ダウンロードで何も記録しなかったときは何もしない
        assert_eq!(merge(&from, &into, Tool::YtDlp).unwrap(), 0);
        assert!(!into.exists());

        // ふだんの記録がなければ作る
        std::fs::write(&from, "youtube aaa\n").unwrap();
        assert_eq!(merge(&from, &into, Tool::YtDlp).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&into).unwrap(), "youtube aaa\n");

        // あるものは足さない。改行で終わっていない記録には改行を補う
        std::fs::write(&into, "youtube aaa\nyoutube bbb").unwrap();
        std::fs::write(&from, "youtube aaa\n\nyoutube ccc\nyoutube ccc\n").unwrap();
        assert_eq!(merge(&from, &into, Tool::YtDlp).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(&into).unwrap(),
            "youtube aaa\nyoutube bbb\nyoutube ccc\n"
        );
    }

    #[test]
    fn merges_redownloaded_rows_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join(".kura-archive.sqlite3");
        let into = dir.path().join("archives/gallery-dl.sqlite3");
        let conn = Connection::open(&from).unwrap();
        conn.execute_batch(
            "CREATE TABLE archive (entry TEXT PRIMARY KEY) WITHOUT ROWID;
             INSERT INTO archive VALUES ('pixiv100_p0'), ('pixiv100_p1');",
        )
        .unwrap();
        drop(conn);

        assert_eq!(merge(&from, &into, Tool::GalleryDl).unwrap(), 2);
        assert_eq!(count_rows(&into), Some(2));
        // 2 回目はすべてある
        assert_eq!(merge(&from, &into, Tool::GalleryDl).unwrap(), 0);
        assert_eq!(count_rows(&into), Some(2));

        // 読めない記録はエラー（ふだんの記録はそのまま）
        std::fs::write(&from, "壊れた").unwrap();
        assert!(merge(&from, &into, Tool::GalleryDl).is_err());
        assert_eq!(count_rows(&into), Some(2));
    }
}
