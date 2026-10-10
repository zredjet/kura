//! ライブラリ（`*.kuralib` フォルダ）の作成・オープン。

mod integrity;
mod layout;
mod location;
mod session;

pub use integrity::{IntegrityReport, check_files, clean_staging};
pub use layout::{DB_FILE, Layout, SUBDIRS, content_rel_path, hex, parse_hex32};
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
