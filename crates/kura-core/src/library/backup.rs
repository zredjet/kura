//! DB の定期バックアップ（FR-SYS-03。ADR-121）。
//!
//! SQLite の backup API で `backups/library-YYYYMMDD-HHMMSS.sqlite`（UTC）を作り、新しい 7 個を残す。
//! 開いたとき（前回から 24 時間以上たっていれば）と、開いているあいだ 24 時間ごとに作る。
//! 読み取り専用の別の接続から写すので、書き込みは止めない（WAL）。戻すのはアプリを閉じてから手で行う
//! （docs/data-model.md §1）。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use super::Layout;
use crate::Result;
use crate::time::{format_utc, now_ms, parse_iso8601_ms};

/// 残す数。
pub const KEEP: usize = 7;
/// 作る間隔（ミリ秒）。
pub const INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

const PREFIX: &str = "library-";
const SUFFIX: &str = ".sqlite";
/// 開いてから最初に確かめるまでの時間（起動の処理と取り合わないように）。
const FIRST_CHECK: Duration = Duration::from_secs(20);
/// 開いているあいだ確かめる間隔。
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// バックアップ 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInfo {
    pub name: String,
    pub path: PathBuf,
    /// 作った時刻（ファイル名から）。
    pub created_at: i64,
    pub size_bytes: u64,
}

/// `ms` に作るバックアップの名前（`library-20261011-035929.sqlite`、UTC）。
pub fn name_for(ms: i64) -> String {
    // 「2026-10-11 03:59:29.497Z」
    let t = format_utc(ms);
    format!(
        "{PREFIX}{}{}{}-{}{}{}{SUFFIX}",
        &t[0..4],
        &t[5..7],
        &t[8..10],
        &t[11..13],
        &t[14..16],
        &t[17..19]
    )
}

/// バックアップの名前から作った時刻を読む（違う名前なら None）。
pub fn parse_name(name: &str) -> Option<i64> {
    let stamp = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (date, time) = stamp.split_once('-')?;
    if date.len() != 8 || time.len() != 6 || !stamp.chars().all(|c| c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    parse_iso8601_ms(&format!(
        "{}-{}-{}T{}:{}:{}Z",
        &date[0..4],
        &date[4..6],
        &date[6..8],
        &time[0..2],
        &time[2..4],
        &time[4..6]
    ))
}

/// 作る時期か（一度もないか、前回から `INTERVAL_MS` 以上たった）。
pub fn is_due(last: Option<i64>, now: i64) -> bool {
    last.is_none_or(|last| now - last >= INTERVAL_MS)
}

/// バックアップの一覧（新しい順）。
pub fn list(layout: &Layout) -> Result<Vec<BackupInfo>> {
    let dir = layout.backups_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(created_at) = parse_name(&name) else {
            continue;
        };
        let meta = entry.metadata()?;
        if meta.is_file() {
            out.push(BackupInfo {
                name,
                path: entry.path(),
                created_at,
                size_bytes: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.name.cmp(&a.name)));
    Ok(out)
}

/// いまのバックアップを作り、古いものを消す（新しい `KEEP` 個を残す）。
pub fn create(layout: &Layout, now: i64) -> Result<BackupInfo> {
    let dir = layout.backups_dir();
    std::fs::create_dir_all(&dir)?;
    let name = name_for(now);
    let path = dir.join(&name);
    if !path.exists() {
        // 書きかけを本物の名前で残さないよう、一時ファイルに写してから名前を変える
        let temp = dir.join(format!(".{name}.part"));
        let _ = std::fs::remove_file(&temp);
        copy_db(&layout.db_path(), &temp)?;
        std::fs::rename(&temp, &path)?;
        tracing::info!(path = %path.display(), "DB のバックアップを作りました");
    }
    prune(layout, KEEP)?;
    let size_bytes = std::fs::metadata(&path)?.len();
    Ok(BackupInfo {
        name,
        path,
        created_at: parse_name(&name_for(now)).unwrap_or(now),
        size_bytes,
    })
}

/// マイグレーションを当てる前のバックアップ（docs/data-model.md §5）。いつもの名前で作り、新しい `KEEP` 個を残す。
/// 開くのを待たせるので、書き込みの接続から休まずに一度に写す。
pub fn before_migration(layout: &Layout, conn: &Connection, now: i64) -> Result<BackupInfo> {
    let dir = layout.backups_dir();
    std::fs::create_dir_all(&dir)?;
    let name = name_for(now);
    let path = dir.join(&name);
    let temp = dir.join(format!(".{name}.part"));
    let _ = std::fs::remove_file(&temp);
    // rusqlite は 0 以下の pages_per_step で panic するので、一度に写すには i32::MAX を渡す
    if let Err(err) = copy_conn(conn, &temp, i32::MAX, Duration::ZERO) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    // 同じ秒の定期のバックアップがあっても、当てる直前の中身で置き換える
    std::fs::rename(&temp, &path)?;
    prune(layout, KEEP)?;
    let size_bytes = std::fs::metadata(&path)?.len();
    tracing::info!(path = %path.display(), size_bytes, "マイグレーションの前に DB のバックアップを作りました");
    Ok(BackupInfo {
        created_at: parse_name(&name).unwrap_or(now),
        name,
        path,
        size_bytes,
    })
}

/// 古いバックアップを消して、新しい `keep` 個を残す。消した数を返す（`backups/` の中の、この形の名前のものだけ）。
pub fn prune(layout: &Layout, keep: usize) -> Result<usize> {
    let mut removed = 0;
    for old in list(layout)?.into_iter().skip(keep) {
        std::fs::remove_file(&old.path)?;
        removed += 1;
    }
    Ok(removed)
}

/// `src` の DB を `dest` に写す（読み取り専用で開き、少しずつ写す）。
fn copy_db(src: &Path, dest: &Path) -> Result<()> {
    let from = Connection::open_with_flags(
        src,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    from.busy_timeout(Duration::from_secs(5))?;
    copy_conn(&from, dest, 512, Duration::from_millis(5))
}

/// 開いている接続 `from` の DB を `dest` に写す（`pages` ページずつ、間に `pause` 休む）。
fn copy_conn(from: &Connection, dest: &Path, pages: i32, pause: Duration) -> Result<()> {
    let mut to = Connection::open(dest)?;
    let backup = rusqlite::backup::Backup::new(from, &mut to)?;
    backup.run_to_completion(pages.max(1), pause, None)?;
    drop(backup);
    // 写したものだけで開けるように、WAL ではなく 1 つのファイルにする
    to.pragma_update(None, "journal_mode", "DELETE")?;
    Ok(())
}

/// 開いているあいだのバックアップの係（閉じると止まる）。
pub(crate) struct Scheduler {
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl Scheduler {
    /// 動かし始める。最初は少し待ってから、そのあとは 1 時間ごとに、時期なら作る。
    pub(crate) fn start(layout: Layout) -> Self {
        let (stop, rx) = mpsc::channel::<()>();
        let handle = std::thread::Builder::new()
            .name("kura-backup".into())
            .spawn(move || {
                let mut wait = FIRST_CHECK;
                loop {
                    match rx.recv_timeout(wait) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                    wait = CHECK_EVERY;
                    let last = list(&layout)
                        .ok()
                        .and_then(|l| l.first().map(|b| b.created_at));
                    if is_due(last, now_ms())
                        && let Err(err) = create(&layout, now_ms())
                    {
                        tracing::warn!(%err, "DB のバックアップを作れませんでした");
                    }
                }
            });
        match handle {
            Ok(handle) => Self {
                stop: Some(stop),
                handle: Some(handle),
            },
            Err(err) => {
                tracing::warn!(%err, "バックアップのスレッドを作れませんでした");
                Self {
                    stop: None,
                    handle: None,
                }
            }
        }
    }

    /// 止める（作っている途中なら終わるまで待つ）。
    pub(crate) fn stop(&mut self) {
        self.stop.take();
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            tracing::error!("バックアップのスレッドが異常終了しました");
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;

    const T: i64 = 1_791_648_418_942; // 2026-10-10 16:06:58.942 UTC

    #[test]
    fn names_round_trip_to_the_second() {
        assert_eq!(name_for(T), "library-20261010-160658.sqlite");
        assert_eq!(parse_name("library-20261010-160658.sqlite"), Some(T - 942));
        for bad in [
            "library-2026101-160658.sqlite",
            "library-20261010-160658.db",
            "other-20261010-160658.sqlite",
            ".library-20261010-160658.sqlite.part",
            "library-2026x010-160658.sqlite",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn due_when_none_or_a_day_has_passed() {
        assert!(is_due(None, T));
        assert!(!is_due(Some(T), T + INTERVAL_MS - 1));
        assert!(is_due(Some(T), T + INTERVAL_MS));
    }

    #[test]
    fn creates_a_readable_copy_and_keeps_the_newest_seven() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        lib.db()
            .write(|tx| {
                tx.execute(
                    "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                     VALUES (x'01', 'jpg', 'image', 1, '夕方', 'a.jpg', 1, 1)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let layout = lib.layout().clone();
        // バックアップではないファイルは数えず、消さない
        std::fs::write(layout.backups_dir().join("メモ.txt"), "x").unwrap();
        for day in 0..9 {
            create(&layout, T + day * INTERVAL_MS).unwrap();
        }
        let all = list(&layout).unwrap();
        assert_eq!(all.len(), KEEP);
        assert_eq!(all[0].name, name_for(T + 8 * INTERVAL_MS));
        assert_eq!(all[KEEP - 1].name, name_for(T + 2 * INTERVAL_MS));
        assert!(layout.backups_dir().join("メモ.txt").exists());

        // 写したものだけで開け、中身がある（WAL のファイルは要らない）
        let copy = Connection::open(&all[0].path).unwrap();
        let title: String = copy
            .query_row("SELECT title FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "夕方");
        let mode: String = copy
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");

        // 同じ秒に重ねて作っても、1 つのまま
        create(&layout, T + 8 * INTERVAL_MS).unwrap();
        assert_eq!(list(&layout).unwrap().len(), KEEP);
    }

    /// 1 つ前の版の DB を持つライブラリのフォルダを作る（アイテム 1 件・タグ 1 件）。
    fn previous_library(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        let mut conn = Connection::open(root.join(crate::library::DB_FILE)).unwrap();
        crate::db::apply_pragmas(&conn).unwrap();
        crate::db::migrate_until(&mut conn, crate::db::MIGRATIONS.len() - 1).unwrap();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (1, x'0101010101010101010101010101010101010101010101010101010101010101', 'jpg', 'image', 3, '夕方', 'a.jpg', 1, 1);
             INSERT INTO tags (id, name, created_at) VALUES (1, '海', 1);
             INSERT INTO item_tags (tag_id, item_id) VALUES (1, 1);",
        )
        .unwrap();
    }

    #[test]
    fn backup_is_taken_before_migrating_an_existing_library_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        previous_library(&root);
        let previous = crate::db::MIGRATIONS.len() - 1;

        assert!(crate::library::needs_upgrade(&root).unwrap());
        let lib = Library::open(&root).unwrap();
        assert!(!crate::library::needs_upgrade(&root).unwrap());
        assert!(!crate::library::needs_upgrade(&dir.path().join("none")).unwrap());
        assert_eq!(lib.migration().from, previous);
        assert_eq!(lib.migration().to, crate::db::MIGRATIONS.len());
        let all = list(lib.layout()).unwrap();
        assert_eq!(all.len(), 1, "いつもの名前で 1 つ");
        assert!(parse_name(&all[0].name).is_some());
        // 当てる前の中身を、1 つのファイル（DELETE モード）で残す。書きかけは残らない
        let copy = Connection::open(&all[0].path).unwrap();
        assert_eq!(crate::db::applied(&copy).unwrap(), previous);
        let mode: String = copy
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let tags: i64 = copy
            .query_row("SELECT count(*) FROM item_tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tags, 1);
        let names: Vec<String> = std::fs::read_dir(lib.layout().backups_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![all[0].name.clone()]);
        drop(lib);

        // 当て済みのライブラリ・新しいライブラリでは作らない
        let lib = Library::open(&root).unwrap();
        assert!(lib.migration().applied.is_empty());
        assert_eq!(list(lib.layout()).unwrap().len(), 1);
        let fresh = Library::create(&dir.path().join("New")).unwrap();
        assert!(list(fresh.layout()).unwrap().is_empty());
    }

    #[test]
    fn backup_before_migrating_keeps_the_newest_seven() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        previous_library(&root);
        let layout = Layout::new(&root);
        std::fs::create_dir_all(layout.backups_dir()).unwrap();
        // 2020 年の古いバックアップが 7 つ
        let old = 1_577_836_800_000; // 2020-01-01 00:00:00 UTC
        for day in 0..7 {
            std::fs::write(
                layout.backups_dir().join(name_for(old + day * INTERVAL_MS)),
                "x",
            )
            .unwrap();
        }
        let lib = Library::open(&root).unwrap();
        let all = list(lib.layout()).unwrap();
        assert_eq!(all.len(), KEEP);
        assert!(
            all[0].created_at > old + 6 * INTERVAL_MS,
            "当てる前のものが最新"
        );
        assert_eq!(all[KEEP - 1].name, name_for(old + INTERVAL_MS));
    }

    #[test]
    fn backup_before_migrating_fails_when_it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        // backups がフォルダでなくファイル
        std::fs::write(layout.backups_dir(), "x").unwrap();
        let conn = crate::db::test_conn();
        assert!(before_migration(&layout, &conn, T).is_err());
    }

    #[test]
    fn scheduler_stops_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let mut scheduler = Scheduler::start(lib.layout().clone());
        let started = std::time::Instant::now();
        scheduler.stop();
        assert!(started.elapsed() < Duration::from_secs(2));
        // 最初の確認の前に止めたので、作っていない
        assert!(list(lib.layout()).unwrap().is_empty());
    }
}
