//! SQLite へのアクセス。書き込みは専用スレッド 1 本に集約し、読み取りは読み取り専用の接続を使い回す。

mod migrate;

pub use migrate::{MIGRATIONS, migrate};

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
    /// DB を開き（なければ作り）、マイグレーションを適用する。
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        apply_pragmas(&conn)?;
        migrate(&mut conn)?;

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
}
