//! ライブラリ（`*.kuralib` フォルダ）の作成・オープン。

mod integrity;
mod layout;
mod location;
mod session;

pub use integrity::{IntegrityReport, check_files, clean_staging};
pub use layout::{DB_FILE, Layout, SUBDIRS, ThumbSize, content_rel_path, hex, parse_hex32};
pub use location::{
    LIBRARY_EXT, LocationCheck, LocationStatus, check_location, is_library, normalize_library_path,
};
pub use session::{EventSink, LibraryInfo, LibrarySession};

use std::path::Path;

use crate::db::Db;
use crate::{CoreError, Result};

/// 開いているライブラリ。フォルダ構成と DB を持つ。
pub struct Library {
    layout: Layout,
    db: Db,
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
        Self::open_dir(&path)
    }

    /// 既存のライブラリを開く。足りないフォルダは作り直し、マイグレーションを適用する。
    pub fn open(path: &Path) -> Result<Self> {
        if !is_library(path) {
            return Err(CoreError::NotFound(format!(
                "ライブラリが見つかりません: {}",
                path.display()
            )));
        }
        Self::open_dir(path)
    }

    fn open_dir(path: &Path) -> Result<Self> {
        let layout = Layout::new(path);
        for dir in SUBDIRS {
            std::fs::create_dir_all(layout.root().join(dir))?;
        }
        let db = Db::open(&layout.db_path())?;
        Ok(Self { layout, db })
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

    /// ゴミ箱を空にする。DB の行を先に消し、そのあと実ファイル・サムネイル・ジャケット・プロキシを消す
    /// （途中で落ちてもファイルが残るだけで、整合性チェックで「DB にないファイル」として数えられる）。
    pub fn empty_trash(&self) -> Result<TrashEmptied> {
        let removed = self.db.write(|tx| crate::items::delete_trashed(tx))?;
        let mut result = TrashEmptied::default();
        for item in &removed {
            let hash_hex = hex(&item.hash);
            let paths = [
                self.layout.content_path(&item.hash, &item.ext),
                self.layout.thumb_path(&hash_hex, ThumbSize::Small),
                self.layout.thumb_path(&hash_hex, ThumbSize::Large),
                self.layout.art_path(&hash_hex),
                self.layout.proxy_path(&hash_hex),
            ];
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
