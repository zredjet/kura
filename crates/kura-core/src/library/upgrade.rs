//! ライブラリの DB を新しい版にするとき（docs/data-model.md §5）の、空き容量の確かめと知らせ（M11 の決定 1）。
//!
//! - 当てる前に、DB の大きさから要る空きを見積もり（[`space_needed`]）、足りなければバックアップも取らずに断る
//!   （`UpgradeError::NoSpace`。ライブラリは変えない）。空きはアプリが渡す（[`UpgradeHooks::free_space`]。macOS は
//!   kura-mac の `volume_capacity`。渡さなければ確かめない）。
//! - 始まり・終わり・失敗を [`UpgradeHooks::notify`] で知らせる（アプリはイベントにしてフロントへ送り、フロントは 300ms を
//!   超えたら「ライブラリを更新しています」を出す）。始まりを知らせたら、終わりか失敗のどちらかを必ず 1 つ知らせる。
//!   新しい DB（版 0）と、当てるものがない DB では何も知らせない（新しい版の DB で断るときも、始まりがないので知らせない。
//!   理由は開く処理の戻り値で返る）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;

use crate::{Result, UpgradeError};

/// 見積もりの余裕（小さな DB でも、SQLite の一時の領域とバックアップの置き場の分を見る）。
pub const SPACE_MARGIN: u64 = 64 << 20;

/// ボリュームの使える空き（バイト）を返す関数。分からなければ None（確かめない）。
pub type FreeSpaceFn = dyn Fn(&Path) -> Option<u64> + Send + Sync;

/// 知らせを受ける関数。開く処理のスレッドで同期に呼ぶので、すぐ返すこと。
pub type UpgradeNotifyFn = dyn Fn(&UpgradeEvent) + Send + Sync;

/// DB を新しい版にするときにアプリから渡すもの（どちらも省ける）。
#[derive(Clone, Default)]
pub struct UpgradeHooks {
    /// `path`（ライブラリのフォルダ）のあるボリュームの使える空き。
    pub free_space: Option<Arc<FreeSpaceFn>>,
    /// 始まり・終わり・失敗の知らせ。
    pub notify: Option<Arc<UpgradeNotifyFn>>,
}

impl UpgradeHooks {
    /// 知らせる（受け手がなければ何もしない）。
    pub(crate) fn notify(&self, event: &UpgradeEvent) {
        if let Some(notify) = &self.notify {
            notify(event);
        }
    }

    /// 当てる前に空きを確かめる。足りなければ `UpgradeError::NoSpace`。
    pub(crate) fn ensure_space(&self, root: &Path, db_bytes: u64) -> Result<()> {
        let Some(free_space) = &self.free_space else {
            return Ok(());
        };
        let needed = space_needed(db_bytes);
        match free_space(root) {
            Some(available) if available < needed => {
                tracing::warn!(
                    db_bytes,
                    needed,
                    available,
                    "空き容量が足りないので、ライブラリを新しい版にしません"
                );
                Err(UpgradeError::NoSpace { needed, available }.into())
            }
            Some(_) => Ok(()),
            None => {
                tracing::warn!(path = %root.display(), "ボリュームの空きを読めないので、確かめずに当てます");
                Ok(())
            }
        }
    }
}

/// DB を新しい版にする知らせ。`path` はライブラリのフォルダ、`from`・`to` はスキーマの版（user_version）。
#[derive(Debug, Clone)]
pub enum UpgradeEvent {
    /// 当て始めた（空きの確かめとバックアップの前）。`db_bytes` は DB の大きさ（頁の数 × 頁の大きさ）。
    Started {
        path: PathBuf,
        from: usize,
        to: usize,
        db_bytes: u64,
    },
    /// 当て終えた（バックアップ・マイグレーション・WAL を空にするまで。`ms` は始まりからの時間）。
    Finished {
        path: PathBuf,
        from: usize,
        to: usize,
        ms: f64,
    },
    /// 当てられなかった（失敗した 1 つは戻り、それより前に当てたものは残る。今の版は `error` が持つ。`ms` は始まりからの時間）。
    Failed {
        path: PathBuf,
        from: usize,
        to: usize,
        ms: f64,
        error: UpgradeError,
    },
}

/// 当てるのに要る空きの見積もり：当てる前のバックアップ（DB と同じ大きさ）＋作り直す表（DB の中と WAL に 1 つずつ。合わせて
/// 多くて DB の 2 倍）＋[`SPACE_MARGIN`]。10 万件の seed（DB 51.8MB）で測った最大の増え方は 119.6MB（DB の 2.31 倍：
/// バックアップ 51.8MB、DB の伸び 34.1MB、WAL の伸び 33.7MB）。
pub fn space_needed(db_bytes: u64) -> u64 {
    db_bytes.saturating_mul(3).saturating_add(SPACE_MARGIN)
}

/// DB の大きさ（頁の数 × 頁の大きさ。WAL にある頁を含む。バックアップはこの大きさになる）。
pub(crate) fn db_bytes(conn: &Connection) -> Result<u64> {
    let pages: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    Ok(u64::try_from(pages)
        .unwrap_or(0)
        .saturating_mul(u64::try_from(size).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::library::{Library, backup};
    use crate::{CoreError, UpgradeError};

    /// 0006 の DB（すべての表に行がある）を持つライブラリのフォルダを作る。
    fn v6_library(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        let mut conn = Connection::open(root.join(crate::library::DB_FILE)).unwrap();
        crate::db::apply_pragmas(&conn).unwrap();
        crate::db::migrate_until(&mut conn, 6).unwrap();
        crate::db::testing::fill_v6(&conn);
    }

    fn version_of(root: &Path) -> usize {
        crate::db::applied(&Connection::open(root.join(crate::library::DB_FILE)).unwrap()).unwrap()
    }

    /// 知らせを覚える。
    fn recording(free: Option<u64>) -> (UpgradeHooks, Arc<Mutex<Vec<UpgradeEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let hooks = UpgradeHooks {
            free_space: free.map(|free| Arc::new(move |_: &Path| Some(free)) as Arc<FreeSpaceFn>),
            notify: Some(Arc::new(move |e: &UpgradeEvent| {
                seen.lock().unwrap().push(e.clone());
            })),
        };
        (hooks, events)
    }

    /// 既存のライブラリを新しい版にするときは、始まりと終わりを 1 つずつ知らせる。新しいライブラリ・当て済みのライブラリでは
    /// 何も知らせない。
    #[test]
    fn upgrading_tells_started_then_finished() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        v6_library(&root);
        let (hooks, events) = recording(Some(u64::MAX));
        let lib = Library::open_with(&root, &hooks).unwrap();
        let to = crate::db::MIGRATIONS.len();
        {
            let events = events.lock().unwrap();
            match events.as_slice() {
                [
                    UpgradeEvent::Started {
                        path,
                        from: 6,
                        to: t1,
                        db_bytes,
                    },
                    UpgradeEvent::Finished {
                        path: p2,
                        from: 6,
                        to: t2,
                        ms,
                    },
                ] => {
                    assert_eq!((path, p2), (&root, &root));
                    assert_eq!((*t1, *t2), (to, to));
                    assert!(*db_bytes > 0);
                    assert!(*ms > 0.0);
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(lib.migration().upgraded());
        assert!(lib.migration().total_ms() > 0.0);
        drop(lib);

        events.lock().unwrap().clear();
        let lib = Library::open_with(&root, &hooks).unwrap();
        assert!(!lib.migration().upgraded());
        drop(lib);
        drop(Library::create(&dir.path().join("New")).unwrap());
        assert!(events.lock().unwrap().is_empty());
    }

    /// 空きが見積もりに足りなければ、バックアップも取らずに断り（版はそのまま）、失敗を知らせる。空きができれば開ける。
    #[test]
    fn short_space_refuses_before_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        v6_library(&root);
        let (hooks, events) = recording(Some(1 << 20));
        let err = Library::open_with(&root, &hooks).err().unwrap();
        match &err {
            CoreError::Upgrade(e @ UpgradeError::NoSpace { needed, available }) => {
                assert_eq!(e.kind(), "noSpace");
                assert_eq!(*available, 1 << 20);
                assert!(*needed > SPACE_MARGIN);
            }
            other => panic!("{other}"),
        }
        assert_eq!(version_of(&root), 6);
        assert!(
            backup::list(&crate::library::Layout::new(&root))
                .unwrap()
                .is_empty()
        );
        {
            let events = events.lock().unwrap();
            assert!(
                matches!(
                    events.as_slice(),
                    [
                        UpgradeEvent::Started { from: 6, .. },
                        UpgradeEvent::Failed {
                            from: 6,
                            error: UpgradeError::NoSpace { .. },
                            ..
                        }
                    ]
                ),
                "{events:?}"
            );
        }

        let (hooks, _) = recording(Some(u64::MAX));
        let lib = Library::open_with(&root, &hooks).unwrap();
        assert_eq!(lib.migration().from, 6);
        assert_eq!(backup::list(lib.layout()).unwrap().len(), 1);
    }

    /// 後ろのマイグレーションで失敗したら、それより前に当てたものは残って版は途中まで上がる。言葉はそれを言い（前の版の
    /// Kura では開けない・当てる前のバックアップがある）、「前の版のまま」と言わない。SQL の本文は入れない（D2）。原因を
    /// 取り除けば、続きを当てて開ける。
    #[test]
    fn a_late_failure_says_the_library_is_now_at_a_middle_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        v6_library(&root);
        // 0009 の CREATE TABLE media_facts を失敗させる（実際にはディスクの空きや I/O の誤り）
        Connection::open(root.join(crate::library::DB_FILE))
            .unwrap()
            .execute_batch("CREATE TABLE media_facts (x);")
            .unwrap();
        let (hooks, events) = recording(None);
        let err = Library::open_with(&root, &hooks).err().unwrap();
        assert_eq!(version_of(&root), 8, "0007・0008 は残る");
        let text = err.to_string();
        assert!(!text.contains("前の版のまま"), "{text}");
        assert!(text.contains("スキーマ 8"), "{text}");
        assert!(text.contains("前の版の Kura"), "{text}");
        assert!(text.contains("バックアップ"), "{text}");
        assert!(text.contains("0009_analysis"), "{text}");
        assert!(text.contains("media_facts already exists"), "{text}");
        assert!(
            !text.contains("CREATE TABLE"),
            "SQL の本文を入れない：{text}"
        );
        assert!(text.chars().count() < 400, "{text}");
        {
            let events = events.lock().unwrap();
            match events.as_slice() {
                [
                    UpgradeEvent::Started { from: 6, .. },
                    UpgradeEvent::Failed { from: 6, error, .. },
                ] => assert_eq!(error.to_string(), text),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            backup::list(&crate::library::Layout::new(&root))
                .unwrap()
                .len(),
            1
        );
        // 原因を取り除けば、続き（0009）を当てて開ける
        Connection::open(root.join(crate::library::DB_FILE))
            .unwrap()
            .execute_batch("DROP TABLE media_facts;")
            .unwrap();
        let lib = Library::open(&root).unwrap();
        assert_eq!(
            (lib.migration().from, lib.migration().to),
            (8, crate::db::MIGRATIONS.len())
        );
    }

    /// 当てる途中の失敗も知らせる（理由はそのまま）。
    #[test]
    fn a_failed_migration_tells_failed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        v6_library(&root);
        Connection::open(root.join(crate::library::DB_FILE))
            .unwrap()
            .execute_batch("CREATE TABLE blobs (x);")
            .unwrap();
        let (hooks, events) = recording(None);
        assert!(Library::open_with(&root, &hooks).is_err());
        let events = events.lock().unwrap();
        assert!(
            matches!(
                events.as_slice(),
                [
                    UpgradeEvent::Started { .. },
                    UpgradeEvent::Failed {
                        error: UpgradeError::Apply { .. },
                        ..
                    }
                ]
            ),
            "{events:?}"
        );
        assert_eq!(version_of(&root), 6);
    }

    /// 別のプロセス（Kura を 2 つ動かした・開発版と本番版）が同じ前の版のライブラリを同時に開いても、どちらも開け、版は
    /// 最新のまま（下がらない）で、次もふつうに開ける（D1。ここではスレッドごとの接続で 2 つのプロセスの代わりにする。
    /// アプリの中の鍵 `AppState::opening` は通らない）。2 つ目を 0〜4 ms 遅らせ、先に開いた側が当てている途中に版を読ませる。
    /// ふだんはライブラリのロック（`library.lock`）が 2 つ目を断るので、ここでは flock が使えないファイルシステムと同じく
    /// ロックなしで開く（`LibraryLock::unlocked`。そのときはマイグレーションの読み直しだけが守る）。
    #[test]
    fn two_processes_opening_the_same_library_at_once_both_open_and_keep_the_version() {
        let latest = crate::db::MIGRATIONS.len();
        for round in 0..40u64 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("L.kuralib");
            v6_library(&root);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let (root, barrier) = (root.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        if i == 1 {
                            std::thread::sleep(std::time::Duration::from_millis(round % 5));
                        }
                        crate::library::LibraryLock::unlocked(&root)
                            .and_then(|lock| Library::open_locked(lock, &UpgradeHooks::default()))
                            .map(|lib| lib.migration().to)
                            .map_err(|e| e.to_string().chars().take(200).collect::<String>())
                    })
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().unwrap(), Ok(latest), "round {round}");
            }
            assert_eq!(version_of(&root), latest, "round {round}：版が下がった");
            let lib = Library::open(&root).unwrap();
            assert!(lib.migration().applied.is_empty(), "round {round}");
            let tags: i64 = lib
                .db()
                .read(|c| Ok(c.query_row("SELECT count(*) FROM item_tags", [], |r| r.get(0))?))
                .unwrap();
            assert_eq!(tags, 3, "round {round}");
            // 当てる前の中身（版 6）のバックアップが残る。遅れた側が先の側の当てている途中の版を読んだときは、秒がずれれば
            // その版のバックアップも残る（同じ秒なら版 6 のものを置き換えない）
            let versions: Vec<usize> = backup::list(lib.layout())
                .unwrap()
                .iter()
                .map(|b| crate::db::applied(&Connection::open(&b.path).unwrap()).unwrap())
                .collect();
            assert!(versions.contains(&6), "round {round}: {versions:?}");
            assert!(
                versions.iter().all(|v| *v < latest),
                "round {round}: {versions:?}"
            );
        }
    }

    /// ほかのアプリがライブラリのロックを持っていれば、前の版のライブラリでも `LibraryInUse` で断り、何もしない：知らせない・
    /// フォルダを作り直さない・空きを確かめない・バックアップを取らない・当てない。放せば、ふつうに新しい版にして開ける。
    #[test]
    fn a_library_held_by_another_app_is_refused_before_anything_happens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        v6_library(&root);
        assert!(!root.join("thumbs").exists());
        let other = crate::library::LibraryLock::acquire(&root).unwrap();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut hooks, events) = recording(Some(u64::MAX));
        let seen = asked.clone();
        hooks.free_space = Some(Arc::new(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(u64::MAX)
        }));
        let err = Library::open_with(&root, &hooks).err().unwrap();
        assert!(matches!(err, CoreError::LibraryInUse), "{err}");
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(!root.join("thumbs").exists(), "フォルダを作り直さない");
        assert!(
            backup::list(&crate::library::Layout::new(&root))
                .unwrap()
                .is_empty()
        );
        assert_eq!(version_of(&root), 6);

        drop(other);
        let lib = Library::open_with(&root, &hooks).unwrap();
        assert_eq!(lib.migration().from, 6);
        assert!(root.join("thumbs").is_dir());
        assert_eq!(backup::list(lib.layout()).unwrap().len(), 1);
        assert_eq!(events.lock().unwrap().len(), 2);
        // 開いている間は、同じプロセスで開き直そうとしても断る
        assert!(matches!(
            Library::open(&root).err(),
            Some(CoreError::LibraryInUse)
        ));
    }

    /// 新しい版の Kura の DB は、当て始めないので知らせず、理由を戻り値で返す。
    #[test]
    fn a_newer_schema_is_refused_without_starting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        drop(Library::create(&root).unwrap());
        Connection::open(root.join(crate::library::DB_FILE))
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        let (hooks, events) = recording(Some(0));
        let err = Library::open_with(&root, &hooks).err().unwrap();
        assert!(
            matches!(
                err,
                CoreError::Upgrade(UpgradeError::Newer { found: 99, .. })
            ),
            "{err}"
        );
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn space_needed_covers_the_backup_and_the_rebuild() {
        assert_eq!(space_needed(0), SPACE_MARGIN);
        // 10 万件の seed で測った増え方（119.6MB）を覆う
        let seed = 51_834_880;
        assert!(space_needed(seed) > 119_631_728 + (8 << 20));
        assert_eq!(space_needed(u64::MAX), u64::MAX);
    }

    #[test]
    fn db_bytes_counts_pages() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (zeroblob(100000));")
            .unwrap();
        let size: i64 = conn
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .unwrap();
        let bytes = db_bytes(&conn).unwrap();
        assert!(bytes >= 100_000, "{bytes}");
        assert_eq!(bytes % u64::try_from(size).unwrap(), 0);
    }

    #[test]
    fn ensure_space_refuses_only_when_the_volume_says_it_is_short() {
        let root = Path::new("/lib");
        let with = |free: Option<u64>| UpgradeHooks {
            free_space: Some(Arc::new(move |_| free)),
            notify: None,
        };
        assert!(UpgradeHooks::default().ensure_space(root, 1 << 40).is_ok());
        assert!(with(None).ensure_space(root, 1 << 40).is_ok());
        let needed = space_needed(1 << 20);
        assert!(with(Some(needed)).ensure_space(root, 1 << 20).is_ok());
        let err = with(Some(needed - 1))
            .ensure_space(root, 1 << 20)
            .unwrap_err();
        match err {
            CoreError::Upgrade(e) => {
                assert_eq!(e.kind(), "noSpace");
                assert!(e.to_string().contains("空き容量"), "{e}");
            }
            other => panic!("{other:?}"),
        }
    }
}
