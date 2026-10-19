//! SQLite へのアクセス。書き込みは専用スレッド 1 本に集約し、読み取りは読み取り専用の接続を使い回す。

mod migrate;
#[cfg(test)]
pub(crate) mod testing;

pub use migrate::{
    AppliedMigration, Kind, MIGRATIONS, Migration, MigrationReport, Violation, applied, migrate,
    migrate_until, pending, violations,
};

use crate::UpgradeError;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::{CoreError, Result};

/// 読み取り専用の接続の数。
const READERS: usize = 4;

/// 他の接続が書き込み中のときに待つ時間。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type WriteJob = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// ライブラリの DB。`write` は書き込みスレッドでトランザクションとして実行し、`read` は呼んだスレッドで実行する。
pub struct Db {
    path: PathBuf,
    writer: Mutex<Option<mpsc::Sender<WriteJob>>>,
    writer_thread: Mutex<Option<JoinHandle<()>>>,
    readers: Mutex<Vec<Connection>>,
    reader_returned: Condvar,
}

impl Db {
    /// DB を開き（なければ作り）、マイグレーションを適用する（当てる前のバックアップは取らない。テストと新しい DB 用）。
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, |_, _| Ok(())).map(|(db, _)| db)
    }

    /// DB を開き、マイグレーションを適用する（docs/data-model.md §5）。未適用のマイグレーションがある既存の DB
    /// （user_version が 1 以上）では、当てる前に `before_migrating(書き込みの接続, 今の版)` を呼ぶ（当てる前のバックアップ）。
    /// それが失敗したら当てずに失敗する。当てたら WAL を本体へ移して空にする。失敗はどれも `CoreError::Upgrade`。
    pub fn open_with<F>(path: &Path, before_migrating: F) -> Result<(Self, MigrationReport)>
    where
        F: FnOnce(&Connection, usize) -> Result<()>,
    {
        Self::open_list(path, MIGRATIONS, before_migrating)
    }

    fn open_list<F>(
        path: &Path,
        list: &[Migration],
        before_migrating: F,
    ) -> Result<(Self, MigrationReport)>
    where
        F: FnOnce(&Connection, usize) -> Result<()>,
    {
        let mut conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        let report = upgrade(&mut conn, list, before_migrating)?;
        Ok((Self::start(path, conn)?, report))
    }

    /// 書き込みのスレッドと読み取りの接続を用意する。
    fn start(path: &Path, conn: Connection) -> Result<Self> {
        let readers = (0..READERS)
            .map(|_| open_reader(path))
            .collect::<Result<Vec<_>>>()?;

        let (tx, rx) = mpsc::channel::<WriteJob>();
        let writer_thread = std::thread::Builder::new()
            .name("kura-db-writer".into())
            .spawn(move || {
                let mut conn = conn;
                for job in rx {
                    job(&mut conn);
                }
            })?;

        Ok(Self {
            path: path.to_path_buf(),
            writer: Mutex::new(Some(tx)),
            writer_thread: Mutex::new(Some(writer_thread)),
            readers: Mutex::new(readers),
            reader_returned: Condvar::new(),
        })
    }

    /// DB ファイルの場所。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 書き込みスレッドで `f` を 1 つのトランザクションとして実行する。`f` がエラーを返したらロールバックする。
    pub fn write<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::sync_channel::<Result<T>>(1);
        let job: WriteJob = Box::new(move |conn| {
            let result = (|| {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let value = f(&tx)?;
                tx.commit()?;
                Ok(value)
            })();
            // 呼び出し側がもう待っていなければ結果は捨てる
            let _ = reply_tx.send(result);
        });
        {
            let sender = self.writer.lock().map_err(|_| CoreError::DbClosed)?;
            sender
                .as_ref()
                .ok_or(CoreError::DbClosed)?
                .send(job)
                .map_err(|_| CoreError::DbClosed)?;
        }
        reply_rx.recv().map_err(|_| CoreError::DbClosed)?
    }

    /// 読み取り専用の接続で `f` を実行する（呼んだスレッドで実行する。async からは spawn_blocking で呼ぶ）。
    pub fn read<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = {
            let mut pool = self.readers.lock().map_err(|_| CoreError::DbClosed)?;
            loop {
                if let Some(conn) = pool.pop() {
                    break conn;
                }
                pool = self
                    .reader_returned
                    .wait(pool)
                    .map_err(|_| CoreError::DbClosed)?;
            }
        };
        let result = f(&conn);
        if let Ok(mut pool) = self.readers.lock() {
            pool.push(conn);
            self.reader_returned.notify_one();
        }
        result
    }

    /// 書き込みスレッドを止める（未処理の書き込みは実行してから止まる）。
    pub fn close(&self) {
        if let Ok(mut sender) = self.writer.lock() {
            sender.take();
        }
        if let Ok(mut handle) = self.writer_thread.lock()
            && let Some(handle) = handle.take()
            && handle.join().is_err()
        {
            tracing::error!("DB の書き込みスレッドが異常終了していました");
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        self.close();
    }
}

/// 未適用のマイグレーションを当てる（Db::open_with の手順）。
fn upgrade<F>(
    conn: &mut Connection,
    list: &[Migration],
    before_migrating: F,
) -> Result<MigrationReport>
where
    F: FnOnce(&Connection, usize) -> Result<()>,
{
    let from = applied(conn)?;
    if from > list.len() {
        return Err(UpgradeError::Newer {
            found: from,
            supported: list.len(),
        }
        .into());
    }
    if from == list.len() {
        return Ok(MigrationReport {
            from,
            to: from,
            ..MigrationReport::default()
        });
    }
    let started = std::time::Instant::now();
    // 新しい DB（版 0）には残すものがないので、フックは既存の DB だけ
    if from > 0 {
        before_migrating(conn, from).map_err(|err| UpgradeError::backup(&err))?;
    }
    let report = migrate::run(conn, list, list.len()).map_err(|(index, err)| {
        let name = index
            .and_then(|i| list.get(i))
            .map_or("マイグレーション", |m| m.name);
        UpgradeError::apply(name, &err)
    })?;
    // 当てた分の WAL を本体へ移して空にする（残すと次に開くときに WAL を読み直して約 40ms かかる）
    if let Err(err) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
        tracing::warn!(%err, "マイグレーションのあとの WAL を空にできませんでした");
    }
    tracing::info!(
        from = report.from,
        to = report.to,
        tolerated_violations = report.tolerated_violations,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "ライブラリの DB を新しい版にしました"
    );
    Ok(report)
}

/// 書き込み用の接続の PRAGMA（docs/data-model.md §2）。
pub fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") && !mode.eq_ignore_ascii_case("memory") {
        tracing::warn!(mode, "WAL に切り替えられませんでした");
    }
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -65536;",
    )?;
    Ok(())
}

fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch(
        "PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -65536;",
    )?;
    Ok(conn)
}

/// テスト用：マイグレーション済みのインメモリ DB。
#[cfg(test)]
pub(crate) fn test_conn() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    migrate(&mut conn).unwrap();
    conn
}

/// テスト用：`EXPLAIN QUERY PLAN` の detail 列をつなげたもの。
#[cfg(test)]
pub(crate) fn query_plan(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    rows.join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("library.sqlite")).unwrap();
        (dir, db)
    }

    #[test]
    fn opens_in_wal_mode_with_foreign_keys() {
        let (_dir, db) = open_temp();
        let (mode, fk) = db
            .write(|tx| {
                let mode: String = tx.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
                let fk: i64 = tx.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
                Ok((mode, fk))
            })
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(fk, 1);
    }

    #[test]
    fn reads_see_committed_writes() {
        let (_dir, db) = open_temp();
        db.write(|tx| {
            tx.execute("INSERT INTO settings (key, value) VALUES ('a', '1')", [])?;
            Ok(())
        })
        .unwrap();
        let value: String = db
            .read(|conn| {
                Ok(
                    conn.query_row("SELECT value FROM settings WHERE key = 'a'", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(value, "1");
    }

    #[test]
    fn failed_write_rolls_back() {
        let (_dir, db) = open_temp();
        let result: Result<()> = db.write(|tx| {
            tx.execute("INSERT INTO settings (key, value) VALUES ('a', '1')", [])?;
            Err(CoreError::Invalid("わざと失敗".into()))
        });
        assert!(result.is_err());
        let count: i64 = db
            .read(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn reads_from_many_threads_while_writing() {
        let (_dir, db) = open_temp();
        let db = std::sync::Arc::new(db);
        let mut handles = Vec::new();
        for i in 0..8 {
            let db = db.clone();
            handles.push(std::thread::spawn(move || {
                db.write(move |tx| {
                    tx.execute(
                        "INSERT INTO settings (key, value) VALUES (?1, '1')",
                        [format!("k{i}")],
                    )?;
                    Ok(())
                })
                .unwrap();
                db.read(|conn| {
                    Ok(conn
                        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get::<_, i64>(0))?)
                })
                .unwrap()
            }));
        }
        for h in handles {
            assert!(h.join().unwrap() >= 1);
        }
        let count: i64 = db
            .read(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(count, 8);
    }

    #[test]
    fn write_after_close_fails() {
        let (_dir, db) = open_temp();
        db.close();
        assert!(matches!(db.write(|_| Ok(())), Err(CoreError::DbClosed)));
    }

    // ===== 開くときのマイグレーション（当てる前のフック・WAL・失敗の言葉。docs/data-model.md §5）=====

    use crate::db::testing::one;

    /// 1 つ前の版の DB をファイルに作り、子のある行を入れる（WAL に書いたまま閉じない形にはしない）。
    fn previous_version(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("library.sqlite");
        let mut conn = Connection::open(&path).unwrap();
        apply_pragmas(&conn).unwrap();
        migrate_until(&mut conn, MIGRATIONS.len() - 1).unwrap();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (1, x'0101010101010101010101010101010101010101010101010101010101010101', 'jpg', 'image', 3, '夕方', 'a.jpg', 1, 1);
             INSERT INTO tags (id, name, created_at) VALUES (1, '海', 1);
             INSERT INTO item_tags (tag_id, item_id) VALUES (1, 1);",
        )
        .unwrap();
        path
    }

    fn version_of(path: &Path) -> usize {
        applied(&Connection::open(path).unwrap()).unwrap()
    }

    fn wal_len(path: &Path) -> u64 {
        let wal = format!("{}-wal", path.display());
        std::fs::metadata(wal).map(|m| m.len()).unwrap_or(0)
    }

    #[test]
    fn hook_runs_only_before_migrating_an_existing_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = previous_version(dir.path());
        let mut seen = None;
        let (db, report) = Db::open_with(&path, |conn, from| {
            // 当てる前の状態が見える
            seen = Some((from, applied(conn)?));
            Ok(())
        })
        .unwrap();
        let previous = MIGRATIONS.len() - 1;
        assert_eq!(seen, Some((previous, previous)));
        assert_eq!((report.from, report.to), (previous, MIGRATIONS.len()));
        assert_eq!(report.applied.len(), 1);
        // 当てたら WAL を本体へ移して空にする（次に開くときに読み直さない）
        assert_eq!(wal_len(&path), 0);
        drop(db);
        // 当て済みの DB と、新しい DB では呼ばない
        let (_db, report) =
            Db::open_with(&path, |_, _| panic!("当て済みの DB でフックを呼んだ")).unwrap();
        assert!(report.applied.is_empty());
        let fresh = dir.path().join("fresh.sqlite");
        let (_db, report) =
            Db::open_with(&fresh, |_, _| panic!("新しい DB でフックを呼んだ")).unwrap();
        assert_eq!(report.to, MIGRATIONS.len());
    }

    #[test]
    fn a_failed_backup_leaves_the_library_unmigrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = previous_version(dir.path());
        let full = || CoreError::Io(std::io::Error::from(std::io::ErrorKind::StorageFull));
        let err = Db::open_with(&path, |_, _| Err(full())).err().unwrap();
        assert!(
            matches!(err, CoreError::Upgrade(UpgradeError::BackupNoSpace { .. })),
            "{err}"
        );
        assert_eq!(version_of(&path), MIGRATIONS.len() - 1);
        let err = Db::open_with(&path, |_, _| Err(CoreError::Invalid("x".into())))
            .err()
            .unwrap();
        assert!(
            matches!(err, CoreError::Upgrade(UpgradeError::Backup { .. })),
            "{err}"
        );
        assert_eq!(version_of(&path), MIGRATIONS.len() - 1);
    }

    #[test]
    fn a_failed_migration_keeps_the_old_version_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = previous_version(dir.path());
        let failing = Migration {
            name: "9999_fails",
            sql: "DELETE FROM item_tags; SELECT * FROM no_such_table;",
            kind: Kind::Plain,
        };
        let list: Vec<Migration> = MIGRATIONS.iter().copied().chain([failing]).collect();
        let mut hooked = false;
        let err = Db::open_list(&path, &list, |_, _| {
            hooked = true;
            Ok(())
        })
        .err()
        .unwrap();
        assert!(hooked, "当てる前のフックは呼ぶ");
        match &err {
            CoreError::Upgrade(UpgradeError::Apply { migration, .. }) => {
                assert_eq!(migration, "9999_fails");
            }
            other => panic!("{other}"),
        }
        // 途中まで当てた分（1 つ前の版からの最新）は残り、失敗した分は戻る
        let conn = Connection::open(&path).unwrap();
        assert_eq!(applied(&conn).unwrap(), MIGRATIONS.len());
        assert_eq!(one::<i64>(&conn, "SELECT count(*) FROM item_tags"), 1);
    }

    #[test]
    fn newer_schema_is_refused_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        let err = Db::open_with(&path, |_, _| panic!("新しい版の DB でフックを呼んだ"))
            .err()
            .unwrap();
        assert!(
            matches!(
                err,
                CoreError::Upgrade(UpgradeError::Newer { found: 99, .. })
            ),
            "{err}"
        );
        assert_eq!(version_of(&path), 99);
    }
}
