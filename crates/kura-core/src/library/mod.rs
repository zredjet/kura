//! ライブラリ（`*.kuralib` フォルダ）の作成・オープン。

pub mod backup;
mod integrity;
mod layout;
pub mod ledger;
mod location;
mod session;
pub mod upgrade;

pub use integrity::{IntegrityReport, check_files, clean_staging};
pub use layout::{
    ANIM_V, DB_FILE, Layout, SUBDIRS, ThumbFormat, ThumbSize, content_rel_path, hex, parse_hex32,
};
pub use location::{
    LIBRARY_EXT, LocationCheck, LocationStatus, check_location, is_library, normalize_library_path,
};
pub use session::{EventSink, LibraryInfo, LibrarySession};
pub use upgrade::{UpgradeEvent, UpgradeHooks};

use std::cell::Cell;
use std::path::Path;
use std::sync::{PoisonError, RwLock, RwLockReadGuard};
use std::time::Instant;

use crate::db::{Db, MigrationReport};
use crate::{CoreError, Result};

/// `path` のライブラリを開くとマイグレーションを当てるか（開く前に「ライブラリを更新しています」を出すのに使う）。
/// DB がなければ false。読み取り専用で版（user_version）だけを読む。
pub fn needs_upgrade(path: &Path) -> Result<bool> {
    let db = path.join(DB_FILE);
    if !db.is_file() {
        return Ok(false);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let version = crate::db::applied(&conn)?;
    Ok(version > 0 && version < crate::db::MIGRATIONS.len())
}

/// 開いているライブラリ。フォルダ構成と DB を持つ。
pub struct Library {
    layout: Layout,
    db: Db,
    /// 開いたときに当てたマイグレーション。
    migration: MigrationReport,
    /// 取り込み（files/ に置いてから DB に書くまで）とゴミ箱を空にする処理を直列にする鍵。
    /// 取り込みどうしは並べてよい（読み取りの側）。ゴミ箱を空にする処理は 1 つだけ（書き込みの側）。
    ingest: RwLock<()>,
}

impl Library {
    /// 新しいライブラリを作る（拡張子 `.kuralib` は補う）。何かがすでにある場所には作らない。
    pub fn create(path: &Path) -> Result<Self> {
        let path = normalize_library_path(path);
        match check_location(&path, None).status {
            LocationStatus::New => {}
            LocationStatus::ExistingLibrary => {
                return Err(CoreError::Conflict(format!(
                    "すでにライブラリがあります: {}",
                    path.display()
                )));
            }
            LocationStatus::Occupied => {
                return Err(CoreError::Conflict(format!(
                    "ライブラリではないファイルがあります: {}",
                    path.display()
                )));
            }
        }
        std::fs::create_dir_all(&path)?;
        // 新しい DB（版 0）には当てる前の手順がないので、空きの確かめと知らせは要らない
        Self::open_dir(&path, &UpgradeHooks::default())
    }

    /// 既存のライブラリを開く。足りないフォルダは作り直し、マイグレーションを適用する。
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, &UpgradeHooks::default())
    }

    /// 既存のライブラリを開く。新しい版にするときは、当てる前に空きを確かめ（`hooks.free_space`）、始まり・終わり・失敗を
    /// 知らせる（`hooks.notify`。`upgrade` の冒頭）。
    pub fn open_with(path: &Path, hooks: &UpgradeHooks) -> Result<Self> {
        if !is_library(path) {
            return Err(CoreError::NotFound(format!(
                "ライブラリが見つかりません: {}",
                path.display()
            )));
        }
        Self::open_dir(path, hooks)
    }

    fn open_dir(path: &Path, hooks: &UpgradeHooks) -> Result<Self> {
        let layout = Layout::new(path);
        for dir in SUBDIRS {
            std::fs::create_dir_all(layout.root().join(dir))?;
        }
        let to = crate::db::MIGRATIONS.len();
        // 当て始めた時刻と版（フックが呼ばれた = 既存の DB に未適用のものがある）
        let started: Cell<Option<(Instant, usize)>> = Cell::new(None);
        // 未適用のマイグレーションがある既存の DB は、空きを確かめ、当てる前にバックアップを取る（どちらかが失敗したら
        // 当てずに開くのを失敗にする）
        let opened = Db::open_with(&layout.db_path(), |conn, from| {
            started.set(Some((Instant::now(), from)));
            let db_bytes = upgrade::db_bytes(conn)?;
            hooks.notify(&UpgradeEvent::Started {
                path: layout.root().to_path_buf(),
                from,
                to,
                db_bytes,
            });
            hooks.ensure_space(layout.root(), db_bytes)?;
            backup::before_migration(&layout, conn, crate::time::now_ms()).map(|_| ())
        });
        if let Some((at, from)) = started.get() {
            let (path, ms) = (
                layout.root().to_path_buf(),
                at.elapsed().as_secs_f64() * 1000.0,
            );
            match &opened {
                Err(CoreError::Upgrade(error)) => hooks.notify(&UpgradeEvent::Failed {
                    path,
                    from,
                    to,
                    ms,
                    error: error.clone(),
                }),
                // 当てたあとの失敗（読み取りの接続・書き込みのスレッドの用意）は、更新は終わっている。理由は戻り値で返る
                Ok(_) | Err(_) => hooks.notify(&UpgradeEvent::Finished { path, from, to, ms }),
            }
        }
        let (db, migration) = opened?;
        Ok(Self {
            layout,
            db,
            migration,
            ingest: RwLock::new(()),
        })
    }

    /// 取り込みの鍵：files/ にファイルを置いてから DB に書き終えるまで持つ（取り込み・ダウンロード・キャプチャ）。
    /// 持っている間はゴミ箱を空にする処理が待つので、同じ中身を置いたファイルを消されない（docs/data-model.md §4）。
    /// 同じスレッドで重ねて取らないこと。
    pub fn ingest_guard(&self) -> RwLockReadGuard<'_, ()> {
        self.ingest.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// 開いたときに当てたマイグレーション（当てていなければ `applied` が空）。
    pub fn migration(&self) -> &MigrationReport {
        &self.migration
    }

    /// フォルダ構成。
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// DB。
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// 表示名（フォルダ名から拡張子を除いたもの）。
    pub fn name(&self) -> String {
        self.layout
            .root()
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// ゴミ箱を除いたアイテム数。
    pub fn item_count(&self) -> Result<u64> {
        self.db.read(|conn| {
            let mut stmt =
                conn.prepare_cached("SELECT COUNT(*) FROM items WHERE trashed_at IS NULL")?;
            let count: i64 = stmt.query_row([], |row| row.get(0))?;
            Ok(u64::try_from(count).unwrap_or(0))
        })
    }

    /// ゴミ箱を空にする。DB の行を消して、使われなくなった台帳の行を同じトランザクションで解放し、コミットのあとで
    /// 解放した中身の実ファイルと派生のファイル（サムネイル・ジャケット・プロキシ・波形・アニメの帯）を消す
    /// （ADR-46。途中で落ちてもファイルが残るだけで、整合性チェックで「DB にないファイル」として数えられる）。
    /// 取り込みの鍵を書き込みの側で持つので、置いたばかりの同じ中身のファイルを消さない。
    pub fn empty_trash(&self) -> Result<TrashEmptied> {
        let _ingest = self.ingest.write().unwrap_or_else(PoisonError::into_inner);
        let deleted = self.db.write(|tx| crate::items::delete_trashed(tx))?;
        let mut result = TrashEmptied::default();
        for blob in &deleted.released {
            let mut paths = self.layout.derived_paths(&hex(&blob.hash));
            paths.insert(0, self.layout.content_path(&blob.hash, &blob.ext));
            for path in paths {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        result.failed += 1;
                        tracing::warn!(%err, path = %path.display(), "ゴミ箱のファイルを消せませんでした");
                    }
                }
            }
        }
        for item in &deleted.items {
            result.items += 1;
            result.bytes += u64::try_from(item.size_bytes).unwrap_or(0);
        }
        Ok(result)
    }
}

/// ゴミ箱を空にした結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrashEmptied {
    /// 消したアイテムの数。
    pub items: u64,
    /// 消したアイテムの合計サイズ。
    pub bytes: u64,
    /// 消せなかったファイルの数。
    pub failed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_trash_removes_rows_and_files_but_keeps_others() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let (gone, kept) = ([1u8; 32], [2u8; 32]);
        for (id, hash) in [(1, gone), (2, kept)] {
            let content = lib.layout().content_path(&hash, "jpg");
            std::fs::create_dir_all(content.parent().unwrap()).unwrap();
            std::fs::write(&content, b"0123456789").unwrap();
            let thumb = lib.layout().thumb_path(&hex(&hash), ThumbSize::Small);
            std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
            std::fs::write(&thumb, b"t").unwrap();
            lib.db()
                .write(move |tx| {
                    tx.execute(
                        "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                         VALUES (?1, ?2, 'jpg', 'image', 10, 't', 't', 0, 0)",
                        rusqlite::params![id, &hash[..]],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        lib.db()
            .write(|tx| crate::items::trash(tx, &[1], 5))
            .unwrap();

        let result = lib.empty_trash().unwrap();
        assert_eq!(
            result,
            TrashEmptied {
                items: 1,
                bytes: 10,
                failed: 0
            }
        );
        assert!(!lib.layout().content_path(&gone, "jpg").exists());
        assert!(
            !lib.layout()
                .thumb_path(&hex(&gone), ThumbSize::Small)
                .exists()
        );
        assert!(lib.layout().content_path(&kept, "jpg").exists());
        assert_eq!(lib.item_count().unwrap(), 1);
        assert_eq!(lib.empty_trash().unwrap(), TrashEmptied::default());
    }

    #[test]
    fn empty_trash_releases_ledger_rows_facts_and_peaks() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let (gone, kept) = ([1u8; 32], [2u8; 32]);
        let put = |path: std::path::PathBuf| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        };
        for (id, hash) in [(1, gone), (2, kept)] {
            put(lib.layout().content_path(&hash, "png"));
            for path in lib.layout().derived_paths(&hex(&hash)) {
                put(path);
            }
            lib.db()
                .write(move |tx| {
                    tx.execute(
                        "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                         VALUES (?1, ?2, 'png', 'image', 10, 't', 't', 0, 0)",
                        rusqlite::params![id, &hash[..]],
                    )?;
                    tx.execute(
                        "INSERT INTO media_facts (blob_id, state, facts_v, alpha, analyzed_at)
                         SELECT id, 'ok', 1, 2, 1 FROM blobs WHERE hash = ?1",
                        [&hash[..]],
                    )?;
                    tx.execute(
                        "INSERT INTO analysis_results (blob_id, analysis, version, params, encoding, output_len, digest, created_at)
                         SELECT id, 'audio.peaks', 1, '{}', 'binary', 0, x'00', 1 FROM blobs WHERE hash = ?1",
                        [&hash[..]],
                    )?;
                    tx.execute(
                        "INSERT INTO asset_settings (item_id, pixel_art, updated_at) VALUES (?1, 1, 1)",
                        [id],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        lib.db()
            .write(|tx| crate::items::trash(tx, &[1], 5))
            .unwrap();

        let result = lib.empty_trash().unwrap();
        assert_eq!((result.items, result.bytes, result.failed), (1, 10, 0));
        // 台帳の行が解放され、中身ごとの事実と結果、アイテムごとの設定も消える
        let counts: (i64, i64, i64, i64) = lib
            .db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT (SELECT count(*) FROM blobs), (SELECT count(*) FROM media_facts),
                            (SELECT count(*) FROM analysis_results), (SELECT count(*) FROM asset_settings)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .unwrap();
        assert_eq!(counts, (1, 1, 1, 1));
        // 実ファイルと、png・jpg のサムネイル・ジャケット・プロキシ・波形・アニメの帯を消す。残すものは残す
        assert!(!lib.layout().content_path(&gone, "png").exists());
        for path in lib.layout().derived_paths(&hex(&gone)) {
            assert!(!path.exists(), "{} が残った", path.display());
        }
        assert!(lib.layout().content_path(&kept, "png").exists());
        for path in lib.layout().derived_paths(&hex(&kept)) {
            assert!(path.exists(), "{} が消えた", path.display());
        }
        let (missing, orphans) = check_files(lib.db(), lib.layout()).unwrap();
        assert_eq!((missing, orphans), (0, 0));
    }

    #[test]
    fn empty_trash_waits_for_an_ingest_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let lib = std::sync::Arc::new(Library::create(&dir.path().join("L")).unwrap());
        let guard = lib.ingest_guard();
        let (tx, rx) = std::sync::mpsc::channel();
        let lib2 = lib.clone();
        let handle = std::thread::spawn(move || {
            let result = lib2.empty_trash();
            tx.send(()).unwrap();
            result.map(|_| ())
        });
        // 取り込みが鍵を持っている間は始まらない
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err()
        );
        drop(guard);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn create_makes_layout_and_db() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("Kura Library")).unwrap();
        let root = dir.path().join("Kura Library.kuralib");
        assert_eq!(lib.layout().root(), root);
        assert_eq!(lib.name(), "Kura Library");
        for sub in SUBDIRS {
            assert!(root.join(sub).is_dir(), "{sub} がない");
        }
        assert!(root.join(DB_FILE).is_file());
        assert_eq!(lib.item_count().unwrap(), 0);
    }

    #[test]
    fn create_refuses_existing_things() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("A.kuralib");
        drop(Library::create(&path).unwrap());
        assert!(matches!(
            Library::create(&path),
            Err(CoreError::Conflict(_))
        ));

        let busy = dir.path().join("B.kuralib");
        std::fs::create_dir(&busy).unwrap();
        std::fs::write(busy.join("x.txt"), "x").unwrap();
        assert!(matches!(
            Library::create(&busy),
            Err(CoreError::Conflict(_))
        ));
    }

    #[test]
    fn open_requires_a_library_and_restores_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Library::open(dir.path()),
            Err(CoreError::NotFound(_))
        ));

        let path = dir.path().join("A.kuralib");
        drop(Library::create(&path).unwrap());
        std::fs::remove_dir(path.join("staging")).unwrap();
        let lib = Library::open(&path).unwrap();
        assert!(lib.layout().staging_dir().is_dir());
    }
}
